//! 应用图标：设计（几何 → 超采样光栅化）与 PNG / ICO / .res 编码。
//!
//! 只由 build.rs 使用：构建期绘制全部尺寸并作为资源嵌入 exe，exe 文件图标、
//! 窗口与托盘都从这份资源加载，保证处处一致。纯 Rust、无外部依赖。
//!
//! 造型：一枚靛蓝键帽（底部深色“裙边”表现按键的立体感，呼应“老板键”），
//! 键面上是一扇白色“磨砂玻璃”窗口：边框与标题栏实心，窗体半透明并向下渐隐
//! （呼应“隐藏 / 半透明”）。48px 及以下的尺寸把边缘对齐到整像素，保证托盘等小尺寸清晰。

/// 生成的全部尺寸。各尺寸均以 PNG 存储（Vista 起系统支持任意尺寸的 PNG 图标帧）。
pub const SIZES: [u32; 11] = [16, 20, 24, 28, 32, 40, 48, 64, 96, 128, 256];
/// 图标组资源 ID，运行时 `src/win/icon.rs` 按此 ID 加载。
pub const RESOURCE_ID: u16 = 1;

// 配色（与 GUI 主题强调色 #6C7BFF 一致）。
const FACE_TOP: [f32; 3] = [0x86 as f32 / 255.0, 0x94 as f32 / 255.0, 0xFF as f32 / 255.0];
const FACE_BOTTOM: [f32; 3] = [0x5E as f32 / 255.0, 0x6D as f32 / 255.0, 0xF6 as f32 / 255.0];
const SKIRT: [f32; 3] = [0x3A as f32 / 255.0, 0x43 as f32 / 255.0, 0xB0 as f32 / 255.0];
const GLYPH: [f32; 3] = [1.0, 1.0, 1.0];
/// 窗体（标题栏以下）顶部 / 底部的不透明度。
const BODY_ALPHA: (f32, f32) = (0.55, 0.12);

#[derive(Clone, Copy)]
struct Rect {
    l: f32,
    t: f32,
    r: f32,
    b: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }

    /// 圆角矩形内部测试。
    fn contains_round(&self, x: f32, y: f32, radius: f32) -> bool {
        if !self.contains(x, y) {
            return false;
        }
        let rad = radius.min((self.r - self.l) / 2.0).min((self.b - self.t) / 2.0);
        let qx = x.clamp(self.l + rad, self.r - rad);
        let qy = y.clamp(self.t + rad, self.b - rad);
        let (dx, dy) = (x - qx, y - qy);
        dx * dx + dy * dy <= rad * rad
    }
}

/// 某一尺寸下的图标几何（像素坐标）。
struct Geometry {
    key: Rect,
    face: Rect,
    key_radius: f32,
    win: Rect,
    win_radius: f32,
    /// 窗口内部（标题栏以下、边框以内）的透空区域。
    win_hole: Rect,
}

impl Geometry {
    fn new(n: u32) -> Geometry {
        let s = n as f32;
        let snap = |v: f32| if n <= 48 { v.round() } else { v };
        let margin = snap(s * 0.0625);
        let skirt = snap((s * 0.075).max(1.0));
        let key = Rect { l: margin, t: margin, r: s - margin, b: s - margin };
        // 大尺寸时键面四周略内收，露出一圈侧边（底部为厚裙边），更像真实键帽；
        // 小尺寸保持齐平以免糊边。
        let inset = if n > 48 { s * 0.025 } else { 0.0 };
        let face = Rect { l: key.l + inset, t: key.t + inset, r: key.r - inset, b: key.b - skirt };

        // 窗口：在键面内水平居中、垂直居中。
        let w = snap(s * 0.52);
        let h = snap(s * 0.42);
        let l = snap((s - w) / 2.0);
        let t = snap((face.t + face.b - h) / 2.0);
        let win = Rect { l, t, r: l + w, b: t + h };
        let stroke = snap((s * 0.06).max(1.0));
        let title = snap((s * 0.13).max(2.0));
        let win_hole = Rect {
            l: win.l + stroke,
            t: win.t + title,
            r: win.r - stroke,
            b: win.b - stroke,
        };
        Geometry {
            key,
            face,
            key_radius: s * 0.22,
            win,
            win_radius: (s * 0.045).max(0.6),
            win_hole,
        }
    }

    /// 单个采样点的颜色（预乘 alpha）。
    fn shade(&self, x: f32, y: f32) -> [f32; 4] {
        let mut c = [0.0f32; 4];
        if self.key.contains_round(x, y, self.key_radius) {
            c = over(c, SKIRT, 1.0);
        }
        if self.face.contains_round(x, y, self.key_radius) {
            let t = ((y - self.face.t) / (self.face.b - self.face.t)).clamp(0.0, 1.0);
            c = over(c, lerp(FACE_TOP, FACE_BOTTOM, t), 1.0);
        }
        if self.win.contains_round(x, y, self.win_radius) {
            let a = if self.win_hole.contains(x, y) {
                let t = (y - self.win_hole.t) / (self.win_hole.b - self.win_hole.t);
                BODY_ALPHA.0 + (BODY_ALPHA.1 - BODY_ALPHA.0) * t.clamp(0.0, 1.0)
            } else {
                1.0
            };
            c = over(c, GLYPH, a);
        }
        c
    }
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// 把直 alpha 颜色叠加到预乘颜色之上。
fn over(dst: [f32; 4], rgb: [f32; 3], a: f32) -> [f32; 4] {
    let k = 1.0 - a;
    [rgb[0] * a + dst[0] * k, rgb[1] * a + dst[1] * k, rgb[2] * a + dst[2] * k, a + dst[3] * k]
}

/// 渲染 n×n 图标，返回直 alpha 的 RGBA8 像素（自顶向下）。
pub fn render(n: u32) -> Vec<u8> {
    let g = Geometry::new(n);
    let ss = if n >= 128 { 4 } else { 8 };
    let mut out = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let mut acc = [0.0f32; 4];
            for sy in 0..ss {
                for sx in 0..ss {
                    let px = x as f32 + (sx as f32 + 0.5) / ss as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / ss as f32;
                    let c = g.shade(px, py);
                    for k in 0..4 {
                        acc[k] += c[k];
                    }
                }
            }
            let cnt = (ss * ss) as f32;
            let a = acc[3] / cnt;
            let i = ((y * n + x) * 4) as usize;
            if a > 0.0 {
                for k in 0..3 {
                    out[i + k] = (acc[k] / cnt / a * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
            out[i + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}

// ——————————————————————— ICO / .res ———————————————————————

/// 独立的 .ico 文件（供预览 / 外部使用）。`images` 为 (边长, PNG 数据)。
pub fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut v = Vec::new();
    put16(&mut v, 0);
    put16(&mut v, 1);
    put16(&mut v, images.len() as u32);
    let mut offset = 6 + 16 * images.len() as u32;
    for (n, data) in images {
        v.extend_from_slice(&[dim(*n), dim(*n), 0, 0]);
        put16(&mut v, 1);
        put16(&mut v, 32);
        put32(&mut v, data.len() as u32);
        put32(&mut v, offset);
        offset += data.len() as u32;
    }
    for (_, data) in images {
        v.extend_from_slice(data);
    }
    v
}

/// Win32 资源文件（.res）：每个尺寸一个 RT_ICON，外加 ID 为 `group_id` 的 RT_GROUP_ICON。
/// MSVC 链接器可直接把 .res 链入 exe，无需 rc.exe。
pub fn res(images: &[(u32, Vec<u8>)], group_id: u16) -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    let mut v = Vec::new();
    res_entry(&mut v, 0, 0, 0, &[]); // .res 文件约定的空首项
    let mut group = Vec::new();
    put16(&mut group, 0);
    put16(&mut group, 1);
    put16(&mut group, images.len() as u32);
    for (i, (n, data)) in images.iter().enumerate() {
        let id = i as u16 + 1;
        res_entry(&mut v, RT_ICON, id, 0x1010, data);
        group.extend_from_slice(&[dim(*n), dim(*n), 0, 0]);
        put16(&mut group, 1);
        put16(&mut group, 32);
        put32(&mut group, data.len() as u32);
        put16(&mut group, id as u32);
    }
    res_entry(&mut v, RT_GROUP_ICON, group_id, 0x1030, &group);
    v
}

fn res_entry(v: &mut Vec<u8>, ty: u16, name: u16, flags: u16, data: &[u8]) {
    put32(v, data.len() as u32);
    put32(v, 32); // 头长度（类型与名称均为数字 ID）
    put16(v, 0xFFFF);
    put16(v, ty as u32);
    put16(v, 0xFFFF);
    put16(v, name as u32);
    put32(v, 0); // DataVersion
    put16(v, flags as u32);
    put16(v, 0); // 语言：中立
    put32(v, 0); // Version
    put32(v, 0); // Characteristics
    v.extend_from_slice(data);
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

/// ICO 目录里的宽高字节：256 记为 0。
fn dim(n: u32) -> u8 {
    if n >= 256 {
        0
    } else {
        n as u8
    }
}

fn put16(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&(x as u16).to_le_bytes());
}

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

// ——————————————————————— PNG ———————————————————————

/// 编码 RGBA8 PNG（每行自适应滤波 + 固定哈夫曼 deflate）。
pub fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = (w * 4) as usize;
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    let zero = vec![0u8; stride];
    for y in 0..h as usize {
        let cur = &rgba[y * stride..(y + 1) * stride];
        let up = if y == 0 { &zero[..] } else { &rgba[(y - 1) * stride..y * stride] };
        let (ft, row) = (0u8..=4)
            .map(|ft| (ft, png_filter(ft, cur, up)))
            .min_by_key(|(_, row)| row.iter().map(|&b| (b as i8).unsigned_abs() as u32).sum::<u32>())
            .unwrap();
        raw.push(ft);
        raw.extend_from_slice(&row);
    }

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn png_filter(ft: u8, cur: &[u8], up: &[u8]) -> Vec<u8> {
    let bpp = 4;
    (0..cur.len())
        .map(|i| {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = up[i];
            let c = if i >= bpp { up[i - bpp] } else { 0 };
            let pred = match ft {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                _ => paeth(a, b, c),
            };
            cur[i].wrapping_sub(pred)
        })
        .collect()
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn png_chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(&deflate(data));
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

struct Bits {
    out: Vec<u8>,
    buf: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, v: u32, n: u32) {
        self.buf |= (v as u64) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.buf as u8);
            self.buf >>= 8;
            self.n -= 8;
        }
    }

    /// 哈夫曼码按高位在前写入。
    fn huff(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    fn literal(&mut self, v: u32) {
        match v {
            0..=143 => self.huff(0x30 + v, 8),
            144..=255 => self.huff(0x190 + v - 144, 9),
            256..=279 => self.huff(v - 256, 7),
            _ => self.huff(0xC0 + v - 280, 8),
        }
    }
}

const LEN_BASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// 单块固定哈夫曼 deflate，LZ77 用哈希链查找匹配。
fn deflate(data: &[u8]) -> Vec<u8> {
    const WINDOW: usize = 32768;
    const MAX_LEN: usize = 258;
    const NONE: usize = usize::MAX;
    let mut bits = Bits { out: Vec::new(), buf: 0, n: 0 };
    bits.put(1, 1); // BFINAL
    bits.put(1, 2); // BTYPE = 固定哈夫曼

    let hash = |i: usize| {
        ((data[i] as usize) << 10 ^ (data[i + 1] as usize) << 5 ^ data[i + 2] as usize) & 0x7FFF
    };
    let mut head = vec![NONE; 1 << 15];
    let mut prev = vec![NONE; data.len()];
    let insert = |i: usize, head: &mut [usize], prev: &mut [usize]| {
        if i + 3 <= data.len() {
            let h = hash(i);
            prev[i] = head[h];
            head[h] = i;
        }
    };

    let mut i = 0;
    while i < data.len() {
        let (mut best_len, mut best_dist) = (0, 0);
        if i + 3 <= data.len() {
            let max = (data.len() - i).min(MAX_LEN);
            let mut j = head[hash(i)];
            let mut chain = 128;
            while j != NONE && i - j <= WINDOW && chain > 0 {
                let mut l = 0;
                while l < max && data[j + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - j;
                    if l == max {
                        break;
                    }
                }
                j = prev[j];
                chain -= 1;
            }
        }
        if best_len >= 3 {
            let k = LEN_BASE.iter().rposition(|&b| b as usize <= best_len).unwrap();
            bits.literal(257 + k as u32);
            bits.put(best_len as u32 - LEN_BASE[k], LEN_EXTRA[k]);
            let d = DIST_BASE.iter().rposition(|&b| b as usize <= best_dist).unwrap();
            bits.huff(d as u32, 5);
            bits.put(best_dist as u32 - DIST_BASE[d], DIST_EXTRA[d]);
            for p in i..i + best_len {
                insert(p, &mut head, &mut prev);
            }
            i += best_len;
        } else {
            bits.literal(data[i] as u32);
            insert(i, &mut head, &mut prev);
            i += 1;
        }
    }
    bits.literal(256);
    if bits.n > 0 {
        bits.out.push(bits.buf as u8);
    }
    bits.out
}
