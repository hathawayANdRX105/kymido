//! ui/scrollbar — T24：transcript 右侧滚动条（预留列 + 缩略块）。
//!
//! 可见性是一个**锁存**（[`App::scrollbar_visible`]），由
//! [`super::sync_viewport`] 每帧迟滞决议（refs jcode `ui.rs:3119-3165`）：
//! 判定按**上一帧**的档（有没有预留列）试溢出，不拿当前帧数字自定——同帧
//! 自定的可见性改变断行宽度 → 行数变 → 溢出测试翻转 → 屏幕重排两次
//! （flicker）。渲染只读锁存，不自己发明状态。
//!
//! 宽度单一权威：[`content_width`] / [`split_columns`] 是预留列的唯一出处
//! （[`super::draw`] 的 `areas` 与 [`super::panels::render`] 都从这抠，
//! 两块结构上不可能漂移）；先定宽 → 再计数 → 再画。
//!
//! 渲染用 ratatui 0.30 自带 [`Scrollbar`] / [`ScrollbarState`]（widget 体在
//! 传递依赖 `ratatui-widgets`，锁文件里已有，零新增依赖）。缩略块描边走
//! [`theme::border`]；视口被跟尾钉底时（[`crate::scroll::ScrollModel::is_following`]）
//! 换 [`theme::dim`] 退进背景，脱钩时亮起来（refs grok
//! `xai-grok-pager-render/src/render/scrollbar.rs:257-269`：following =
//! very dim，not following = brighter）。

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Scrollbar, ScrollbarState};

use crate::app::App;
use crate::theme;

/// 预留列宽（`visible` 且基宽 ≥2 时 1，否则 0）。
fn reserved(base_width: u16, visible: bool) -> u16 {
    u16::from(visible && base_width > 1)
}

/// T24：transcript 内容列宽（断行 / 计数 / 画面的单一宽度权威）：预留
/// 滚动条列时比基宽少 1 列。[`super::sync_viewport`] 迟滞决议后提交，
/// `transcript::render` / `mark_focus` / `search_overlay` 消费的都是同一
/// 份决议后宽度。
pub(crate) fn content_width(base_width: u16, visible: bool) -> u16 {
    base_width - reserved(base_width, visible)
}

/// T24：把 transcript 基矩形切成 `(内容, 滚动条列)`——预留列的**唯一**
/// 出处（refs jcode `ui.rs:3660-3675` `split_native_scrollbar_area`）。
/// [`super::draw`] 的 `areas` 与 [`super::panels::render`]（transcript 底部
/// 贴底的任务面板）都从这里抠，两块结构上不可能各自漂移；未预留时
/// 滚动条列零宽。
pub(crate) fn split_columns(transcript: Rect, visible: bool) -> (Rect, Rect) {
    let width = reserved(transcript.width, visible);
    let content = Rect {
        width: transcript.width - width,
        ..transcript
    };
    let bar = Rect {
        x: transcript.x + content.width,
        width,
        ..transcript
    };
    (content, bar)
}

/// T24：迟滞决议（refs jcode `ui.rs:3119-3165` 逐分支对齐）：`prev` = 上一
/// 帧的可见性（[`App::scrollbar_visible`]），`base_width` = **未**预留的
/// transcript 列宽，`height` = 视口行数。返回 `(本帧可见性, 提交宽度)`——
/// 计数与抠列都用提交宽度。
///
/// 判定按**上一帧**的档试溢出：上帧已预留 → 窄宽试（fits 则退滚动条——
/// 窄断行总不少于宽断行，「窄放得下 ⟹ 宽放得下」，refs jcode
/// `ui.rs:3125-3126`）；上帧未预留 → 宽宽试（溢出则窄必溢出，直接上）。
/// 稳态只数一个宽度；第二个宽度只在决议翻转帧出现。
///
/// 退化输入（基宽 <2 抠不出列 / 视口 0 行）：不预留、不计数，不 panic。
pub(crate) fn resolve(prev: bool, app: &App, base_width: u16, height: usize) -> (bool, u16) {
    if base_width < 2 || height == 0 {
        return (false, base_width);
    }
    // 窄断行行数 ≥ 宽断行行数（贪心断行的单调性）：上帧窄 → 数窄即定局；
    // 上帧宽且溢出 → 数宽即定局（窄只会更多行）。各数一次，不做双宽双数。
    let overflow = |w: u16| super::transcript::total_lines(app, w) > height;
    let visible = if prev {
        overflow(base_width - 1)
    } else {
        overflow(base_width)
    };
    (visible, content_width(base_width, visible))
}

/// T24：把缩略块画进预留列（`bar` = [`split_columns`] 的列；零宽 / 零高 /
/// 无内容一律不画，退化输入不 panic）。
///
/// 状态全由滚动模型**只读**驱动（[`crate::scroll::ScrollModel::total`] / `height` /
/// `view_top`）——滚动条是模型的投影，不回写滚动状态（T24 红线：`scroll.rs`
/// 零触碰）。`position` = 视口首行（ratatui 0.30 的 `ScrollbarState` 口径：
/// position = 内容内当前位，与 `view_top` 同义）。
pub(crate) fn render(frame: &mut Frame, bar: Rect, app: &App) {
    if bar.width == 0 || bar.height == 0 {
        return;
    }
    let view = app.viewport();
    let total = view.total();
    if total == 0 {
        return; // 无内容不画缩略块（ratatui `content_length == 0` 自身亦 no-op）
    }
    let height = view.height();
    let top = view.view_top(total, height);
    // 跟尾钉底 → 缩略块退进背景（grok `scrollbar_styles` 的 following 臂）；
    // 脱钩回看 → 描边色亮起来。样式全部出自 `theme`（D11：ui/* 不持 raw
    // `Color`）。
    let thumb = if view.is_following() {
        theme::dim()
    } else {
        theme::border()
    };
    let mut state = ScrollbarState::new(total)
        .position(top)
        .viewport_content_length(height.max(1));
    frame.render_stateful_widget(Scrollbar::default().style(thumb), bar, &mut state);
}
