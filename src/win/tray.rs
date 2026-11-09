//! 系统托盘图标封装（Shell_NotifyIcon）。
//!
//! 主窗口关闭后程序退到托盘继续运行，托盘图标承载左键切换 / 右键菜单。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::HICON;

use crate::util::copy_to_wide_buf;

/// 托盘图标。生命周期结束（Drop）时自动从托盘移除。
pub struct Tray {
    nid: NOTIFYICONDATAW,
}

impl Tray {
    /// 在托盘区添加图标。
    ///
    /// * `hwnd` 接收回调消息的窗口
    /// * `id`   图标 id（同一窗口可有多个图标时区分用）
    /// * `callback_msg` 托盘事件回调消息（一般取 WM_APP+n）
    /// * `icon` 图标句柄
    /// * `tip`  悬停提示文字
    pub fn new(hwnd: HWND, id: u32, callback_msg: u32, icon: HICON, tip: &str) -> Tray {
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = id;
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = callback_msg;
        nid.hIcon = icon;
        copy_to_wide_buf(&mut nid.szTip, tip);

        unsafe {
            Shell_NotifyIconW(NIM_ADD, &nid);
        }
        Tray { nid }
    }

    /// 更新悬停提示文字。
    pub fn set_tip(&mut self, tip: &str) {
        self.nid.uFlags = NIF_TIP;
        copy_to_wide_buf(&mut self.nid.szTip, tip);
        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &self.nid);
        }
    }

}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.nid);
        }
    }
}
