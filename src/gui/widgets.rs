//! 自绘控件的绘制原语：圆角矩形、圆、对勾与文本。
//!
//! 背景缓冲是 32bpp DIB（见 `main.rs` 的 WM_PAINT）。形状用有符号距离场逐像素计算
//! 覆盖率再混合，得到抗锯齿边缘（GDI 的 RoundRect / Ellipse / LineTo 都没有抗锯齿）；
//! 文字仍交给 GDI DrawText（ClearType）。形状与文字共用同一裁剪区，见 [`Canvas::clipped`]。

use std::cell::Cell;

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    DrawTextW, GdiFlush, IntersectClipRect, RestoreDC, SaveDC, SelectObject, SetBkMode,
    SetTextColor, HDC, HFONT, TRANSPARENT,
};

use crate::util::wide;

// 常用 DrawText 标志组合。
pub const DT_LEFT: u32 = 0x0000_0000;
pub const DT_CENTER: u32 = 0x0000_0001;
pub const DT_RIGHT: u32 = 0x0000_0002;
pub const DT_VCENTER: u32 = 0x0000_0004;
pub const DT_SINGLELINE: u32 = 0x0000_0020;
pub const DT_NOPREFIX: u32 = 0x0000_0800;
pub const DT_END_ELLIPSIS: u32 = 0x0000_8000;

/// 绘制目标：选入了自顶向下 32bpp DIB 的内存 DC，以及直接访问像素的指针。
pub struct Canvas {
    hdc: HDC,
    bits: *mut u32,
    width: i32,
    clip: Cell<RECT>,
}

impl Canvas {
    /// # Safety
    /// `bits` 必须指向已选入 `hdc` 的自顶向下 32bpp DIB 像素（`width`×`height`），
    /// 并在 Canvas 存活期间保持有效。
    pub unsafe fn new(hdc: HDC, bits: *mut u32, width: i32, height: i32) -> Canvas {
        Canvas {
            hdc,
            bits,
            width,
            clip: Cell::new(RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            }),
        }
    }

    /// 在 `r` 与当前裁剪区的交集内执行 `f`；形状与文字都会被裁剪。
    pub fn clipped(&self, r: &RECT, f: impl FnOnce()) {
        let old = self.clip.get();
        let clip = RECT {
            left: old.left.max(r.left),
            top: old.top.max(r.top),
            right: old.right.min(r.right),
            bottom: old.bottom.min(r.bottom),
        };
        unsafe {
            let saved = SaveDC(self.hdc);
            IntersectClipRect(self.hdc, r.left, r.top, r.right, r.bottom);
            self.clip.set(clip);
            f();
            self.clip.set(old);
            RestoreDC(self.hdc, saved);
        }
    }

    /// 对 `bounds`（含抗锯齿边缘的外接框）内的每个像素，按 `cover(像素中心 x, y)`
    /// 返回的覆盖率（0..=1）把 `color` 混合上去。
    fn shade(&self, bounds: [f32; 4], color: u32, cover: impl Fn(f32, f32) -> f32) {
        let c = self.clip.get();
        let x0 = (bounds[0].floor() as i32).max(c.left);
        let y0 = (bounds[1].floor() as i32).max(c.top);
        let x1 = (bounds[2].ceil() as i32).min(c.right);
        let y1 = (bounds[3].ceil() as i32).min(c.bottom);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let src = to_pixel(color);
        unsafe {
            GdiFlush(); // 先让排队中的 GDI 文字落到像素上，保持绘制顺序
            for y in y0..y1 {
                let row = self.bits.add((y * self.width) as usize);
                for x in x0..x1 {
                    let a = cover(x as f32 + 0.5, y as f32 + 0.5);
                    if a <= 0.0 {
                        continue;
                    }
                    let p = row.add(x as usize);
                    *p = if a >= 1.0 { src } else { blend(*p, src, a) };
                }
            }
        }
    }
}

/// COLORREF(0x00BBGGRR) → DIB 像素(0x00RRGGBB)。
fn to_pixel(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

fn blend(dst: u32, src: u32, a: f32) -> u32 {
    let a = (a * 256.0) as u32;
    let mix = |shift: u32| {
        let d = (dst >> shift) & 0xFF;
        let s = (src >> shift) & 0xFF;
        ((s * a + d * (256 - a)) >> 8) << shift
    };
    mix(16) | mix(8) | mix(0)
}

/// 距离（负值在内）→ 像素覆盖率。
#[inline]
fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// 向量长度。用 sqrt 而非 `f32::hypot`：后者会从静态 CRT 拉入额外代码，增大 exe。
#[inline]
fn len(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

/// 圆角矩形的有符号距离（RECT 的边即几何边，right/bottom 不含）。
fn sd_round_rect(px: f32, py: f32, r: [f32; 4], radius: f32) -> f32 {
    let hx = (r[2] - r[0]) / 2.0;
    let hy = (r[3] - r[1]) / 2.0;
    let rad = radius.min(hx).min(hy).max(0.0);
    let qx = (px - (r[0] + hx)).abs() - (hx - rad);
    let qy = (py - (r[1] + hy)).abs() - (hy - rad);
    len(qx.max(0.0), qy.max(0.0)) + qx.max(qy).min(0.0) - rad
}

fn dist_to_segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((px - a.0) * abx + (py - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    len(px - (a.0 + abx * t), py - (a.1 + aby * t))
}

fn frect(r: &RECT) -> [f32; 4] {
    [r.left as f32, r.top as f32, r.right as f32, r.bottom as f32]
}

/// 实心矩形（轴对齐、整像素边，无需抗锯齿）。
pub fn fill_rect(c: &Canvas, r: &RECT, color: u32) {
    c.shade(frect(r), color, |_, _| 1.0);
}

/// 实心圆角矩形。
pub fn round_fill(c: &Canvas, r: &RECT, radius: i32, color: u32) {
    let fr = frect(r);
    c.shade(fr, color, |x, y| coverage(sd_round_rect(x, y, fr, radius as f32)));
}

/// 带描边的圆角矩形，可选填充。
pub fn round_outline(c: &Canvas, r: &RECT, radius: i32, border: u32, border_w: i32, fill: Option<u32>) {
    let outer = frect(r);
    let bw = border_w as f32;
    let inner = [outer[0] + bw, outer[1] + bw, outer[2] - bw, outer[3] - bw];
    let (ro, ri) = (radius as f32, (radius - border_w) as f32);
    match fill {
        Some(f) => {
            c.shade(outer, border, |x, y| coverage(sd_round_rect(x, y, outer, ro)));
            c.shade(inner, f, |x, y| coverage(sd_round_rect(x, y, inner, ri)));
        }
        None => c.shade(outer, border, |x, y| {
            coverage(sd_round_rect(x, y, outer, ro)) - coverage(sd_round_rect(x, y, inner, ri))
        }),
    }
}

/// 实心圆（圆心 cx, cy 为几何坐标）。
pub fn disc(c: &Canvas, cx: i32, cy: i32, radius: i32, color: u32) {
    let (fx, fy, fr) = (cx as f32, cy as f32, radius as f32);
    let bounds = [fx - fr - 1.0, fy - fr - 1.0, fx + fr + 1.0, fy + fr + 1.0];
    c.shade(bounds, color, |x, y| coverage(len(x - fx, y - fy) - fr));
}

/// 在给定矩形内画一个对勾（用于复选框选中态），`weight` 为线宽（像素）。
pub fn checkmark(c: &Canvas, r: &RECT, color: u32, weight: f32) {
    let (w, h) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
    let at = |fx: f32, fy: f32| (r.left as f32 + w * fx, r.top as f32 + h * fy);
    let (p0, p1, p2) = (at(0.26, 0.52), at(0.43, 0.68), at(0.75, 0.34));
    let half = weight / 2.0;
    c.shade(frect(r), color, |x, y| {
        coverage(dist_to_segment(x, y, p0, p1).min(dist_to_segment(x, y, p1, p2)) - half)
    });
}

/// 绘制单行文本。
pub fn text(c: &Canvas, s: &str, r: &RECT, font: HFONT, color: u32, flags: u32) {
    unsafe {
        let mut rc = *r;
        let w = wide(s);
        let old = SelectObject(c.hdc, font);
        SetBkMode(c.hdc, TRANSPARENT as i32);
        SetTextColor(c.hdc, color);
        DrawTextW(c.hdc, w.as_ptr(), -1, &mut rc, flags);
        SelectObject(c.hdc, old); // 还原原字体，与其它原语保持一致
    }
}
