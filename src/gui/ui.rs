//! 设置窗口的布局、绘制与交互。
//!
//! 以 `impl App` 的形式扩展应用：所有可视状态来自 `App`，这里负责把它画出来
//! 并把鼠标 / 键盘事件翻译成对 `App` 的操作。绘制全部在内存 DC 上完成
//! （双缓冲），由 `main.rs` 的 WM_PAINT 负责把结果一次性 BitBlt 到屏幕。
//!
//! 布局语法（让布局本身说明含义，不加标题 / 副标题 / 提示文字）：
//! * 双栏：左栏是窗口列表卡片（底部“拾取窗口”）、老板键与操作按钮；右栏是各项设置。
//! * 每项设置占一行：左侧标签，右侧控件；整行可点。
//! * 依赖某开关的子项缩进排在其下，开关关闭时变暗且不可点击。
//! * 分组之间只用细分隔线。文字只用于说明状态。
//!
//! 新增控件：在 [`Hot`] 加变体 → `compute_layout` 算出矩形并登记到 `hits`
//! → `paint` 绘制 → `on_lbutton_down` 处理。命中测试无需改动。

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

use crate::app::App;
use crate::config::{MAX_DELAY_MS, MAX_OPACITY, MIN_OPACITY};
use crate::gui::theme::{self, Fonts};
use crate::gui::widgets::{self as w, Canvas};
use crate::win::hotkey::{is_modifier_vk, HkSlot, Hotkey, MouseButton};
use crate::win::scale;

// 以下尺寸均为 96 DPI 下的逻辑像素，使用前经 `scale` 换算。
/// 窗口客户区宽度。
const LOGICAL_W: i32 = 800;
/// 外边距。
const PAD: i32 = 20;
/// 左右两栏之间的间距。
const COL_GAP: i32 = 40;
/// 窗口列表的行高。
const LIST_ROW_H: i32 = 34;
/// 窗口列表至少显示的行数（列表高度随右栏伸展，不低于此值）。
const LIST_MIN_ROWS: i32 = 5;
/// 设置行的行高。
const LINE_H: i32 = 36;
/// 子项相对父开关的缩进。
const INDENT: i32 = 16;
/// 步进器每次增减的毫秒数。
const DELAY_STEP_MS: u32 = 100;
/// 点亮屏幕角开关时默认选用的角（右下，Windows“显示桌面”所在处）。
const DEFAULT_CORNER: u8 = 4;
/// 时薪输入框最多容纳的字符数。
const WAGE_MAX_CHARS: usize = 12;

const TEXT_FLAGS: u32 = w::DT_VCENTER | w::DT_SINGLELINE | w::DT_NOPREFIX;

/// 可交互元素（悬停 / 点击的目标）。
#[derive(Clone, Copy, PartialEq)]
pub enum Hot {
    None,
    Pick,
    HideKey,
    ShowKey,
    Peek,
    Stats,
    Wage,
    Mute,
    Cover,
    Topmost,
    AutoHide,
    DelayMinus,
    DelayPlus,
    RevealMinus,
    RevealPlus,
    AutoFocus,
    /// 屏幕角整行（开 / 关）。
    Corner,
    /// 屏幕角选择器里的某个角（1 左上，2 右上，3 左下，4 右下）。
    CornerAt(u8),
    Away,
    Projector,
    Headphones,
    Opacity,
    Slider,
    ClickThrough,
    Wheel,
    Disguise,
    Tray,
    HideAll,
    ShowAll,
    /// 列表中的第 N 行（`layout.rows` 下标，兼容已选中 / 离线 / 可选三类行）。
    Row(usize),
}

/// 列表中一行的类别。点击复选框时据此决定“取消谁”。
#[derive(Clone, Copy)]
pub enum RowKind {
    /// 已选中且有存活窗口（可见或被我们隐藏）。点击 → 取消管理该 hwnd。
    Selected { hwnd: isize },
    /// 已选中但当前无存活窗口（进程未运行）。点击 → 删除第 rt_index 条记忆项。
    Offline { rt_index: usize },
    /// 未选中的存活窗口。点击 → 加入管理。
    Available { hwnd: isize },
}

/// 与布局无关的一行数据（由 `App::build_list_rows` 生成，供布局 / 绘制消费）。
pub struct ListRow {
    pub kind: RowKind,
    pub title: String,
    /// 复选框是否勾选（已选中组为 true）。
    pub checked: bool,
    /// 是否以次要（灰）色显示标题（离线项）。
    pub dim: bool,
    /// 行右侧的状态标签（如“已隐藏”/“未运行”），空串表示无标签。
    pub tag: &'static str,
}

/// 列表中的一行（经过布局、带命中矩形）。
pub struct RowHit {
    pub kind: RowKind,
    /// 行下标，作为稳定标识用于 hover 高亮与命中派发。
    pub key: usize,
    pub title: String,
    pub checked: bool,
    pub dim: bool,
    pub tag: &'static str,
    pub row: RECT,
    pub check: RECT,
}

const ZERO: RECT = RECT {
    left: 0,
    top: 0,
    right: 0,
    bottom: 0,
};

/// 设置行：整行（标签 + 控件）与右侧控件区域。
#[derive(Clone, Copy)]
pub struct Field {
    pub row: RECT,
    pub ctrl: RECT,
}

impl Field {
    const NONE: Field = Field {
        row: ZERO,
        ctrl: ZERO,
    };

    fn new(left: i32, right: i32, top: i32, h: i32, ctrl_w: i32, ctrl_h: i32) -> Field {
        let ct = top + (h - ctrl_h) / 2;
        Field {
            row: rect(left, top, right, top + h),
            ctrl: rect(right - ctrl_w, ct, right, ct + ctrl_h),
        }
    }
}

/// 步进器行：`标签 …… [−] 值 [+]`。
#[derive(Clone, Copy)]
pub struct Stepper {
    pub row: RECT,
    pub minus: RECT,
    pub value: RECT,
    pub plus: RECT,
}

impl Stepper {
    const NONE: Stepper = Stepper {
        row: ZERO,
        minus: ZERO,
        value: ZERO,
        plus: ZERO,
    };

    fn new(left: i32, right: i32, top: i32, h: i32, btn: i32, val_w: i32, gap: i32) -> Stepper {
        let ct = top + (h - btn) / 2;
        let plus = rect(right - btn, ct, right, ct + btn);
        let value = rect(plus.left - gap - val_w, ct, plus.left - gap, ct + btn);
        let minus = rect(value.left - gap - btn, ct, value.left - gap, ct + btn);
        Stepper {
            row: rect(left, top, right, top + h),
            minus,
            value,
            plus,
        }
    }
}

/// 一次布局计算的结果（物理像素）。绘制与命中测试共用。
///
/// 左栏：窗口列表卡片（含底部“拾取窗口”）、老板键、摸鱼统计、操作按钮；
/// 右栏：隐藏 / 显示时的动作、自动隐藏条件、半透明、隐蔽。
pub struct Layout {
    pub height_needed: i32,
    /// 窗口列表卡片（含底部“拾取窗口”）。
    pub list_panel: RECT,
    /// 列表可滚动的可视区。
    pub list_view: RECT,
    pub pick_btn: RECT,
    pub rows: Vec<RowHit>,
    pub row_full_height: i32,
    pub list_inner_h: i32,
    pub hide_key: Field,
    pub show_key: Field,
    pub peek: Field,
    pub stats: Field,
    pub wage: Field,
    pub hide_btn: RECT,
    pub show_btn: RECT,
    pub mute: Field,
    pub cover: Field,
    pub topmost: Field,
    pub auto_hide: Field,
    pub hide_delay: Stepper,
    pub reveal_delay: Stepper,
    pub auto_focus: Field,
    pub corner: Field,
    /// 屏幕角选择器的四个角（左上、右上、左下、右下）。
    pub corner_cells: [RECT; 4],
    pub away: Field,
    pub projector: Field,
    pub headphones: Field,
    pub opacity: Field,
    pub slider_track: RECT,
    pub click_through: Field,
    pub wheel: Field,
    pub disguise: Field,
    pub tray: Field,
    pub dividers: Vec<RECT>,
    /// 当前可交互的区域，按命中优先级排列；禁用的控件不登记。
    pub hits: Vec<(RECT, Hot)>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            height_needed: 0,
            list_panel: ZERO,
            list_view: ZERO,
            pick_btn: ZERO,
            rows: Vec::new(),
            row_full_height: 0,
            list_inner_h: 0,
            hide_key: Field::NONE,
            show_key: Field::NONE,
            peek: Field::NONE,
            stats: Field::NONE,
            wage: Field::NONE,
            hide_btn: ZERO,
            show_btn: ZERO,
            mute: Field::NONE,
            cover: Field::NONE,
            topmost: Field::NONE,
            auto_hide: Field::NONE,
            hide_delay: Stepper::NONE,
            reveal_delay: Stepper::NONE,
            auto_focus: Field::NONE,
            corner: Field::NONE,
            corner_cells: [ZERO; 4],
            away: Field::NONE,
            projector: Field::NONE,
            headphones: Field::NONE,
            opacity: Field::NONE,
            slider_track: ZERO,
            click_through: Field::NONE,
            wheel: Field::NONE,
            disguise: Field::NONE,
            tray: Field::NONE,
            dividers: Vec::new(),
            hits: Vec::new(),
        }
    }
}

/// GUI 运行时状态（与 App 解耦的纯界面状态）。
pub struct Ui {
    pub dpi: u32,
    pub fonts: Fonts,
    pub list_scroll: i32,
    /// 正在捕获热键的槽位（None = 未在捕获）。
    pub capturing_hotkey: Option<HkSlot>,
    /// 正在编辑的时薪文本（None = 未在编辑）。
    pub editing_wage: Option<String>,
    /// 刚进入编辑、原文处于全选状态：输入的第一个字符替换整段。
    pub wage_selected: bool,
    pub dragging_slider: bool,
    pub hover: Hot,
    pub layout: Layout,
}

impl Ui {
    pub fn new(dpi: u32) -> Ui {
        Ui {
            dpi,
            fonts: Fonts::new(dpi),
            list_scroll: 0,
            capturing_hotkey: None,
            editing_wage: None,
            wage_selected: false,
            dragging_slider: false,
            hover: Hot::None,
            layout: Layout::default(),
        }
    }

    /// DPI 改变时重建字体。
    pub fn update_dpi(&mut self, dpi: u32) {
        if dpi != self.dpi {
            self.fonts.destroy();
            self.fonts = Fonts::new(dpi);
            self.dpi = dpi;
        }
    }
}

#[inline]
fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

#[inline]
fn pt_in(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// 两矩形的交集；不相交时返回 None。
fn intersect(a: &RECT, b: &RECT) -> Option<RECT> {
    let r = rect(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    );
    (r.left < r.right && r.top < r.bottom).then_some(r)
}

impl App {
    /// 计算布局（width 为客户区物理宽度）。
    ///
    /// 先排右栏（设置项），它决定窗口高度；左栏的按钮与右栏底部对齐，
    /// 窗口列表卡片伸展占满其余高度。
    pub fn compute_layout(&self, width: i32) -> Layout {
        let dpi = self.ui.dpi;
        let s = |v: i32| scale(v, dpi);
        let pad = s(PAD);
        let col_w = (width - 2 * pad - s(COL_GAP)) / 2;
        let (ll, lr) = (pad, pad + col_w);
        let (rl, rr) = (width - pad - col_w, width - pad);
        let line_h = s(LINE_H);
        let gap = s(4);
        let (tgl_w, tgl_h) = (s(46), s(26));
        let div_h = s(1).max(1);
        let mut hits: Vec<(RECT, Hot)> = Vec::new();
        let mut dividers = Vec::new();
        let divider = |y: &mut i32, l: i32, r: i32, out: &mut Vec<RECT>| {
            *y += s(12);
            out.push(rect(l, *y, r, *y + div_h));
            *y += div_h + s(12);
        };
        // 开关行：从 y 开始放一行并下移；`last` 表示组内最后一行（不留行距）。
        let toggle = |y: &mut i32, l: i32, r: i32, last: bool| {
            let f = Field::new(l, r, *y, line_h, tgl_w, tgl_h);
            *y += line_h + if last { 0 } else { gap };
            f
        };

        // ======================= 右栏 =======================
        let rsub = rl + s(INDENT);
        let mut y = pad;

        // —— 隐藏 / 显示时的动作 ——
        let mute = toggle(&mut y, rl, rr, false);
        let cover = toggle(&mut y, rl, rr, false);
        let topmost = toggle(&mut y, rl, rr, true);
        hits.push((mute.row, Hot::Mute));
        hits.push((cover.row, Hot::Cover));
        hits.push((topmost.row, Hot::Topmost));
        divider(&mut y, rl, rr, &mut dividers);

        // —— 自动隐藏条件：鼠标移出（及其子项）、屏幕角、离开、接投影、耳机断开 ——
        let auto_hide = toggle(&mut y, rl, rr, false);
        let hide_delay = Stepper::new(rsub, rr, y, line_h, s(30), s(70), s(4));
        y += line_h + gap;
        let reveal_delay = Stepper::new(rsub, rr, y, line_h, s(30), s(70), s(4));
        y += line_h + gap;
        let auto_focus = toggle(&mut y, rsub, rr, false);
        let corner = Field::new(rl, rr, y, line_h, s(46), s(30));
        y += line_h + gap;
        let away = toggle(&mut y, rl, rr, false);
        let projector = toggle(&mut y, rl, rr, false);
        let headphones = toggle(&mut y, rl, rr, true);
        hits.push((auto_hide.row, Hot::AutoHide));
        if self.cfg.auto_hide {
            hits.push((hide_delay.minus, Hot::DelayMinus));
            hits.push((hide_delay.plus, Hot::DelayPlus));
            hits.push((reveal_delay.minus, Hot::RevealMinus));
            hits.push((reveal_delay.plus, Hot::RevealPlus));
            hits.push((auto_focus.row, Hot::AutoFocus));
        }
        let c = corner.ctrl;
        let (mx, my) = ((c.left + c.right) / 2, (c.top + c.bottom) / 2);
        let corner_cells = [
            rect(c.left, c.top, mx, my),
            rect(mx, c.top, c.right, my),
            rect(c.left, my, mx, c.bottom),
            rect(mx, my, c.right, c.bottom),
        ];
        for (i, cell) in corner_cells.iter().enumerate() {
            hits.push((*cell, Hot::CornerAt(i as u8 + 1)));
        }
        hits.push((corner.row, Hot::Corner));
        hits.push((away.row, Hot::Away));
        hits.push((projector.row, Hot::Projector));
        hits.push((headphones.row, Hot::Headphones));
        divider(&mut y, rl, rr, &mut dividers);

        // —— 半透明，及其滑块、鼠标穿透、滚轮调节 ——
        let opacity = toggle(&mut y, rl, rr, false);
        let slider_track = rect(rsub, y, rr - s(52), y + s(32));
        y = slider_track.bottom + gap;
        let click_through = toggle(&mut y, rsub, rr, false);
        let wheel = toggle(&mut y, rsub, rr, true);
        hits.push((opacity.row, Hot::Opacity));
        if self.cfg.transparency_enabled {
            hits.push((slider_track, Hot::Slider));
            hits.push((click_through.row, Hot::ClickThrough));
            hits.push((wheel.row, Hot::Wheel));
        }
        divider(&mut y, rl, rr, &mut dividers);

        // —— 隐蔽 ——
        let disguise = toggle(&mut y, rl, rr, false);
        let tray = toggle(&mut y, rl, rr, true);
        hits.push((disguise.row, Hot::Disguise));
        hits.push((tray.row, Hot::Tray));
        let bottom = y;

        // ======================= 左栏（自底向上排，与右栏底部对齐） =======================
        let btn_h = s(42);
        let btn_top = bottom - btn_h;
        let wage_top = btn_top - s(12) - line_h;
        let stats_top = wage_top - gap - line_h;
        let div_y = stats_top - s(12) - div_h;
        dividers.push(rect(ll, div_y, lr, div_y + div_h));
        let peek_top = div_y - s(12) - line_h;
        let show_top = peek_top - gap - line_h;
        let hide_top = show_top - gap - line_h;

        // —— 窗口列表卡片：可滚动行 + 底部“拾取窗口” ——
        let inner = s(6);
        let row_h = s(LIST_ROW_H);
        let pick_h = s(38);
        let list_inner_h = (hide_top - s(16) - pad - 2 * inner - pick_h).max(row_h * LIST_MIN_ROWS);
        let list_view = rect(ll + inner, pad + inner, lr - inner, pad + inner + list_inner_h);
        let pick_btn = rect(ll, list_view.bottom + inner, lr, list_view.bottom + inner + pick_h);
        let list_panel = rect(ll, pad, lr, pick_btn.bottom);

        // 行数据由 App 统一生成：已选中组（含隐藏 / 未运行，始终显示且可取消）在上，
        // 可选组在下。位置稳定，不因窗口隐藏 / 进程退出而消失。
        let base = list_view.top - self.ui.list_scroll;
        let list_rows = self.build_list_rows();
        let mut rows = Vec::with_capacity(list_rows.len());
        for (i, lr_) in list_rows.into_iter().enumerate() {
            let top = base + i as i32 * row_h;
            let row = rect(list_view.left, top, list_view.right, top + row_h - s(3));
            let cb = s(20);
            let cb_top = top + (row_h - cb) / 2 - s(1);
            let check = rect(row.left + s(8), cb_top, row.left + s(8) + cb, cb_top + cb);
            // 只有落在可视区内的部分可点。
            if let Some(visible) = intersect(&row, &list_view) {
                hits.push((visible, Hot::Row(i)));
            }
            rows.push(RowHit {
                kind: lr_.kind,
                key: i,
                title: lr_.title,
                checked: lr_.checked,
                dim: lr_.dim,
                tag: lr_.tag,
                row,
                check,
            });
        }
        let row_full_height = rows.len() as i32 * row_h;
        hits.push((pick_btn, Hot::Pick));

        // —— 老板键：隐藏键 / 显示键（两者相同即为切换键），及按住显示 ——
        let hide_key = Field::new(ll, lr, hide_top, line_h, s(180), s(32));
        let show_key = Field::new(ll, lr, show_top, line_h, s(180), s(32));
        let peek = Field::new(ll + s(INDENT), lr, peek_top, line_h, tgl_w, tgl_h);
        hits.push((hide_key.row, Hot::HideKey));
        hits.push((show_key.row, Hot::ShowKey));
        hits.push((peek.row, Hot::Peek));

        // —— 摸鱼统计（及其时薪）、底部按钮 ——
        let stats = Field::new(ll, lr, stats_top, line_h, tgl_w, tgl_h);
        let wage = Field::new(ll + s(INDENT), lr, wage_top, line_h, s(120), s(32));
        let mid = (ll + lr) / 2;
        let hide_btn = rect(ll, btn_top, mid - s(6), bottom);
        let show_btn = rect(mid + s(6), btn_top, lr, bottom);
        hits.push((stats.row, Hot::Stats));
        if self.cfg.stats {
            hits.push((wage.row, Hot::Wage));
        }
        hits.push((hide_btn, Hot::HideAll));
        hits.push((show_btn, Hot::ShowAll));

        Layout {
            height_needed: list_panel.bottom.max(bottom) + pad,
            list_panel,
            list_view,
            pick_btn,
            rows,
            row_full_height,
            list_inner_h,
            hide_key,
            show_key,
            peek,
            stats,
            wage,
            hide_btn,
            show_btn,
            mute,
            cover,
            topmost,
            auto_hide,
            hide_delay,
            reveal_delay,
            auto_focus,
            corner,
            corner_cells,
            away,
            projector,
            headphones,
            opacity,
            slider_track,
            click_through,
            wheel,
            disguise,
            tray,
            dividers,
            hits,
        }
    }

    /// 期望的客户区尺寸（物理像素）。
    pub fn desired_client(&self) -> (i32, i32) {
        let width = scale(LOGICAL_W, self.ui.dpi);
        let l = self.compute_layout(width);
        (width, l.height_needed)
    }

    /// 有效滑块轨道边界（考虑旋钮半径，使旋钮不超出）。
    fn slider_bounds(&self, track: &RECT) -> (i32, i32, i32) {
        let kr = scale(9, self.ui.dpi);
        (track.left + kr, track.right - kr, kr)
    }

    fn opacity_knob_x(&self, track: &RECT) -> i32 {
        let (tl, tr, _) = self.slider_bounds(track);
        let t = (self.cfg.opacity.saturating_sub(MIN_OPACITY)) as f32
            / (MAX_OPACITY - MIN_OPACITY) as f32;
        tl + (t * (tr - tl) as f32).round() as i32
    }

    fn opacity_from_x(&self, x: i32, track: &RECT) -> u8 {
        let (tl, tr, _) = self.slider_bounds(track);
        let t = ((x - tl) as f32 / (tr - tl).max(1) as f32).clamp(0.0, 1.0);
        let v = MIN_OPACITY as f32 + t * (MAX_OPACITY - MIN_OPACITY) as f32;
        v.round() as u8
    }

    // ——————————————————————— 绘制 ———————————————————————

    /// 把整个界面绘制到给定 DC（应为内存 DC）。
    pub fn paint(&mut self, c: &Canvas, client: RECT) {
        let width = client.right - client.left;
        let mut layout = self.compute_layout(width);
        // 针对本帧的实际行数钳制滚动量：行数变少（如删除离线项 / 取消选中）后，
        // 陈旧的 list_scroll 可能越界，使顶部行被卷出面板且滚动条 / 滚轮均失效而无法
        // 滚回。发现越界则钳制并按新滚动量重算一次布局。
        let max_scroll = (layout.row_full_height - layout.list_inner_h).max(0);
        if self.ui.list_scroll > max_scroll || self.ui.list_scroll < 0 {
            self.ui.list_scroll = self.ui.list_scroll.clamp(0, max_scroll);
            layout = self.compute_layout(width);
        }

        w::fill_rect(c, &client, theme::BG);

        // —— 左栏 ——
        self.draw_window_list(c, &layout);
        self.draw_hotkey(c, &layout.hide_key, "隐藏键", HkSlot::Hide, Hot::HideKey);
        self.draw_hotkey(c, &layout.show_key, "显示键", HkSlot::Show, Hot::ShowKey);
        self.draw_toggle_field(c, &layout.peek, "按住显示", self.cfg.peek, true, Hot::Peek);
        self.draw_toggle_field(c, &layout.stats, "摸鱼统计", self.cfg.stats, true, Hot::Stats);
        self.draw_status(c, &layout.stats, &self.stats_text(), theme::TEXT_DIM);
        self.draw_wage_field(c, &layout.wage, self.cfg.stats);
        self.draw_button(c, &layout.hide_btn, "全部隐藏", true, Hot::HideAll, true);
        self.draw_button(c, &layout.show_btn, "全部恢复", false, Hot::ShowAll, true);

        // —— 右栏 ——
        self.draw_toggle_field(c, &layout.mute, "隐藏时静音", self.cfg.mute_on_hide, true, Hot::Mute);
        self.draw_toggle_field(c, &layout.cover, "切回工作窗口", self.cfg.cover, true, Hot::Cover);
        self.draw_toggle_field(c, &layout.topmost, "显示时置顶", self.cfg.topmost, true, Hot::Topmost);

        let auto = self.cfg.auto_hide;
        self.draw_toggle_field(c, &layout.auto_hide, "鼠标移出隐藏", self.cfg.auto_hide, true, Hot::AutoHide);
        self.draw_stepper(
            c,
            &layout.hide_delay,
            "隐藏延迟",
            self.cfg.auto_hide_delay_ms,
            auto,
            (Hot::DelayMinus, Hot::DelayPlus),
        );
        self.draw_stepper(
            c,
            &layout.reveal_delay,
            "显示延迟",
            self.cfg.reveal_delay_ms,
            auto,
            (Hot::RevealMinus, Hot::RevealPlus),
        );
        self.draw_toggle_field(c, &layout.auto_focus, "显示时聚焦", self.cfg.auto_focus, auto, Hot::AutoFocus);
        self.draw_corner_field(c, &layout.corner, &layout.corner_cells);
        self.draw_toggle_field(c, &layout.away, "离开时隐藏", self.cfg.hide_on_away, true, Hot::Away);
        self.draw_toggle_field(c, &layout.projector, "接投影时隐藏", self.cfg.hide_on_projector, true, Hot::Projector);
        self.draw_toggle_field(c, &layout.headphones, "耳机断开时隐藏", self.cfg.hide_on_headphones, true, Hot::Headphones);

        let trans = self.cfg.transparency_enabled;
        self.draw_toggle_field(c, &layout.opacity, "半透明", trans, true, Hot::Opacity);
        self.draw_slider(c, &layout.slider_track, trans);
        self.draw_toggle_field(c, &layout.click_through, "鼠标穿透", self.cfg.click_through, trans, Hot::ClickThrough);
        self.draw_toggle_field(c, &layout.wheel, "Alt + 滚轮调节", self.cfg.wheel_opacity, trans, Hot::Wheel);

        self.draw_toggle_field(c, &layout.disguise, "进程伪装", self.cfg.disguise, true, Hot::Disguise);
        self.draw_toggle_field(c, &layout.tray, "托盘图标", self.cfg.tray_icon, true, Hot::Tray);

        for d in &layout.dividers {
            w::fill_rect(c, d, theme::DIVIDER);
        }

        self.ui.layout = layout;
    }

    /// 某元素此刻是否处于悬停态。
    fn hovered(&self, which: Hot) -> bool {
        self.ui.hover == which
    }

    /// 设置行的状态文字（如“被占用”“今日 1h05m”）：右对齐，紧贴控件左侧。
    fn draw_status(&self, c: &Canvas, f: &Field, txt: &str, color: u32) {
        if txt.is_empty() {
            return;
        }
        let r = rect(f.row.left, f.row.top, f.ctrl.left - scale(10, self.ui.dpi), f.row.bottom);
        w::text(c, txt, &r, self.ui.fonts.small, color, w::DT_RIGHT | TEXT_FLAGS);
    }

    /// 时薪行：标签 + 右侧可点击编辑的输入框（交互同热键框：点击进入编辑，
    /// Enter / 点别处确认，Esc 取消）。编辑时原文先全选，输入即替换。
    fn draw_wage_field(&self, c: &Canvas, f: &Field, enabled: bool) {
        let s = |v: i32| scale(v, self.ui.dpi);
        self.draw_label(c, &f.row, f.ctrl.left, "时薪", enabled);
        let editing = self.ui.editing_wage.as_deref();
        let border = if editing.is_some() {
            theme::ACCENT
        } else if enabled && self.hovered(Hot::Wage) {
            theme::ACCENT_HOVER
        } else {
            theme::BORDER
        };
        w::round_outline(c, &f.ctrl, s(8), border, 1, Some(theme::SURFACE));
        let font = self.ui.fonts.body_strong;
        let inner = rect(f.ctrl.left + s(8), f.ctrl.top, f.ctrl.right - s(8), f.ctrl.bottom);
        match editing {
            Some(buf) => c.clipped(&inner, || {
                let tw = w::text_width(c, buf, font);
                let x0 = (f.ctrl.left + f.ctrl.right - tw) / 2;
                let (top, bottom) = (f.ctrl.top + s(7), f.ctrl.bottom - s(7));
                let r = rect(x0, f.ctrl.top, x0 + tw + s(2), f.ctrl.bottom);
                if self.ui.wage_selected && !buf.is_empty() {
                    w::round_fill(c, &rect(x0 - s(3), top, x0 + tw + s(3), bottom), s(3), theme::ACCENT);
                    w::text(c, buf, &r, font, theme::ON_ACCENT, w::DT_LEFT | TEXT_FLAGS);
                } else {
                    w::text(c, buf, &r, font, theme::TEXT, w::DT_LEFT | TEXT_FLAGS);
                    let x = x0 + tw + s(1);
                    w::fill_rect(c, &rect(x, top, x + s(1).max(1), bottom), theme::TEXT);
                }
            }),
            None => {
                let txt = if self.cfg.wage.is_empty() { "—" } else { self.cfg.wage.as_str() };
                let color = match (self.cfg.wage.is_empty(), enabled) {
                    (_, false) => theme::TEXT_OFF,
                    (true, true) => theme::TEXT_DIM,
                    (false, true) => theme::TEXT,
                };
                w::text(c, txt, &inner, font, color, w::DT_CENTER | TEXT_FLAGS | w::DT_END_ELLIPSIS);
            }
        }
    }

    /// 屏幕角行：标签 + 右侧的屏幕缩略框，点框里的某个角选中它，再点一次关闭。
    fn draw_corner_field(&self, c: &Canvas, f: &Field, cells: &[RECT; 4]) {
        let s = |v: i32| scale(v, self.ui.dpi);
        self.draw_label(c, &f.row, f.ctrl.left, "屏幕角隐藏", true);
        let selected = self.cfg.corner;
        let row_hovered = self.hovered(Hot::Corner) || matches!(self.ui.hover, Hot::CornerAt(_));
        let border = if selected != 0 {
            theme::ACCENT
        } else if row_hovered {
            theme::ACCENT_HOVER
        } else {
            theme::BORDER
        };
        w::round_outline(c, &f.ctrl, s(6), border, 1, Some(theme::SURFACE));
        let (mw, mh, inset) = (s(12), s(8), s(4));
        for (i, cell) in cells.iter().enumerate() {
            let which = i as u8 + 1;
            let x = if i % 2 == 0 { cell.left + inset } else { cell.right - inset - mw };
            let y = if i < 2 { cell.top + inset } else { cell.bottom - inset - mh };
            let color = if selected == which {
                theme::ACCENT
            } else if self.hovered(Hot::CornerAt(which)) {
                theme::TEXT_DIM
            } else {
                theme::BORDER
            };
            w::round_fill(c, &rect(x, y, x + mw, y + mh), s(2), color);
        }
    }

    /// 设置行左侧的标签，右边界止于控件左侧。
    fn draw_label(&self, c: &Canvas, row: &RECT, ctrl_left: i32, txt: &str, enabled: bool) {
        let r = rect(row.left, row.top, ctrl_left - scale(8, self.ui.dpi), row.bottom);
        let color = if enabled { theme::TEXT } else { theme::TEXT_OFF };
        w::text(c, txt, &r, self.ui.fonts.body, color, w::DT_LEFT | TEXT_FLAGS | w::DT_END_ELLIPSIS);
    }

    fn draw_window_list(&self, c: &Canvas, layout: &Layout) {
        let dpi = self.ui.dpi;
        let s = |v: i32| scale(v, dpi);
        let panel = &layout.list_panel;
        let view = &layout.list_view;
        w::round_outline(c, panel, s(10), theme::BORDER, 1, Some(theme::CARD));

        // 底部“拾取窗口”：与列表同卡片，以分隔线隔开。
        let pick = &layout.pick_btn;
        w::fill_rect(c, &rect(panel.left + 1, pick.top, panel.right - 1, pick.top + 1), theme::BORDER);
        let pick_hovered = self.hovered(Hot::Pick);
        if pick_hovered {
            let inset = s(4);
            let r = rect(pick.left + inset, pick.top + inset, pick.right - inset, pick.bottom - inset);
            w::round_fill(c, &r, s(7), theme::SURFACE_HOVER);
        }
        let pick_color = if pick_hovered { theme::TEXT } else { theme::ACCENT_HOVER };
        w::text(c, "＋  拾取窗口", pick, self.ui.fonts.body_strong, pick_color, w::DT_CENTER | TEXT_FLAGS);

        if layout.rows.is_empty() {
            w::text(c, "没有可选窗口", view, self.ui.fonts.body, theme::TEXT_DIM, w::DT_CENTER | TEXT_FLAGS);
            return;
        }

        // 裁剪到可视区内绘制行。
        c.clipped(view, || {
            for row in &layout.rows {
                if row.row.bottom < view.top || row.row.top > view.bottom {
                    continue;
                }
                let hovered = self.hovered(Hot::Row(row.key));
                if hovered || row.checked {
                    let bg = if row.checked { theme::SURFACE } else { theme::SURFACE_HOVER };
                    w::round_fill(c, &row.row, s(7), bg);
                }
                if row.checked {
                    w::round_fill(c, &row.check, s(5), theme::ACCENT);
                    w::checkmark(c, &row.check, theme::ON_ACCENT, 2.0 * dpi as f32 / 96.0);
                } else {
                    w::round_outline(c, &row.check, s(5), theme::BORDER, 1, Some(theme::CARD));
                }
                // 右侧状态标签（“已隐藏”/“未运行”），并为标题让出空间。
                let mut title_right = row.row.right - s(8);
                if !row.tag.is_empty() {
                    let tag_rect = rect(title_right - s(52), row.row.top, title_right, row.row.bottom);
                    w::text(c, row.tag, &tag_rect, self.ui.fonts.small, theme::TEXT_DIM, w::DT_RIGHT | TEXT_FLAGS);
                    title_right = tag_rect.left - s(6);
                }
                let txt = rect(row.check.right + s(10), row.row.top, title_right, row.row.bottom);
                let color = if row.dim { theme::TEXT_DIM } else { theme::TEXT };
                w::text(c, &row.title, &txt, self.ui.fonts.body, color, w::DT_LEFT | TEXT_FLAGS | w::DT_END_ELLIPSIS);
            }
            // 已选中组与可选组之间的细分隔线。
            if let Some(first_avail) = layout.rows.iter().find(|r| !r.checked) {
                let has_selected = layout.rows.iter().any(|r| r.checked);
                let dy = first_avail.row.top - s(2);
                if has_selected && dy > view.top && dy < view.bottom {
                    let line = rect(view.left + s(4), dy, view.right - s(4), dy + s(1).max(1));
                    w::fill_rect(c, &line, theme::BORDER);
                }
            }
        });

        // 滚动条
        if layout.row_full_height > layout.list_inner_h {
            let track_h = layout.list_inner_h;
            let thumb_h = (track_h * track_h / layout.row_full_height).max(s(20));
            let max_scroll = layout.row_full_height - layout.list_inner_h;
            let t = self.ui.list_scroll as f32 / max_scroll as f32;
            let thumb_top = view.top + (t * (track_h - thumb_h) as f32) as i32;
            let bar = rect(panel.right - s(6), thumb_top, panel.right - s(3), thumb_top + thumb_h);
            w::round_fill(c, &bar, s(2), theme::BORDER);
        }
    }

    /// 热键行：标签 + 右侧按键框。点击整行进入捕获。
    fn draw_hotkey(&self, c: &Canvas, f: &Field, label: &str, slot: HkSlot, which: Hot) {
        let dpi = self.ui.dpi;
        let s = |v: i32| scale(v, dpi);
        let (hk, ok) = match slot {
            HkSlot::Hide => (&self.cfg.hide_hotkey, self.hide_ok),
            HkSlot::Show => (&self.cfg.show_hotkey, self.show_ok),
        };
        let capturing = self.ui.capturing_hotkey == Some(slot);
        // 注册失败（通常是与其它软件的全局热键冲突）。
        let conflict = !ok && !capturing;

        self.draw_label(c, &f.row, f.ctrl.left, label, true);
        if conflict {
            self.draw_status(c, f, "被占用", theme::DANGER);
        }

        let border = if capturing {
            theme::ACCENT
        } else if conflict {
            theme::DANGER
        } else if self.hovered(which) {
            theme::ACCENT_HOVER
        } else {
            theme::BORDER
        };
        w::round_outline(c, &f.ctrl, s(8), border, 1, Some(theme::SURFACE));

        let (txt, color) = if capturing {
            ("按下按键…".to_string(), theme::ACCENT_HOVER)
        } else if conflict {
            (hk.to_display_string(), theme::DANGER)
        } else {
            (hk.to_display_string(), theme::TEXT)
        };
        let r = rect(f.ctrl.left + s(10), f.ctrl.top, f.ctrl.right - s(10), f.ctrl.bottom);
        w::text(c, &txt, &r, self.ui.fonts.body_strong, color, w::DT_CENTER | TEXT_FLAGS | w::DT_END_ELLIPSIS);
    }

    /// 开关行：标签 + 右侧开关。
    fn draw_toggle_field(&self, c: &Canvas, f: &Field, label: &str, on: bool, enabled: bool, which: Hot) {
        self.draw_label(c, &f.row, f.ctrl.left, label, enabled);
        let r = &f.ctrl;
        let h = r.bottom - r.top;
        let radius = h / 2;
        let hovered = enabled && self.hovered(which);
        let (track, knob) = match (enabled, on, hovered) {
            (false, _, _) => (theme::SURFACE, theme::TEXT_OFF),
            (true, true, false) => (theme::ACCENT, theme::ON_ACCENT),
            (true, true, true) => (theme::ACCENT_HOVER, theme::ON_ACCENT),
            (true, false, false) => (theme::SURFACE, theme::ON_ACCENT),
            (true, false, true) => (theme::SURFACE_HOVER, theme::ON_ACCENT),
        };
        w::round_fill(c, r, radius, track);
        let pad = scale(3, self.ui.dpi);
        let kr = radius - pad;
        let cx = if on { r.right - pad - kr } else { r.left + pad + kr };
        w::disc(c, cx, r.top + radius, kr, knob);
    }

    /// 步进器行：`标签 …… [−] 数值 ms [+]`。
    fn draw_stepper(&self, c: &Canvas, st: &Stepper, label: &str, value_ms: u32, enabled: bool, hots: (Hot, Hot)) {
        let s = |v: i32| scale(v, self.ui.dpi);
        self.draw_label(c, &st.row, st.minus.left, label, enabled);
        self.draw_button(c, &st.minus, "−", false, hots.0, enabled);
        self.draw_button(c, &st.plus, "+", false, hots.1, enabled);
        w::round_outline(c, &st.value, s(8), theme::BORDER, 1, Some(theme::SURFACE));
        let color = if enabled { theme::TEXT } else { theme::TEXT_OFF };
        let txt = format!("{} ms", value_ms);
        w::text(c, &txt, &st.value, self.ui.fonts.body, color, w::DT_CENTER | TEXT_FLAGS);
    }

    fn draw_slider(&self, c: &Canvas, track: &RECT, enabled: bool) {
        let s = |v: i32| scale(v, self.ui.dpi);
        let cy = (track.top + track.bottom) / 2;
        let (tl, tr, kr) = self.slider_bounds(track);
        w::round_fill(c, &rect(tl, cy - s(3), tr, cy + s(3)), s(3), theme::TRACK);
        let kx = self.opacity_knob_x(track);
        let fill = if enabled { theme::ACCENT } else { theme::BORDER };
        w::round_fill(c, &rect(tl, cy - s(3), kx, cy + s(3)), s(3), fill);
        let knob = if enabled { theme::ON_ACCENT } else { theme::TEXT_OFF };
        w::disc(c, kx, cy, kr, knob);
        if enabled && (self.hovered(Hot::Slider) || self.ui.dragging_slider) {
            w::disc(c, kx, cy, kr - s(4), theme::ACCENT);
        }
        let pr = rect(track.right + s(8), track.top, track.right + s(52), track.bottom);
        let color = if enabled { theme::TEXT } else { theme::TEXT_OFF };
        let txt = format!("{}%", self.cfg.opacity);
        w::text(c, &txt, &pr, self.ui.fonts.body_strong, color, w::DT_RIGHT | TEXT_FLAGS);
    }

    /// 通用按钮（primary 用强调色填充；禁用时不响应悬停）。
    fn draw_button(&self, c: &Canvas, r: &RECT, txt: &str, primary: bool, which: Hot, enabled: bool) {
        let radius = scale(8, self.ui.dpi);
        let hovered = enabled && self.hovered(which);
        let color = if primary {
            let bg = if hovered { theme::ACCENT_HOVER } else { theme::ACCENT };
            w::round_fill(c, r, radius, bg);
            theme::ON_ACCENT
        } else {
            let fill = if hovered { theme::SURFACE_HOVER } else { theme::SURFACE };
            w::round_outline(c, r, radius, theme::BORDER, 1, Some(fill));
            if enabled {
                theme::TEXT
            } else {
                theme::TEXT_OFF
            }
        };
        w::text(c, txt, r, self.ui.fonts.body_strong, color, w::DT_CENTER | TEXT_FLAGS);
    }

    // ——————————————————————— 命中测试与交互 ———————————————————————

    pub fn hit_test(&self, x: i32, y: i32) -> Hot {
        self.ui
            .layout
            .hits
            .iter()
            .find(|(r, _)| pt_in(r, x, y))
            .map_or(Hot::None, |&(_, hot)| hot)
    }

    /// 处理左键按下。返回是否需要重绘。
    pub fn on_lbutton_down(&mut self, x: i32, y: i32) -> bool {
        let hot = self.hit_test(x, y);
        // 点击热键行以外的地方时取消录制；点击时薪框以外的地方时确认编辑。
        if !matches!(hot, Hot::HideKey | Hot::ShowKey) {
            self.end_hotkey_capture(None);
        }
        if hot != Hot::Wage {
            self.end_wage_edit(true);
        }
        match hot {
            Hot::Pick => self.enter_pick_mode(),
            Hot::HideKey => self.begin_hotkey_capture(HkSlot::Hide),
            Hot::ShowKey => self.begin_hotkey_capture(HkSlot::Show),
            Hot::Wage => {
                if self.ui.editing_wage.is_none() {
                    self.ui.editing_wage = Some(self.cfg.wage.clone());
                    self.ui.wage_selected = true;
                }
            }
            Hot::AutoHide => self.set_auto_hide(!self.cfg.auto_hide),
            Hot::AutoFocus => self.set_auto_focus(!self.cfg.auto_focus),
            Hot::Peek => self.set_peek(!self.cfg.peek),
            Hot::Stats => self.set_stats(!self.cfg.stats),
            Hot::Mute => self.set_mute_on_hide(!self.cfg.mute_on_hide),
            Hot::Cover => self.set_cover(!self.cfg.cover),
            Hot::Topmost => self.set_topmost(!self.cfg.topmost),
            Hot::Corner => self.set_corner(if self.cfg.corner == 0 { DEFAULT_CORNER } else { 0 }),
            Hot::CornerAt(k) => self.set_corner(if self.cfg.corner == k { 0 } else { k }),
            Hot::Away => self.set_hide_on_away(!self.cfg.hide_on_away),
            Hot::Projector => self.set_hide_on_projector(!self.cfg.hide_on_projector),
            Hot::Headphones => self.set_hide_on_headphones(!self.cfg.hide_on_headphones),
            Hot::ClickThrough => self.set_click_through(!self.cfg.click_through),
            Hot::Wheel => self.set_wheel_opacity(!self.cfg.wheel_opacity),
            Hot::Tray => self.set_tray_icon(!self.cfg.tray_icon),
            // set_delay / set_reveal_delay 内部已 clamp 到 [MIN_DELAY_MS, MAX_DELAY_MS]。
            Hot::DelayMinus => self.set_delay(self.cfg.auto_hide_delay_ms.saturating_sub(DELAY_STEP_MS)),
            Hot::DelayPlus => self.set_delay((self.cfg.auto_hide_delay_ms + DELAY_STEP_MS).min(MAX_DELAY_MS)),
            Hot::RevealMinus => self.set_reveal_delay(self.cfg.reveal_delay_ms.saturating_sub(DELAY_STEP_MS)),
            Hot::RevealPlus => self.set_reveal_delay((self.cfg.reveal_delay_ms + DELAY_STEP_MS).min(MAX_DELAY_MS)),
            Hot::Opacity => self.set_transparency_enabled(!self.cfg.transparency_enabled),
            Hot::Disguise => self.set_disguise(!self.cfg.disguise),
            Hot::Slider => {
                self.ui.dragging_slider = true;
                let track = self.ui.layout.slider_track;
                let p = self.opacity_from_x(x, &track);
                self.set_opacity_percent(p);
            }
            Hot::HideAll => self.on_hide_key(),
            Hot::ShowAll => {
                self.show_all();
                self.update_tray_tip();
            }
            Hot::Row(key) => {
                // 先取出行类别（Copy），再释放对 layout 的借用，避免与 &mut self 冲突。
                match self.ui.layout.rows.get(key).map(|r| r.kind) {
                    Some(RowKind::Selected { hwnd }) | Some(RowKind::Available { hwnd }) => {
                        self.toggle_managed(hwnd);
                    }
                    Some(RowKind::Offline { rt_index }) => {
                        self.remove_remembered(rt_index);
                    }
                    None => {}
                }
            }
            Hot::None => {}
        }
        true
    }

    pub fn on_lbutton_up(&mut self) -> bool {
        if self.ui.dragging_slider {
            self.ui.dragging_slider = false;
            return true;
        }
        false
    }

    /// 鼠标移动：更新拖动 / 悬停。返回是否需要重绘。
    pub fn on_mouse_move(&mut self, x: i32, y: i32) -> bool {
        if self.ui.dragging_slider {
            let track = self.ui.layout.slider_track;
            let p = self.opacity_from_x(x, &track);
            self.set_opacity_percent(p);
            return true;
        }
        let new_hover = self.hit_test(x, y);
        if new_hover != self.ui.hover {
            self.ui.hover = new_hover;
            return true;
        }
        false
    }

    pub fn on_mouse_leave(&mut self) -> bool {
        if self.ui.hover != Hot::None {
            self.ui.hover = Hot::None;
            return true;
        }
        false
    }

    /// 列表区域滚轮滚动。
    pub fn on_mouse_wheel(&mut self, delta: i32) -> bool {
        let l = &self.ui.layout;
        if l.row_full_height <= l.list_inner_h {
            return false;
        }
        let step = scale(LIST_ROW_H, self.ui.dpi);
        let lines = -(delta / 120);
        self.ui.list_scroll += lines * step;
        self.clamp_scroll();
        true
    }

    pub(crate) fn clamp_scroll(&mut self) {
        let l = &self.ui.layout;
        let max_scroll = (l.row_full_height - l.list_inner_h).max(0);
        self.ui.list_scroll = self.ui.list_scroll.clamp(0, max_scroll);
    }

    /// 键盘按下（用于热键录制与时薪编辑）。返回是否需要重绘。
    pub fn on_key_down(&mut self, vk: u32) -> bool {
        if let Some(buf) = self.ui.editing_wage.as_mut() {
            match vk {
                0x0D => self.end_wage_edit(true),  // Enter
                0x1B => self.end_wage_edit(false), // Esc
                0x08 => {
                    // Backspace：全选时清空，否则删去最后一个字符。
                    if self.ui.wage_selected {
                        buf.clear();
                    } else {
                        buf.pop();
                    }
                    self.ui.wage_selected = false;
                }
                _ => return false, // 字符由 WM_CHAR 送达
            }
            return true;
        }
        if self.ui.capturing_hotkey.is_none() {
            return false;
        }
        if vk == 0x1B {
            // Esc 取消
            self.end_hotkey_capture(None);
            return true;
        }
        if is_modifier_vk(vk) {
            return false; // 等待非修饰主键
        }
        self.end_hotkey_capture(Some(Hotkey::Key { mods: held_mods(), vk }));
        true
    }

    /// 热键录制中按下鼠标中键 / 侧键时调用：连同当前修饰键设为该槽位的老板键。
    pub fn capture_mouse(&mut self, button: MouseButton) {
        self.end_hotkey_capture(Some(Hotkey::Mouse { mods: held_mods(), button }));
    }

    /// 输入字符（WM_CHAR，含输入法上屏的文字）。返回是否需要重绘。
    pub fn on_char(&mut self, code: u32) -> bool {
        let Some(buf) = self.ui.editing_wage.as_mut() else {
            return false;
        };
        // 控制字符（Enter / Esc / Backspace 等）已在 on_key_down 处理；代理对（emoji）不接受。
        let Some(ch) = char::from_u32(code).filter(|c| !c.is_control()) else {
            return false;
        };
        if self.ui.wage_selected {
            buf.clear();
            self.ui.wage_selected = false;
        }
        if buf.chars().count() < WAGE_MAX_CHARS {
            buf.push(ch);
        }
        true
    }

    /// 结束时薪编辑：`commit` 为 true 时保存（无法解析的输入保持原值）。
    pub fn end_wage_edit(&mut self, commit: bool) {
        if let Some(buf) = self.ui.editing_wage.take() {
            if commit {
                self.set_wage(&buf);
            }
        }
    }
}

/// 当前按住的修饰键（MOD_* 位组合）。
fn held_mods() -> u32 {
    let mut mods = 0u32;
    if key_pressed(0x11) {
        mods |= MOD_CONTROL;
    }
    if key_pressed(0x12) {
        mods |= MOD_ALT;
    }
    if key_pressed(0x10) {
        mods |= MOD_SHIFT;
    }
    if key_pressed(0x5B) || key_pressed(0x5C) {
        mods |= MOD_WIN;
    }
    mods
}

fn key_pressed(vk: i32) -> bool {
    unsafe { (GetKeyState(vk) as u16 & 0x8000) != 0 }
}
