//! Win32 封装层：把原始 FFI 调用收拢成内聚、易用的小模块。
//!
//! * [`window`] 顶层窗口枚举与隐藏 / 显示 / 半透明 / 鼠标穿透
//! * [`hotkey`] 全局热键（老板键）
//! * [`mouse_hook`] 鼠标钩子：鼠标老板键、拾取模式、Alt+滚轮
//! * [`audio`]  按程序静音、默认输出设备监视
//! * [`watch`]  锁屏 / 熄屏 / 显示输出 / 屏幕角
//! * [`tray`]   系统托盘图标
//! * [`icon`]   从 exe 资源加载应用图标
//! * [`disguise`] 进程伪装（随机名临时副本运行）
//!
//! 另外提供 DPI 相关的小工具。进程级 DPI 感知由 app.manifest 声明
//! （Per-Monitor V2），运行时只需按窗口 DPI 做尺寸换算。

pub mod audio;
pub mod disguise;
pub mod hotkey;
pub mod icon;
pub mod mouse_hook;
pub mod overlay;
pub mod tray;
pub mod watch;
pub mod window;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;

/// 获取某窗口当前所在显示器的 DPI（失败回退到 96）。
pub fn dpi_for(hwnd: HWND) -> u32 {
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 {
        96
    } else {
        d
    }
}

/// 把以 96 DPI 设计的逻辑像素，按给定 DPI 缩放为物理像素。
#[inline]
pub fn scale(value: i32, dpi: u32) -> i32 {
    (value as i64 * dpi as i64 / 96) as i32
}
