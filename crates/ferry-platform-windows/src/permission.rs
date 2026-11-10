//! 系统权限：`Permissions` 的 Windows 实现。
//!
//! 普通键鼠注入不需要额外授权。管理员权限的窗口不接受普通权限进程注入的输入，因此常驻后台
//! 会以管理员权限运行；命令行子命令不提升。

use ferry_platform::{Permissions, Result};
use tracing::warn;

/// 本机权限检查。
#[derive(Clone, Copy, Debug, Default)]
pub struct WinPermissions;

impl Permissions for WinPermissions {
    fn check_input(&self) -> Result<()> {
        if !crate::elevation::is_process_elevated().unwrap_or(false) {
            warn!(
                "当前 ScreenFerry 不是管理员权限；以管理员身份运行的窗口可能无法接收键鼠。\
                 常驻后台会自动提升，命令行调试时可以手动以管理员身份启动。"
            );
        }
        Ok(())
    }
}
