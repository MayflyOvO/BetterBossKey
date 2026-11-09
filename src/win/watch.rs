//! 系统状态监视：锁屏 / 熄屏通知、激活的显示输出数、屏幕角判定。

use std::sync::atomic::{AtomicIsize, Ordering};

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::Display::{GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS};
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::Power::{
    RegisterPowerSettingNotification, UnregisterPowerSettingNotification, POWERBROADCAST_SETTING,
};
use windows_sys::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE;

pub const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
pub const WTS_SESSION_LOCK: usize = 0x7;
pub const WTS_SESSION_UNLOCK: usize = 0x8;
pub const WM_POWERBROADCAST: u32 = 0x0218;
const PBT_POWERSETTINGCHANGE: usize = 0x8013;

/// GUID_CONSOLE_DISPLAY_STATE：显示器 关(0) / 开(1) / 变暗(2)。
const GUID_CONSOLE_DISPLAY_STATE: GUID = GUID::from_u128(0x6FE69556_704A_47A0_8F24_C28D936FDA47);

static POWER_NOTIFY: AtomicIsize = AtomicIsize::new(0);

/// 注册锁屏与显示器开关通知（分别以 WM_WTSSESSION_CHANGE / WM_POWERBROADCAST 送达）。
pub fn register(hwnd: HWND) {
    unsafe {
        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        let h = RegisterPowerSettingNotification(
            hwnd as _,
            &GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        POWER_NOTIFY.store(h as isize, Ordering::SeqCst);
    }
}

pub fn unregister(hwnd: HWND) {
    unsafe {
        WTSUnRegisterSessionNotification(hwnd);
        let h = POWER_NOTIFY.swap(0, Ordering::SeqCst);
        if h != 0 {
            UnregisterPowerSettingNotification(h as _);
        }
    }
}

/// 解析 WM_POWERBROADCAST：若是显示器状态变化，返回显示器是否亮着（变暗也算亮着）。
pub fn display_state(wparam: usize, lparam: LPARAM) -> Option<bool> {
    if wparam != PBT_POWERSETTINGCHANGE || lparam == 0 {
        return None;
    }
    unsafe {
        let s = &*(lparam as *const POWERBROADCAST_SETTING);
        let g = &s.PowerSetting;
        let ours = &GUID_CONSOLE_DISPLAY_STATE;
        if g.data1 != ours.data1 || g.data2 != ours.data2 || g.data3 != ours.data3 || g.data4 != ours.data4 {
            return None;
        }
        let state = std::ptr::read_unaligned(s.Data.as_ptr() as *const u32);
        Some(state != 0)
    }
}

/// 激活的显示输出数。复制屏幕模式下每个输出各算一条路径，故接投影时也会增加。
pub fn active_display_paths() -> u32 {
    let (mut paths, mut modes) = (0u32, 0u32);
    let ok = unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes) };
    if ok == 0 {
        paths
    } else {
        0
    }
}

/// 光标是否位于其所在显示器的某个角（1 左上，2 右上，3 左下，4 右下）。
pub fn at_corner(p: POINT, corner: u8) -> bool {
    const SLOP: i32 = 2;
    unsafe {
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi) == 0 {
            return false;
        }
        let r = mi.rcMonitor;
        let left = p.x < r.left + SLOP;
        let right = p.x >= r.right - SLOP;
        let top = p.y < r.top + SLOP;
        let bottom = p.y >= r.bottom - SLOP;
        match corner {
            1 => left && top,
            2 => right && top,
            3 => left && bottom,
            4 => right && bottom,
            _ => false,
        }
    }
}
