//! 顶层窗口的枚举与操作：隐藏 / 显示 / 半透明 / 置顶 / 去掉标题栏。
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
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, GWL_EXSTYLE, GWL_STYLE, GW_OWNER, HWND_NOTOPMOST, HWND_TOPMOST,
    LWA_ALPHA, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE,
    SW_RESTORE, SW_SHOW, SW_SHOWNA, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_THICKFRAME,
};

use crate::util::from_wide;

/// 本程序主窗口使用的窗口类名。枚举时据此排除自身。
pub const APP_WINDOW_CLASS: &str = "BetterBossKeyMainWindow";

/// 被本程序隐藏、尚未恢复的窗口登记表。
///
/// 即使程序异常退出 / 崩溃，也能据此把窗口恢复出来，避免用户的窗口永久隐藏
/// （隐藏的窗口不在任务栏 / Alt-Tab 中，很难手动找回）。`hide` / `show` 会维护它，
/// [`restore_all_hidden`] 在退出 / 会话结束 / panic 时兜底恢复。登记表的每次变化
/// 还会写入崩溃恢复文件（见 [`crate::recovery`]），连强制结束进程也能在下次启动时找回。
static HIDDEN_WINDOWS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// 被本程序设置了半透明 / 鼠标穿透的窗口登记表，兜底方式同上。
static STYLED_WINDOWS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// 被本程序设为置顶的窗口登记表，兜底方式同上。原本就置顶的窗口不登记，撤销时也不动它。
static TOPMOST_WINDOWS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// 被本程序去掉了标题栏的窗口登记表：句柄及去掉的样式位（撤销时原样补回），兜底方式同上。
static CAPTIONLESS_WINDOWS: Mutex<Vec<(isize, u32)>> = Mutex::new(Vec::new());

/// 去掉标题栏时一并去掉的样式：标题栏本身，以及缩放边框——只去标题栏的话，
/// 缩放边框会在窗口顶部留下一条浅色边，且跨进程无法改它的颜色。
const FRAME_STYLE: u32 = WS_CAPTION | WS_THICKFRAME;

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
    if let Some(title) = candidate_title(hwnd) {
        out.push(WindowInfo {
            hwnd: hwnd as isize,
            title,
        });
    }
    1 // 继续枚举
}

/// 窗口是否“适合作为目标”（类似 Alt-Tab 列表），是则返回其标题。
fn candidate_title(hwnd: HWND) -> Option<String> {
    unsafe {
        // 必须可见。
        if IsWindowVisible(hwnd) == 0 {
            return None;
        }

        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let is_tool = ex_style & WS_EX_TOOLWINDOW != 0;
        let is_app = ex_style & WS_EX_APPWINDOW != 0;
        let has_owner = !GetWindow(hwnd, GW_OWNER).is_null();

        // Alt-Tab 可见性规则：要么显式声明为 AppWindow，要么是“无属主、非工具窗口”。
        if !is_app && (is_tool || has_owner) {
            return None;
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
            return None;
        }
    }

    // 必须有非空标题。
    let title = window_text(hwnd);
    if title.trim().is_empty() {
        return None;
    }

    // 排除本程序自身的窗口。
    if class_name(hwnd) == APP_WINDOW_CLASS {
        return None;
    }
    Some(title)
}

/// 当前前台窗口（无则为 0）。
pub fn foreground() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

/// 窗口是否是一个可切换到的普通应用窗口（与窗口列表的筛选规则一致）。
pub fn is_switchable(hwnd: isize) -> bool {
    hwnd != 0 && is_alive(hwnd) && candidate_title(as_hwnd(hwnd)).is_some()
}

/// 窗口是否最小化。
pub fn is_minimized(hwnd: isize) -> bool {
    unsafe { IsIconic(as_hwnd(hwnd)) != 0 }
}

/// 把窗口切到前台；最小化的先还原。
pub fn activate(hwnd: isize) {
    if !is_alive(hwnd) {
        return;
    }
    unsafe {
        if IsIconic(as_hwnd(hwnd)) != 0 {
            ShowWindow(as_hwnd(hwnd), SW_RESTORE);
        }
    }
    force_foreground(as_hwnd(hwnd));
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

/// 往登记表加入一项；有变化时同步到崩溃恢复文件。
fn registry_add(reg: &Mutex<Vec<isize>>, hwnd: isize) {
    let added = match reg.lock() {
        Ok(mut v) if !v.contains(&hwnd) => {
            v.push(hwnd);
            true
        }
        _ => false,
    };
    if added {
        crate::recovery::persist();
    }
}

/// 从登记表移除一项；有变化时同步到崩溃恢复文件。返回该项是否在册。
fn registry_remove(reg: &Mutex<Vec<isize>>, hwnd: isize) -> bool {
    let removed = match reg.lock() {
        Ok(mut v) => {
            let n = v.len();
            v.retain(|&h| h != hwnd);
            v.len() != n
        }
        Err(_) => false,
    };
    if removed {
        crate::recovery::persist();
    }
    removed
}

/// 仅登记到隐藏兜底表（不改变窗口可见性）。用于重启后重新接管“此前被我们
/// 隐藏、仍处于隐藏状态”的窗口，使其重新获得老板键恢复与退出兜底。
pub fn register_hidden(hwnd: isize) {
    registry_add(&HIDDEN_WINDOWS, hwnd);
}

/// 显示窗口并强制置于前台，并从隐藏登记表移除。
pub fn show(hwnd: isize) {
    if is_alive(hwnd) {
        unsafe {
            ShowWindow(as_hwnd(hwnd), SW_SHOW);
        }
        force_foreground(as_hwnd(hwnd));
    }
    registry_remove(&HIDDEN_WINDOWS, hwnd);
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
    registry_remove(&HIDDEN_WINDOWS, hwnd);
}

/// 恢复所有登记在册（被本程序隐藏）的窗口并清空登记表。
///
/// 供正常退出 / 会话结束 / panic 兜底调用。用 try_lock 避免在 panic 时（若锁正被
/// 同一线程持有）死锁；锁中毒时仍取回数据继续恢复。
pub fn restore_all_hidden() {
    for h in drain(&HIDDEN_WINDOWS) {
        if is_alive(h) {
            unsafe {
                ShowWindow(as_hwnd(h), SW_SHOW);
            }
        }
    }
}

/// 撤销所有登记在册的半透明 / 穿透样式并清空登记表。用途同 [`restore_all_hidden`]。
pub fn restore_all_styled() {
    for h in drain(&STYLED_WINDOWS) {
        reset_style(h);
    }
}

/// 取出并清空登记表（try_lock，理由见 [`restore_all_hidden`]）。
fn drain<T>(reg: &Mutex<Vec<T>>) -> Vec<T> {
    use std::sync::TryLockError;
    let mut guard = match reg.try_lock() {
        Ok(g) => g,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => return Vec::new(),
    };
    std::mem::take(&mut *guard)
}

/// 撤销所有登记在册的置顶并清空登记表。用途同 [`restore_all_hidden`]。
pub fn restore_all_topmost() {
    for h in drain(&TOPMOST_WINDOWS) {
        reset_topmost(h);
    }
}

/// 补回所有登记在册窗口的标题栏并清空登记表。用途同 [`restore_all_hidden`]。
pub fn restore_all_captions() {
    for (h, bits) in drain(&CAPTIONLESS_WINDOWS) {
        reset_caption(h, bits);
    }
}

/// 各登记表的快照及其在崩溃恢复文件中的键名，供 [`crate::recovery`] 持久化。
pub fn registry_snapshot() -> [(&'static str, Vec<isize>); 3] {
    let get = |reg: &Mutex<Vec<isize>>| reg.try_lock().map(|v| v.clone()).unwrap_or_default();
    [
        ("hidden", get(&HIDDEN_WINDOWS)),
        ("styled", get(&STYLED_WINDOWS)),
        ("topmost", get(&TOPMOST_WINDOWS)),
    ]
}

/// 去掉了标题栏的窗口及去掉的样式位，供 [`crate::recovery`] 持久化。
pub fn captionless_snapshot() -> Vec<(isize, u32)> {
    CAPTIONLESS_WINDOWS.try_lock().map(|v| v.clone()).unwrap_or_default()
}

/// 窗口当前是否置顶。
pub fn is_topmost(hwnd: isize) -> bool {
    unsafe { GetWindowLongPtrW(as_hwnd(hwnd), GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0 }
}

/// 把窗口置顶并升到置顶层的最上方，不激活。原本不置顶的才登记，撤销时只还原我们改过的。
pub fn set_topmost(hwnd: isize) {
    if !is_alive(hwnd) {
        return;
    }
    if !is_topmost(hwnd) {
        registry_add(&TOPMOST_WINDOWS, hwnd);
    }
    place_z(hwnd, HWND_TOPMOST);
}

/// 取消由本程序设置的置顶（原本就置顶的窗口保持不变）。
pub fn clear_topmost(hwnd: isize) {
    if registry_remove(&TOPMOST_WINDOWS, hwnd) {
        reset_topmost(hwnd);
    }
}

/// 仅登记到置顶兜底表（不改变窗口状态）。用于接管上次运行时被我们置顶的窗口。
pub fn register_topmost(hwnd: isize) {
    registry_add(&TOPMOST_WINDOWS, hwnd);
}

/// 无条件取消置顶。
pub fn reset_topmost(hwnd: isize) {
    if is_alive(hwnd) {
        place_z(hwnd, HWND_NOTOPMOST);
    }
}

fn place_z(hwnd: isize, after: HWND) {
    unsafe {
        SetWindowPos(as_hwnd(hwnd), after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// 去掉窗口的标题栏与缩放边框，并登记去掉的样式位以便撤销。窗口矩形不变，
/// 原先不可见的缩放边框区域变为客户区，可见部分因此左、右、下各外扩几个像素。
///
/// 只处理带完整 `WS_CAPTION` 的窗口：自绘标题栏的程序（浏览器、Electron、UWP 等）
/// 本就没有它，保持原样。已去掉的再调用是空操作，因此可周期性调用，抵消目标程序自行恢复标题栏。
pub fn remove_caption(hwnd: isize) {
    if !is_alive(hwnd) {
        return;
    }
    let h = as_hwnd(hwnd);
    let style = unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32;
    if style & WS_CAPTION != WS_CAPTION {
        return;
    }
    let bits = style & FRAME_STYLE;
    register_captionless(hwnd, bits);
    unsafe {
        SetWindowLongPtrW(h, GWL_STYLE, (style & !bits) as isize);
    }
    refresh_frame(h);
}

/// 补回由本程序去掉的标题栏（其它窗口不受影响）。
pub fn restore_caption(hwnd: isize) {
    let removed = match CAPTIONLESS_WINDOWS.lock() {
        Ok(mut v) => v.iter().position(|&(h, _)| h == hwnd).map(|i| v.remove(i)),
        Err(_) => None,
    };
    if let Some((h, bits)) = removed {
        crate::recovery::persist();
        reset_caption(h, bits);
    }
}

/// 登记到标题栏兜底表（不改变窗口样式），`bits` 为去掉的样式位。已登记的保留原记录：
/// 目标程序自行恢复标题栏后再次去掉时，要补回的仍是最初去掉的样式。
/// 也用于接管上次运行时被我们去掉标题栏的窗口。
pub fn register_captionless(hwnd: isize, bits: u32) {
    let added = match CAPTIONLESS_WINDOWS.lock() {
        Ok(mut v) if !v.iter().any(|&(h, _)| h == hwnd) => {
            v.push((hwnd, bits));
            true
        }
        _ => false,
    };
    if added {
        crate::recovery::persist();
    }
}

/// 无条件补回去掉的样式位。
pub fn reset_caption(hwnd: isize, bits: u32) {
    if !is_alive(hwnd) {
        return;
    }
    let h = as_hwnd(hwnd);
    let style = unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32;
    if style & bits != bits {
        unsafe {
            SetWindowLongPtrW(h, GWL_STYLE, (style | bits) as isize);
        }
        refresh_frame(h);
    }
}

/// 让窗口按新样式重新计算并绘制非客户区。
fn refresh_frame(h: HWND) {
    unsafe {
        SetWindowPos(
            h,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
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
    registry_add(&STYLED_WINDOWS, hwnd);
}

/// 设置 / 取消鼠标穿透（点击落到下方窗口）。需配合半透明的 layered 样式使用。
pub fn set_click_through(hwnd: isize, on: bool) {
    if !is_alive(hwnd) {
        return;
    }
    unsafe {
        let h = as_hwnd(hwnd);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        let want = if on { ex | WS_EX_TRANSPARENT } else { ex & !WS_EX_TRANSPARENT };
        if want != ex {
            SetWindowLongPtrW(h, GWL_EXSTYLE, want as isize);
        }
    }
}

/// 取消半透明与鼠标穿透，恢复为完全不透明，并移除 layered 扩展样式。
pub fn clear_opacity(hwnd: isize) {
    reset_style(hwnd);
    registry_remove(&STYLED_WINDOWS, hwnd);
}

fn reset_style(hwnd: isize) {
    if !is_alive(hwnd) {
        return;
    }
    unsafe {
        let h = as_hwnd(hwnd);
        // 先把 alpha 拉满，确保即便移除样式失败窗口也是可见的。
        SetLayeredWindowAttributes(h, 0, 255, LWA_ALPHA);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if ex & (WS_EX_LAYERED | WS_EX_TRANSPARENT) != 0 {
            SetWindowLongPtrW(h, GWL_EXSTYLE, (ex & !(WS_EX_LAYERED | WS_EX_TRANSPARENT)) as isize);
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
    process_name_of(window_pid(hwnd))
}

/// 读取某进程的可执行文件名。失败返回空串。
pub fn process_name_of(pid: u32) -> String {
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
