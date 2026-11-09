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

/// 默认货币符号（只填数字时使用）。
pub const DEFAULT_CURRENCY: &str = "¥";

/// 解析后的时薪：数字前后的文字即货币符号，金额以“分”（1/100）为单位，避免浮点。
pub struct Wage {
    prefix: String,
    cents_per_hour: u64,
    suffix: String,
}

impl Wage {
    /// 从 `¥50`、`$25.5`、`50元`、`50元/小时` 这样的输入解析；没有有效的正数则返回 None。
    /// 数字之后到 `/` 为止的文字作为后缀；小数超过两位的部分舍去。
    pub fn parse(s: &str) -> Option<Wage> {
        let s = s.trim();
        let is_num = |c: char| c.is_ascii_digit() || c == '.';
        let start = s.find(is_num)?;
        let rest = &s[start..];
        let len = rest.find(|c: char| !is_num(c)).unwrap_or(rest.len());
        let (int, frac) = rest[..len].split_once('.').unwrap_or((&rest[..len], ""));
        if frac.contains('.') {
            return None;
        }
        let int: u64 = if int.is_empty() { 0 } else { int.parse().ok()? };
        let frac: u64 = match frac.len() {
            0 => 0,
            1 => frac.parse::<u64>().ok()? * 10,
            _ => frac[..2].parse().ok()?,
        };
        let cents = int.checked_mul(100)?.checked_add(frac)?;
        // 上限一亿每小时：足够任何人，也保证一天的乘积不溢出。
        if cents == 0 || cents > 10_000_000_000 {
            return None;
        }
        let tail = &rest[len..];
        let suffix = tail.split('/').next().unwrap_or("").trim();
        let mut prefix = s[..start].trim();
        if prefix.is_empty() && suffix.is_empty() {
            prefix = DEFAULT_CURRENCY;
        }
        Some(Wage {
            prefix: prefix.to_string(),
            cents_per_hour: cents,
            suffix: suffix.to_string(),
        })
    }

    /// 摸了 `secs` 秒对应的金额，如 `¥54.17`。
    pub fn earned(&self, secs: u64) -> String {
        let c = secs * self.cents_per_hour / 3600;
        format!("{}{}.{:02}{}", self.prefix, c / 100, c % 100, self.suffix)
    }
}

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
    /// 按住显示：显示键只在按住期间显示，松开即隐藏。
    pub peek: bool,
    /// 老板键隐藏时静音受管程序，显示时恢复。
    pub mute_on_hide: bool,
    /// 老板键隐藏后把上一个使用过的非受管窗口切到前台。
    pub cover: bool,
    /// 受管窗口被显示时设为置顶（目标程序自行取消时重新置顶）。
    pub topmost: bool,
    /// 鼠标甩到屏幕角时隐藏：0 = 关，1 左上，2 右上，3 左下，4 右下。
    pub corner: u8,
    /// 锁屏或熄屏时隐藏。
    pub hide_on_away: bool,
    /// 接投影（激活的显示输出变多）时隐藏。
    pub hide_on_projector: bool,
    /// 耳机断开（默认输出设备从耳机切走）时隐藏并静音。
    pub hide_on_headphones: bool,
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
    /// 半透明窗口鼠标穿透（按住 Ctrl 时暂停穿透）。
    pub click_through: bool,
    /// 在受管窗口上 Alt+滚轮调节不透明度。
    pub wheel_opacity: bool,
    /// 统计今日受管窗口的可见时长。
    pub stats: bool,
    /// 时薪，原样保存用户输入（如 `¥50`、`$25.5`、`50元`），留空表示不计算金额。
    pub wage: String,
    /// 统计所属日期（YYYYMMDD，内部键）。
    pub stats_day: u32,
    /// 当日累计秒数（内部键）。
    pub stats_secs: u64,
    /// 是否显示托盘图标。关闭后再次运行程序即可打开设置。
    pub tray_icon: bool,
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
            peek: false,
            mute_on_hide: false,
            cover: false,
            topmost: false,
            corner: 0,
            hide_on_away: false,
            hide_on_projector: false,
            hide_on_headphones: false,
            auto_hide: false,
            auto_hide_delay_ms: 600,
            reveal_delay_ms: 300,
            auto_focus: false,
            opacity: 85,
            transparency_enabled: false,
            click_through: false,
            wheel_opacity: false,
            stats: false,
            wage: String::new(),
            stats_day: 0,
            stats_secs: 0,
            tray_icon: true,
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
    /// 配置目录：`%APPDATA%\BetterBossKey`。
    pub fn dir() -> Option<PathBuf> {
        let appdata = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(appdata).join("BetterBossKey"))
    }

    /// 返回配置文件的完整路径：`%APPDATA%\BetterBossKey\config.ini`。
    pub fn path() -> Option<PathBuf> {
        Some(Config::dir()?.join("config.ini"))
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
            let on = value == "1" || value.eq_ignore_ascii_case("true");
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
                "peek" => cfg.peek = on,
                "mute_on_hide" => cfg.mute_on_hide = on,
                "cover" => cfg.cover = on,
                "topmost" => cfg.topmost = on,
                "corner" => {
                    if let Ok(v) = value.parse::<u8>() {
                        cfg.corner = if v <= 4 { v } else { 0 };
                    }
                }
                "hide_on_away" => cfg.hide_on_away = on,
                "hide_on_projector" => cfg.hide_on_projector = on,
                "hide_on_headphones" => cfg.hide_on_headphones = on,
                "auto_hide" => cfg.auto_hide = on,
                "auto_focus" => cfg.auto_focus = on,
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
                "transparency_enabled" => cfg.transparency_enabled = on,
                "click_through" => cfg.click_through = on,
                "wheel_opacity" => cfg.wheel_opacity = on,
                "stats" => cfg.stats = on,
                "wage" => cfg.wage = value.to_string(),
                "stats_day" => cfg.stats_day = value.parse().unwrap_or(0),
                "stats_secs" => cfg.stats_secs = value.parse().unwrap_or(0),
                "tray_icon" => cfg.tray_icon = on,
                "disguise" => cfg.disguise = on,
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

        let b = |v: bool| if v { 1 } else { 0 };
        let mut out = String::new();
        out.push_str("# BetterBossKey 配置文件 (自动生成)\n");
        out.push_str(&format!("hide_hotkey={}\n", self.hide_hotkey.to_display_string()));
        out.push_str(&format!("show_hotkey={}\n", self.show_hotkey.to_display_string()));
        out.push_str(&format!("peek={}\n", b(self.peek)));
        out.push_str(&format!("mute_on_hide={}\n", b(self.mute_on_hide)));
        out.push_str(&format!("cover={}\n", b(self.cover)));
        out.push_str(&format!("topmost={}\n", b(self.topmost)));
        out.push_str(&format!("corner={}\n", self.corner));
        out.push_str(&format!("hide_on_away={}\n", b(self.hide_on_away)));
        out.push_str(&format!("hide_on_projector={}\n", b(self.hide_on_projector)));
        out.push_str(&format!("hide_on_headphones={}\n", b(self.hide_on_headphones)));
        out.push_str(&format!("auto_hide={}\n", b(self.auto_hide)));
        out.push_str(&format!("auto_hide_delay_ms={}\n", self.auto_hide_delay_ms));
        out.push_str(&format!("reveal_delay_ms={}\n", self.reveal_delay_ms));
        out.push_str(&format!("auto_focus={}\n", b(self.auto_focus)));
        out.push_str(&format!("opacity={}\n", self.opacity));
        out.push_str(&format!("transparency_enabled={}\n", b(self.transparency_enabled)));
        out.push_str(&format!("click_through={}\n", b(self.click_through)));
        out.push_str(&format!("wheel_opacity={}\n", b(self.wheel_opacity)));
        out.push_str(&format!("stats={}\n", b(self.stats)));
        if !self.wage.is_empty() {
            out.push_str(&format!("wage={}\n", self.wage));
        }
        out.push_str(&format!("stats_day={}\n", self.stats_day));
        out.push_str(&format!("stats_secs={}\n", self.stats_secs));
        out.push_str(&format!("tray_icon={}\n", b(self.tray_icon)));
        out.push_str(&format!("disguise={}\n", b(self.disguise)));
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
