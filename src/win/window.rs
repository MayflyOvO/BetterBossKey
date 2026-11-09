//! 顶层窗口的枚举与操作：隐藏 / 显示 / 半透明。
//!
//! HWND 在内部以 `isize` 形式保存（便于比较、克隆与持久化），调用 Win32 API
//! 时再转换回指针类型。所有操作前都会用 `IsWindow` 校验句柄是否仍然有效，
//! 以应对窗口已被关闭、句柄被复用的情况。

use std::ffi::c_void;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, GWL_EXSTYLE, GW_OWNER, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOW, SW_SHOWNA, WS_EX_APPWINDOW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW,
};

use crate::util::from_wide;

/// 本程序主窗口使用的窗口类名。枚举时据此排除自身。
pub const APP_WINDOW_CLASS: &str = "BetterBossKeyMainWindow";

/// 被本程序隐藏、尚未恢复的窗口登记表。
///
/// 即使程序异常退出 / 崩溃，也能据此把窗口恢复出来，避免用户的窗口永久隐藏
/// （隐藏的窗口不在任务栏 / Alt-Tab 中，很难手动找回）。`hide` / `show` 会维护它，
/// [`restore_all_hidden`] 在退出 / 会话结束 / panic 时兜底恢复。
static HIDDEN_WINDOWS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// 一个可被管理的顶层窗口的快照信息。
#[derive(Clone)]
pub struct WindowInfo {
    /// 窗口句柄（以 isize 保存）。
    pub hwnd: isize,
    /// 标题。
    pub title: String,
}

/// 把内部 isize 句柄转换为 Win32 的 HWND 指针。
#[inline]
fn as_hwnd(h: isize) -> HWND {
    h as HWND
}

/// 枚举当前所有“适合作为目标”的可见顶层窗口（类似 Alt-Tab 列表）。
pub fn enumerate() -> Vec<WindowInfo> {
    let mut result: Vec<WindowInfo> = Vec::new();
    unsafe {
        EnumWindows(
            Some(enum_proc),
            &mut result as *mut Vec<WindowInfo> as LPARAM,
        );
    }
    result
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
    let out = &mut *(lparam as *mut Vec<WindowInfo>);

    // 必须可见。
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }

    let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    let is_tool = ex_style & WS_EX_TOOLWINDOW != 0;
    let is_app = ex_style & WS_EX_APPWINDOW != 0;
    let has_owner = !GetWindow(hwnd, GW_OWNER).is_null();

    // Alt-Tab 可见性规则：要么显式声明为 AppWindow，要么是“无属主、非工具窗口”。
    if !is_app && (is_tool || has_owner) {
        return 1;
    }

    // 排除被 DWM 隐藏（cloaked）的窗口，例如未激活的 UWP 应用残留句柄。
    let mut cloaked: i32 = 0;
    let hr = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED as u32,
        &mut cloaked as *mut i32 as *mut c_void,
        std::mem::size_of::<i32>() as u32,
    );
    if hr == 0 && cloaked != 0 {
        return 1;
    }

    // 必须有非空标题。
    let title = window_text(hwnd);
    if title.trim().is_empty() {
        return 1;
    }

    // 排除本程序自身的窗口。
    if class_name(hwnd) == APP_WINDOW_CLASS {
        return 1;
    }

    out.push(WindowInfo {
        hwnd: hwnd as isize,
        title,
    });
    1 // 继续枚举
}

/// 读取窗口标题。
///
/// 先测长再读取之间标题可能变长（如带实时计数的窗口），因此读满缓冲时
/// 扩容重试，尽量避免截断，保证标题保真（持久化与再匹配都依赖它）。
pub fn window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut cap = len as usize + 1;
        loop {
            let mut buf = vec![0u16; cap + 1];
            let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) as usize;
            // 未读满缓冲（末尾仍有空位）说明没有被截断。
            if n < buf.len() - 1 || cap >= 4096 {
                return from_wide(&buf[..n]);
            }
            cap *= 2;
        }
    }
}

/// 读取窗口类名。
fn class_name(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        from_wide(&buf[..n as usize])
    }
}

/// 读取窗口所属进程 ID。失败返回 0。
pub fn window_pid(hwnd: isize) -> u32 {
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(as_hwnd(hwnd), &mut pid);
    }
    pid
}

/// 读取窗口类名（句柄已失效返回空串）。
pub fn class_of(hwnd: isize) -> String {
    if !is_alive(hwnd) {
        return String::new();
    }
    class_name(as_hwnd(hwnd))
}

/// 句柄是否仍指向一个存在的窗口。
pub fn is_alive(hwnd: isize) -> bool {
    unsafe { IsWindow(as_hwnd(hwnd)) != 0 }
}

/// 窗口当前是否可见。
pub fn is_visible(hwnd: isize) -> bool {
    unsafe { IsWindowVisible(as_hwnd(hwnd)) != 0 }
}

/// 读取某句柄当前标题（用于受管窗口的再匹配 / 显示刷新）。
pub fn title_of(hwnd: isize) -> String {
    if !is_alive(hwnd) {
        return String::new();
    }
    window_text(as_hwnd(hwnd))
}

/// 隐藏窗口（同时会从任务栏消失），并登记以便异常退出时恢复。
pub fn hide(hwnd: isize) {
    if is_alive(hwnd) {
        unsafe {
            ShowWindow(as_hwnd(hwnd), SW_HIDE);
        }
        register_hidden(hwnd);
    }
}

/// 仅登记到隐藏兜底表（不改变窗口可见性）。用于重启后重新接管“此前被我们
/// 隐藏、仍处于隐藏状态”的窗口，使其重新获得老板键恢复与退出兜底。
pub fn register_hidden(hwnd: isize) {
    if let Ok(mut v) = HIDDEN_WINDOWS.lock() {
        if !v.contains(&hwnd) {
            v.push(hwnd);
        }
    }
}

/// 显示窗口并强制置于前台，并从隐藏登记表移除。
pub fn show(hwnd: isize) {
    if is_alive(hwnd) {
        unsafe {
            ShowWindow(as_hwnd(hwnd), SW_SHOW);
        }
        force_foreground(as_hwnd(hwnd));
    }
    if let Ok(mut v) = HIDDEN_WINDOWS.lock() {
        v.retain(|&h| h != hwnd);
    }
}

/// 把窗口强制切到前台。
///
/// 后台程序（托盘里的本程序）直接调用 `SetForegroundWindow` 常被 Windows 的
/// “前台锁定”挡下——只显示不激活。经典做法是临时把本线程的输入队列附加到当前
/// 前台窗口所属线程上，绕过锁定，再恢复。供老板键“全部恢复”及自动唤出聚焦使用。
fn force_foreground(h: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        if fg == h {
            return;
        }
        let cur = GetCurrentThreadId();
        let mut pid = 0u32;
        // fg 为 null 时返回 0，AttachThreadInput 会被跳过。
        let fg_thread = GetWindowThreadProcessId(fg, &mut pid);
        let attached = fg_thread != 0 && fg_thread != cur && AttachThreadInput(cur, fg_thread, 1) != 0;
        SetForegroundWindow(h);
        BringWindowToTop(h);
        if attached {
            AttachThreadInput(cur, fg_thread, 0);
        }
    }
}

/// 显示窗口但不激活（不抢前台、不打断当前操作），并从隐藏登记表移除。
/// 用于“鼠标移回原位自动唤出”时不希望抢焦点的情形。
pub fn show_no_activate(hwnd: isize) {
    if is_alive(hwnd) {
        unsafe {
            ShowWindow(as_hwnd(hwnd), SW_SHOWNA);
        }
    }
    if let Ok(mut v) = HIDDEN_WINDOWS.lock() {
        v.retain(|&h| h != hwnd);
    }
}

/// 恢复所有登记在册（被本程序隐藏）的窗口并清空登记表。
///
/// 供正常退出 / 会话结束 / panic 兜底调用。用 try_lock 避免在 panic 时（若锁正被
/// 同一线程持有）死锁；锁中毒时仍取回数据继续恢复。
pub fn restore_all_hidden() {
    use std::sync::TryLockError;
    let mut guard = match HIDDEN_WINDOWS.try_lock() {
        Ok(g) => g,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    for &h in guard.iter() {
        if is_alive(h) {
            unsafe {
                ShowWindow(as_hwnd(h), SW_SHOW);
            }
        }
    }
    guard.clear();
}

/// 给窗口设置半透明度（alpha：0=全透明，255=不透明）。
pub fn set_opacity(hwnd: isize, alpha: u8) {
    if !is_alive(hwnd) {
        return;
    }
    unsafe {
        let h = as_hwnd(hwnd);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_LAYERED == 0 {
            SetWindowLongPtrW(h, GWL_EXSTYLE, (ex | WS_EX_LAYERED) as isize);
        }
        SetLayeredWindowAttributes(h, 0, alpha, LWA_ALPHA);
    }
}

/// 取消半透明，恢复为完全不透明，并移除 layered 扩展样式。
pub fn clear_opacity(hwnd: isize) {
    if !is_alive(hwnd) {
        return;
    }
    unsafe {
        let h = as_hwnd(hwnd);
        // 先把 alpha 拉满，确保即便移除样式失败窗口也是可见的。
        SetLayeredWindowAttributes(h, 0, 255, LWA_ALPHA);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_LAYERED != 0 {
            SetWindowLongPtrW(h, GWL_EXSTYLE, (ex & !WS_EX_LAYERED) as isize);
            // 触发重绘，让窗口以不透明方式重新呈现。
            SetWindowPos(
                h,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// 获取窗口矩形（屏幕坐标）。失败返回 None。
pub fn window_rect(hwnd: isize) -> Option<RECT> {
    if !is_alive(hwnd) {
        return None;
    }
    unsafe {
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(as_hwnd(hwnd), &mut r) != 0 {
            Some(r)
        } else {
            None
        }
    }
}

/// 返回屏幕坐标点下、最顶层且“可作为目标”的窗口（用于拾取模式）。
/// 基于 [`enumerate`] 的 Z 序（顶层在前），返回第一个矩形包含该点的窗口。
pub fn window_at(pt: POINT) -> Option<WindowInfo> {
    enumerate().into_iter().find(|w| {
        window_rect(w.hwnd).is_some_and(|r| {
            pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
        })
    })
}

/// 读取窗口所属进程的可执行文件名（如 `phoneCast.exe`）。失败返回空串。
pub fn process_name(hwnd: isize) -> String {
    let pid = window_pid(hwnd);
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 260];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(h);
        if ok != 0 {
            let path = from_wide(&buf[..size as usize]);
            path.rsplit(['\\', '/']).next().unwrap_or("").to_string()
        } else {
            String::new()
        }
    }
}
