//! 全局热键（老板键）：支持键盘组合键，以及鼠标中键 / 侧键（可带修饰键）。
//!
//! 键盘触发用 `RegisterHotKey`；鼠标触发由 [`crate::win::mouse_hook`] 的低级钩子处理。
//! 本模块负责热键模型，以及热键与可读字符串（如 `Ctrl+Alt+H`、`中键`）的互转。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};

/// 可作为老板键触发的鼠标按键。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Middle, // 中键
    X1,     // 侧键1（通常是“后退”）
    X2,     // 侧键2（通常是“前进”）
}

/// 老板键的两个可独立配置的槽位：隐藏键 / 显示键。
/// 两者相同时退化为“切换键”（同一组合不能重复注册）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HkSlot {
    Hide,
    Show,
}

/// 老板键触发方式：键盘组合键，或带修饰键的鼠标按键。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    Key { mods: u32, vk: u32 },
    Mouse { mods: u32, button: MouseButton },
}

impl Default for Hotkey {
    fn default() -> Self {
        // 默认老板键：Ctrl+Alt+H
        Hotkey::Key {
            mods: MOD_CONTROL | MOD_ALT,
            vk: 0x48, // 'H'
        }
    }
}

impl Hotkey {
    pub fn mods(&self) -> u32 {
        match self {
            Hotkey::Key { mods, .. } => *mods,
            Hotkey::Mouse { mods, .. } => *mods,
        }
    }

    /// 注册键盘热键。鼠标触发返回 false（应由鼠标钩子处理，调用方据此分流）。
    /// 自动附加 MOD_NOREPEAT，避免长按连发。
    pub fn register_key(&self, hwnd: HWND, id: i32) -> bool {
        if let Hotkey::Key { mods, vk } = self {
            if *vk == 0 {
                return false;
            }
            unsafe { RegisterHotKey(hwnd, id, mods | MOD_NOREPEAT, *vk) != 0 }
        } else {
            false
        }
    }

    /// 注销之前用同一 id 注册的键盘热键。
    pub fn unregister(hwnd: HWND, id: i32) {
        unsafe {
            UnregisterHotKey(hwnd, id);
        }
    }

    /// 该热键是否合法。
    pub fn is_valid(&self) -> bool {
        match self {
            Hotkey::Key { vk, .. } => *vk != 0,
            Hotkey::Mouse { .. } => true,
        }
    }

    /// 生成可读字符串，如 `Ctrl+Alt+H` 或 `Ctrl+中键`。
    pub fn to_display_string(self) -> String {
        let mods = self.mods();
        let mut parts: Vec<&str> = Vec::new();
        if mods & MOD_CONTROL != 0 {
            parts.push("Ctrl");
        }
        if mods & MOD_ALT != 0 {
            parts.push("Alt");
        }
        if mods & MOD_SHIFT != 0 {
            parts.push("Shift");
        }
        if mods & MOD_WIN != 0 {
            parts.push("Win");
        }
        let key = match self {
            Hotkey::Key { vk, .. } => vk_name(vk),
            Hotkey::Mouse { button, .. } => mouse_name(button).to_string(),
        };
        let mut s = parts.join("+");
        if !s.is_empty() {
            s.push('+');
        }
        s.push_str(&key);
        s
    }

    /// 从 `Ctrl+Alt+H`、`中键`、`Ctrl+侧键1` 这样的字符串解析。
    pub fn parse(s: &str) -> Option<Hotkey> {
        let mut mods = 0u32;
        let mut vk: Option<u32> = None;
        let mut mouse: Option<MouseButton> = None;
        for token in s.split('+') {
            let t = token.trim();
            if t.is_empty() {
                continue;
            }
            match t.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods |= MOD_CONTROL,
                "alt" => mods |= MOD_ALT,
                "shift" => mods |= MOD_SHIFT,
                "win" | "super" | "meta" => mods |= MOD_WIN,
                _ => {
                    if let Some(b) = parse_mouse(t) {
                        mouse = Some(b);
                    } else {
                        vk = name_to_vk(t);
                    }
                }
            }
        }
        if let Some(button) = mouse {
            Some(Hotkey::Mouse { mods, button })
        } else {
            vk.map(|vk| Hotkey::Key { mods, vk })
        }
    }
}

fn mouse_name(b: MouseButton) -> &'static str {
    match b {
        MouseButton::Middle => "中键",
        MouseButton::X1 => "侧键1",
        MouseButton::X2 => "侧键2",
    }
}

fn parse_mouse(t: &str) -> Option<MouseButton> {
    match t {
        "中键" => return Some(MouseButton::Middle),
        "侧键1" => return Some(MouseButton::X1),
        "侧键2" => return Some(MouseButton::X2),
        _ => {}
    }
    match t.to_ascii_lowercase().as_str() {
        "middle" | "mbutton" | "mid" => Some(MouseButton::Middle),
        "x1" | "xbutton1" | "mouse4" | "back" => Some(MouseButton::X1),
        "x2" | "xbutton2" | "mouse5" | "forward" => Some(MouseButton::X2),
        _ => None,
    }
}

/// 判断某个虚拟键码本身是否是修饰键（用于热键捕获时忽略单独的 Ctrl/Alt 等）。
pub fn is_modifier_vk(vk: u32) -> bool {
    matches!(
        vk,
        0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0 | 0xA1 | 0xA2 | 0xA3 | 0xA4 | 0xA5
    )
}

/// 虚拟键码 -> 可读名称。
pub fn vk_name(vk: u32) -> String {
    if (0x41..=0x5A).contains(&vk) {
        return ((vk as u8) as char).to_string();
    }
    if (0x30..=0x39).contains(&vk) {
        return ((vk as u8) as char).to_string();
    }
    if (0x70..=0x87).contains(&vk) {
        return format!("F{}", vk - 0x70 + 1);
    }
    if (0x60..=0x69).contains(&vk) {
        return format!("Num{}", vk - 0x60);
    }
    let named = match vk {
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0D => "Enter",
        0x1B => "Esc",
        0x20 => "Space",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2D => "Insert",
        0x2E => "Delete",
        0x6A => "Num*",
        0x6B => "Num+",
        0x6D => "Num-",
        0x6E => "Num.",
        0x6F => "Num/",
        0xBA => ";",
        0xBB => "=",
        0xBC => ",",
        0xBD => "-",
        0xBE => ".",
        0xBF => "/",
        0xC0 => "`",
        0xDB => "[",
        0xDC => "\\",
        0xDD => "]",
        0xDE => "'",
        _ => "",
    };
    if !named.is_empty() {
        named.to_string()
    } else if vk == 0 {
        "—".to_string()
    } else {
        format!("0x{:02X}", vk)
    }
}

/// 名称 -> 虚拟键码（parse 的反向）。
fn name_to_vk(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    if upper.len() == 1 {
        let c = upper.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + n - 1);
            }
        }
    }
    let vk = match upper.as_str() {
        "BACKSPACE" => 0x08,
        "TAB" => 0x09,
        "ENTER" | "RETURN" => 0x0D,
        "ESC" | "ESCAPE" => 0x1B,
        "SPACE" => 0x20,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "END" => 0x23,
        "HOME" => 0x24,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "INSERT" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        ";" => 0xBA,
        "=" => 0xBB,
        "," => 0xBC,
        "-" => 0xBD,
        "." => 0xBE,
        "/" => 0xBF,
        "`" => 0xC0,
        "[" => 0xDB,
        "\\" => 0xDC,
        "]" => 0xDD,
        "'" => 0xDE,
        _ => return None,
    };
    Some(vk)
}
