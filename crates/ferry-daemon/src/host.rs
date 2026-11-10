//! 本机能力：按目标平台创建平台对象，以及只有应用才知道怎么做的事（打开设置界面、读计算机名）。

use std::path::PathBuf;

/// 指定设置界面位置的环境变量，调试时用。
const SETTINGS_ENV: &str = "SCREENFERRY_SETTINGS_APP";

/// 按顺序找设置界面：环境变量、安装包里的位置、开发时 Flutter 的构建产物。
fn settings_app(candidates: &[PathBuf]) -> Option<PathBuf> {
    std::env::var_os(SETTINGS_ENV)
        .map(PathBuf::from)
        .into_iter()
        .chain(candidates.iter().cloned())
        .find(|p| p.exists())
}

/// 登录时执行的命令：本程序加上 `--autostart`。
#[cfg(target_os = "macos")]
fn autostart_command() -> Option<Vec<String>> {
    let exe = exe_path()?;
    Some(vec![exe.display().to_string(), "--autostart".into()])
}

/// 可执行文件所在的目录。
fn exe_dir() -> Option<PathBuf> {
    exe_path()?.parent().map(PathBuf::from)
}

/// 系统的界面语言：中文的用简体中文，其他用英文。
fn system_language() -> ferry_types::Lang {
    sys_locale::get_locale().map_or(ferry_types::Lang::En, |tag| {
        ferry_types::Lang::from_tag(&tag)
    })
}

/// 处理器架构，写进诊断文件：`arm64`、`x64` 等。
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    }
}

/// 本程序的完整路径。
fn exe_path() -> Option<PathBuf> {
    Some(plain_path(
        std::env::current_exe().ok()?.canonicalize().ok()?,
    ))
}

/// Windows 上 `canonicalize` 得到的是 `\\?\C:\…` 这种扩展长度路径，写进注册表的启动项、
/// 交给别的程序都可能认不出来，去掉这个前缀。其他系统上原样返回。
fn plain_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

#[cfg(target_os = "macos")]
pub(crate) use ferry_platform_macos::MacEventLoop as NativeEventLoop;
#[cfg(target_os = "macos")]
pub(crate) use mac::NativeHost;

#[cfg(target_os = "macos")]
mod mac {
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};

    use ferry_engine::resident::Host;
    use ferry_platform::{Autostart, Error, Permissions, Result};
    use ferry_platform_macos::{
        MacAutostart, MacCapture, MacClipboard, MacDisplays, MacInject, MacOverlay, MacPermissions,
    };
    use ferry_types::Role;
    use tracing::warn;

    use super::{autostart_command, exe_dir, settings_app};

    /// LaunchAgent 的标识，与安装包的 bundle id 一致。
    const BUNDLE_ID: &str = "io.github.icloudza.screenferry";

    pub(crate) struct NativeHost {
        autostart: Option<MacAutostart>,
        /// 是否已经向系统申请过辅助功能权限。
        prompted: AtomicBool,
    }

    impl NativeHost {
        pub(crate) fn new(_data_dir: Option<&Path>) -> Self {
            let autostart =
                autostart_command().and_then(|args| MacAutostart::new(BUNDLE_ID, args).ok());
            Self {
                autostart,
                prompted: AtomicBool::new(false),
            }
        }
    }

    impl Host for NativeHost {
        type Capture = MacCapture;
        type Inject = MacInject;
        type Displays = MacDisplays;
        type Clipboard = MacClipboard;
        type Overlay = MacOverlay;
        type Updater = ferry_platform_macos::MacUpdater;

        fn device_name(&self) -> String {
            Command::new("scutil")
                .args(["--get", "ComputerName"])
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Mac".into())
        }

        fn check_permissions(&self, _role: Role) -> Result<()> {
            // 控制别的电脑（读取键鼠）和被控制（发出键鼠）都要辅助功能权限。
            // 第一次发现缺权限时向系统申请一次：系统弹出提示，并把本程序加进授权列表。
            if self.prompted.swap(true, Ordering::SeqCst) {
                MacPermissions.check_input()
            } else {
                MacPermissions.request_input()
            }
        }

        fn capture(&self) -> Result<MacCapture> {
            Ok(MacCapture::new())
        }

        fn inject(&self) -> Result<MacInject> {
            MacInject::new()
        }

        fn displays(&self) -> MacDisplays {
            MacDisplays
        }

        fn clipboard(&self) -> MacClipboard {
            MacClipboard::new()
        }

        fn overlay(&self) -> MacOverlay {
            MacOverlay::new()
        }

        fn updater(&self) -> ferry_platform_macos::MacUpdater {
            ferry_platform_macos::MacUpdater::new()
        }

        fn autostart(&self) -> bool {
            self.autostart
                .as_ref()
                .is_some_and(|a| a.is_enabled().unwrap_or(false))
        }

        fn set_autostart(&self, on: bool) -> Result<()> {
            self.autostart
                .as_ref()
                .ok_or(Error::Unsupported("找不到程序的位置"))?
                .set_enabled(on)
        }

        fn open_settings(&self) {
            let candidates: Vec<_> = exe_dir()
                .map(|dir| {
                    vec![
                        // 安装包：ScreenFerry.app/Contents/MacOS/screenferry 旁边的 Helpers。
                        dir.join("../Helpers/ScreenFerry.app"),
                        // 开发：target/<profile>/screenferry 与 app/ 的构建产物。
                        dir.join("../../app/build/macos/Build/Products/Release/ScreenFerry.app"),
                        dir.join("../../app/build/macos/Build/Products/Debug/ScreenFerry.app"),
                    ]
                })
                .unwrap_or_default();
            match settings_app(&candidates) {
                // `open` 会把已经打开的设置界面带到前台。
                Some(app) => {
                    if let Err(e) = Command::new("open").arg(&app).spawn() {
                        warn!("打开设置界面失败：{e}");
                    }
                }
                None => warn!("找不到设置界面，可以用环境变量 SCREENFERRY_SETTINGS_APP 指定"),
            }
        }

        fn open_permission_settings(&self) {
            // 先申请一次，确保本程序出现在授权列表里。
            let _ = MacPermissions.request_input();
            let url =
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
            if let Err(e) = Command::new("open").arg(url).spawn() {
                warn!("打开系统设置失败：{e}");
            }
        }

        fn natural_scrolling(&self) -> Option<bool> {
            Some(ferry_platform_macos::natural_scrolling())
        }

        fn system_language(&self) -> ferry_types::Lang {
            super::system_language()
        }

        fn system(&self) -> String {
            let version = Command::new("sw_vers")
                .arg("-productVersion")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default();
            match version.as_str() {
                "" => format!("macOS · {}", super::arch()),
                v => format!("macOS {v} · {}", super::arch()),
            }
        }

        fn utc_offset(&self) -> i32 {
            crate::logging::utc_offset()
        }

        fn set_verbose(&self, on: bool) {
            crate::logging::set_verbose(on);
        }

        fn reveal(&self, path: &Path) {
            if let Err(e) = Command::new("open").arg("-R").arg(path).spawn() {
                warn!("在访达中显示失败：{e}");
            }
        }

        fn open_folder(&self, path: &Path) {
            if let Err(e) = Command::new("open").arg(path).spawn() {
                warn!("打开文件夹失败：{e}");
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) use ferry_platform_windows::WinEventLoop as NativeEventLoop;
#[cfg(target_os = "windows")]
pub(crate) use win::NativeHost;

#[cfg(target_os = "windows")]
mod win {
    use std::ffi::OsString;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use ferry_engine::resident::Host;
    use ferry_platform::{Autostart, Error, Permissions, Result};
    use ferry_platform_windows::{
        WinAutostart, WinCapture, WinClipboard, WinDisplays, WinInject, WinOverlay, WinPermissions,
    };
    use ferry_types::Role;
    use tracing::{info, warn};

    use super::{exe_dir, exe_path, settings_app};

    pub(crate) struct NativeHost {
        autostart: Option<WinAutostart>,
    }

    impl NativeHost {
        pub(crate) fn new(data_dir: Option<&Path>) -> Self {
            let autostart = exe_path().map(|exe| {
                let mut arguments = Vec::new();
                if let Some(dir) = data_dir {
                    arguments.push(OsString::from("--data-dir"));
                    arguments.push(dir.as_os_str().to_owned());
                }
                arguments.push(OsString::from("--autostart"));
                WinAutostart::new(ferry_platform_windows::TASK_NAME, exe, arguments)
            });
            // 开着自启时按本程序现在的位置重新登记：换了安装位置（例如从解压的文件夹换成安装版）
            // 以后，登录时启动的仍是正在用的这一份。
            if let Some(a) = &autostart
                && a.is_enabled().unwrap_or(false)
                && let Err(e) = a.set_enabled(true)
            {
                warn!("更新开机自启的登记失败：{e}");
            }
            Self { autostart }
        }
    }

    impl Host for NativeHost {
        type Capture = WinCapture;
        type Inject = WinInject;
        type Displays = WinDisplays;
        type Clipboard = WinClipboard;
        type Overlay = WinOverlay;
        type Updater = ferry_platform_windows::WinUpdater;

        fn device_name(&self) -> String {
            std::env::var("COMPUTERNAME")
                .ok()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Windows".into())
        }

        fn check_permissions(&self, _role: Role) -> Result<()> {
            WinPermissions.check_input()
        }

        fn capture(&self) -> Result<WinCapture> {
            Ok(WinCapture::new())
        }

        fn inject(&self) -> Result<WinInject> {
            Ok(WinInject::new())
        }

        fn displays(&self) -> WinDisplays {
            WinDisplays::new()
        }

        fn clipboard(&self) -> WinClipboard {
            WinClipboard::new()
        }

        fn overlay(&self) -> WinOverlay {
            WinOverlay::new()
        }

        fn updater(&self) -> ferry_platform_windows::WinUpdater {
            ferry_platform_windows::WinUpdater::new()
        }

        fn autostart(&self) -> bool {
            self.autostart
                .as_ref()
                .is_some_and(|a| a.is_enabled().unwrap_or(false))
        }

        fn set_autostart(&self, on: bool) -> Result<()> {
            self.autostart
                .as_ref()
                .ok_or(Error::Unsupported("找不到程序的位置"))?
                .set_enabled(on)
        }

        fn open_settings(&self) {
            // 安装目录：screenferry.exe 旁边的 settings\screenferry-settings.exe。
            // 设置界面自己保证只开一个窗口：已经开着时，新启动的那个把旧窗口带到最前面就退出。
            let candidates: Vec<_> = exe_dir()
                .map(|dir| vec![dir.join("settings").join("screenferry-settings.exe")])
                .unwrap_or_default();
            let Some(exe) = settings_app(&candidates) else {
                warn!("找不到设置界面，可以用环境变量 SCREENFERRY_SETTINGS_APP 指定");
                return;
            };
            info!(path = %exe.display(), "打开设置界面");
            ferry_platform_windows::allow_foreground();
            // 后台以管理员身份运行；直接启动会让设置界面继承管理员令牌。交给资源管理器启动，
            // 新进程就是普通权限，和从开始菜单打开时一样。
            let result = if ferry_platform_windows::is_process_elevated().unwrap_or(false) {
                let explorer = std::env::var_os("SystemRoot")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
                    .join("explorer.exe");
                Command::new(explorer)
                    .arg(&exe)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            } else {
                let mut command = Command::new(&exe);
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                if let Some(dir) = exe.parent() {
                    command.current_dir(dir);
                }
                command.spawn()
            };
            if let Err(e) = result {
                warn!("打开设置界面失败：{e}");
            }
        }

        fn open_permission_settings(&self) {}

        fn system_language(&self) -> ferry_types::Lang {
            super::system_language()
        }

        fn system(&self) -> String {
            // `ver` 输出“Microsoft Windows [版本 10.0.26100.4652]”：各种语言的系统里这串数字都一样。
            // 不开控制台窗口，免得闪一下。
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let output = Command::new("cmd")
                .args(["/c", "ver"])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let version = output
                .split(|c: char| !(c.is_ascii_digit() || c == '.'))
                .find(|t| t.matches('.').count() >= 2)
                .unwrap_or("");
            let build: u32 = version
                .split('.')
                .nth(2)
                .and_then(|b| b.parse().ok())
                .unwrap_or(0);
            let name = match build {
                0 => "Windows",
                b if b >= 22000 => "Windows 11",
                _ => "Windows 10",
            };
            match version {
                "" => format!("{name} · {}", super::arch()),
                v => format!("{name}（{v}）· {}", super::arch()),
            }
        }

        fn utc_offset(&self) -> i32 {
            crate::logging::utc_offset()
        }

        fn set_verbose(&self, on: bool) {
            crate::logging::set_verbose(on);
        }

        fn reveal(&self, path: &Path) {
            ferry_platform_windows::allow_foreground();
            let select = format!("/select,\"{}\"", path.display());
            if let Err(e) = Command::new("explorer").raw_arg(select).spawn() {
                warn!("在资源管理器中显示失败：{e}");
            }
        }

        fn open_folder(&self, path: &Path) {
            ferry_platform_windows::allow_foreground();
            if let Err(e) = Command::new("explorer").arg(path).spawn() {
                warn!("打开文件夹失败：{e}");
            }
        }
    }
}
