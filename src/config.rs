//! 配置的持久化。
//!
//! 为了保持“零额外依赖”，这里不引入 serde / json 等库，而是用一个极简的
//! `key=value` 文本格式，保存在 `%APPDATA%\BetterBossKey\config.ini`。

use std::fs;
use std::path::PathBuf;

use crate::win::hotkey::Hotkey;

/// 透明度允许的范围（百分比）。低于此值窗口几乎不可见，没有意义。
pub const MIN_OPACITY: u8 = 20;
pub const MAX_OPACITY: u8 = 100;

/// 自动隐藏延迟的范围（毫秒）。
pub const MIN_DELAY_MS: u32 = 0;
pub const MAX_DELAY_MS: u32 = 5000;

/// 一个被记住的受管目标，用于跨重启（重启本程序或目标程序）自动恢复管理。
///
/// 仅凭标题再匹配很脆弱（标题常含动态内容，重启后还可能变化），因此额外记录
/// 进程可执行名与窗口类名：即便目标程序重启、HWND 改变、标题略变，也能据
/// `进程名 + 类名` 把它重新认出来。
#[derive(Clone)]
pub struct RememberedTarget {
    /// 进程可执行文件名，如 `phoneCast.exe`（读不到进程名时为空）。
    pub exe: String,
    /// 窗口类名。
    pub class: String,
    /// 选中时的窗口标题。
    pub title: String,
}

#[derive(Clone)]
pub struct Config {
    /// 老板键·隐藏键。与 `show_hotkey` 相同时退化为切换键。
    pub hide_hotkey: Hotkey,
    /// 老板键·显示键。
    pub show_hotkey: Hotkey,
    /// 是否启用“鼠标移出窗口后自动隐藏”。
    pub auto_hide: bool,
    /// 鼠标离开窗口后，延迟多少毫秒再隐藏。
    pub auto_hide_delay_ms: u32,
    /// 鼠标回到原窗口位置后，延迟多少毫秒再显示。
    pub reveal_delay_ms: u32,
    /// 自动唤出窗口时是否把它切到前台（抢焦点）。关闭则只显示、不打断当前操作。
    pub auto_focus: bool,
    /// 受管窗口的不透明度（百分比，100 = 完全不透明）。
    pub opacity: u8,
    /// 是否对受管窗口应用半透明。
    pub transparency_enabled: bool,
    /// 启动时伪装进程名：以别名副本运行（见 `win::disguise`）。
    pub disguise: bool,
    /// 伪装别名（进程名，不含扩展名，如 `NovaSync`）。首次随机生成后固定复用。
    pub alias: String,
    /// 伪装前原始 exe 的完整路径，用于取消伪装时重启回原程序（内部键）。
    pub origin: String,
    /// 记住的受管目标，用于跨重启自动恢复。
    pub remembered_targets: Vec<RememberedTarget>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hide_hotkey: Hotkey::default(),
            show_hotkey: Hotkey::default(),
            auto_hide: false,
            auto_hide_delay_ms: 600,
            reveal_delay_ms: 300,
            auto_focus: false,
            opacity: 85,
            transparency_enabled: false,
            disguise: true,
            alias: String::new(),
            origin: String::new(),
            remembered_targets: Vec::new(),
        }
    }
}

/// 把记住的目标编码进 `target=` 行：`exe<TAB>class<TAB>title`。
/// 标题中的制表符 / 换行会破坏格式，统一替换为空格。
fn encode_target(t: &RememberedTarget) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
            .collect()
    };
    format!("{}\t{}\t{}", clean(&t.exe), clean(&t.class), clean(&t.title))
}

/// 解析 `target=` 行的值（未 trim，保留标题空白）：`exe<TAB>class<TAB>title`。
fn decode_target(raw: &str) -> Option<RememberedTarget> {
    let mut parts = raw.splitn(3, '\t');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(exe), Some(class), Some(title)) => Some(RememberedTarget {
            exe: exe.to_string(),
            class: class.to_string(),
            title: title.to_string(),
        }),
        _ => None,
    }
}

impl Config {
    /// 返回配置文件的完整路径：`%APPDATA%\BetterBossKey\config.ini`。
    pub fn path() -> Option<PathBuf> {
        let appdata = std::env::var_os("APPDATA")?;
        let mut p = PathBuf::from(appdata);
        p.push("BetterBossKey");
        p.push("config.ini");
        Some(p)
    }

    /// 从磁盘加载配置；文件不存在或解析失败时返回默认配置。
    pub fn load() -> Config {
        let mut cfg = Config::default();
        let Some(path) = Config::path() else {
            return cfg;
        };
        let Ok(text) = fs::read_to_string(&path) else {
            return cfg;
        };
        // 去掉可能的 UTF-8 BOM（记事本等编辑器保存时会加），否则首行键名会带上
        // U+FEFF 而匹配不上（Rust 的 trim 不会去除 BOM）。
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

        for raw in text.lines() {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // 在原始行上按首个 '=' 切分：target 标题保留首尾空白（与保存端字节一致，
            // 保证精确再匹配），其余数值键照常 trim。
            let Some((key, raw_value)) = raw.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = raw_value.trim();
            match key {
                "hide_hotkey" => {
                    if let Some(hk) = Hotkey::parse(value) {
                        cfg.hide_hotkey = hk;
                    }
                }
                "show_hotkey" => {
                    if let Some(hk) = Hotkey::parse(value) {
                        cfg.show_hotkey = hk;
                    }
                }
                "auto_hide" => cfg.auto_hide = value == "1" || value.eq_ignore_ascii_case("true"),
                "auto_focus" => {
                    cfg.auto_focus = value == "1" || value.eq_ignore_ascii_case("true")
                }
                "auto_hide_delay_ms" => {
                    if let Ok(v) = value.parse::<u32>() {
                        cfg.auto_hide_delay_ms = v.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
                    }
                }
                "reveal_delay_ms" => {
                    if let Ok(v) = value.parse::<u32>() {
                        cfg.reveal_delay_ms = v.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
                    }
                }
                "opacity" => {
                    if let Ok(v) = value.parse::<u8>() {
                        cfg.opacity = v.clamp(MIN_OPACITY, MAX_OPACITY);
                    }
                }
                "transparency_enabled" => {
                    cfg.transparency_enabled = value == "1" || value.eq_ignore_ascii_case("true")
                }
                "disguise" => cfg.disguise = value == "1" || value.eq_ignore_ascii_case("true"),
                "alias" => cfg.alias = value.to_string(),
                // origin 含完整路径（可能有空格 / 非 ASCII），不做 trim 之外的加工。
                "origin" => cfg.origin = raw_value.trim().to_string(),
                "target" => {
                    if let Some(t) = decode_target(raw_value) {
                        cfg.remembered_targets.push(t);
                    }
                }
                _ => {}
            }
        }
        cfg
    }

    /// 把当前配置写回磁盘。出错时静默忽略（配置丢失不致命）。
    pub fn save(&self) {
        let Some(path) = Config::path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }

        let mut out = String::new();
        out.push_str("# BetterBossKey 配置文件 (自动生成)\n");
        out.push_str(&format!("hide_hotkey={}\n", self.hide_hotkey.to_display_string()));
        out.push_str(&format!("show_hotkey={}\n", self.show_hotkey.to_display_string()));
        out.push_str(&format!("auto_hide={}\n", if self.auto_hide { 1 } else { 0 }));
        out.push_str(&format!("auto_hide_delay_ms={}\n", self.auto_hide_delay_ms));
        out.push_str(&format!("reveal_delay_ms={}\n", self.reveal_delay_ms));
        out.push_str(&format!("auto_focus={}\n", if self.auto_focus { 1 } else { 0 }));
        out.push_str(&format!("opacity={}\n", self.opacity));
        out.push_str(&format!(
            "transparency_enabled={}\n",
            if self.transparency_enabled { 1 } else { 0 }
        ));
        out.push_str(&format!("disguise={}\n", if self.disguise { 1 } else { 0 }));
        if !self.alias.is_empty() {
            out.push_str(&format!("alias={}\n", self.alias));
        }
        if !self.origin.is_empty() {
            out.push_str(&format!("origin={}\n", self.origin));
        }
        for t in &self.remembered_targets {
            out.push_str(&format!("target={}\n", encode_target(t)));
        }

        let _ = fs::write(&path, out);
    }
}
