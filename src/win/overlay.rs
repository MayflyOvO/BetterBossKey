//! 拾取模式用的红框 + 信息条覆盖窗。
//!
//! 实现方式：一个置顶、点击穿透(WS_EX_TRANSPARENT)、颜色键(LWA_COLORKEY)的分层窗。
//! 把内部填成“键色”即透明，只留红框与左上角的深色信息条可见。相比 UpdateLayeredWindow，
//! 颜色键无需处理预乘 alpha，GDI 画文字也不会出现透明问题，简单可靠。

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    InvalidateRect, SelectObject, SetBkMode, SetTextColor, UpdateWindow, ANTIALIASED_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, HFONT, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    RegisterClassExW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    GWLP_USERDATA, HWND_TOPMOST, LWA_COLORKEY, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::util::{rgb, wide};

const OVERLAY_CLASS: &str = "BetterBossKeyOverlay";
/// 颜色键：内部填成此色即透明。取一个几乎不会自然出现的颜色。
const KEY_COLOR: u32 = 0x00_03_02_01; // rgb(1,2,3)

// DrawText 标志
const DT_LEFT: u32 = 0x0;
const DT_SINGLELINE: u32 = 0x20;
const DT_NOPREFIX: u32 = 0x800;
const DT_END_ELLIPSIS: u32 = 0x8000;

/// 覆盖窗当前要绘制的内容（由 WM_PAINT 读取）。
struct DrawState {
    ft: i32,
    bar_w: i32,
    bar_h: i32,
    line1: Vec<u16>,
    line2: Vec<u16>,
    font1: HFONT,
    font2: HFONT,
}

pub struct Overlay {
    hwnd: HWND,
    state: *mut DrawState,
}

impl Overlay {
    pub fn new() -> Overlay {
        unsafe {
            let hinst = GetModuleHandleW(std::ptr::null());
            let class = wide(OVERLAY_CLASS);
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(overlay_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&wc); // 已注册则失败，无妨

            let state = Box::into_raw(Box::new(DrawState {
                ft: 3,
                bar_w: 0,
                bar_h: 0,
                line1: Vec::new(),
                line2: Vec::new(),
                font1: make_font(-19, 600),
                font2: make_font(-15, 400),
            }));

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );
            if !hwnd.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
                // 键色透明。
                SetLayeredWindowAttributes(hwnd, KEY_COLOR, 0, LWA_COLORKEY);
            }
            Overlay { hwnd, state }
        }
    }

    /// 在目标窗口外缘显示红框，并在左上角显示两行信息。
    pub fn show_for(&self, rect: RECT, line1: &str, line2: &str) {
        if self.hwnd.is_null() {
            return;
        }
        let ft = 3;
        let w = (rect.right - rect.left).max(1);
        let h = (rect.bottom - rect.top).max(1);
        // 覆盖窗向外扩 ft，使红框落在目标窗口的外缘。
        let ox = rect.left - ft;
        let oy = rect.top - ft;
        let ow = w + 2 * ft;
        let oh = h + 2 * ft;
        unsafe {
            let st = &mut *self.state;
            st.ft = ft;
            st.bar_h = 46.min(oh - 2 * ft).max(0);
            st.bar_w = 380.min(ow - 2 * ft).max(0);
            st.line1 = wide(line1);
            st.line2 = wide(line2);
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                ox,
                oy,
                ow,
                oh,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            InvalidateRect(self.hwnd, std::ptr::null(), 1);
            UpdateWindow(self.hwnd);
        }
    }

    pub fn hide(&self) {
        if !self.hwnd.is_null() {
            unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    pub fn destroy(&mut self) {
        unsafe {
            if !self.hwnd.is_null() {
                DestroyWindow(self.hwnd);
                self.hwnd = std::ptr::null_mut();
            }
            if !self.state.is_null() {
                let st = Box::from_raw(self.state);
                if !st.font1.is_null() {
                    DeleteObject(st.font1);
                }
                if !st.font2.is_null() {
                    DeleteObject(st.font2);
                }
                self.state = std::ptr::null_mut();
            }
        }
    }
}

fn make_font(height: i32, weight: i32) -> HFONT {
    let face = wide("Segoe UI");
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            ANTIALIASED_QUALITY as u32,
            (DEFAULT_PITCH | FF_DONTCARE) as u32,
            face.as_ptr(),
        )
    }
}

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const WM_PAINT: u32 = 0x000F;
    if msg == WM_PAINT {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rc);
        let w = rc.right;
        let h = rc.bottom;

        // 整体填键色（透明）。
        let key = CreateSolidBrush(KEY_COLOR);
        FillRect(hdc, &rc, key);
        DeleteObject(key);

        let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const DrawState;
        if !st.is_null() {
            let st = &*st;
            let ft = st.ft;
            // 红框（四条边）。
            let red = CreateSolidBrush(rgb(255, 45, 45));
            let edges = [
                RECT { left: 0, top: 0, right: w, bottom: ft },
                RECT { left: 0, top: h - ft, right: w, bottom: h },
                RECT { left: 0, top: 0, right: ft, bottom: h },
                RECT { left: w - ft, top: 0, right: w, bottom: h },
            ];
            for e in &edges {
                FillRect(hdc, e, red);
            }
            DeleteObject(red);

            // 信息条。
            if st.bar_w > 0 && st.bar_h > 0 {
                let bar = RECT {
                    left: ft,
                    top: ft,
                    right: ft + st.bar_w,
                    bottom: ft + st.bar_h,
                };
                let dark = CreateSolidBrush(rgb(22, 22, 30));
                FillRect(hdc, &bar, dark);
                DeleteObject(dark);

                SetBkMode(hdc, TRANSPARENT as i32);
                let flags = DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;
                let tx = ft + 10;
                let tr = ft + st.bar_w - 8;

                SelectObject(hdc, st.font1);
                SetTextColor(hdc, rgb(255, 255, 255));
                let mut r1 = RECT {
                    left: tx,
                    top: ft + 5,
                    right: tr,
                    bottom: ft + 26,
                };
                DrawTextW(hdc, st.line1.as_ptr(), -1, &mut r1, flags);

                SelectObject(hdc, st.font2);
                SetTextColor(hdc, rgb(178, 184, 208));
                let mut r2 = RECT {
                    left: tx,
                    top: ft + 26,
                    right: tr,
                    bottom: ft + 44,
                };
                DrawTextW(hdc, st.line2.as_ptr(), -1, &mut r2, flags);
            }
        }
        EndPaint(hwnd, &ps);
        return 0;
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
