//! BetterBossKey —— 轻量级 Windows 窗口隐藏 / 半透明 / 老板键工具。
//!
//! 入口、窗口类注册、消息循环与 WndProc 分发都在这里；具体业务逻辑在
//! [`app`]，界面绘制在 [`gui`]。整个程序仅依赖 `windows-sys` 一个 crate，
//! 静态链接 CRT，产出一个无外部依赖的小体积单文件 exe。

// 以 GUI 子系统编译：不弹出控制台窗口。
// debug 构建保留控制台，便于查看 panic / 调试输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod gui;
mod recovery;
mod util;
mod win;

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
    InvalidateRect, SelectObject, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, PAINTSTRUCT, SRCCOPY,
};
use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{CreateMutexW, Sleep};
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow,
    DispatchMessageW, FindWindowW, GetClientRect, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, TrackPopupMenu,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, MF_SEPARATOR, MF_STRING,
    MSG, SPI_GETWORKAREA, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SWP_SHOWWINDOW, TPM_LEFTALIGN,
    TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_DISPLAYCHANGE,
    WM_DPICHANGED,
    WM_ENDSESSION, WM_ERASEBKGND, WM_CHAR, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NULL, WM_PAINT, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_TIMER,
    WM_XBUTTONDOWN, WNDCLASSEXW, WS_CAPTION, WS_MINIMIZEBOX, WS_SYSMENU,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{WA_INACTIVE, WM_ACTIVATE};

use crate::app::{
    App, KeySource, HOTKEY_ID_HIDE, HOTKEY_ID_SHOW, HOTKEY_ID_TOGGLE, PEEK_TIMER_ID, TIMER_ID,
    WM_AUDIO_DEVICE, WM_BOSS_HIDE, WM_BOSS_RELEASE, WM_BOSS_SHOW, WM_MOUSE_HOTKEY, WM_PICK_CANCEL,
    WM_PICK_MOVE, WM_PICK_SELECT, WM_TRAYICON, WM_WHEEL_OPACITY,
};
use crate::config::Config;
use crate::gui::widgets::Canvas;
use crate::util::wide;
use crate::win::hotkey::{Hotkey, MouseButton};
use crate::win::icon::IconSize;
use crate::win::watch;
use crate::win::window::APP_WINDOW_CLASS;

/// 从另一个实例 / 托盘请求显示设置窗口的自定义消息。
const WM_APP_SHOW: u32 = WM_APP + 2;

// 托盘右键菜单项 id。
const IDM_SHOW: usize = 1;
const IDM_HIDE_ALL: usize = 2;
const IDM_SHOW_ALL: usize = 3;
const IDM_EXIT: usize = 4;

// 固定窗口样式：有标题栏、系统菜单、最小化按钮，但不可缩放 / 最大化。
const WINDOW_STYLE: u32 = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

/// 单实例互斥体名：不透明 GUID，不含应用名（窗口类名对深层工具仍是可见的，
/// 但任务管理器 / tasklist 不显示窗口类）。
const SINGLE_INSTANCE_MUTEX: &str = "Local\\{2B7A96D0-5F1C-4E8B-9A34-6C0D7E52F8A1}";
const ERROR_ALREADY_EXISTS: u32 = 183;

/// 尝试取得单实例互斥体。`wait` 为 true 时最多等 3 秒（接力重启 / 前任退出中）。
fn acquire_single_instance(wait: bool) -> bool {
    let name = wide(SINGLE_INSTANCE_MUTEX);
    let start = std::time::Instant::now();
    unsafe {
        loop {
            let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if GetLastError() != ERROR_ALREADY_EXISTS {
                return true; // 句柄由进程退出时释放
            }
            if !h.is_null() {
                CloseHandle(h);
            }
            if !wait || start.elapsed().as_millis() > 3000 {
                return false;
            }
            Sleep(25);
        }
    }
}

fn main() {
    // 即使发生 panic（abort 之前），也尽力恢复被隐藏 / 半透明的窗口，避免用户窗口永久消失。
    // 链式调用原 hook，保留 panic 信息打印。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::win::window::restore_all_hidden();
        crate::win::window::restore_all_styled();
        crate::win::window::restore_all_topmost();
        crate::win::window::restore_all_captions();
        default_hook(info);
    }));

    unsafe {
        let class_name = wide(APP_WINDOW_CLASS);
        // 开机启动：只在后台运行，不弹出设置窗口。
        let silent = std::env::args().skip(1).any(|a| a == win::autostart::SILENT_ARG);

        // —— 单实例 ——
        // 命名互斥体（名字用不透明 GUID，不含应用名）。分两种情况：
        //   · 接力重启中（见 win::disguise）：直接等前任释放互斥体，不去唤起窗口。
        //   · 普通二次启动：立即唤起现有实例的设置窗口退出，不等待（开机启动则不唤起）。
        //     唤起不到窗口（前任退出中途）时再回退等待，避免接棒者误判成二次启动。
        if !acquire_single_instance(false) {
            let relay = win::disguise::relay_in_progress();
            if !relay {
                let existing = FindWindowW(class_name.as_ptr(), std::ptr::null());
                if !existing.is_null() {
                    if !silent {
                        PostMessageW(existing, WM_APP_SHOW, 0, 0);
                    }
                    return;
                }
            }
            // 接力中 / 无窗口可唤起（前任退出中途）：等它释放互斥体，最多 3 秒。
            if !acquire_single_instance(true) {
                return;
            }
        }

        // —— 进程伪装（见 win::disguise）：以别名副本运行，本进程随即退出 ——
        let mut cfg = Config::load();
        if cfg.disguise && !win::disguise::is_disguised() {
            if let Ok(exe) = std::env::current_exe() {
                cfg.origin = exe.to_string_lossy().into_owned();
                cfg.save(); // 记住原始位置，供“关闭伪装”时重启回来
            }
            if win::disguise::launch_disguised_copy(&mut cfg, silent) {
                return;
            }
            // 复制 / 启动失败（个别杀软会拦）：回退为不伪装继续运行。
        }

        let hinst = GetModuleHandleW(std::ptr::null());
        // 音频功能（静音 / 耳机检测）用到的 Core Audio 是 COM 接口。
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let icon_large = win::icon::load(IconSize::Large);
        let icon_small = win::icon::load(IconSize::Small);

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: icon_large,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(), // 自绘背景
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: icon_small,
        };
        RegisterClassExW(&wc);

        let title = wide(if win::disguise::is_disguised() {
            // 已伪装：窗口标题与托盘提示都不含应用名。
            "设置"
        } else {
            "BetterBossKey"
        });
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WINDOW_STYLE,
            0,
            0,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }

        // 用 SetWindowPos(SWP_SHOWWINDOW) 强制显示：ShowWindow 的首次调用会被
        // STARTUPINFO（例如以隐藏方式启动）覆盖而忽略参数，SetWindowPos 不受此限制。
        // 几何已在 WM_CREATE 的 resize_and_center 中设置好，这里只负责显示。
        if !silent {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_SHOWWINDOW,
            );
            SetForegroundWindow(hwnd);
            UpdateWindow(hwnd);
        }

        // 消息循环。
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 释放窗口类图标。
        for icon in [icon_large, icon_small] {
            if !icon.is_null() {
                DestroyIcon(icon);
            }
        }
    }
}

/// 取出绑定在窗口上的 App 指针。
unsafe fn app_from(hwnd: HWND) -> *mut App {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App
}

/// 请求重绘整个客户区（不擦背景，交由双缓冲绘制）。
unsafe fn invalidate(hwnd: HWND) {
    InvalidateRect(hwnd, std::ptr::null(), 0);
}

unsafe fn lo_word(l: isize) -> i32 {
    (l & 0xFFFF) as u16 as i16 as i32
}
unsafe fn hi_word(l: isize) -> i32 {
    ((l >> 16) & 0xFFFF) as u16 as i16 as i32
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            // 创建 App 并绑定到窗口。
            let app = Box::new(App::new(hwnd));
            let ptr = Box::into_raw(app);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
            (*ptr).on_create();

            apply_modern_frame(hwnd);
            resize_and_center(hwnd, &mut *ptr);
            0
        }
        WM_ERASEBKGND => 1, // 背景在 WM_PAINT 全量绘制，避免闪烁
        WM_PAINT => {
            let ptr = app_from(hwnd);
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut client);
            let (w, h) = (client.right, client.bottom);

            // 双缓冲：自顶向下 32bpp DIB，形状直接写像素（抗锯齿），文字走 GDI。
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let mut bits: *mut c_void = std::ptr::null_mut();
            let membmp = if w > 0 && h > 0 {
                CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0)
            } else {
                std::ptr::null_mut()
            };
            if !membmp.is_null() && !bits.is_null() {
                let memdc = CreateCompatibleDC(hdc);
                let oldbmp = SelectObject(memdc, membmp);
                if !ptr.is_null() {
                    let canvas = Canvas::new(memdc, bits as *mut u32, w, h);
                    (*ptr).paint(&canvas, client);
                }
                BitBlt(hdc, 0, 0, w, h, memdc, 0, 0, SRCCOPY);
                SelectObject(memdc, oldbmp);
                DeleteDC(memdc);
            }
            if !membmp.is_null() {
                DeleteObject(membmp);
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_LBUTTONDOWN => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                SetFocus(hwnd);
                let x = lo_word(lparam);
                let y = hi_word(lparam);
                let changed = (*ptr).on_lbutton_down(x, y);
                if (*ptr).ui.dragging_slider {
                    SetCapture(hwnd);
                }
                if changed {
                    invalidate(hwnd);
                }
            }
            0
        }
        WM_LBUTTONUP => {
            let ptr = app_from(hwnd);
            ReleaseCapture();
            if !ptr.is_null() && (*ptr).on_lbutton_up() {
                invalidate(hwnd);
            }
            0
        }
        WM_MBUTTONDOWN => {
            // 热键捕获中：把中键设为老板键。
            let ptr = app_from(hwnd);
            if !ptr.is_null() && (*ptr).ui.capturing_hotkey.is_some() {
                (*ptr).capture_mouse(MouseButton::Middle);
                invalidate(hwnd);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_XBUTTONDOWN => {
            // 热键捕获中：把侧键设为老板键。
            let ptr = app_from(hwnd);
            if !ptr.is_null() && (*ptr).ui.capturing_hotkey.is_some() {
                let xbtn = (wparam >> 16) & 0xFFFF; // 1=X1, 2=X2
                let b = if xbtn == 1 {
                    MouseButton::X1
                } else {
                    MouseButton::X2
                };
                (*ptr).capture_mouse(b);
                invalidate(hwnd);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSEMOVE => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                // 注册鼠标离开通知，以便清除 hover。
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
                let x = lo_word(lparam);
                let y = hi_word(lparam);
                if (*ptr).on_mouse_move(x, y) {
                    invalidate(hwnd);
                }
            }
            0
        }
        WM_PICK_MOVE => {
            // 拾取模式：钩子通知有移动，按屏幕坐标定位光标下的窗口并高亮。
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                let mut p = POINT { x: 0, y: 0 };
                GetCursorPos(&mut p);
                (*ptr).pick_move(p);
            }
            0
        }
        WM_PICK_SELECT => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).pick_select();
            }
            0
        }
        WM_PICK_CANCEL => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).pick_cancel();
            }
            0
        }
        WM_MOUSELEAVE => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() && (*ptr).on_mouse_leave() {
                invalidate(hwnd);
            }
            0
        }
        WM_MOUSEWHEEL => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                let delta = hi_word(wparam as isize);
                if (*ptr).on_mouse_wheel(delta) {
                    invalidate(hwnd);
                }
            }
            0
        }
        WM_KEYDOWN => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() && (*ptr).on_key_down(wparam as u32) {
                invalidate(hwnd);
            }
            0
        }
        WM_CHAR => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() && (*ptr).on_char(wparam as u32) {
                invalidate(hwnd);
            }
            0
        }
        WM_SYSKEYDOWN => {
            // Alt+键、F10 走 WM_SYSKEYDOWN：录制中同样交给热键录制，并吞掉以免激活系统菜单。
            let ptr = app_from(hwnd);
            if !ptr.is_null() && (*ptr).ui.capturing_hotkey.is_some() {
                if (*ptr).on_key_down(wparam as u32) {
                    invalidate(hwnd);
                }
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_ACTIVATE => {
            // 失去激活（切走 / 关到托盘）时结束录制，避免老板键一直处于暂停状态；
            // 时薪编辑则按确认处理。
            let ptr = app_from(hwnd);
            if !ptr.is_null() && (wparam & 0xFFFF) as u32 == WA_INACTIVE {
                let app = &mut *ptr;
                if app.ui.capturing_hotkey.is_some() || app.ui.editing_wage.is_some() {
                    app.end_hotkey_capture(None);
                    app.end_wage_edit(true);
                    invalidate(hwnd);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_HOTKEY => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                let app = &mut *ptr;
                let vk_of = |hk: Hotkey| match hk {
                    Hotkey::Key { vk, .. } => vk,
                    Hotkey::Mouse { .. } => 0,
                };
                match wparam as i32 {
                    HOTKEY_ID_TOGGLE => {
                        let vk = vk_of(app.cfg.hide_hotkey);
                        app.on_toggle_key(KeySource::Key(vk));
                    }
                    HOTKEY_ID_HIDE => app.on_hide_key(),
                    HOTKEY_ID_SHOW => {
                        let vk = vk_of(app.cfg.show_hotkey);
                        app.on_show_key(KeySource::Key(vk));
                    }
                    _ => {}
                }
                invalidate(hwnd);
            }
            0
        }
        WM_MOUSE_HOTKEY => {
            // 鼠标钩子检测到“切换键”（隐藏键与显示键相同）。
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).on_toggle_key(KeySource::Mouse);
                invalidate(hwnd);
            }
            0
        }
        WM_BOSS_HIDE => {
            // 鼠标钩子检测到隐藏键（隐藏键与显示键不同）。
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).on_hide_key();
                invalidate(hwnd);
            }
            0
        }
        WM_BOSS_SHOW => {
            // 鼠标钩子检测到显示键（隐藏键与显示键不同）。
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).on_show_key(KeySource::Mouse);
                invalidate(hwnd);
            }
            0
        }
        WM_BOSS_RELEASE => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).on_mouse_key_release();
                invalidate(hwnd);
            }
            0
        }
        WM_WHEEL_OPACITY => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).on_wheel_opacity(wparam as isize as i32);
                invalidate(hwnd);
            }
            0
        }
        WM_AUDIO_DEVICE => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).on_output_changed();
            }
            0
        }
        WM_DISPLAYCHANGE => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).check_displays();
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        watch::WM_WTSSESSION_CHANGE => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).on_session_change(wparam);
            }
            0
        }
        watch::WM_POWERBROADCAST => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).on_power(wparam, lparam);
            }
            1 // TRUE：同意（仅对查询类事件有意义）
        }
        WM_TIMER => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                match wparam {
                    TIMER_ID => (*ptr).on_timer(),
                    PEEK_TIMER_ID => (*ptr).on_peek_timer(),
                    _ => {}
                }
            }
            0
        }
        WM_TRAYICON => {
            // lParam 低位是鼠标事件。
            let event = (lparam as u32) & 0xFFFF;
            if event == WM_LBUTTONUP || event == 0x0203
            /* WM_LBUTTONDBLCLK */
            {
                show_settings(hwnd);
            } else if event == WM_RBUTTONUP {
                show_tray_menu(hwnd);
            }
            0
        }
        WM_APP_SHOW => {
            show_settings(hwnd);
            0
        }
        WM_COMMAND => {
            let id = wparam & 0xFFFF;
            let ptr = app_from(hwnd);
            match id {
                IDM_SHOW => show_settings(hwnd),
                IDM_HIDE_ALL if !ptr.is_null() => {
                    (*ptr).hide_all();
                    (*ptr).update_tray_tip();
                    invalidate(hwnd);
                }
                IDM_SHOW_ALL if !ptr.is_null() => {
                    (*ptr).show_all();
                    (*ptr).update_tray_tip();
                    invalidate(hwnd);
                }
                IDM_EXIT => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_DPICHANGED => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                let new_dpi = (wparam & 0xFFFF) as u32;
                (*ptr).ui.update_dpi(new_dpi);
                let prc = lparam as *const RECT;
                if !prc.is_null() {
                    let r = *prc;
                    // 用建议矩形的左上角定位，但按新 DPI 重算窗口尺寸，保证客户区与
                    // 内容高度一致（否则底部按钮可能被裁切或留白）。
                    let (cw, ch) = (*ptr).desired_client();
                    let mut wr = RECT {
                        left: 0,
                        top: 0,
                        right: cw,
                        bottom: ch,
                    };
                    AdjustWindowRectExForDpi(&mut wr, WINDOW_STYLE, 0, 0, new_dpi);
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        r.left,
                        r.top,
                        wr.right - wr.left,
                        wr.bottom - wr.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                invalidate(hwnd);
            }
            0
        }
        WM_CLOSE => {
            // 关闭设置窗口 = 退到托盘继续运行，而不是退出程序。
            ShowWindow(hwnd, SW_HIDE);
            0
        }
        WM_ENDSESSION => {
            // 注销 / 关机：恢复所有被隐藏 / 半透明的窗口，避免会话结束时窗口仍被隐藏。
            if wparam != 0 {
                let ptr = app_from(hwnd);
                if !ptr.is_null() {
                    (*ptr).show_all();
                    (*ptr).clear_opacity_all();
                }
                crate::win::window::restore_all_hidden();
                crate::win::window::restore_all_styled();
                crate::win::window::restore_all_topmost();
                crate::win::window::restore_all_captions();
                crate::recovery::persist();
            }
            0
        }
        WM_DESTROY => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                let mut app = Box::from_raw(ptr);
                app.cleanup();
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 显示并激活设置窗口。
unsafe fn show_settings(hwnd: HWND) {
    // 开机启动可能已在任务管理器里被改动，打开时重新读取。
    let ptr = app_from(hwnd);
    if !ptr.is_null() {
        (*ptr).refresh_autostart();
    }
    ShowWindow(hwnd, SW_SHOW);
    SetForegroundWindow(hwnd);
    invalidate(hwnd);
}

/// 弹出托盘右键菜单。
unsafe fn show_tray_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    AppendMenuW(menu, MF_STRING, IDM_SHOW, wide("打开设置").as_ptr());
    AppendMenuW(menu, MF_STRING, IDM_HIDE_ALL, wide("全部隐藏").as_ptr());
    AppendMenuW(menu, MF_STRING, IDM_SHOW_ALL, wide("全部恢复").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, IDM_EXIT, wide("退出").as_ptr());

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    // 必须先把窗口设为前台，菜单才能在点击别处时正确消失。
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_LEFTALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
}

/// 应用现代窗口外观：暗色标题栏 + 圆角（Win11）。
unsafe fn apply_modern_frame(hwnd: HWND) {
    // DWMWA_USE_IMMERSIVE_DARK_MODE = 20
    let dark: i32 = 1;
    DwmSetWindowAttribute(hwnd, 20, &dark as *const i32 as *const c_void, 4);
    // DWMWA_WINDOW_CORNER_PREFERENCE = 33, DWMWCP_ROUND = 2
    let round: u32 = 2;
    DwmSetWindowAttribute(hwnd, 33, &round as *const u32 as *const c_void, 4);
}

/// 按内容所需尺寸调整窗口，并在工作区居中。
unsafe fn resize_and_center(hwnd: HWND, app: &mut App) {
    let dpi = win::dpi_for(hwnd);
    let (cw, ch) = app.desired_client();
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: cw,
        bottom: ch,
    };
    AdjustWindowRectExForDpi(&mut rc, WINDOW_STYLE, 0, 0, dpi);
    let ww = rc.right - rc.left;
    let wh = rc.bottom - rc.top;

    // 在工作区（不含任务栏）居中。
    let mut wa: RECT = std::mem::zeroed();
    SystemParametersInfoW(
        SPI_GETWORKAREA,
        0,
        &mut wa as *mut RECT as *mut c_void,
        0,
    );
    let x = wa.left + ((wa.right - wa.left) - ww) / 2;
    let y = wa.top + ((wa.bottom - wa.top) - wh) / 2;
    SetWindowPos(hwnd, std::ptr::null_mut(), x, y, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
}
