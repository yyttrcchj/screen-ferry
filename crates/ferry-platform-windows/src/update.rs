//! 自己更新：`Updater` 的 Windows 实现（ADR 0014）。
//!
//! 更新包就是安装包（`ScreenFerry-<版本>-windows-x64-setup.exe`，ADR 0011）。安装时以管理员身份
//! 静默运行它：弹一次 UAC，安装包请正在运行的后台退出，装好后以管理员权限重新启动它。
//!
//! - 程序自己下载的文件没有“来自网络”的标记，SmartScreen 不会拦。
//! - 从核对完到安装包开始运行，一直以只读、不许别人写的方式打开着它，中途换不掉。
//! - 不是用安装包装的（例如开发时直接运行）不能自己更新：安装目录里没有卸载程序 `unins000.exe`。

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;

use ferry_platform::{Error, Prepared, Result, Updater};
use windows::Win32::Foundation::{ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW,
    RegQueryValueExW,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, w};

/// 安装包的静默安装参数：不显示界面和消息框，装完不重启电脑。
const SILENT: &str = "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-";

pub struct WinUpdater {
    /// 核对过的安装包，在它开始运行前一直打开着，别人写不了、删不掉。
    held: Mutex<Option<File>>,
}

impl WinUpdater {
    pub fn new() -> Self {
        Self {
            held: Mutex::new(None),
        }
    }
}

impl Default for WinUpdater {
    fn default() -> Self {
        Self::new()
    }
}

impl Updater for WinUpdater {
    fn platform(&self) -> Option<&'static str> {
        cfg!(target_arch = "x86_64").then_some("windows-x86_64")
    }

    fn can_install(&self) -> Result<()> {
        let exe = std::env::current_exe().map_err(|e| Error::Failed(e.to_string()))?;
        let installed = exe
            .parent()
            .is_some_and(|dir| dir.join("unins000.exe").is_file());
        if installed {
            Ok(())
        } else {
            Err(Error::Unsupported("不是用安装包装的"))
        }
    }

    fn proxy(&self) -> Option<String> {
        let key = InternetSettings::open()?;
        if key.dword(w!("ProxyEnable")) != Some(1) {
            return None;
        }
        parse_proxy_server(&key.string(w!("ProxyServer"))?)
    }

    fn prepare(&self, package: &Path, _version: &str) -> Result<Prepared> {
        self.can_install()?;
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(package)
            .map_err(|e| Error::Failed(format!("打不开安装包：{e}")))?;
        *self.held.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
        Ok(Prepared {
            target: package.to_owned(),
            backup: None,
            restart_self: false,
        })
    }

    fn launch(&self, prepared: &Prepared) -> Result<()> {
        let file = HSTRING::from(prepared.target.as_os_str());
        let params = HSTRING::from(SILENT);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: info 里的字符串都以 0 结尾，在调用期间一直有效；cbSize 是结构体的实际大小。
        let result = unsafe { ShellExecuteExW(&mut info) };
        // 安装包已经开始运行（或者没运行起来），不用再拦着了。
        self.held.lock().unwrap_or_else(|e| e.into_inner()).take();
        match result {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => Err(Error::Cancelled),
            Err(e) => Err(Error::Os {
                call: "ShellExecuteExW",
                detail: e.to_string(),
            }),
        }
    }

    fn confirm(&self, _prepared: &Prepared) {}

    fn rollback(&self, _prepared: &Prepared) -> Result<()> {
        Ok(())
    }
}

/// `ProxyServer` 的值：`主机:端口`，或者按协议分开的 `http=主机:端口;https=主机:端口`。
/// 取 HTTPS 的（没有时取 HTTP 的，再没有时取不分协议的），写成 `http://主机:端口`。
pub fn parse_proxy_server(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let pick = |scheme: &str| {
        value.split(';').find_map(|part| {
            let (k, v) = part.split_once('=')?;
            k.trim().eq_ignore_ascii_case(scheme).then(|| v.trim())
        })
    };
    let server = if value.contains('=') {
        pick("https").or_else(|| pick("http"))?
    } else {
        value
    };
    let server = server.trim_start_matches("http://");
    (!server.is_empty()).then(|| format!("http://{server}"))
}

/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`：系统的代理设置。
struct InternetSettings(HKEY);

impl InternetSettings {
    fn open() -> Option<Self> {
        let mut key = HKEY::default();
        // SAFETY: 路径是以 0 结尾的常量字符串，key 指向有效的输出位置。
        let opened = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings"),
                None,
                KEY_QUERY_VALUE,
                &mut key,
            )
        };
        opened.ok().ok().map(|()| Self(key))
    }

    fn query(&self, name: PCWSTR) -> Option<(REG_VALUE_TYPE, Vec<u8>)> {
        let mut kind = REG_VALUE_TYPE::default();
        let mut len = 0u32;
        // SAFETY: 值名以 0 结尾；先只问长度。
        let found =
            unsafe { RegQueryValueExW(self.0, name, None, Some(&mut kind), None, Some(&mut len)) };
        if found == ERROR_FILE_NOT_FOUND || found != WIN32_ERROR(0) {
            return None;
        }
        let mut data = vec![0u8; len as usize];
        // SAFETY: data 的长度就是上面问到的长度。
        let read = unsafe {
            RegQueryValueExW(
                self.0,
                name,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut len),
            )
        };
        read.ok().ok()?;
        data.truncate(len as usize);
        Some((kind, data))
    }

    fn dword(&self, name: PCWSTR) -> Option<u32> {
        let (_, data) = self.query(name)?;
        Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?))
    }

    fn string(&self, name: PCWSTR) -> Option<String> {
        let (_, data) = self.query(name)?;
        let wide: Vec<u16> = data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        Some(String::from_utf16_lossy(&wide))
    }
}

impl Drop for InternetSettings {
    fn drop(&mut self) {
        // SAFETY: 键由 RegOpenKeyExW 打开，只关闭一次。
        let _ = unsafe { RegCloseKey(self.0) };
    }
}
