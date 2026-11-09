//! 应用图标：由 build.rs（设计见 `build/icon.rs`）在构建期绘制并作为资源嵌入
//! exe；exe 文件、窗口与托盘共用这一份，运行时按系统 DPI 取最合适的尺寸。

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{LoadIconMetric, LIM_LARGE, LIM_SMALL};
use windows_sys::Win32::UI::WindowsAndMessaging::{CopyIcon, LoadIconW, HICON, IDI_APPLICATION};

/// 图标组资源 ID，须与 `build/icon.rs` 的 `RESOURCE_ID` 一致。
const APP_ICON_ID: u16 = 1;

#[derive(Clone, Copy)]
pub enum IconSize {
    /// 托盘、窗口标题栏（SM_CXSMICON）。
    Small,
    /// 任务栏、Alt+Tab（SM_CXICON）。
    Large,
}

/// 加载应用图标。调用方负责 `DestroyIcon`。
/// 资源缺失（如非 MSVC 构建）时回退到系统默认图标的副本。
pub fn load(size: IconSize) -> HICON {
    let metric = match size {
        IconSize::Small => LIM_SMALL,
        IconSize::Large => LIM_LARGE,
    };
    let mut icon: HICON = std::ptr::null_mut();
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let name = APP_ICON_ID as usize as *const u16; // MAKEINTRESOURCEW
        if LoadIconMetric(hinst, name, metric, &mut icon) < 0 || icon.is_null() {
            icon = CopyIcon(LoadIconW(std::ptr::null_mut(), IDI_APPLICATION));
        }
    }
    icon
}
