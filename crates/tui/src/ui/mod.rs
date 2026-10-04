//! ui/* — enhanced 外壳渲染（route §3：transcript 上、dock 下；T3 起加
//! 工具卡 / 问题面板 / footer 状态条）。
//!
//! 结构：[`layout`] 定竖向切分，[`transcript`] 出消息行（工具 part 交
//! [`tool_card`] 卡片化），[`footer`] 是状态条，[`questions`] 是 dock 上方
//! 的问题面板，[`dock`] 出活动/排队/提示，[`composer`] 出输入行与光标。
//! 样式全部来自 `crate::theme`（D11）：本目录不构造 `Color`，只消费语义 token。

mod composer;
pub mod diff;
mod dock;
pub mod fold;
pub mod footer;
pub mod hit;
mod layout;
pub mod markdown;
pub mod panels;
pub mod questions;
mod scrollbar;
pub mod search_overlay;
pub mod session_picker;
mod slash_palette;
mod theme_panel;
pub mod thinking;
pub mod tool_card;
mod transcript;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, BorderType, Borders};

use crate::app::App;
use crate::theme;

/// 一帧的区域几何（T2 竖向切分 + T3 让位 + T9 斜杠面板让行 + T11 搜索
/// overlay 让行 + T24 滚动条预留列）。[`draw`] 与 [`sync_viewport`] 共用
/// 同一计算——滚动模型与渲染看到的 transcript 视口永远是同一套，窗口不会
/// 漂移。
struct Areas {
    transcript: Rect,
    footer: Rect,
    panel: Rect,
    palette: Rect,
    search: Rect,
    dock: Rect,
    /// T24：transcript 右侧滚动条预留列（未预留时零宽，见
    /// [`scrollbar::split_columns`]）。
    scrollbar: Rect,
}

/// 把整屏切成 transcript / 斜杠面板 / 搜索 overlay / footer / 问题面板 /
/// dock 六块（语义与 T2/T3/T9/T11 逐条一致，抽出来给两条调用方共用）——
/// **不**扣横向预留列：横向唯一权威是 T24 滚动条预留列（见 [`areas`]）。
fn base_areas(app: &App, area: Rect) -> Areas {
    let (transcript, dock) = layout::split(area, app.queued().is_some());
    // footer：dock 之上恒 1 行（dock 压底时才有行可让）。
    let footer_height = u16::from(dock.y > 0);
    let footer = Rect {
        x: area.x,
        y: dock.y.saturating_sub(footer_height),
        width: area.width,
        height: footer_height,
    };
    // 问题面板：footer 之上、按 pending 实占行数让位（无题 = 0 行）。
    let panel_rows = app.questions().rows();
    let panel_y = footer.y.saturating_sub(panel_rows);
    let panel = Rect {
        x: area.x,
        y: panel_y,
        width: area.width,
        height: footer.y - panel_y,
    };
    // T11 搜索 overlay：问题面板之上恒 `search_rows()` 行（内容 `search::OVERLAY_ROWS` +
    // T20 边框 2 行；0 行 = 关闭）。与斜杠面板开合互斥（`slash_visible` 在 overlay 打开时恒
    // false），两块不会同帧同时占行。
    // 让行几何与斜杠面板**共用** `overlay_rect`。
    let search = overlay_rect(area, panel_y, app.search_rows(), transcript.y);
    // T9 斜杠面板：贴在搜索 overlay 之下（0 行 = 关闭；行数 = 面板候选数 +
    // T20 边框 2 行，无命中也留 1 行提示；小屏按 transcript 余量夹紧——同款几何）。
    let palette = overlay_rect(area, search.y, app.panel_rows(), transcript.y);
    // transcript：吃掉斜杠面板 + 搜索 overlay + footer + 面板让出的行。
    let transcript = Rect {
        height: palette.y.saturating_sub(transcript.y),
        ..transcript
    };
    Areas {
        transcript,
        footer,
        panel,
        palette,
        search,
        dock,
        // T24：基几何无预留列；[`areas`] 按锁存态再抠。
        scrollbar: Rect::ZERO,
    }
}

/// T24：区域几何的唯一入口（[`draw`] / [`sync_viewport`] 共用）：先切基六
/// 块（[`base_areas`]），再按 [`App::scrollbar_visible`]（迟滞决议的提交
/// 结果，见 [`scrollbar::resolve`]）抠 transcript 右端预留列——渲染 /
/// 模型 / 任务面板看到的 transcript 永远是同一套列。
fn areas(app: &App, area: Rect) -> Areas {
    let mut a = base_areas(app, area);
    let (content, bar) = scrollbar::split_columns(a.transcript, app.scrollbar_visible());
    a.transcript = content;
    a.scrollbar = bar;
    a
}

/// 一块贴底 overlay 的让行矩形：T9 斜杠面板与 T11 搜索 overlay 用**同一套**
/// 几何（贴 `boundary` 底向上让 `rows` 行，小屏按 `ceiling` 余量夹紧；0 行 =
/// 关闭，矩形 y 退回 boundary）。两块本是同一模式的两次实例（改名复制），
/// 抽成一处后结构上不可能各自漂移。
fn overlay_rect(area: Rect, boundary: u16, rows: u16, ceiling: u16) -> Rect {
    let rows = rows.min(boundary.saturating_sub(ceiling));
    let y = boundary.saturating_sub(rows);
    Rect {
        x: area.x,
        y,
        width: area.width,
        height: boundary - y,
    }
}

/// T20：四个浮动面板（问题面板 / 斜杠候选 / 主题方案 / 搜索 overlay）共用的
/// 轮廓块：圆角边框 + 短标题。边框与标题出自**同一个** `Block`（标题画在顶
/// 边框行内，两者结构上不可能漂移）；描边走 [`theme::border`]（D11：ui/*
/// 不持有 raw `Color`）。四面板都调这一个入口——新增浮动面板也用这个块，
/// 且其让行数必须连边框（上下各 1 行）一起数，与 panel_rows 让行口径同。
pub fn panel_block(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title)
        .style(theme::border())
}

/// 画一帧（自上而下）：transcript（含 T24 滚动条预留列 + 批 A 命中登记）→
/// 斜杠面板（T9，0 行不画）→ footer 状态条 → 问题面板（无题 0 行）→ dock
/// （活动 / 排队 / composer / 按键提示，恒压底——hints 仍是屏幕最底一行，
/// T2 布局契约不变）→ 搜索 overlay（T11，0 行不画）→ 回到底部 overlay
/// （批 A，脱钩时画，最后一笔 + 命中区最优先）。
///
/// 命中表流程（批 A）：`hit.clear()` → `transcript::render` 把本帧画出的卡头/
/// 思考头/stub 行推进表 → `scroll_to_bottom` 把 overlay 格推进表（后登记优先，
/// 盖住其下 transcript 行）。`hit` 是 `&App` 上的 `RefCell`——渲染收不可变
/// 借用，命中表是渲染副产物要 interior mutability。
///
/// footer 与两层面板的行从 transcript 底部让出：dock 几何仍由 [`layout::split`]
/// 原样决定，不动 T2 的切分语义（route §1 T3 白名单外的 layout/dock 零改动）。
pub fn draw(frame: &mut Frame, app: &App) {
    let areas = areas(app, frame.area());
    app.hit().borrow_mut().clear();
    transcript::render(frame, areas.transcript, app, app.hit());
    mark_focus(frame, areas.transcript, app);
    // T24：滚动条缩略块（零宽列 = 未预留，内部不画）。
    scrollbar::render(frame, areas.scrollbar, app);
    slash_palette::render(frame, areas.palette, app);
    theme_panel::render(frame, areas.palette, app);
    footer::render(frame, areas.footer, app);
    app.questions().render(frame, areas.panel);
    dock::render(frame, areas.dock, app);
    search_overlay::render(frame, areas.search, app);
    scroll_to_bottom::render(frame, areas.transcript, app);
}

/// 批 A：脱钩时的「回到底部」overlay（freebuff `scroll-to-bottom-button` 对应）。
/// 画在 transcript 区**右下角**最后一行（不占 dock 行——`dock_layout_fits`
/// 44×20 契约不动），单格 `↓` + 命中区（点它 = `scroll.to_end()` 恢复跟尾）。
/// 只在脱钩时画（跟尾 = 已在底，无物可点）。渲染同时登记命中：后登记优先，
/// 盖住该行 transcript 命中的格。
mod scroll_to_bottom {
    use ratatui::Frame;
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    use crate::app::App;
    use crate::theme;

    /// 画 overlay + 登记命中。`area` = transcript 区（已扣滚动条预留列的
    /// 几何——overlay 落在文本列内最右格，不占滚动条列）。
    pub fn render(frame: &mut Frame, area: Rect, app: &App) {
        if app.viewport().is_following() || area.width == 0 || area.height == 0 {
            return;
        }
        // 右下角最后一格：x = 右缘 - 1，y = 底缘 - 1。
        let x = area.x + area.width - 1;
        let y = area.y + area.height - 1;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("↓", theme::brand_bold()))),
            Rect {
                x,
                y,
                width: 1,
                height: 1,
            },
        );
        app.hit()
            .borrow_mut()
            .push(x, y, 1, 1, super::hit::HitAction::ScrollToBottom);
    }
}

/// T12：焦点行标记（route §3 注记④「焦点消息行最小标记」）——transcript
/// **只读消费**：行坐标 = [`transcript::total_lines`] + [`search_overlay::line_offset`]
/// + 视口 `view_top`（与渲染同一套断行口径，行数只数一次，`transcript.rs`
///   内核零触碰）；标记 = 该消息首条**可见**行（滚动到首行之上时取窗口内
///   第一行）加 [`theme::focus`] 下划线，叠在本帧 buffer 上、下一帧由重绘
///   复原——聚焦是瞬时状态，不回写渲染数据。
fn mark_focus(frame: &mut Frame, area: Rect, app: &App) {
    let Some(idx) = app.focused_message() else {
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(msg) = app.messages().get(idx) else {
        return;
    };
    let width = area.width.max(1);
    let height = area.height as usize;
    let total = transcript::total_lines(app, width);
    let top = app.viewport().view_top(total, height);
    let first = search_overlay::line_offset(app, idx, width);
    let rows = transcript::message_rows(app, msg, idx, width as usize);
    // 首条可见行：消息窗口 [first, first+rows) 与视口 [top, top+height) 的交集
    // 起点；交不上 = 本帧看不到这条消息，不画。
    let mark = first.max(top);
    if mark >= (first + rows).min(top + height) {
        return;
    }
    let y = area.y + (mark - top) as u16;
    // 下划线到该行最后一个非空字符为止：整行（含行尾空白）下划线会读成
    // 一条横线分隔，盖过消息内容本身。
    let buf = frame.buffer_mut();
    let last = (area.x..area.x + area.width).rev().find(|&x| {
        buf.cell((x, y))
            .is_some_and(|c| !c.symbol().trim().is_empty())
    });
    let Some(last) = last else {
        return;
    };
    for x in area.x..=last {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_style(theme::focus());
        }
    }
}

/// T6：本帧滚动几何同步——把 transcript 视口行数与内容总行数喂给
/// [`ScrollModel::sync`](crate::scroll::ScrollModel::sync)（跟尾滑动 /
/// clamp 在模型里推进，渲染只读模型）。事件循环在每次 `draw` 前调用；
/// 契约测试用同一入口驱动整帧，不绕开模型自己算边界。
///
/// T11 顺带记下 transcript 列宽（[`App::set_view_width`]）——命中行定位
/// 按同一份断行几何算，与 `transcript::total_lines` 同源。
///
/// T24：宽度权威先于任何计数决议——先取基几何（[`base_areas`]，未扣预留
/// 列），滚动条迟滞决议（[`scrollbar::resolve`]）后提交可见性锁存与宽度，
/// 再数行喂模型；其后 `draw` 按刚提交的锁存抠出的预留列与这里计数的宽度
/// 是同一套。
pub fn sync_viewport(app: &mut App, screen: Rect) {
    let base = base_areas(app, screen);
    let height = base.transcript.height as usize;
    let (visible, width) =
        scrollbar::resolve(app.scrollbar_visible(), app, base.transcript.width, height);
    app.set_scrollbar_visible(visible);
    let total = transcript::total_lines(app, width);
    app.set_view_width(width);
    app.viewport_mut().sync(total, height);
}
