//! 应用状态与核心业务逻辑。
//!
//! `App` 持有全部运行时状态，并实现各项功能：
//!   1. 老板键：一键隐藏 / 恢复所有受管窗口（全局热键触发），可选按住显示
//!   2. 隐藏时的附带动作：静音、切回工作窗口
//!   3. 自动隐藏：鼠标移出、屏幕角、锁屏 / 熄屏、接投影、耳机断开
//!   4. 半透明：可调不透明度、鼠标穿透、Alt+滚轮调节
//!   5. 摸鱼统计、崩溃恢复
//!
//! GUI 的布局 / 绘制 / 交互方法定义在 `gui::ui` 中（同一 `impl App` 拆分到不同模块）。

use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT, SYSTEMTIME};
use windows_sys::Win32::Graphics::Gdi::InvalidateRect;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DestroyWindow, GetCursorPos, KillTimer, SetForegroundWindow, SetTimer, ShowWindow,
    HICON, SW_HIDE, SW_SHOW, WM_APP,
};

use crate::config::{
    Config, RememberedTarget, Wage, DEFAULT_CURRENCY, MAX_DELAY_MS, MAX_OPACITY, MIN_DELAY_MS,
    MIN_OPACITY,
};
use crate::gui::ui::Ui;
use crate::win::hotkey::{HkSlot, Hotkey};
use crate::win::mouse_hook::Binding;
use crate::win::tray::Tray;
use crate::win::{self, audio, mouse_hook, watch, window};

// —— 运行时常量 ——
/// 老板键键盘热键的注册 id：隐藏 / 显示 / 切换（隐藏键与显示键相同时用切换）。
pub const HOTKEY_ID_HIDE: i32 = 1;
pub const HOTKEY_ID_SHOW: i32 = 2;
pub const HOTKEY_ID_TOGGLE: i32 = 3;
pub const TIMER_ID: usize = 1;
/// “按住显示”期间轮询按键是否松开的定时器。
pub const PEEK_TIMER_ID: usize = 2;
const PEEK_POLL_MS: u32 = 15;
/// 定时器轮询间隔：兼顾自动隐藏的灵敏度与半透明的快速重申（目标程序重置后能很快恢复）。
pub const TIMER_INTERVAL_MS: u32 = 80;
/// 周期性维护（刷新列表、回收失效目标、按记忆重新挂接）的最小间隔。
pub const MAINTENANCE_INTERVAL_MS: u32 = 750;
/// 托盘回调消息。
pub const WM_TRAYICON: u32 = WM_APP + 1;
/// 鼠标钩子触发老板键“切换”的回调消息（WM_APP+2 已被主窗口的“显示设置”占用）。
pub const WM_MOUSE_HOTKEY: u32 = WM_APP + 3;
/// 拾取模式钩子投递给主窗口的消息。
pub const WM_PICK_MOVE: u32 = WM_APP + 4;
pub const WM_PICK_SELECT: u32 = WM_APP + 5;
pub const WM_PICK_CANCEL: u32 = WM_APP + 6;
/// 鼠标钩子触发老板键“隐藏 / 显示”（隐藏键与显示键不同时）的回调消息。
pub const WM_BOSS_HIDE: u32 = WM_APP + 7;
pub const WM_BOSS_SHOW: u32 = WM_APP + 8;
/// 鼠标老板键抬起（“按住显示”据此在松开时隐藏）。
pub const WM_BOSS_RELEASE: u32 = WM_APP + 9;
/// 默认输出设备变化（耳机断开检测）。
pub const WM_AUDIO_DEVICE: u32 = WM_APP + 10;
/// Alt+滚轮调节不透明度，wParam 为滚动量。
pub const WM_WHEEL_OPACITY: u32 = WM_APP + 11;
pub const TRAY_UID: u32 = 1;
/// Alt+滚轮每格调节的不透明度（百分点）。
const WHEEL_OPACITY_STEP: i32 = 5;
/// 摸鱼统计每累计这么多秒写一次盘。
const STATS_SAVE_SECS: u64 = 60;

/// 触发老板键的按键来源（“按住显示”需要知道松开的是哪个键）。
#[derive(Clone, Copy, PartialEq)]
pub enum KeySource {
    /// 键盘热键，携带主键虚拟键码。
    Key(u32),
    /// 鼠标按键（松开时由钩子通知）。
    Mouse,
}

/// 一个受管窗口及其自动隐藏 / 透明度的运行时状态。
pub struct Target {
    pub hwnd: isize,
    pub title: String,
    /// 加入时窗口所属进程 ID，用于识别 HWND 被回收复用。
    pub pid: u32,
    /// 加入时窗口类名，用于识别 HWND 被回收复用，以及跨重启再匹配。
    pub class: String,
    /// 加入时进程可执行名（如 `phoneCast.exe`），用于跨重启再匹配。
    pub exe: String,
    /// 鼠标是否已进入过该窗口（自动隐藏只在“进入后再离开”时触发，避免一启用就消失）。
    pub armed: bool,
    /// 鼠标离开窗口的时间点，用于延迟隐藏判定。
    pub left_at: Option<Instant>,
    /// 该窗口当前是否被本程序隐藏。
    pub hidden_by_us: bool,
    /// 是否由“鼠标移出自动隐藏”隐藏（区别于老板键隐藏）。
    /// 仅自动隐藏的窗口才会在光标回到原位置时自动显示。
    pub auto_hidden: bool,
    /// 自动隐藏时记下的窗口屏幕矩形，用于光标回到原位时自动唤出。
    pub hidden_rect: Option<RECT>,
    /// 光标回到原位置的时间点，用于“显示延迟”判定。
    pub reenter_at: Option<Instant>,
    /// 是否已对该窗口应用了半透明。
    pub opacity_applied: bool,
    /// 该目标对应的记忆项标识（“记忆时”的 exe/class/title）。
    ///
    /// 取消选中时据此**精确**删除对应记忆项：实时标题可能已漂移（动态标题、
    /// 或重启后再匹配到的标题不同），若仍按当前标题去匹配就会删不掉记忆项，
    /// 导致维护 tick 又把它重新挂接回来。这里锁定“记忆时”的标识以保证可取消。
    pub remembered: RememberedTarget,
}

impl Target {
    fn new(hwnd: isize, title: String) -> Target {
        let pid = window::window_pid(hwnd);
        let class = window::class_of(hwnd);
        let exe = window::process_name(hwnd);
        // 默认记忆标识 = 当前 (exe, class, title)，即“手动勾选加入”那一刻的身份。
        // reattach 时会用记忆项的原始标识覆盖它。
        let remembered = RememberedTarget {
            exe: exe.clone(),
            class: class.clone(),
            title: title.clone(),
        };
        Target {
            hwnd,
            title,
            pid,
            class,
            exe,
            armed: false,
            left_at: None,
            hidden_by_us: false,
            auto_hidden: false,
            hidden_rect: None,
            reenter_at: None,
            opacity_applied: false,
            remembered,
        }
    }
}

/// 校验目标句柄当前是否仍是当初那个窗口。
///
/// 仅用 `IsWindow` 不够：窗口销毁后 Windows 会把同一 HWND 数值分配给新窗口，
/// 若不校验身份，老板键 / 自动隐藏 / 半透明可能误作用到无辜的新窗口上。
/// 因此额外比对进程 ID 与类名。
fn target_live(t: &Target) -> bool {
    window::is_alive(t.hwnd)
        && window::window_pid(t.hwnd) == t.pid
        && window::class_of(t.hwnd) == t.class
}

/// 候选窗口与记忆项的匹配分（0 = 不匹配）。
///
/// 以「进程名」为锚——用户重启的是某个“软件”，进程名最稳定；再要求类名或标题至少
/// 命中其一，以免误抓同进程的无关窗口（如控制台）。这样无论标题是动态的，还是窗口类名
/// 随进程变化（某些框架如此），只要该软件再次出现就能被认出。命中越多分越高，便于在
/// 多个同进程窗口里挑最贴近的。
fn match_score(exe: &str, class: &str, title: &str, rt: &RememberedTarget) -> i32 {
    if !exe.eq_ignore_ascii_case(&rt.exe) {
        return 0; // 进程名必须一致
    }
    let mut s = 1;
    if class == rt.class {
        s += 2;
    }
    if title == rt.title {
        s += 4;
    }
    // 仅进程名相同、类名与标题都不同 → 视为无关窗口，不匹配。
    if s == 1 {
        0
    } else {
        s
    }
}

/// 某个存活目标是否对应某个记忆项（用于判断记忆项是否已被挂接）。
fn remembered_matches_target(rt: &RememberedTarget, t: &Target) -> bool {
    match_score(&t.exe, &t.class, &t.title, rt) > 0
}

/// 两个记忆项是否为同一条（进程名忽略大小写，类名 / 标题精确相等）。
/// 用于去重、以及取消选中时精确定位要删除的那一条记忆项。
fn same_remembered(a: &RememberedTarget, b: &RememberedTarget) -> bool {
    a.exe.eq_ignore_ascii_case(&b.exe) && a.class == b.class && a.title == b.title
}

pub struct App {
    pub hwnd: HWND,
    pub cfg: Config,
    pub targets: Vec<Target>,
    pub window_list: Vec<window::WindowInfo>,
    pub tray: Option<Tray>,
    pub icon: HICON,
    pub ui: Ui,
    /// 老板键整体隐藏态。
    pub all_hidden: bool,
    /// 隐藏键注册是否成功（失败通常意味着与其它软件的全局热键冲突）。
    pub hide_ok: bool,
    /// 显示键注册是否成功。
    pub show_ok: bool,
    /// 是否处于“拾取窗口”模式。
    pub picking: bool,
    /// 拾取模式下当前高亮的窗口句柄（用于避免重复重绘）。
    last_pick_hwnd: isize,
    /// 拾取模式的红框 + 信息条覆盖窗。
    overlay: win::overlay::Overlay,
    /// 上次周期性维护的时间点（节流 refresh / 重新挂接）。
    last_maintenance: Option<Instant>,
    /// 正在以新形态重启自己（切换进程伪装开关时置位）：退出清理时
    /// 不恢复被隐藏 / 半透明的窗口，由接棒的下一个进程重新接管。
    relaunching: bool,
    /// “按住显示”进行中：触发它的按键。
    peeking: Option<KeySource>,
    /// 老板键隐藏期间持续静音受管程序（新出现的音频会话也会补上）。
    mute_active: bool,
    /// 最近一次位于前台的非受管普通窗口（“切回工作窗口”的目标）。
    last_work: isize,
    /// 上一轮看到的前台窗口，用于只在前台变化时做判定。
    last_fg: isize,
    /// 光标离开屏幕角后才允许再次触发，避免停在角落时反复触发。
    corner_armed: bool,
    /// 会话是否锁定。
    locked: bool,
    /// 显示器是否亮着。
    display_on: bool,
    /// 激活的显示输出数（接投影检测）。
    display_paths: u32,
    /// 默认输出设备：(设备 ID, 是否耳机)。
    output: Option<(String, bool)>,
    /// 摸鱼统计：不足一秒的累计毫秒数。
    stats_ms: u64,
    /// 摸鱼统计：上一轮计时的时间点。
    stats_tick: Option<Instant>,
    /// 摸鱼统计：上次写盘时的秒数。
    stats_saved: u64,
    /// 上次显示的统计文本（变化时刷新托盘提示与界面）。
    stats_shown: String,
}

impl App {
    pub fn new(hwnd: HWND) -> App {
        let cfg = Config::load();
        let dpi = win::dpi_for(hwnd);
        let icon = win::icon::load(win::icon::IconSize::Small);
        let stats_saved = cfg.stats_secs;
        let mut app = App {
            hwnd,
            cfg,
            targets: Vec::new(),
            window_list: Vec::new(),
            tray: None,
            icon,
            ui: Ui::new(dpi),
            all_hidden: false,
            hide_ok: true,
            show_ok: true,
            picking: false,
            last_pick_hwnd: 0,
            overlay: win::overlay::Overlay::new(),
            last_maintenance: None,
            relaunching: false,
            peeking: None,
            mute_active: false,
            last_work: 0,
            last_fg: 0,
            corner_armed: false,
            locked: false,
            display_on: true,
            display_paths: watch::active_display_paths(),
            output: None,
            stats_ms: 0,
            stats_tick: None,
            stats_saved,
            stats_shown: String::new(),
        };
        app.recover();
        app
    }

    /// 启动时接管上次运行留下的现场（见 [`crate::recovery`]）。
    ///
    /// 上次被强制结束 / 切换伪装接力重启时，窗口可能仍处于隐藏、半透明或静音状态：
    /// 能对应到记忆项的隐藏窗口重新接管为受管且保持隐藏，其余的一律复原。
    fn recover(&mut self) {
        let rec = crate::recovery::load();
        audio::unmute_exes(&rec.muted);

        let mut adopted = false;
        for e in &rec.hidden {
            if !e.is_same_window() || window::is_visible(e.hwnd) {
                continue;
            }
            let title = window::title_of(e.hwnd);
            let exe = window::process_name(e.hwnd);
            let rt = self
                .cfg
                .remembered_targets
                .iter()
                .filter(|rt| !self.targets.iter().any(|t| same_remembered(&t.remembered, rt)))
                .map(|rt| (match_score(&exe, &e.class, &title, rt), rt))
                .filter(|(s, _)| *s > 0)
                .max_by_key(|(s, _)| *s)
                .map(|(_, rt)| rt.clone());
            match rt {
                Some(rt) => {
                    let mut t = Target::new(e.hwnd, title);
                    t.remembered = rt;
                    t.hidden_by_us = true;
                    window::register_hidden(e.hwnd);
                    self.targets.push(t);
                    adopted = true;
                }
                None => window::show_no_activate(e.hwnd),
            }
        }
        if adopted {
            self.all_hidden = true;
            if self.cfg.mute_on_hide {
                self.mute_targets();
            }
        }

        self.refresh_windows();
        self.reattach_remembered();

        for e in &rec.styled {
            if e.is_same_window() && !(self.cfg.transparency_enabled && self.is_managed(e.hwnd)) {
                window::clear_opacity(e.hwnd);
            }
        }
        crate::recovery::persist();
    }

    /// 窗口创建后初始化运行时资源：热键、定时器、托盘、系统事件监视。
    pub fn on_create(&mut self) {
        // 注册老板键（记录是否成功，失败一般是与其它软件热键冲突）。
        self.reapply_hotkeys();
        // 启动自动隐藏轮询定时器。
        unsafe {
            SetTimer(self.hwnd, TIMER_ID, TIMER_INTERVAL_MS, None);
        }
        if self.cfg.tray_icon {
            self.create_tray();
        }
        watch::register(self.hwnd);
        if self.cfg.hide_on_headphones {
            self.watch_output(true);
        }
        // 若启用了半透明，初始应用到已恢复的受管窗口。
        if self.cfg.transparency_enabled {
            self.apply_opacity_all();
        }
        self.sync_wheel_hook();
        if self.cfg.stats {
            self.roll_stats_day();
        }
        self.update_tray_tip();
    }

    fn create_tray(&mut self) {
        let tip = if win::disguise::is_disguised() {
            "双击打开设置，右键菜单"
        } else {
            "BetterBossKey — 双击打开，右键菜单"
        };
        self.tray = Some(Tray::new(self.hwnd, TRAY_UID, WM_TRAYICON, self.icon, tip));
    }

    // —— 窗口列表 / 受管集合 ——

    /// 重新枚举当前窗口，并清理已经关闭的受管目标。
    pub fn refresh_windows(&mut self) {
        self.window_list = window::enumerate();
        // 移除已失效（含 HWND 被回收复用）的目标。
        self.targets.retain(target_live);
        // 更新仍存活目标的标题（标题可能变化）。
        for t in &mut self.targets {
            let title = window::title_of(t.hwnd);
            if !title.is_empty() {
                t.title = title;
            }
        }
    }

    /// 按记忆把当前出现的目标窗口重新挂接为受管。
    ///
    /// 用于跨重启自动恢复：无论是重启本程序，还是目标程序自己重启（HWND 变了、
    /// 标题可能也变了），只要它再次出现，就凭“进程名 + 类名”把它重新认出来并恢复
    /// 管理与半透明设置。每个记忆项已被挂接（存在对应存活目标）时跳过。
    pub(crate) fn reattach_remembered(&mut self) {
        if self.cfg.remembered_targets.is_empty() {
            return;
        }
        let remembered = self.cfg.remembered_targets.clone();
        let alpha = opacity_to_alpha(self.cfg.opacity);
        let trans = self.cfg.transparency_enabled;
        for rt in &remembered {
            // 已有对应存活目标 → 视为已满足，避免重复挂接。
            // 用严格标识判定（而非模糊 match_score）：只有当某个存活目标的“记忆标识”
            // 恰好等于本记忆项时才算已挂接。否则同一进程 + 类名的另一窗口会被误判为
            // “已满足”，导致重开的同类窗口永远不再自动挂接，变成常驻“未运行”幽灵。
            if self
                .targets
                .iter()
                .any(|t| target_live(t) && same_remembered(&t.remembered, rt))
            {
                continue;
            }
            let Some(hwnd) = self.find_window_for(rt) else {
                continue;
            };
            if self.is_managed(hwnd) {
                continue;
            }
            let mut t = Target::new(hwnd, window::title_of(hwnd));
            // 锁定“记忆时”的标识（而非当前实时标题），保证之后能被精确取消。
            t.remembered = rt.clone();
            // 窗口此刻是隐藏的（重启 / 接力前被我们藏起）→ 重新接管为已隐藏：
            // 登记进崩溃兜底表，老板键“全部恢复”与退出恢复都能再次覆盖它。
            if !window::is_visible(hwnd) {
                t.hidden_by_us = true;
                window::register_hidden(hwnd);
            }
            if trans {
                window::set_opacity(hwnd, alpha);
                t.opacity_applied = true;
            }
            self.targets.push(t);
        }
    }

    /// 在当前窗口列表里为某个记忆项挑出匹配分最高、且尚未被管理的窗口。
    fn find_window_for(&self, rt: &RememberedTarget) -> Option<isize> {
        let mut best: Option<(isize, i32)> = None;
        for w in &self.window_list {
            if self.is_managed(w.hwnd) {
                continue;
            }
            let exe = window::process_name(w.hwnd);
            let class = window::class_of(w.hwnd);
            let score = match_score(&exe, &class, &w.title, rt);
            if score > 0 && best.is_none_or(|(_, bs)| score > bs) {
                best = Some((w.hwnd, score));
            }
        }
        best.map(|(h, _)| h)
    }

    pub fn is_managed(&self, hwnd: isize) -> bool {
        self.targets.iter().any(|t| t.hwnd == hwnd)
    }

    /// 勾选 / 取消勾选某窗口的受管状态。
    pub fn toggle_managed(&mut self, hwnd: isize) {
        if let Some(pos) = self.targets.iter().position(|t| t.hwnd == hwnd) {
            // 取消管理：先移出目标，再恢复其显示与不透明（仅当仍是原窗口）。
            let removed = self.targets.remove(pos);
            if target_live(&removed) {
                if removed.hidden_by_us {
                    window::show(removed.hwnd);
                }
                if removed.opacity_applied {
                    window::clear_opacity(removed.hwnd);
                }
            }
            // 从记忆表删除对应记忆项，使其不再跨重启自动恢复。规则：
            //   · 若某记忆项仍被“另一个存活目标”严格支撑（该目标的记忆标识恰好等于此项），
            //     保留它——避免取消一个同标识窗口时误删仍受管的兄弟窗口的持久化项。
            //     这里必须用严格 same_remembered：若用模糊命中，同进程 + 类名的另一窗口
            //     会“替”被取消窗口的记忆项撑腰，使它被错误保留，下个 tick 又抓回来。
            //   · 否则删除：既删“记忆时”的精确标识项（标题漂移也删得掉），也删这个刚
            //     释放的窗口会模糊命中的“同一进程”离线兄弟项——否则该离线项会在下个维护
            //     tick 用 find_window_for 把刚释放的窗口重新抓回来，重现“取消后对勾又恢复”
            //     （同 exe+类名的多窗口用现有标识无法区分，只能牺牲已关闭兄弟项的持久化，
            //     换取“取消一定生效”这一用户核心诉求）。模糊删除仅限带 exe 的项：读不到
            //     进程名的项无法区分进程，模糊命中可能跨进程误删。
            let others = &self.targets;
            self.cfg.remembered_targets.retain(|r| {
                if others
                    .iter()
                    .any(|o| target_live(o) && same_remembered(&o.remembered, r))
                {
                    return true;
                }
                !(same_remembered(r, &removed.remembered)
                    || (!r.exe.is_empty() && remembered_matches_target(r, &removed)))
            });
        } else {
            let title = self
                .window_list
                .iter()
                .find(|w| w.hwnd == hwnd)
                .map(|w| w.title.clone())
                .unwrap_or_else(|| window::title_of(hwnd));
            let mut t = Target::new(hwnd, title);
            // 新加入时若开启了半透明，立即应用。
            if self.cfg.transparency_enabled {
                window::set_opacity(hwnd, opacity_to_alpha(self.cfg.opacity));
                t.opacity_applied = true;
            }
            // 记入记忆表（去重），以便跨重启自动恢复。t.remembered 即“此刻加入”的身份。
            if !self
                .cfg
                .remembered_targets
                .iter()
                .any(|r| same_remembered(r, &t.remembered))
            {
                self.cfg.remembered_targets.push(t.remembered.clone());
            }
            self.targets.push(t);
        }
        self.cfg.save();
        // 立即刷新列表，使刚取消的（可能刚被恢复显示的）窗口即时回到“可选组”。
        self.refresh_windows();
        self.targets_changed();
    }

    /// 受管集合变化后同步依赖它的状态：滚轮钩子的目标、静音范围。
    fn targets_changed(&mut self) {
        self.sync_wheel_hook();
        if self.mute_active {
            // 重新计算静音范围：被移出的程序恢复声音，其余保持静音。
            audio::unmute_all();
            self.mute_targets();
        }
    }

    /// 直接删除某条记忆项（用于取消一个“已选中但当前未运行”的离线项）。
    ///
    /// 该记忆项通常没有对应的存活窗口，仅从持久化列表移除即可。极窄竞态下
    /// （离线渲染与点击之间该项恰好被维护 tick 重新挂接），若存在匹配的存活目标，
    /// 则一并解除并恢复显示 / 透明，避免留下受管却无记忆项的孤儿窗口。
    pub fn remove_remembered(&mut self, index: usize) {
        if index >= self.cfg.remembered_targets.len() {
            return;
        }
        let rt = self.cfg.remembered_targets.remove(index);
        if let Some(pos) = self
            .targets
            .iter()
            .position(|t| same_remembered(&t.remembered, &rt))
        {
            let t = &self.targets[pos];
            if target_live(t) {
                if t.hidden_by_us {
                    window::show(t.hwnd);
                }
                if t.opacity_applied {
                    window::clear_opacity(t.hwnd);
                }
            }
            self.targets.remove(pos);
        }
        self.cfg.save();
        self.targets_changed();
    }

    /// 构建窗口列表的行数据（供 GUI 布局 / 绘制使用）。
    ///
    /// 分两组，解决“隐藏 / 未运行的已选中窗口从列表消失、无法取消”的问题：
    ///   1. **已选中组**：以持久化的 `remembered_targets` 为准，无论窗口当前是否
    ///      可见、是否被我们隐藏、甚至进程是否在运行，都会显示，且始终可取消。
    ///      有存活目标 → 显示实时标题（并标注“已隐藏”）；否则 → 离线项（标注“未运行”）。
    ///   2. **可选组**：当前实时枚举到、且尚未被管理的窗口。
    pub fn build_list_rows(&self) -> Vec<crate::gui::ui::ListRow> {
        use crate::gui::ui::{ListRow, RowKind};
        let mut rows: Vec<ListRow> = Vec::new();

        // —— 已选中组（按记忆表顺序，位置稳定，不因运行/隐藏而跳动） ——
        for (i, rt) in self.cfg.remembered_targets.iter().enumerate() {
            if let Some(t) = self
                .targets
                .iter()
                .find(|t| target_live(t) && same_remembered(&t.remembered, rt))
            {
                let hidden = !window::is_visible(t.hwnd);
                rows.push(ListRow {
                    kind: RowKind::Selected { hwnd: t.hwnd },
                    title: t.title.clone(),
                    checked: true,
                    dim: false,
                    tag: if hidden { "已隐藏" } else { "" },
                });
            } else {
                let title = if rt.title.trim().is_empty() {
                    rt.exe.clone()
                } else {
                    rt.title.clone()
                };
                rows.push(ListRow {
                    kind: RowKind::Offline { rt_index: i },
                    title,
                    checked: true,
                    dim: true,
                    tag: "未运行",
                });
            }
        }

        // 兜底：任何存活但未被记忆项覆盖的目标（正常不会发生），也显示为已选中，
        // 避免出现“受管却不可见、无法取消”的孤儿窗口。
        for t in self.targets.iter().filter(|t| target_live(t)) {
            let shown = rows.iter().any(|r| matches!(r.kind, RowKind::Selected { hwnd } if hwnd == t.hwnd));
            if !shown {
                let hidden = !window::is_visible(t.hwnd);
                rows.push(ListRow {
                    kind: RowKind::Selected { hwnd: t.hwnd },
                    title: t.title.clone(),
                    checked: true,
                    dim: false,
                    tag: if hidden { "已隐藏" } else { "" },
                });
            }
        }

        // —— 可选组（实时未受管窗口） ——
        for w in &self.window_list {
            if !self.is_managed(w.hwnd) {
                rows.push(ListRow {
                    kind: RowKind::Available { hwnd: w.hwnd },
                    title: w.title.clone(),
                    checked: false,
                    dim: false,
                    tag: "",
                });
            }
        }

        rows
    }

    // —— 老板键：隐藏 / 显示 ——

    /// 老板键切换：有任一受管窗口可见就全部隐藏；否则全部恢复。
    pub fn boss_key_toggle(&mut self) {
        let any_visible = self.any_visible();
        if any_visible {
            self.hide_all();
        } else {
            self.show_all();
        }
        self.update_tray_tip();
    }

    fn any_visible(&self) -> bool {
        self.targets
            .iter()
            .any(|t| target_live(t) && window::is_visible(t.hwnd))
    }

    /// 隐藏键按下。
    pub fn on_hide_key(&mut self) {
        self.peeking = None;
        self.hide_all();
        self.update_tray_tip();
    }

    /// 显示键按下（隐藏键与显示键不同时）。
    pub fn on_show_key(&mut self, src: KeySource) {
        if self.cfg.peek {
            self.begin_peek(src);
        } else {
            self.show_all();
            self.update_tray_tip();
        }
    }

    /// 切换键按下（隐藏键与显示键相同时）。开启“按住显示”后，它只负责按住显示。
    pub fn on_toggle_key(&mut self, src: KeySource) {
        if self.cfg.peek {
            self.begin_peek(src);
        } else {
            self.boss_key_toggle();
        }
    }

    /// 按住显示：显示但不抢焦点（只是看一眼），松开按键时再隐藏。
    fn begin_peek(&mut self, src: KeySource) {
        if self.peeking.is_some() {
            return;
        }
        self.show_all_with(false);
        self.peeking = Some(src);
        if let KeySource::Key(_) = src {
            // RegisterHotKey 不报告松开，改为快速轮询按键状态。
            unsafe {
                SetTimer(self.hwnd, PEEK_TIMER_ID, PEEK_POLL_MS, None);
            }
        }
        self.update_tray_tip();
    }

    fn end_peek(&mut self) {
        if self.peeking.take().is_none() {
            return;
        }
        unsafe {
            KillTimer(self.hwnd, PEEK_TIMER_ID);
        }
        self.hide_all();
        self.update_tray_tip();
        self.invalidate();
    }

    /// 轮询键盘老板键是否已松开。
    pub fn on_peek_timer(&mut self) {
        match self.peeking {
            Some(KeySource::Key(vk)) if !key_down(vk as i32) => self.end_peek(),
            Some(KeySource::Key(_)) => {}
            _ => unsafe {
                KillTimer(self.hwnd, PEEK_TIMER_ID);
            },
        }
    }

    /// 鼠标老板键松开。
    pub fn on_mouse_key_release(&mut self) {
        if self.peeking == Some(KeySource::Mouse) {
            self.end_peek();
        }
    }

    pub fn hide_all(&mut self) {
        self.hide_all_with(self.cfg.mute_on_hide);
    }

    fn hide_all_with(&mut self, mute: bool) {
        let was_visible = self.any_visible();
        for t in &mut self.targets {
            if target_live(t) {
                window::hide(t.hwnd);
                t.hidden_by_us = true;
            }
            // 老板键隐藏：转为“非自动隐藏”，避免鼠标悬停就被唤出（破坏老板键语义）。
            t.auto_hidden = false;
            t.hidden_rect = None;
        }
        self.all_hidden = true;
        if mute {
            self.mute_targets();
        }
        if self.cfg.cover && was_visible {
            self.activate_cover();
        }
    }

    pub fn show_all(&mut self) {
        self.peeking = None;
        self.show_all_with(true);
    }

    /// `activate`：是否把窗口切到前台（按住显示时不抢焦点）。
    fn show_all_with(&mut self, activate: bool) {
        for t in &mut self.targets {
            if target_live(t) {
                if activate {
                    window::show(t.hwnd);
                } else {
                    window::show_no_activate(t.hwnd);
                }
            }
            t.hidden_by_us = false;
            // 恢复后重置所有隐藏 / 自动隐藏状态：需重新进入窗口才会再次触发。
            t.auto_hidden = false;
            t.hidden_rect = None;
            t.armed = false;
            t.left_at = None;
        }
        self.all_hidden = false;
        self.unmute();
    }

    /// 自动隐藏条件（屏幕角、离开、接投影、耳机断开）触发：等同于按一次“全部隐藏”。
    /// `mute` 为 true 时无论“隐藏时静音”是否开启都静音（耳机断开时声音会外放）。
    fn trigger_hide(&mut self, mute: bool) {
        if self.picking || !self.targets.iter().any(target_live) {
            return;
        }
        self.peeking = None;
        unsafe {
            KillTimer(self.hwnd, PEEK_TIMER_ID);
        }
        self.hide_all_with(self.cfg.mute_on_hide || mute);
        self.update_tray_tip();
        self.invalidate();
    }

    fn invalidate(&self) {
        unsafe {
            InvalidateRect(self.hwnd, std::ptr::null(), 0);
        }
    }

    // —— 隐藏时静音 / 切回工作窗口 ——

    /// 静音所有受管程序，并在隐藏期间持续补上新出现的音频会话。
    fn mute_targets(&mut self) {
        let mut exes: Vec<String> = Vec::new();
        for t in self.targets.iter().filter(|t| target_live(t)) {
            if !t.exe.is_empty() && !exes.iter().any(|e| e.eq_ignore_ascii_case(&t.exe)) {
                exes.push(t.exe.clone());
            }
        }
        audio::mute(&exes);
        self.mute_active = true;
    }

    fn unmute(&mut self) {
        self.mute_active = false;
        audio::unmute_all();
    }

    pub fn set_mute_on_hide(&mut self, on: bool) {
        self.cfg.mute_on_hide = on;
        if on && self.all_hidden {
            self.mute_targets();
        } else if !on {
            self.unmute();
        }
        self.cfg.save();
    }

    pub fn set_cover(&mut self, on: bool) {
        self.cfg.cover = on;
        self.last_fg = 0;
        self.cfg.save();
    }

    /// 记录最近位于前台的非受管普通窗口。
    fn track_work_window(&mut self) {
        let fg = window::foreground();
        if fg == self.last_fg {
            return;
        }
        self.last_fg = fg;
        if fg != self.hwnd as isize && !self.is_managed(fg) && window::is_switchable(fg) {
            self.last_work = fg;
        }
    }

    /// 把最近使用的工作窗口切到前台。
    fn activate_cover(&mut self) {
        let h = self.last_work;
        if h != 0 && !self.is_managed(h) && window::is_alive(h) && window::is_visible(h) {
            window::activate(h);
            self.last_fg = h;
        }
    }

    // —— 自动隐藏条件 ——

    pub fn set_corner(&mut self, corner: u8) {
        self.cfg.corner = corner.min(4);
        self.corner_armed = false; // 光标先离开角落才生效，避免选中当下就触发
        self.cfg.save();
    }

    pub fn set_hide_on_away(&mut self, on: bool) {
        self.cfg.hide_on_away = on;
        self.cfg.save();
    }

    pub fn set_hide_on_projector(&mut self, on: bool) {
        self.cfg.hide_on_projector = on;
        self.display_paths = watch::active_display_paths();
        self.cfg.save();
    }

    pub fn set_hide_on_headphones(&mut self, on: bool) {
        self.cfg.hide_on_headphones = on;
        self.watch_output(on);
        self.cfg.save();
    }

    fn watch_output(&mut self, on: bool) {
        if on {
            audio::watch_default_output(self.hwnd, WM_AUDIO_DEVICE);
            self.output = audio::default_output();
        } else {
            audio::unwatch_default_output();
            self.output = None;
        }
    }

    /// 锁屏 / 解锁通知。
    pub fn on_session_change(&mut self, event: usize) {
        match event {
            watch::WTS_SESSION_LOCK => {
                self.locked = true;
                if self.cfg.hide_on_away {
                    self.trigger_hide(false);
                }
            }
            watch::WTS_SESSION_UNLOCK => self.locked = false,
            _ => {}
        }
    }

    /// 电源通知：显示器熄灭时隐藏。
    pub fn on_power(&mut self, wparam: usize, lparam: LPARAM) {
        if let Some(on) = watch::display_state(wparam, lparam) {
            let turned_off = self.display_on && !on;
            self.display_on = on;
            if turned_off && self.cfg.hide_on_away {
                self.trigger_hide(false);
            }
        }
    }

    /// 显示配置变化（WM_DISPLAYCHANGE 及周期维护）：激活的输出变多即视为接了投影。
    pub fn check_displays(&mut self) {
        let n = watch::active_display_paths();
        if n == 0 {
            return;
        }
        let more = self.display_paths != 0 && n > self.display_paths;
        self.display_paths = n;
        if more && self.cfg.hide_on_projector {
            self.trigger_hide(false);
        }
    }

    /// 默认输出设备变化：从耳机切走即视为耳机断开。
    pub fn on_output_changed(&mut self) {
        if !self.cfg.hide_on_headphones {
            return;
        }
        let now = audio::default_output();
        let prev = std::mem::replace(&mut self.output, now);
        if let Some((prev_id, was_headphones)) = prev {
            let changed = self.output.as_ref().is_none_or(|(id, _)| *id != prev_id);
            if changed && was_headphones {
                self.trigger_hide(true);
            }
        }
    }

    /// 屏幕角：光标进入所选角落时触发一次。
    fn check_corner(&mut self, cursor: POINT) {
        if self.cfg.corner == 0 {
            return;
        }
        let at = watch::at_corner(cursor, self.cfg.corner);
        if at && self.corner_armed {
            self.corner_armed = false;
            self.trigger_hide(false);
        } else if !at {
            self.corner_armed = true;
        }
    }

    // —— 半透明 ——

    pub fn apply_opacity_all(&mut self) {
        let alpha = opacity_to_alpha(self.cfg.opacity);
        for t in &mut self.targets {
            if target_live(t) {
                window::set_opacity(t.hwnd, alpha);
                t.opacity_applied = true;
            }
        }
    }

    pub fn clear_opacity_all(&mut self) {
        for t in &mut self.targets {
            if t.opacity_applied && target_live(t) {
                window::clear_opacity(t.hwnd);
            }
            t.opacity_applied = false;
        }
    }

    /// 设置不透明度百分比（并在启用时实时应用）。
    pub fn set_opacity_percent(&mut self, percent: u8) {
        self.cfg.opacity = percent.clamp(MIN_OPACITY, MAX_OPACITY);
        if self.cfg.transparency_enabled {
            self.apply_opacity_all();
        }
        self.cfg.save();
    }

    /// 开启 / 关闭半透明。
    pub fn set_transparency_enabled(&mut self, enabled: bool) {
        self.cfg.transparency_enabled = enabled;
        if enabled {
            self.apply_opacity_all();
        } else {
            self.clear_opacity_all();
        }
        self.sync_wheel_hook();
        self.cfg.save();
    }

    pub fn set_click_through(&mut self, on: bool) {
        self.cfg.click_through = on;
        if !on {
            for t in self.targets.iter().filter(|t| target_live(t)) {
                window::set_click_through(t.hwnd, false);
            }
        }
        self.cfg.save();
    }

    pub fn set_wheel_opacity(&mut self, on: bool) {
        self.cfg.wheel_opacity = on;
        self.sync_wheel_hook();
        self.cfg.save();
    }

    /// 按配置安装 / 卸载 Alt+滚轮钩子，并同步它的目标窗口。
    fn sync_wheel_hook(&self) {
        if self.cfg.transparency_enabled && self.cfg.wheel_opacity {
            mouse_hook::install_wheel(self.hwnd, WM_WHEEL_OPACITY);
            mouse_hook::set_wheel_targets(
                self.targets.iter().filter(|t| target_live(t)).map(|t| t.hwnd).collect(),
            );
        } else {
            mouse_hook::uninstall_wheel();
        }
    }

    /// Alt+滚轮：向上滚更不透明，向下滚更透明。
    pub fn on_wheel_opacity(&mut self, delta: i32) {
        if !self.cfg.transparency_enabled {
            return;
        }
        let notches = if delta.abs() >= 120 { delta / 120 } else { delta.signum() };
        let p = (self.cfg.opacity as i32 + notches * WHEEL_OPACITY_STEP)
            .clamp(MIN_OPACITY as i32, MAX_OPACITY as i32);
        self.set_opacity_percent(p as u8);
        // 吞掉了滚轮，目标程序只看到单独的 Alt 按下 / 松开，会激活其菜单栏。
        if key_down(0x12) {
            mouse_hook::suppress_alt_menu();
        }
    }

    // —— 托盘 / 摸鱼统计 ——

    pub fn set_tray_icon(&mut self, on: bool) {
        self.cfg.tray_icon = on;
        if on {
            self.create_tray();
            self.update_tray_tip();
        } else {
            self.tray = None;
        }
        self.cfg.save();
    }

    pub fn set_stats(&mut self, on: bool) {
        self.cfg.stats = on;
        self.stats_tick = None;
        self.stats_ms = 0;
        if on {
            self.roll_stats_day();
        }
        self.stats_shown = self.stats_text();
        self.cfg.save();
        self.stats_saved = self.cfg.stats_secs;
        self.update_tray_tip();
    }

    /// 日期变了就清零。
    fn roll_stats_day(&mut self) {
        let today = today();
        if self.cfg.stats_day != today {
            self.cfg.stats_day = today;
            self.cfg.stats_secs = 0;
            self.stats_ms = 0;
            self.stats_saved = 0;
            self.cfg.save();
        }
    }

    /// 计时：受管窗口有任一可见（未最小化）、且未锁屏、显示器亮着。
    fn tick_stats(&mut self) {
        let now = Instant::now();
        let prev = self.stats_tick.replace(now);
        let counting = !self.locked
            && self.display_on
            && self.targets.iter().any(|t| {
                target_live(t) && window::is_visible(t.hwnd) && !window::is_minimized(t.hwnd)
            });
        if let (true, Some(prev)) = (counting, prev) {
            // 封顶一秒：系统睡眠唤醒后两次定时器之间可能隔了很久。
            self.stats_ms += (now - prev).as_millis().min(1000) as u64;
            self.cfg.stats_secs += self.stats_ms / 1000;
            self.stats_ms %= 1000;
        }
    }

    /// 界面与托盘显示的今日时长与摸鱼工资，如 `今日 1h05m · ¥54.17`；未开启统计时为空。
    pub fn stats_text(&self) -> String {
        if !self.cfg.stats {
            return String::new();
        }
        let m = self.cfg.stats_secs / 60;
        let mut s = if m >= 60 {
            format!("今日 {}h{:02}m", m / 60, m % 60)
        } else {
            format!("今日 {m}m")
        };
        if let Some(wage) = Wage::parse(&self.cfg.wage) {
            s.push_str(" · ");
            s.push_str(&wage.earned(self.cfg.stats_secs));
        }
        s
    }

    /// 设置时薪。空串表示不计算金额；无法解析的输入保持原值不变。
    /// 只填了数字时补上默认货币符号，让输入框显示出金额的单位。
    pub fn set_wage(&mut self, raw: &str) {
        let raw = raw.trim();
        let wage = if raw.is_empty() {
            String::new()
        } else if Wage::parse(raw).is_some() {
            if raw.chars().all(|c| c.is_ascii_digit() || c == '.') {
                format!("{DEFAULT_CURRENCY}{raw}")
            } else {
                raw.to_string()
            }
        } else {
            return;
        };
        if wage != self.cfg.wage {
            self.cfg.wage = wage;
            self.cfg.save();
            self.stats_shown = self.stats_text();
            self.update_tray_tip();
        }
    }

    // —— 自动隐藏 ——

    pub fn set_auto_hide(&mut self, enabled: bool) {
        self.cfg.auto_hide = enabled;
        if !enabled {
            // 关闭时重置所有 arm 状态。
            for t in &mut self.targets {
                t.armed = false;
                t.left_at = None;
            }
        }
        self.cfg.save();
    }

    pub fn set_delay(&mut self, ms: u32) {
        self.cfg.auto_hide_delay_ms = ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
        self.cfg.save();
    }

    /// 设置“移入原位置自动唤出时是否抢前台焦点”。
    pub fn set_auto_focus(&mut self, enabled: bool) {
        self.cfg.auto_focus = enabled;
        self.cfg.save();
    }

    pub fn set_reveal_delay(&mut self, ms: u32) {
        self.cfg.reveal_delay_ms = ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
        self.cfg.save();
    }

    // —— 进程伪装 ——

    /// 开 / 关进程伪装，并立即以新形态重启自己，让效果当场可见。
    ///
    /// 开启：复制为随机名副本并启动，当前进程退出（窗口标题 / 托盘提示同步转为中性）。
    /// 关闭：从 `origin`（伪装前记录的原始 exe 路径）重启回去；找不到原始文件时
    /// 只保存设置、继续以当前名字运行，用户下次从原位置启动即恢复。
    pub fn set_disguise(&mut self, on: bool) {
        if self.cfg.disguise == on {
            return;
        }
        self.cfg.disguise = on;
        if on {
            if win::disguise::is_disguised() {
                self.cfg.save();
                return;
            }
            if let Ok(exe) = std::env::current_exe() {
                self.cfg.origin = exe.to_string_lossy().into_owned();
            }
            self.cfg.save();
            if win::disguise::launch_disguised_copy(&mut self.cfg) {
                self.relaunch_exit();
            }
        } else {
            self.cfg.save();
            let origin = std::path::PathBuf::from(&self.cfg.origin);
            if !self.cfg.origin.is_empty() && origin.is_file() && win::disguise::spawn(&origin) {
                self.relaunch_exit();
            }
        }
    }

    /// 以新形态重启：请求销毁窗口退出。清理时保留被隐藏 / 半透明的窗口，
    /// 由下一个进程在重新挂接时接管（见 `reattach_remembered`）。
    fn relaunch_exit(&mut self) {
        self.relaunching = true;
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }

    // —— 拾取窗口模式 ——

    /// 进入拾取模式：安装全局鼠标钩子（不依赖捕获 / 前台，能收到所有移动与点击），
    /// 隐藏设置窗口，等待用户在目标窗口上点击选择。
    pub fn enter_pick_mode(&mut self) {
        if self.picking {
            return;
        }
        // 钩子装不上就不进入，避免卡在“拾取中但收不到鼠标”的状态。
        if !mouse_hook::install_pick(self.hwnd, WM_PICK_MOVE, WM_PICK_SELECT, WM_PICK_CANCEL) {
            return;
        }
        self.picking = true;
        self.last_pick_hwnd = 0;
        let _ = esc_pressed(); // 排空 ESC 低位，避免进入前按过的 ESC 立即触发取消
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// 拾取模式下鼠标移动：高亮光标下的窗口并在其左上角显示进程名 / 标题 / 尺寸。
    pub fn pick_move(&mut self, cursor: POINT) {
        // 允许钩子投递下一次移动（合并高频移动）。
        mouse_hook::pick_move_handled();
        if !self.picking {
            return;
        }
        match window::window_at(cursor) {
            Some(wi) if wi.hwnd != self.last_pick_hwnd => {
                self.last_pick_hwnd = wi.hwnd;
                if let Some(r) = window::window_rect(wi.hwnd) {
                    let proc = window::process_name(wi.hwnd);
                    let line1 = if proc.is_empty() {
                        "(未知进程)".to_string()
                    } else {
                        proc
                    };
                    let line2 = format!("{}  ·  {}×{}", wi.title, r.right - r.left, r.bottom - r.top);
                    self.overlay.show_for(r, &line1, &line2);
                }
            }
            Some(_) => {} // 同一窗口，红框已在原位
            None => {
                if self.last_pick_hwnd != 0 {
                    self.last_pick_hwnd = 0;
                    self.overlay.hide();
                }
            }
        }
    }

    /// 拾取模式下左键：选中当前高亮的窗口并加入受管列表。
    pub fn pick_select(&mut self) {
        let hwnd = self.last_pick_hwnd;
        self.exit_pick_mode();
        if hwnd != 0 && window::is_alive(hwnd) && !self.is_managed(hwnd) {
            self.toggle_managed(hwnd);
        }
    }

    /// 取消拾取模式（右键 / ESC）。
    pub fn pick_cancel(&mut self) {
        self.exit_pick_mode();
    }

    fn exit_pick_mode(&mut self) {
        if !self.picking {
            return;
        }
        self.picking = false;
        self.last_pick_hwnd = 0;
        self.overlay.hide();
        mouse_hook::uninstall_pick();
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
        }
        self.refresh_windows();
    }

    /// 定时器回调：① 周期性重申半透明（防止目标程序自行重置）② 鼠标移出自动隐藏。
    pub fn on_timer(&mut self) {
        // 拾取模式下：仅监听 ESC 取消，其余逻辑暂停。
        if self.picking {
            if esc_pressed() {
                self.pick_cancel();
            }
            return;
        }

        // ⓪ 周期性维护（节流）：刷新窗口列表、回收失效目标、按记忆重新挂接。
        //    这样即便目标程序重启（HWND 改变），出现后也能自动恢复管理与透明设置。
        let due = match self.last_maintenance {
            None => true,
            Some(t) => t.elapsed().as_millis() as u32 >= MAINTENANCE_INTERVAL_MS,
        };
        if due {
            self.last_maintenance = Some(Instant::now());
            if self.maintenance_tick() {
                self.update_tray_tip();
                self.invalidate();
            }
        }

        // ① 重申半透明。部分程序（自渲染 / 投屏 / 视频类窗口，如 phoneCast）会在重绘、
        //    获得焦点或鼠标移入时重置分层窗口属性，把我们设的透明度冲掉。由于我们只在
        //    设置那一刻施加一次，必须周期性重新施加才能稳定保持透明。鼠标穿透同理，
        //    并在按住 Ctrl 时暂停，以便直接操作该窗口。
        if self.cfg.transparency_enabled {
            let alpha = opacity_to_alpha(self.cfg.opacity);
            let through = self.cfg.click_through && !key_down(0x11);
            for t in &self.targets {
                if t.opacity_applied
                    && !t.hidden_by_us
                    && target_live(t)
                    && window::is_visible(t.hwnd)
                {
                    window::set_opacity(t.hwnd, alpha);
                    if self.cfg.click_through {
                        window::set_click_through(t.hwnd, through);
                    }
                }
            }
        }

        if self.cfg.cover {
            self.track_work_window();
        }
        if self.cfg.stats {
            self.tick_stats();
        }
        let cursor = cursor_pos();
        self.check_corner(cursor);

        // ② 自动隐藏。
        if !self.cfg.auto_hide || self.all_hidden {
            return;
        }
        let delay = self.cfg.auto_hide_delay_ms;
        let reveal_delay = self.cfg.reveal_delay_ms;
        let auto_focus = self.cfg.auto_focus;
        let mut changed = false;

        for t in &mut self.targets {
            if !target_live(t) {
                continue;
            }

            // 自动隐藏的窗口：光标回到它原来的位置并停留“显示延迟”后自动显示。
            if t.auto_hidden {
                if window::is_visible(t.hwnd) {
                    // 被外部（任务栏、程序自身等）恢复可见 → 清除自动隐藏态，按正常处理。
                    t.auto_hidden = false;
                    t.hidden_by_us = false;
                    t.hidden_rect = None;
                    t.reenter_at = None;
                    t.armed = false;
                    t.left_at = None;
                } else {
                    if let Some(rect) = t.hidden_rect {
                        if point_in_rect(&cursor, &rect) {
                            // 光标在原位置：等待显示延迟。
                            match t.reenter_at {
                                None => t.reenter_at = Some(Instant::now()),
                                Some(since) => {
                                    if since.elapsed().as_millis() as u32 >= reveal_delay {
                                        // 按设置决定唤出时是否抢前台焦点。
                                        if auto_focus {
                                            window::show(t.hwnd);
                                        } else {
                                            window::show_no_activate(t.hwnd);
                                        }
                                        t.auto_hidden = false;
                                        t.hidden_by_us = false;
                                        t.hidden_rect = None;
                                        t.reenter_at = None;
                                        t.armed = true; // 光标此刻就在窗口内
                                        t.left_at = None;
                                        changed = true;
                                    }
                                }
                            }
                        } else {
                            // 离开原位置：重置显示延迟计时。
                            t.reenter_at = None;
                        }
                    }
                    continue;
                }
            }

            if !window::is_visible(t.hwnd) {
                continue;
            }
            let Some(rect) = window::window_rect(t.hwnd) else {
                continue;
            };
            let inside = point_in_rect(&cursor, &rect);

            if inside {
                t.armed = true;
                t.left_at = None;
            } else if t.armed {
                match t.left_at {
                    None => t.left_at = Some(Instant::now()),
                    Some(since) => {
                        if since.elapsed().as_millis() as u32 >= delay {
                            // 记下当前位置后隐藏，标记为自动隐藏，供回到原位时唤出。
                            t.hidden_rect = Some(rect);
                            window::hide(t.hwnd);
                            t.hidden_by_us = true;
                            t.auto_hidden = true;
                            t.left_at = None;
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            self.update_tray_tip();
        }
    }

    // —— 热键 ——

    /// 启用隐藏键 + 显示键。键盘走 RegisterHotKey，鼠标合并为一组低级钩子绑定。
    /// 两键相同时退化为“切换键”（同一组合不能重复注册）。返回 (隐藏键 ok, 显示键 ok)。
    fn apply_hotkeys(&self) -> (bool, bool) {
        let same = self.cfg.hide_hotkey == self.cfg.show_hotkey;
        let mut bindings: Vec<Binding> = Vec::new();
        let mut hide_ok = true;
        let mut show_ok = true;

        if same {
            match self.cfg.hide_hotkey {
                Hotkey::Key { .. } => {
                    let ok = self.cfg.hide_hotkey.register_key(self.hwnd, HOTKEY_ID_TOGGLE);
                    hide_ok = ok;
                    show_ok = ok;
                }
                Hotkey::Mouse { mods, button } => {
                    bindings.push(Binding { button, mods, msg: WM_MOUSE_HOTKEY });
                }
            }
        } else {
            match self.cfg.hide_hotkey {
                Hotkey::Key { .. } => {
                    hide_ok = self.cfg.hide_hotkey.register_key(self.hwnd, HOTKEY_ID_HIDE)
                }
                Hotkey::Mouse { mods, button } => {
                    bindings.push(Binding { button, mods, msg: WM_BOSS_HIDE })
                }
            }
            match self.cfg.show_hotkey {
                Hotkey::Key { .. } => {
                    show_ok = self.cfg.show_hotkey.register_key(self.hwnd, HOTKEY_ID_SHOW)
                }
                Hotkey::Mouse { mods, button } => {
                    bindings.push(Binding { button, mods, msg: WM_BOSS_SHOW })
                }
            }
        }

        if !bindings.is_empty() && !mouse_hook::install(self.hwnd, &bindings, WM_BOSS_RELEASE) {
            // 鼠标钩子装不上：相关的鼠标槽位标记为失败。
            if matches!(self.cfg.hide_hotkey, Hotkey::Mouse { .. }) {
                hide_ok = false;
            }
            if matches!(self.cfg.show_hotkey, Hotkey::Mouse { .. }) {
                show_ok = false;
            }
        }
        (hide_ok, show_ok)
    }

    /// 清除当前所有热键（注销三个键盘热键 id 并卸载鼠标钩子）。
    fn clear_hotkeys(&self) {
        Hotkey::unregister(self.hwnd, HOTKEY_ID_HIDE);
        Hotkey::unregister(self.hwnd, HOTKEY_ID_SHOW);
        Hotkey::unregister(self.hwnd, HOTKEY_ID_TOGGLE);
        mouse_hook::uninstall();
    }

    /// 开始为某个槽位录制热键。
    ///
    /// 录制期间暂停全部老板键（注销键盘热键、卸载鼠标钩子）：否则按下已注册的组合时
    /// 系统会直接触发老板键，设置窗口收不到这次按键——既会误隐藏 / 显示窗口，也无法
    /// 把两个槽位设成同一个键（切换键）。
    pub fn begin_hotkey_capture(&mut self, slot: HkSlot) {
        if self.ui.capturing_hotkey.is_none() {
            self.clear_hotkeys();
        }
        self.ui.capturing_hotkey = Some(slot);
    }

    /// 结束录制并恢复老板键。`Some(hk)` 尝试启用新键（失败则回滚），`None` 为取消。
    pub fn end_hotkey_capture(&mut self, hk: Option<Hotkey>) {
        let Some(slot) = self.ui.capturing_hotkey.take() else {
            return;
        };
        match hk.filter(Hotkey::is_valid) {
            Some(hk) => self.set_hotkey(slot, hk),
            None => self.reapply_hotkeys(),
        }
    }

    /// 按当前配置重新启用老板键，并记录各槽位是否成功。
    fn reapply_hotkeys(&mut self) {
        let (h, s) = self.apply_hotkeys();
        self.hide_ok = h;
        self.show_ok = s;
    }

    /// 设置某个槽位（隐藏键 / 显示键）的热键，失败则回滚。
    fn set_hotkey(&mut self, slot: HkSlot, hk: Hotkey) {
        let (old_hide, old_show) = (self.cfg.hide_hotkey, self.cfg.show_hotkey);
        self.clear_hotkeys();
        match slot {
            HkSlot::Hide => self.cfg.hide_hotkey = hk,
            HkSlot::Show => self.cfg.show_hotkey = hk,
        }
        let (h, s) = self.apply_hotkeys();
        let ok = match slot {
            HkSlot::Hide => h,
            HkSlot::Show => s,
        };
        if ok {
            // 该槽位启用成功：采用并持久化。
            self.hide_ok = h;
            self.show_ok = s;
            self.cfg.save();
        } else {
            // 启用失败（多为与其它软件冲突）：回滚到旧组合，避免老板键失效，也不写盘。
            self.clear_hotkeys();
            self.cfg.hide_hotkey = old_hide;
            self.cfg.show_hotkey = old_show;
            self.reapply_hotkeys();
        }
    }

    /// 开 / 关“按住显示”。
    pub fn set_peek(&mut self, on: bool) {
        self.cfg.peek = on;
        self.cfg.save();
    }

    // —— 托盘 ——

    pub fn update_tray_tip(&mut self) {
        if self.tray.is_none() {
            return;
        }
        let managed = self.targets.len();
        let hidden = self
            .targets
            .iter()
            .filter(|t| target_live(t) && !window::is_visible(t.hwnd))
            .count();
        let mut tip = if win::disguise::is_disguised() {
            format!("管理 {managed} 个窗口，已隐藏 {hidden} 个")
        } else {
            format!("BetterBossKey — 管理 {managed} 个窗口，已隐藏 {hidden} 个")
        };
        let stats = self.stats_text();
        if !stats.is_empty() {
            tip.push_str(" · ");
            tip.push_str(&stats);
        }
        if let Some(tray) = &mut self.tray {
            tray.set_tip(&tip);
        }
    }

    // —— 周期性维护 ——

    /// 刷新窗口列表、回收失效目标、按记忆重新挂接，并处理各项低频检查。
    /// 返回界面是否需要重绘（列表内容、受管数量或统计时长发生了变化）。
    fn maintenance_tick(&mut self) -> bool {
        let before: Vec<(isize, String)> =
            self.window_list.iter().map(|w| (w.hwnd, w.title.clone())).collect();
        let before_targets = self.targets.len();
        self.refresh_windows();
        self.clamp_scroll();
        // 仅当有记忆项尚未挂接时才尝试再匹配（reattach 内部还会逐项判定）。
        if self.targets.len() < self.cfg.remembered_targets.len() {
            self.reattach_remembered();
        }
        if before_targets != self.targets.len() {
            self.sync_wheel_hook();
        }
        // 隐藏期间新出现的音频会话（如重新开始播放）也要静音。
        if self.mute_active && self.all_hidden {
            self.mute_targets();
        }
        if self.cfg.hide_on_projector {
            self.check_displays();
        }

        let mut stats_changed = false;
        if self.cfg.stats {
            self.roll_stats_day();
            if self.cfg.stats_secs >= self.stats_saved + STATS_SAVE_SECS {
                self.stats_saved = self.cfg.stats_secs;
                self.cfg.save();
            }
            let text = self.stats_text();
            if text != self.stats_shown {
                self.stats_shown = text;
                stats_changed = true;
            }
        }

        let after: Vec<(isize, String)> =
            self.window_list.iter().map(|w| (w.hwnd, w.title.clone())).collect();
        before != after || before_targets != self.targets.len() || stats_changed
    }

    /// 退出前的清理：恢复所有被我们隐藏 / 半透明 / 静音的窗口，释放资源。
    /// 重启接力（`relaunching`）时跳过恢复——窗口保持现状，由下一个进程接管。
    pub fn cleanup(&mut self) {
        self.peeking = None;
        for t in &self.targets {
            if target_live(t) && t.opacity_applied && !self.relaunching {
                window::clear_opacity(t.hwnd);
            }
        }
        if !self.relaunching {
            // 兜底：恢复所有登记在册的隐藏 / 半透明窗口（含 identity 校验可能跳过的）。
            window::restore_all_hidden();
            window::restore_all_styled();
            audio::unmute_all();
        }
        if self.cfg.stats {
            self.cfg.save();
        }
        self.clear_hotkeys();
        mouse_hook::uninstall_wheel();
        audio::unwatch_default_output();
        watch::unregister(self.hwnd);
        unsafe {
            KillTimer(self.hwnd, TIMER_ID);
            KillTimer(self.hwnd, PEEK_TIMER_ID);
        }
        crate::recovery::persist();
        self.tray = None; // Drop -> 移除托盘图标
        self.overlay.destroy();
        self.ui.fonts.destroy();
        if !self.icon.is_null() {
            unsafe {
                DestroyIcon(self.icon);
            }
        }
    }
}

/// 某个按键当前是否按下。
fn key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

/// 今天的本地日期，形如 20261109。
fn today() -> u32 {
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        GetLocalTime(&mut st);
    }
    st.wYear as u32 * 10000 + st.wMonth as u32 * 100 + st.wDay as u32
}

/// 百分比不透明度 -> alpha(0..255)。
pub fn opacity_to_alpha(percent: u8) -> u8 {
    ((percent as u32 * 255 + 50) / 100).min(255) as u8
}

/// 获取当前鼠标光标的屏幕坐标。
fn cursor_pos() -> POINT {
    let mut p = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut p);
    }
    p
}

/// 点是否落在矩形内（左闭右开）。
fn point_in_rect(p: &POINT, r: &RECT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// ESC 是否按下，或自上次轮询以来被按过（拾取模式下轮询取消用）。
/// 同时看高位(当前按下)与低位(期间按过)，避免快速一按被 80ms 轮询错过。
fn esc_pressed() -> bool {
    unsafe { (GetAsyncKeyState(0x1B) as u16 & 0x8001) != 0 }
}
