//! 视觉主题：配色方案与字体。
//!
//! 采用现代深色（indigo / charcoal）配色，强调色为靛蓝。所有颜色是
//! COLORREF(0x00BBGGRR)。字体按窗口 DPI 创建，DPI 变化时重建。

use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, CLEARTYPE_QUALITY, DEFAULT_CHARSET, FF_DONTCARE, HFONT,
    OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, DEFAULT_PITCH,
};

use crate::util::{rgb, wide};

// —— 配色 ——
pub const BG: u32 = rgb(0x15, 0x17, 0x21); // 窗口背景（近黑靛蓝）
pub const CARD: u32 = rgb(0x1E, 0x20, 0x2E); // 卡片 / 分区背景
pub const SURFACE: u32 = rgb(0x27, 0x2A, 0x3B); // 控件表面
pub const SURFACE_HOVER: u32 = rgb(0x32, 0x35, 0x4A); // 控件 hover
pub const BORDER: u32 = rgb(0x36, 0x39, 0x50); // 描边
pub const TEXT: u32 = rgb(0xEC, 0xEC, 0xF4); // 主文字
pub const TEXT_DIM: u32 = rgb(0x97, 0x9A, 0xB6); // 次要文字
pub const TEXT_OFF: u32 = rgb(0x5C, 0x5F, 0x78); // 禁用态文字 / 旋钮
pub const DIVIDER: u32 = rgb(0x26, 0x29, 0x39); // 分组分隔线
pub const ACCENT: u32 = rgb(0x6C, 0x7B, 0xFF); // 强调色（靛蓝）
pub const ACCENT_HOVER: u32 = rgb(0x84, 0x90, 0xFF); // 强调色 hover
pub const ON_ACCENT: u32 = rgb(0xFF, 0xFF, 0xFF); // 强调色之上的文字
pub const TRACK: u32 = rgb(0x33, 0x36, 0x4B); // 滑块轨道
pub const DANGER: u32 = rgb(0xFF, 0x6B, 0x6B); // 危险 / 强提示

/// 把磅值(pt)按 DPI 换算为 CreateFont 需要的负高度像素。
fn pt_to_height(pt: f32, dpi: u32) -> i32 {
    -((pt * dpi as f32 / 72.0).round() as i32)
}

fn make_font(pt: f32, dpi: u32, weight: i32) -> HFONT {
    let face = wide("Segoe UI");
    unsafe {
        CreateFontW(
            pt_to_height(pt, dpi),
            0,
            0,
            0,
            weight,
            0, // italic
            0, // underline
            0, // strikeout
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            (DEFAULT_PITCH | FF_DONTCARE) as u32,
            face.as_ptr(),
        )
    }
}

const FW_NORMAL: i32 = 400;
const FW_SEMIBOLD: i32 = 600;

/// 一组按 DPI 创建的字体。
pub struct Fonts {
    pub body: HFONT,
    pub body_strong: HFONT,
    pub small: HFONT,
}

impl Fonts {
    pub fn new(dpi: u32) -> Fonts {
        Fonts {
            body: make_font(10.0, dpi, FW_NORMAL),
            body_strong: make_font(10.0, dpi, FW_SEMIBOLD),
            small: make_font(8.5, dpi, FW_NORMAL),
        }
    }

    /// 释放字体对象。
    pub fn destroy(&self) {
        unsafe {
            for f in [self.body, self.body_strong, self.small] {
                if !f.is_null() {
                    DeleteObject(f);
                }
            }
        }
    }
}
