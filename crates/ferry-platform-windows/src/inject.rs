//! 输入注入：`InputInject` 的 Windows 实现，基于 `SendInput`。
//!
//! 绝对移动使用整个虚拟桌面的归一化坐标（0–65535），换算时落在目标像素的中心，避免差一像素。
//! 键盘用扫描码注入，不受键盘布局和输入法影响。
//! 光标在对方屏幕上时，本机光标停到一个看不见的位置（见 [`hider`]）。

pub(crate) mod hider;

use ferry_platform::{Error, InputInject, Result};
use ferry_types::{InputEvent, KeyAction, KeyCode, LockKeys, MouseButton, Point};
use windows::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_CAPITAL, VK_NUMLOCK, VK_SCROLL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    XBUTTON1, XBUTTON2,
};

use crate::display::ensure_dpi_aware;
use crate::keymap::{WinKey, win_key};
use hider::CursorHider;

/// 本机键鼠注入。
#[derive(Debug)]
pub struct WinInject {
    /// 第一次需要隐藏光标时才创建。
    hider: Option<CursorHider>,
}

impl WinInject {
    pub fn new() -> Self {
        ensure_dpi_aware();
        Self { hider: None }
    }
}

impl Default for WinInject {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInject for WinInject {
    fn inject(&mut self, event: &InputEvent) -> Result<()> {
        match *event {
            InputEvent::PointerMotion { position } => {
                let (x, y) = normalize(position);
                send(mouse(
                    x,
                    y,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                ))
            }
            InputEvent::PointerDelta { dx, dy } => send(mouse(dx, dy, 0, MOUSEEVENTF_MOVE)),
            InputEvent::Button {
                button, pressed, ..
            } => {
                let (flags, data) = button_flags(button, pressed)
                    .ok_or(Error::Unsupported("5 号以上的鼠标按键"))?;
                send(mouse(0, 0, data, flags))
            }
            InputEvent::Wheel { dx, dy, .. } => {
                // mouseData 是有符号数按位存放。
                if dy != 0 {
                    send(mouse(0, 0, dy as u32, MOUSEEVENTF_WHEEL))?;
                }
                if dx != 0 {
                    send(mouse(0, 0, dx as u32, MOUSEEVENTF_HWHEEL))?;
                }
                Ok(())
            }
            InputEvent::Key { code, action } => {
                let key = win_key(code).ok_or(Error::Unsupported("没有对应 Windows 按键的键"))?;
                send(keyboard(key, action == KeyAction::Up))
            }
        }
    }

    fn sync_locks(&mut self, wanted: LockKeys, mask: LockKeys) -> Result<()> {
        let locks = [
            (LockKeys::CAPS, VK_CAPITAL, KeyCode::CAPS_LOCK),
            (LockKeys::NUM, VK_NUMLOCK, KeyCode::NUM_LOCK),
            (LockKeys::SCROLL, VK_SCROLL, KeyCode::SCROLL_LOCK),
        ];
        for (lock, vk, code) in locks {
            if !mask.contains(lock) {
                continue;
            }
            // SAFETY: 只读查询键盘状态；最低位表示锁定键是否打开。
            let on = unsafe { GetKeyState(i32::from(vk.0)) } & 1 != 0;
            if on != wanted.contains(lock) {
                let key = win_key(code).ok_or(Error::Unsupported("锁定键"))?;
                send(keyboard(key, false))?;
                send(keyboard(key, true))?;
            }
        }
        Ok(())
    }

    fn set_cursor_hidden(&mut self, hidden: bool) -> Result<()> {
        let hider = match &self.hider {
            Some(hider) => hider,
            // 从没藏过，也就不需要再显示。
            None if !hidden => return Ok(()),
            None => self.hider.insert(CursorHider::start()?),
        };
        hider.set_hidden(hidden)
    }
}

fn keyboard(key: WinKey, up: bool) -> INPUT {
    let (vk, scan, mut flags) = match key {
        WinKey::Scan {
            code,
            extended: false,
        } => (VIRTUAL_KEY(0), code, KEYEVENTF_SCANCODE),
        WinKey::Scan {
            code,
            extended: true,
        } => (
            VIRTUAL_KEY(0),
            code,
            KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY,
        ),
        WinKey::Virtual(vk) => (VIRTUAL_KEY(vk), 0, KEYBD_EVENT_FLAGS(0)),
        WinKey::Media { vk, scan } => (VIRTUAL_KEY(vk), scan, KEYEVENTF_EXTENDEDKEY),
    };
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(input: INPUT) -> Result<()> {
    // SAFETY: 传入一个完整初始化的 INPUT，大小参数与之一致。
    let sent = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
    if sent == 1 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        // SendInput 被 UIPI 拦下时，Windows 文档不保证 GetLastError 会说明原因；
        // 再看一眼前台窗口，至少把“管理员窗口在前台”这个最常见的情况说清楚。
        let uipi_blocked = match (
            crate::elevation::is_process_elevated(),
            crate::elevation::foreground_is_elevated(),
        ) {
            (Ok(false), Ok(true)) => true,
            _ => error.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32),
        };
        if uipi_blocked {
            Err(Error::PermissionDenied(
                "当前前台窗口以管理员身份运行，ScreenFerry 后台需要管理员权限才能控制它",
            ))
        } else {
            Err(Error::Os {
                call: "SendInput",
                detail: error.to_string(),
            })
        }
    }
}

fn button_flags(button: MouseButton, pressed: bool) -> Option<(MOUSE_EVENT_FLAGS, u32)> {
    let pick = |down, up| if pressed { down } else { up };
    Some(match button.number() {
        1 => (pick(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP), 0),
        2 => (pick(MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP), 0),
        3 => (pick(MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP), 0),
        4 => (
            pick(MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP),
            u32::from(XBUTTON1),
        ),
        5 => (
            pick(MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP),
            u32::from(XBUTTON2),
        ),
        _ => return None,
    })
}

/// 虚拟桌面坐标 → `SendInput` 的归一化坐标。
fn normalize(p: Point) -> (i32, i32) {
    // SAFETY: 只读查询系统度量。
    let (left, top, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    (to_unit(p.x, left, width), to_unit(p.y, top, height))
}

/// 把 `[start, start + extent)` 中的像素映射到 0–65535，取像素中心。
fn to_unit(v: i32, start: i32, extent: i32) -> i32 {
    if extent <= 0 {
        return 0;
    }
    let offset = (i64::from(v) - i64::from(start)).clamp(0, i64::from(extent) - 1);
    ((2 * offset + 1) * 65_536 / (2 * i64::from(extent))) as i32
}
