//! GUI 层：现代深色主题的设置窗口。
//!
//! * [`theme`]   配色与字体
//! * [`widgets`] 绘制原语（抗锯齿形状 + GDI 文字）
//! * [`ui`]      布局、绘制与交互（以 `impl App` 的形式扩展应用）

pub mod theme;
pub mod ui;
pub mod widgets;
