//! 通用工具：宽字符串转换、颜色构造等无依赖的小工具。

/// Rust `&str` -> 以 NUL 结尾的 UTF-16 缓冲，供 Win32 的 *W API 使用。
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 从（可能不以 NUL 结尾的）UTF-16 缓冲构造 String，遇到第一个 NUL 截断。
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 把固定长度 `[u16; N]` 字段（如 szTip）写入字符串，自动截断并保证 NUL 结尾。
pub fn copy_to_wide_buf(dst: &mut [u16], s: &str) {
    if dst.is_empty() {
        return;
    }
    let src: Vec<u16> = s.encode_utf16().collect();
    let n = src.len().min(dst.len().saturating_sub(1));
    dst[..n].copy_from_slice(&src[..n]);
    dst[n] = 0;
    for c in dst.iter_mut().skip(n + 1) {
        *c = 0;
    }
}

/// 构造 Win32 COLORREF(0x00BBGGRR)。
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}
