//! 进程权限：查询进程是否已提升、用 UAC 重新启动自己，以及显示启动失败的原因。
//!
//! Windows 的 UIPI 不允许中等完整性的进程向管理员窗口注入输入。后台常驻进程需要以管理员
//! 身份运行；命令行子命令没有这个要求，所以程序清单仍然保持 `asInvoker`，只在常驻模式启动时
//! 按需提升。开机自启由 `WinAutostart` 登记为最高权限的计划任务，登录时不会再弹 UAC。

use std::ffi::{OsStr, OsString};

use ferry_platform::{Error, Result};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, MB_ICONERROR, MB_OK, MB_SETFOREGROUND,
    MessageBoxW, SW_SHOWNORMAL,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::display::last_error;

/// 当前进程是否拥有完整的管理员令牌。
pub fn is_process_elevated() -> Result<bool> {
    let mut token = HANDLE::default();
    // SAFETY: 只读查询本进程令牌；伪句柄不需要关闭，打开出来的令牌由 `token_is_elevated` 关闭。
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|_| last_error("OpenProcessToken"))?;
    token_is_elevated(token)
}

/// 指定进程是否拥有完整的管理员令牌。
///
/// 同一个用户可以查询以管理员身份运行的进程；权限不够时返回错误，调用方应当把它当成
/// “无法确认”，不要据此直接放行或拒绝键鼠。
pub fn process_is_elevated(pid: u32) -> Result<bool> {
    // SAFETY: 只请求查询权限，不修改目标进程；句柄在函数返回前关闭。
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|_| last_error("OpenProcess"))?;
    let mut token = HANDLE::default();
    // SAFETY: 进程句柄有效，令牌句柄由 `token_is_elevated` 关闭。
    let result = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }
        .map_err(|_| last_error("OpenProcessToken"))
        .and_then(|()| token_is_elevated(token));
    // SAFETY: `process` 是 OpenProcess 打开的独立句柄，只关闭一次。
    let _ = unsafe { CloseHandle(process) };
    result
}

/// 当前前台窗口是否属于管理员进程。
///
/// `SendInput` 被 UIPI 拦下时不保证设置 `GetLastError`，所以发送失败后用它判断
/// 是否正对着管理员窗口，给用户一条能照着处理的错误信息。查询失败按“不确定”处理，
/// 不影响原来的错误返回。
pub fn foreground_is_elevated() -> Result<bool> {
    // SAFETY: 只读取当前前台窗口和它的进程号，不修改任何窗口。
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return Ok(false);
    }
    let mut pid = 0;
    // SAFETY: `pid` 是有效的输出位置；窗口句柄来自系统。
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
    }
    if pid == 0 {
        return Ok(false);
    }
    process_is_elevated(pid)
}

fn token_is_elevated(token: HANDLE) -> Result<bool> {
    let mut elevation = TOKEN_ELEVATION::default();
    let mut needed = 0;
    // SAFETY: `elevation` 是大小正确的本地缓冲区，令牌由调用方保证有效。
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut needed,
        )
    };
    let error = result
        .as_ref()
        .err()
        .map(|_| last_error("GetTokenInformation"));
    // SAFETY: 令牌句柄由本函数接管，只关闭一次。
    let _ = unsafe { CloseHandle(token) };
    if let Some(error) = error {
        return Err(error);
    }
    Ok(elevation.TokenIsElevated != 0)
}

/// 通过 UAC 以管理员身份启动同一份程序，并保持原来的工作目录。
///
/// 这里不等待新进程：常驻后台不会退出。UAC 被取消时返回 [`Error::Cancelled`]。
pub fn relaunch_elevated(args: &[OsString]) -> Result<()> {
    let exe =
        std::env::current_exe().map_err(|e| Error::Failed(format!("找不到本程序的位置：{e}")))?;
    let file = HSTRING::from(exe.as_os_str());
    let parameters = HSTRING::from(command_line(args).as_str());
    let directory = std::env::current_dir()
        .ok()
        .map(|path| HSTRING::from(path.as_os_str()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        lpDirectory: directory
            .as_ref()
            .map_or(PCWSTR::null(), |path| PCWSTR(path.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: 结构体里的字符串都在本次调用期间有效，cbSize 是结构体的实际大小。
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => Err(Error::Cancelled),
        Err(e) => Err(Error::Os {
            call: "ShellExecuteExW",
            detail: e.message(),
        }),
    }
}

/// 给用户看启动失败的原因。后台没有控制台，只写日志等于什么也没说。
pub fn show_error(message: &str) {
    let message = HSTRING::from(message);
    // SAFETY: 字符串在调用期间一直有效；消息框只读它，返回后立即释放。
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            w!("ScreenFerry"),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

/// 把参数拼成 Windows 命令行，按 `CommandLineToArgvW` 的规则加引号。
pub fn command_line(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| quote(arg.as_os_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 给单个参数加引号。始终加引号，空参数和含空白的路径都不需要调用方另做判断。
pub(crate) fn quote(arg: &OsStr) -> String {
    let text = arg.to_string_lossy();
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in text.chars() {
        match ch {
            '\\' => {
                quoted.push('\\');
                backslashes += 1;
            }
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push(ch);
                backslashes = 0;
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes));
    quoted.push('"');
    quoted
}
