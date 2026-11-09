//! 全局低级鼠标钩子（WH_MOUSE_LL）：鼠标中键 / 侧键老板键、拾取模式、Alt+滚轮调节不透明度。
//!
//! 钩子在安装它的线程（我们的主线程，有消息循环）上回调。匹配到配置的鼠标按键
//! （并满足修饰键）时，向主窗口投递回调消息并“吞掉”该事件，使该按键成为专用老板键；
//! 抬起时另行通知，供“按住显示”在松开时隐藏。三类钩子相互独立，按需安装。
//! 状态用原子量保存，供静态回调函数读取（单线程，无并发问题）。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetWindowRect, IsWindowVisible, PostMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::win::hotkey::MouseButton;

/// 一条鼠标老板键绑定：按键 + 修饰键 + 命中后投递给主窗口的回调消息。
#[derive(Clone, Copy)]
pub struct Binding {
    pub button: MouseButton,
    pub mods: u32,
    pub msg: u32,
}

/// 最多支持的鼠标绑定数（隐藏键 + 显示键）。
const MAX_BINDINGS: usize = 2;

static HOOK: AtomicIsize = AtomicIsize::new(0);
static TARGET_HWND: AtomicIsize = AtomicIsize::new(0);
// 每个槽位的按键码(0=无 1=中键 2=X1 3=X2) / 修饰键 / 回调消息。
static B_BTN: [AtomicU32; MAX_BINDINGS] = [AtomicU32::new(0), AtomicU32::new(0)];
static B_MODS: [AtomicU32; MAX_BINDINGS] = [AtomicU32::new(0), AtomicU32::new(0)];
static B_MSG: [AtomicU32; MAX_BINDINGS] = [AtomicU32::new(0), AtomicU32::new(0)];
static NBIND: AtomicU32 = AtomicU32::new(0);
// 已吞掉按下的按键位掩码（按 button_code 置位），用于吞掉配对的抬起。
static CONSUMED_MASK: AtomicU32 = AtomicU32::new(0);
/// 被吞掉的按键抬起时投递给主窗口的消息（“按住显示”据此在松开时隐藏）。
static RELEASE_MSG: AtomicU32 = AtomicU32::new(0);

fn button_code(b: MouseButton) -> u32 {
    match b {
        MouseButton::Middle => 1,
        MouseButton::X1 => 2,
        MouseButton::X2 => 3,
    }
}

/// 安装鼠标钩子并设定要监听的若干绑定。重复调用会先卸载旧钩子。返回是否安装成功。
/// 传入空切片等价于卸载（不安装钩子）。`release_msg` 在绑定的按键抬起时投递。
pub fn install(hwnd: HWND, bindings: &[Binding], release_msg: u32) -> bool {
    uninstall();
    if bindings.is_empty() {
        return false;
    }
    TARGET_HWND.store(hwnd as isize, Ordering::SeqCst);
    RELEASE_MSG.store(release_msg, Ordering::SeqCst);
    let n = bindings.len().min(MAX_BINDINGS);
    for i in 0..n {
        B_BTN[i].store(button_code(bindings[i].button), Ordering::SeqCst);
        B_MODS[i].store(bindings[i].mods, Ordering::SeqCst);
        B_MSG[i].store(bindings[i].msg, Ordering::SeqCst);
    }
    NBIND.store(n as u32, Ordering::SeqCst);
    CONSUMED_MASK.store(0, Ordering::SeqCst);
    unsafe {
        let h = SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(hook_proc),
            GetModuleHandleW(std::ptr::null()),
            0,
        );
        HOOK.store(h as isize, Ordering::SeqCst);
        !h.is_null()
    }
}

/// 卸载鼠标钩子。
pub fn uninstall() {
    let h = HOOK.swap(0, Ordering::SeqCst);
    if h != 0 {
        unsafe {
            UnhookWindowsHookEx(h as *mut c_void);
        }
    }
    NBIND.store(0, Ordering::SeqCst);
    CONSUMED_MASK.store(0, Ordering::SeqCst);
}

/// 当前所需修饰键是否都按下（无修饰键则恒为真）。
fn mods_satisfied(mods: u32) -> bool {
    let down = |vk: i32| unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 };
    (mods & MOD_CONTROL == 0 || down(0x11))
        && (mods & MOD_ALT == 0 || down(0x12))
        && (mods & MOD_SHIFT == 0 || down(0x10))
        && (mods & MOD_WIN == 0 || down(0x5B) || down(0x5C))
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let n = NBIND.load(Ordering::SeqCst) as usize;
        if n != 0 {
            let msg = wparam as u32;
            let info = &*(lparam as *const MSLLHOOKSTRUCT);
            let xbtn = (info.mouseData >> 16) & 0xFFFF; // 1=X1, 2=X2
            let (this, is_down) = match msg {
                WM_MBUTTONDOWN => (1u32, true),
                WM_MBUTTONUP => (1u32, false),
                WM_XBUTTONDOWN => (if xbtn == 1 { 2 } else { 3 }, true),
                WM_XBUTTONUP => (if xbtn == 1 { 2 } else { 3 }, false),
                _ => (0u32, false),
            };
            if this != 0 {
                if is_down {
                    // 找到一条按键匹配且修饰键满足的绑定即触发并吞掉。
                    for i in 0..n {
                        if B_BTN[i].load(Ordering::SeqCst) == this
                            && mods_satisfied(B_MODS[i].load(Ordering::SeqCst))
                        {
                            let hwnd = TARGET_HWND.load(Ordering::SeqCst) as HWND;
                            PostMessageW(hwnd, B_MSG[i].load(Ordering::SeqCst), 0, 0);
                            CONSUMED_MASK.fetch_or(1 << this, Ordering::SeqCst);
                            return 1; // 吞掉按下，使该按键成为专用老板键
                        }
                    }
                } else {
                    let bit = 1u32 << this;
                    if CONSUMED_MASK.load(Ordering::SeqCst) & bit != 0 {
                        CONSUMED_MASK.fetch_and(!bit, Ordering::SeqCst);
                        let hwnd = TARGET_HWND.load(Ordering::SeqCst) as HWND;
                        PostMessageW(hwnd, RELEASE_MSG.load(Ordering::SeqCst), 0, 0);
                        return 1; // 吞掉配对的抬起，避免下游收到半个事件
                    }
                }
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

// ——————————————————————— 拾取模式钩子 ———————————————————————
//
// 与老板键钩子相互独立。拾取模式需要监听“全局鼠标移动 / 左右键”；SetCapture 在未按键
// 时收不到其它窗口上的移动，故改用低级钩子。移动事件做合并投递，避免高频刷屏。

static PICK_HOOK: AtomicIsize = AtomicIsize::new(0);
static PICK_HWND: AtomicIsize = AtomicIsize::new(0);
static PICK_MOVE_MSG: AtomicU32 = AtomicU32::new(0);
static PICK_SELECT_MSG: AtomicU32 = AtomicU32::new(0);
static PICK_CANCEL_MSG: AtomicU32 = AtomicU32::new(0);
static PICK_MOVE_PENDING: AtomicBool = AtomicBool::new(false);

/// 安装拾取钩子。move/select/cancel 为投递给主窗口的回调消息。
pub fn install_pick(hwnd: HWND, move_msg: u32, select_msg: u32, cancel_msg: u32) -> bool {
    uninstall_pick();
    PICK_HWND.store(hwnd as isize, Ordering::SeqCst);
    PICK_MOVE_MSG.store(move_msg, Ordering::SeqCst);
    PICK_SELECT_MSG.store(select_msg, Ordering::SeqCst);
    PICK_CANCEL_MSG.store(cancel_msg, Ordering::SeqCst);
    PICK_MOVE_PENDING.store(false, Ordering::SeqCst);
    unsafe {
        let h = SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(pick_proc),
            GetModuleHandleW(std::ptr::null()),
            0,
        );
        PICK_HOOK.store(h as isize, Ordering::SeqCst);
        !h.is_null()
    }
}

pub fn uninstall_pick() {
    let h = PICK_HOOK.swap(0, Ordering::SeqCst);
    if h != 0 {
        unsafe {
            UnhookWindowsHookEx(h as *mut c_void);
        }
    }
    PICK_HWND.store(0, Ordering::SeqCst);
}

/// 主线程处理完一次移动通知后调用，允许钩子再次投递（合并高频移动）。
pub fn pick_move_handled() {
    PICK_MOVE_PENDING.store(false, Ordering::SeqCst);
}

unsafe extern "system" fn pick_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let hwnd = PICK_HWND.load(Ordering::SeqCst) as HWND;
        if !hwnd.is_null() {
            match wparam as u32 {
                // 合并高频移动：仅当无待处理移动时投递一次（swap 有副作用，故置于 guard）。
                WM_MOUSEMOVE if !PICK_MOVE_PENDING.swap(true, Ordering::SeqCst) => {
                    PostMessageW(hwnd, PICK_MOVE_MSG.load(Ordering::SeqCst), 0, 0);
                }
                WM_LBUTTONDOWN => {
                    PostMessageW(hwnd, PICK_SELECT_MSG.load(Ordering::SeqCst), 0, 0);
                    return 1; // 吞掉点击，避免落到目标窗口上
                }
                WM_RBUTTONDOWN => {
                    PostMessageW(hwnd, PICK_CANCEL_MSG.load(Ordering::SeqCst), 0, 0);
                    return 1;
                }
                WM_LBUTTONUP | WM_RBUTTONUP => {
                    return 1; // 吞掉配对的抬起
                }
                _ => {}
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

// ——————————————————————— Alt+滚轮调节不透明度 ———————————————————————
//
// 光标位于某个受管窗口上且按住 Alt 时，吞掉滚轮并把滚动量投递给主窗口。
// 按窗口矩形判断而不是 WindowFromPoint：后者会跳过鼠标穿透的窗口。

static WHEEL_HOOK: AtomicIsize = AtomicIsize::new(0);
static WHEEL_HWND: AtomicIsize = AtomicIsize::new(0);
static WHEEL_MSG: AtomicU32 = AtomicU32::new(0);
static WHEEL_TARGETS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// 安装滚轮钩子（已安装则保持）。滚动量（120 的倍数）作为 wParam 投递。
pub fn install_wheel(hwnd: HWND, msg: u32) -> bool {
    if WHEEL_HOOK.load(Ordering::SeqCst) != 0 {
        return true;
    }
    WHEEL_HWND.store(hwnd as isize, Ordering::SeqCst);
    WHEEL_MSG.store(msg, Ordering::SeqCst);
    unsafe {
        let h = SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(wheel_proc),
            GetModuleHandleW(std::ptr::null()),
            0,
        );
        WHEEL_HOOK.store(h as isize, Ordering::SeqCst);
        !h.is_null()
    }
}

pub fn uninstall_wheel() {
    let h = WHEEL_HOOK.swap(0, Ordering::SeqCst);
    if h != 0 {
        unsafe {
            UnhookWindowsHookEx(h as *mut c_void);
        }
    }
}

/// 更新可响应 Alt+滚轮 的窗口集合。
pub fn set_wheel_targets(hwnds: Vec<isize>) {
    if let Ok(mut v) = WHEEL_TARGETS.lock() {
        *v = hwnds;
    }
}

/// 发送一个无含义的按键，避免单独按下又松开的 Alt 激活目标程序的菜单栏。
pub fn suppress_alt_menu() {
    const VK_NONE: u16 = 0xE8; // 未分配的虚拟键
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_NONE,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [key(0), key(KEYEVENTF_KEYUP)];
    unsafe {
        SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn wheel_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam as u32 == WM_MOUSEWHEEL && (GetAsyncKeyState(0x12) as u16 & 0x8000) != 0 {
        let info = &*(lparam as *const MSLLHOOKSTRUCT);
        let over_target = WHEEL_TARGETS.try_lock().is_ok_and(|v| {
            v.iter().any(|&h| {
                let hwnd = h as HWND;
                let mut r: RECT = std::mem::zeroed();
                IsWindowVisible(hwnd) != 0
                    && GetWindowRect(hwnd, &mut r) != 0
                    && info.pt.x >= r.left
                    && info.pt.x < r.right
                    && info.pt.y >= r.top
                    && info.pt.y < r.bottom
            })
        });
        if over_target {
            let delta = (info.mouseData >> 16) as u16 as i16 as isize;
            let hwnd = WHEEL_HWND.load(Ordering::SeqCst) as HWND;
            PostMessageW(hwnd, WHEEL_MSG.load(Ordering::SeqCst), delta as usize, 0);
            return 1;
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}
