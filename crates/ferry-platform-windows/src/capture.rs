//! 输入捕获：`InputCapture` 的 Windows 实现，基于低级键鼠钩子（`WH_MOUSE_LL`、`WH_KEYBOARD_LL`）。
//!
//! 钩子装在专用线程上，那个线程只跑消息循环。钩子回调必须很快返回，否则系统会把钩子摘掉，
//! 所以回调里只做换算，事件立即交给 sink。
//!
//! 鼠标：钩子给的是“这次移动之后光标会到哪”，此刻光标还在原处，两者之差就是这次的位移。
//! 抓取期间：
//! - 事件被吞掉，本机程序收不到，光标也不动；
//! - 光标停在主显示器中央并藏起来（见 [`crate::inject::hider`]）。停在中央是为了四个方向都留出余地：
//!   停在边上的话，朝外的位移会被屏幕边缘截掉。
//!
//! 光标在本机时，贴着屏幕边缘往外推，系统已经把位置截在边上，算出来的位移是 0。
//! 光标连续两次推不动、又正贴着最外侧的边时，补一个朝外的位移，不然切不过去。
//!
//! 键盘：光标在本机时只记录哪些键按着，不交给 sink；抓取期间转交 sink 并吞掉。
//! 抓取前就按着的键，之后的连发和松开仍交给本机，免得本机以为它一直按着。
//!
//! 钩子拦不住 Ctrl + Alt + Del。后台以管理员权限运行时，管理员窗口在前台也能收到键鼠。

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

use ferry_platform::{CaptureEvent, CaptureSink, Captured, Error, InputCapture, Result};
use ferry_types::{KeyAction, KeyCode, LockKeys, MouseButton, Point};
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL, VK_NUMLOCK, VK_SCROLL};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HC_ACTION,
    HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_UP, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE,
    PeekMessageW, PostThreadMessageW, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetCursorPos, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_USER, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1,
    XBUTTON2,
};

use crate::display::{ensure_dpi_aware, last_error};
use crate::inject::hider::CursorHider;
use crate::keymap::hid_from_windows;

/// 没有待认领的挪光标记录。
const NO_WARP: u64 = u64::MAX;
/// 连续这么多次推不动才算贴着边往外推。偶尔一次不动的移动（有的设备会发）不算。
const STUCK_PUSHES: u8 = 2;

/// 本机键鼠捕获。
pub struct WinCapture {
    shared: Arc<Shared>,
    /// 抓取时藏起光标。创建失败时只把光标挪到中央，不藏。
    hider: Option<CursorHider>,
    worker: Option<Worker>,
}

/// 钩子线程与调用方共用的状态。
struct Shared {
    grabbed: AtomicBool,
    /// 本程序最近一次把光标挪到的位置（打包的 x、y）。挪光标本身也可能经过钩子，
    /// 落在这里的下一次移动当作没动。
    warp: AtomicU64,
}

impl Shared {
    fn set_warp(&self, x: i32, y: i32) {
        let packed = (u64::from(x as u32) << 32) | u64::from(y as u32);
        self.warp.store(packed, Ordering::SeqCst);
    }

    /// 取走挪光标记录：返回它是否正好是 `pt`。
    fn take_warp(&self, pt: POINT) -> bool {
        let packed = self.warp.swap(NO_WARP, Ordering::SeqCst);
        packed != NO_WARP && (packed >> 32) as u32 as i32 == pt.x && packed as u32 as i32 == pt.y
    }
}

struct Worker {
    thread_id: u32,
    thread: JoinHandle<()>,
}

/// 钩子线程独占的状态。钩子回调没有用户参数，只能放在线程局部变量里。
struct HookState {
    sink: CaptureSink,
    shared: Arc<Shared>,
    /// 交给本机处理、尚未松开的键。
    local_keys: BTreeSet<KeyCode>,
    /// 按着的键，用来把重复的按下认作连发。
    held: BTreeSet<KeyCode>,
    /// 连续推不动的次数。
    stuck: u8,
}

thread_local! {
    static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

impl WinCapture {
    pub fn new() -> Self {
        ensure_dpi_aware();
        Self {
            shared: Arc::new(Shared {
                grabbed: AtomicBool::new(false),
                warp: AtomicU64::new(NO_WARP),
            }),
            hider: None,
            worker: None,
        }
    }
}

impl Default for WinCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl InputCapture for WinCapture {
    fn start(&mut self, sink: CaptureSink) -> Result<()> {
        if self.worker.is_some() {
            return Err(Error::Os {
                call: "WinCapture::start",
                detail: "已经在捕获".into(),
            });
        }
        // 藏不了光标不影响控制。
        self.hider = CursorHider::start().ok();
        let shared = Arc::clone(&self.shared);
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("ferry-capture".into())
            .spawn(move || run(shared, sink, ready_tx))
            .map_err(|e| Error::Os {
                call: "thread::spawn",
                detail: e.to_string(),
            })?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => {
                self.worker = Some(Worker { thread_id, thread });
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(Error::Os {
                    call: "SetWindowsHookExW",
                    detail: "捕获线程意外退出".into(),
                })
            }
        }
    }

    fn grab(&self) -> Result<()> {
        // SAFETY: 只读查询系统度量。
        let (x, y) = unsafe {
            (
                GetSystemMetrics(SM_CXSCREEN) / 2,
                GetSystemMetrics(SM_CYSCREEN) / 2,
            )
        };
        self.shared.set_warp(x, y);
        match &self.hider {
            // 把光标挪到主显示器中央的那个透明光标上。
            Some(hider) => hider.set_hidden(true)?,
            // SAFETY: 简单的系统调用。
            None => unsafe { SetCursorPos(x, y) }.map_err(|_| last_error("SetCursorPos"))?,
        }
        self.shared.grabbed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn release(&self, at: Point) -> Result<()> {
        self.shared.set_warp(at.x, at.y);
        // SAFETY: 简单的系统调用。
        let moved = unsafe { SetCursorPos(at.x, at.y) }.map_err(|_| last_error("SetCursorPos"));
        // 光标已经离开那一个透明的像素，收起窗口即可。
        if let Some(hider) = &self.hider {
            let _ = hider.set_hidden(false);
        }
        // 光标放好之后才结束抓取：从这一刻起标记为未抓取的事件，位置都是回来之后的。
        self.shared.grabbed.store(false, Ordering::SeqCst);
        moved
    }

    fn lock_keys(&self) -> LockKeys {
        let mut locks = LockKeys::empty();
        for (lock, vk) in [
            (LockKeys::CAPS, VK_CAPITAL),
            (LockKeys::NUM, VK_NUMLOCK),
            (LockKeys::SCROLL, VK_SCROLL),
        ] {
            // SAFETY: 只读查询键盘状态；最低位表示锁定键是否打开。
            if unsafe { GetKeyState(i32::from(vk.0)) } & 1 != 0 {
                locks |= lock;
            }
        }
        locks
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        if self.shared.grabbed.swap(false, Ordering::SeqCst)
            && let Some(hider) = &self.hider
        {
            let _ = hider.set_hidden(false);
        }
        if let Some(worker) = self.worker.take() {
            // SAFETY: 线程号来自钩子线程自己；它的消息队列在启动时已经建好。
            let _ = unsafe { PostThreadMessageW(worker.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            let _ = worker.thread.join();
        }
    }
}

/// 钩子线程主体：装上钩子，跑消息循环直到收到 `WM_QUIT`。
fn run(shared: Arc<Shared>, sink: CaptureSink, ready: mpsc::Sender<Result<u32>>) {
    let mut msg = MSG::default();
    // SAFETY: 标准调用；先建好消息队列，别的线程才能给它发 WM_QUIT。
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
    }
    HOOK.with(|hook| {
        *hook.borrow_mut() = Some(HookState {
            sink,
            shared,
            local_keys: BTreeSet::new(),
            held: BTreeSet::new(),
            stuck: 0,
        });
    });

    let hooks = install();
    let hooks = match hooks {
        Ok(hooks) => hooks,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // SAFETY: 简单的系统调用。
    let thread_id = unsafe { GetCurrentThreadId() };
    if ready.send(Ok(thread_id)).is_ok() {
        // SAFETY: 标准的消息循环，msg 指向有效的缓冲区。
        unsafe {
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    for hook in hooks {
        // SAFETY: 钩子由本线程装上，只摘这一次。
        let _ = unsafe { UnhookWindowsHookEx(hook) };
    }
    HOOK.with(|hook| hook.borrow_mut().take());
}

/// 装上鼠标和键盘两个钩子。有一个失败就把已装上的摘掉。
fn install() -> Result<[HHOOK; 2]> {
    // SAFETY: 回调符合 HOOKPROC 的约定；模块句柄是本程序自己的。
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|_| last_error("GetModuleHandleW"))?;
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(instance.into()), 0)
            .map_err(|_| last_error("SetWindowsHookExW"))?;
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), Some(instance.into()), 0) {
            Ok(keyboard) => Ok([mouse, keyboard]),
            Err(_) => {
                let e = last_error("SetWindowsHookExW");
                let _ = UnhookWindowsHookEx(mouse);
                Err(e)
            }
        }
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: HC_ACTION 时 lparam 指向系统准备好的 MSLLHOOKSTRUCT，回调期间有效。
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let eat = with_state(|state| state.on_mouse(wparam.0 as u32, info));
        if eat {
            return LRESULT(1);
        }
    }
    // SAFETY: 原样交给下一个钩子。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: HC_ACTION 时 lparam 指向系统准备好的 KBDLLHOOKSTRUCT，回调期间有效。
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let eat = with_state(|state| state.on_key(info));
        if eat {
            return LRESULT(1);
        }
    }
    // SAFETY: 原样交给下一个钩子。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 在钩子线程的状态上执行 `f`，返回是否吞掉事件。状态不在（或正被占用）时不吞。
fn with_state(f: impl FnOnce(&mut HookState) -> bool) -> bool {
    HOOK.with(|hook| match hook.try_borrow_mut() {
        Ok(mut state) => state.as_mut().is_some_and(f),
        Err(_) => false,
    })
}

impl HookState {
    /// 处理一个鼠标事件，返回是否吞掉它。
    fn on_mouse(&mut self, msg: u32, info: &MSLLHOOKSTRUCT) -> bool {
        let grabbed = self.shared.grabbed.load(Ordering::SeqCst);
        let high = (info.mouseData >> 16) as u16;
        let button = |number, pressed| {
            MouseButton::new(number).map(|button| CaptureEvent::Button { button, pressed })
        };
        let event = match msg {
            WM_MOUSEMOVE => self.motion(info.pt, grabbed),
            WM_LBUTTONDOWN => button(1, true),
            WM_LBUTTONUP => button(1, false),
            WM_RBUTTONDOWN => button(2, true),
            WM_RBUTTONUP => button(2, false),
            WM_MBUTTONDOWN => button(3, true),
            WM_MBUTTONUP => button(3, false),
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                let number = match high {
                    XBUTTON1 => 4,
                    XBUTTON2 => 5,
                    _ => 0,
                };
                button(number, msg == WM_XBUTTONDOWN)
            }
            // 向前（向上）为正，向右为正，单位都是 120 一格，与协议一致。
            WM_MOUSEWHEEL => Some(CaptureEvent::Wheel {
                dx: 0,
                dy: i32::from(high as i16),
            }),
            WM_MOUSEHWHEEL => Some(CaptureEvent::Wheel {
                dx: i32::from(high as i16),
                dy: 0,
            }),
            _ => None,
        };
        if let Some(event) = event {
            self.emit(event, grabbed);
        }
        grabbed
    }

    fn motion(&mut self, pt: POINT, grabbed: bool) -> Option<CaptureEvent> {
        if self.shared.take_warp(pt) {
            return None;
        }
        let mut now = POINT::default();
        // SAFETY: 输出参数指向有效的缓冲区。钩子返回之前光标还没动，读到的是这次移动之前的位置。
        unsafe { GetCursorPos(&mut now) }.ok()?;
        let (mut dx, mut dy) = (pt.x - now.x, pt.y - now.y);
        if grabbed {
            return (dx != 0 || dy != 0).then_some(CaptureEvent::Pointer {
                position: Point::new(now.x, now.y),
                dx,
                dy,
            });
        }

        let screen = virtual_screen();
        if dx == 0 && dy == 0 {
            self.stuck = self.stuck.saturating_add(1);
            if self.stuck < STUCK_PUSHES {
                return None;
            }
            // 推不动：贴着哪条最外侧的边，就是在往那边推。
            if now.x <= screen.left {
                dx = -1;
            } else if now.x >= screen.right - 1 {
                dx = 1;
            }
            if now.y <= screen.top {
                dy = -1;
            } else if now.y >= screen.bottom - 1 {
                dy = 1;
            }
            if dx == 0 && dy == 0 {
                return None;
            }
        } else {
            self.stuck = 0;
        }
        let position = Point::new(
            pt.x.clamp(screen.left, screen.right - 1),
            pt.y.clamp(screen.top, screen.bottom - 1),
        );
        Some(CaptureEvent::Pointer { position, dx, dy })
    }

    /// 处理一个键盘事件，返回是否吞掉它。
    fn on_key(&mut self, info: &KBDLLHOOKSTRUCT) -> bool {
        let grabbed = self.shared.grabbed.load(Ordering::SeqCst);
        let extended = info.flags.contains(LLKHF_EXTENDED);
        let Some(code) = hid_from_windows(info.vkCode as u16, info.scanCode as u16, extended)
        else {
            // 认不出的键（输入法合成的字符、AltGr 附带的左 Ctrl 等）留给本机。
            return false;
        };
        let action = if info.flags.contains(LLKHF_UP) {
            self.held.remove(&code);
            KeyAction::Up
        } else if self.held.insert(code) {
            KeyAction::Down
        } else {
            KeyAction::Repeat
        };

        if !grabbed {
            // 光标在本机：按键照常交给本机，只记下哪些键按着。
            match action {
                KeyAction::Up => self.local_keys.remove(&code),
                KeyAction::Down | KeyAction::Repeat => self.local_keys.insert(code),
            };
            return false;
        }
        if self.local_keys.contains(&code) {
            // 抓取前就按着的键：连发和松开都交给本机。
            if action == KeyAction::Up {
                self.local_keys.remove(&code);
            }
            return false;
        }
        self.emit(CaptureEvent::Key { code, action }, true);
        true
    }

    /// 把事件交给 sink。panic 不能穿过系统回调，这里截住。
    fn emit(&mut self, event: CaptureEvent, grabbed: bool) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            (self.sink)(Captured { event, grabbed })
        }));
    }
}

/// 虚拟桌面（所有显示器合起来的外接矩形）的边界，右、下两边不含。
struct Screen {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

fn virtual_screen() -> Screen {
    // SAFETY: 只读查询系统度量。
    unsafe {
        let (left, top) = (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        );
        Screen {
            left,
            top,
            right: left + GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            bottom: top + GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        }
    }
}
