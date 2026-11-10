//! 开机自启：`Autostart` 的 Windows 实现。
//!
//! 在任务计划程序里登记一条“登录时启动、最高权限运行”的当前用户任务。后台必须以管理员
//! 身份运行才能操作管理员窗口；计划任务在登录时直接给出完整令牌，不会弹 UAC。
//!
//! 旧版本把启动项写在 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，那里只能以普通
//! 权限启动。发现旧登记时会自动换成计划任务并删掉旧值。

use std::ffi::OsStr;
use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use ferry_platform::{Autostart, Error, Result};
use tracing::{info, warn};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SAM_FLAGS, RegCloseKey,
    RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
};
use windows::core::{PCWSTR, w};

const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const LEGACY_NAME: PCWSTR = w!("ScreenFerry");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 任务计划程序里的任务名，登记、查询、删除和手动启动都用它。
pub const TASK_NAME: &str = "ScreenFerry";

/// 请任务计划程序立刻运行已经登记的任务。
///
/// 任务以最高权限登记，由任务计划程序启动，不会弹 UAC。任务不存在或已被禁用时返回错误，
/// 调用方再退回 [`crate::relaunch_elevated`]。`schtasks /Run` 只表示“已经转交给任务计划
/// 程序”，不表示程序真的起来了，调用方仍需确认新进程接管了。
pub fn run_task(name: &str) -> Result<()> {
    let output = command()
        .args([OsStr::new("/Run"), OsStr::new("/TN"), OsStr::new(name)])
        .output()
        .map_err(|e| Error::Os {
            call: "schtasks.exe",
            detail: e.to_string(),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(failed("启动计划任务失败", &output))
    }
}

pub struct WinAutostart {
    /// 任务计划程序里的任务名。
    name: String,
    /// 登录时启动的程序。
    exe: PathBuf,
    /// 程序参数，也就是常驻模式的 `--autostart`。
    arguments: Vec<OsString>,
}

impl WinAutostart {
    pub fn new(name: &str, exe: PathBuf, arguments: Vec<OsString>) -> Self {
        let autostart = Self {
            name: name.to_owned(),
            exe,
            arguments,
        };
        autostart.migrate_legacy();
        autostart
    }

    /// 旧版本用的是当前用户的 Run 键。发现旧登记时换成计划任务；换不了就留着，
    /// 至少不会把用户的开机自启关掉。
    fn migrate_legacy(&self) {
        if !legacy_enabled().unwrap_or(false) {
            return;
        }
        if self.is_enabled().unwrap_or(false) {
            if let Err(e) = remove_legacy() {
                warn!("删除旧的 Windows 开机自启登记失败：{e}");
            }
            return;
        }
        match self.set_enabled(true) {
            Ok(()) => info!("已把 Windows 开机自启从注册表换成最高权限的计划任务"),
            Err(e) => warn!("旧的开机自启登记换成计划任务失败：{e}"),
        }
    }

    /// 计划任务要执行的完整命令行。
    fn task_command(&self) -> String {
        let mut command = Vec::with_capacity(self.arguments.len() + 1);
        command.push(self.exe.as_os_str().to_owned());
        command.extend(self.arguments.iter().cloned());
        crate::elevation::command_line(&command)
    }

    fn register(&self) -> Result<()> {
        let task_command = self.task_command();
        let output = command()
            .args([
                OsStr::new("/Create"),
                OsStr::new("/SC"),
                OsStr::new("ONLOGON"),
                OsStr::new("/TN"),
                OsStr::new(&self.name),
                OsStr::new("/TR"),
                OsStr::new(&task_command),
                OsStr::new("/RL"),
                OsStr::new("HIGHEST"),
                OsStr::new("/IT"),
                OsStr::new("/F"),
            ])
            .output();
        match output {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(failed("登记开机自启失败", &output)),
            Err(e) => Err(Error::Os {
                call: "schtasks.exe",
                detail: e.to_string(),
            }),
        }?;
        remove_legacy()
    }

    fn unregister(&self) -> Result<()> {
        let output = command()
            .args([
                OsStr::new("/Delete"),
                OsStr::new("/TN"),
                OsStr::new(&self.name),
                OsStr::new("/F"),
            ])
            .output();
        match output {
            Ok(output) if output.status.success() => {}
            Ok(_) if !self.is_enabled()? => {}
            Ok(output) => return Err(failed("删除开机自启失败", &output)),
            Err(e) => {
                return Err(Error::Os {
                    call: "schtasks.exe",
                    detail: e.to_string(),
                });
            }
        }
        remove_legacy()
    }
}

impl Autostart for WinAutostart {
    fn is_enabled(&self) -> Result<bool> {
        let output = command()
            .args([
                OsStr::new("/Query"),
                OsStr::new("/TN"),
                OsStr::new(&self.name),
            ])
            .output()
            .map_err(|e| Error::Os {
                call: "schtasks.exe",
                detail: e.to_string(),
            })?;
        Ok(output.status.success())
    }

    fn set_enabled(&self, enabled: bool) -> Result<()> {
        if enabled {
            self.register()
        } else {
            self.unregister()
        }
    }
}

fn command() -> Command {
    let mut command = Command::new(schtasks());
    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null());
    command
}

/// 用绝对路径，避免当前目录里的同名程序被优先执行。
fn schtasks() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("schtasks.exe")
}

fn failed(action: &str, output: &Output) -> Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Error::Failed(if detail.is_empty() {
        format!("{action}（schtasks 返回 {}）", output.status)
    } else {
        format!("{action}：{detail}")
    })
}

/// 旧版本在当前用户的 Run 键下登记的开机自启。
fn legacy_enabled() -> Result<bool> {
    let Some(key) = open_run(KEY_QUERY_VALUE)? else {
        return Ok(false);
    };
    // SAFETY: 值名以 0 结尾；只问是否存在，不读数据。
    let found = unsafe { RegQueryValueExW(key.0, LEGACY_NAME, None, None, None, None) };
    match found {
        e if e == ERROR_FILE_NOT_FOUND => Ok(false),
        e => check("RegQueryValueExW", e).map(|()| true),
    }
}

fn remove_legacy() -> Result<()> {
    let Some(key) = open_run(KEY_SET_VALUE)? else {
        return Ok(());
    };
    // SAFETY: 值名以 0 结尾。
    match unsafe { RegDeleteValueW(key.0, LEGACY_NAME) } {
        e if e == ERROR_FILE_NOT_FOUND => Ok(()),
        e => check("RegDeleteValueW", e),
    }
}

fn open_run(access: REG_SAM_FLAGS) -> Result<Option<RunKey>> {
    let mut key = HKEY::default();
    // SAFETY: RUN 是以 0 结尾的常量字符串，key 指向有效的输出位置。
    let opened = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN, None, access, &mut key) };
    match opened {
        e if e == ERROR_FILE_NOT_FOUND => Ok(None),
        e => check("RegOpenKeyExW", e).map(|()| Some(RunKey(key))),
    }
}

/// 用完自动关闭的注册表键。
struct RunKey(HKEY);

impl Drop for RunKey {
    fn drop(&mut self) {
        // SAFETY: 键由 RegOpenKeyExW 打开，只关闭一次。
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn check(call: &'static str, result: WIN32_ERROR) -> Result<()> {
    result.ok().map_err(|e| Error::Os {
        call,
        detail: e.to_string(),
    })
}
