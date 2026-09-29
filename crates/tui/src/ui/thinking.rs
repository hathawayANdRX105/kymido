//! ui/thinking — 模型思考块（T21）：一条 assistant 消息的 `reasoning` 渲成
//! `◇ Thought` 块，与普通正文（base 样式）、工具卡（`▸` + 三态色）都分得开：
//! 卡头与「N lines hidden」提示用 `theme::warn()`，正文用 `theme::dim()`。
//!
//! **对偶契约**：[`rows`]（计数侧，`transcript::message_rows`）与 [`lines`]
//! （物化侧，`transcript::push_message`）共用同一折叠分支 [`visible`] 与
//! `transcript` 的同一 `wrap_with` 断行核心——折叠策略或断行算法任何一处
//! 改动同时落到两边，分叉由 `tests/transcript_window.rs::
//! reasoning_render_and_count_agree` 钉（同 `tests/scroll_follow.rs` 的
//! 窗口计数/物化一致性钉形状）。
//!
//! **零虚构契约**：空 / 纯空白 reasoning 占 0 行（历史会话回填
//! `web-state::convert::message_to_chat` 的 reasoning 恒为空——渲一个下面
//! 什么都不挂的 `◇ Thought` 卡头是虚构内容，且总行数错 1 会让窗口化
//! transcript 的滚动位与画面失同步），由 `tests/transcript_window.rs::
//! empty_reasoning_takes_zero_rows` 钉。
//!
//! 顺序契约：思考块在**同一消息**的文本 / 工具 part 之前出（模型先思考
//! 后作答，视觉顺序 = 发生顺序），由
//! `tests/transcript_window.rs::reasoning_precedes_text_parts` 钉。

use std::borrow::Cow;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme;

use super::fold::Fold;

/// 折叠阈值：reasoning 逻辑行超过该数即折成「头尾预览 + 隐藏行数」（与
/// 工具卡折叠同形，独立常数——P2 的展开键留独立调参空间）。恒大于
/// [`HEAD`] + [`TAIL`]，折叠切片不越界。
const FOLD_AFTER: usize = 8;
/// 折叠时保留的头部逻辑行数。
const HEAD: usize = 3;
/// 折叠时保留的尾部逻辑行数。
const TAIL: usize = 2;
/// 思考块卡头。字形 `◇` 与工具卡 `▸`、用户前缀 `❯` 分得开；单格
/// （unicode-width 1），永不换行、不影响行数。
const HEADER: &str = "◇ Thought";

/// 思考块行数（计数侧，`message_rows` 的对偶）：空 / 纯空白 reasoning =
/// 0 行（零虚构契约不变）；Hidden = 1 行 stub；否则卡头 1 行 + 可见逻辑行
/// 的断行数（长内容含「… N lines hidden」提示行）。分支与 [`lines`] 同源。
pub fn rows(reasoning: &str, fold: Fold, width: usize) -> usize {
    if reasoning.trim().is_empty() {
        return 0;
    }
    if fold == Fold::Hidden {
        return 1; // stub 恒 1 行
    }
    let clean = super::transcript::sanitize(reasoning);
    let visible = visible(&clean, fold);
    let body: usize = visible
        .iter()
        .map(|(line, _)| super::transcript::count_wrapped(line, width, "  "))
        .sum();
    1 + body
}

/// 思考块行（物化侧，`push_message` 的对偶）：空 / 纯空白 = 不出一行；
/// Hidden = 一行 stub；否则卡头行（warn）+ 可见逻辑行（正文 dim、隐藏提示
/// warn）。0 行 / 窄列宽永不 panic。分支与断行核心与 [`rows`] 同源。
pub fn lines(reasoning: &str, fold: Fold, width: usize) -> Vec<Line<'static>> {
    if reasoning.trim().is_empty() {
        return Vec::new();
    }
    if fold == Fold::Hidden {
        return vec![stub()];
    }
    let clean = super::transcript::sanitize(reasoning);
    let vis = visible(&clean, fold);
    let mut out: Vec<Line<'static>> = Vec::with_capacity(vis.len() + 1);
    out.push(Line::from(Span::styled(HEADER, theme::warn())));
    for (line, style) in vis {
        super::transcript::push_wrapped(&mut out, &line, width, "  ", style);
    }
    out
}

/// [`Fold::Hidden`] 的一行 stub（同工具卡：整卡消失会让鼠标点不回来）：
/// `◇ Thought ‹hidden›`，warn 样式，可点回 [`Fold::Collapsed`]。
fn stub() -> Line<'static> {
    Line::from(Span::styled("◇ Thought ‹hidden›", theme::warn()))
}

/// 可见逻辑行规格（计数 / 物化两侧同一调用点——对偶的结构性所在）：
/// `Expanded` = 全部逻辑行；`Collapsed` = 短内容全显、长内容头 [`HEAD`] 行 +
/// 「… N lines hidden」提示 + 尾 [`TAIL`] 行。`Hidden` 不走到这里（在
/// `rows`/`lines` 顶层短路成 stub）。每项 `(文本, 样式)`：正文行 dim、提示行
/// warn（提示也走断行核心，窄列宽下提示自身换行时两侧同步换行）。
fn visible(clean: &str, fold: Fold) -> Vec<(Cow<'_, str>, Style)> {
    let logical: Vec<&str> = clean.lines().collect();
    if fold == Fold::Expanded || logical.len() <= FOLD_AFTER {
        return logical
            .iter()
            .map(|line| (Cow::Borrowed(*line), theme::dim()))
            .collect();
    }
    let hidden = logical.len() - (HEAD + TAIL);
    let mut out: Vec<(Cow<'_, str>, Style)> = Vec::with_capacity(HEAD + 1 + TAIL);
    for line in logical.iter().take(HEAD) {
        out.push((Cow::Borrowed(*line), theme::dim()));
    }
    out.push((
        Cow::Owned(format!("… {hidden} lines hidden")),
        theme::warn(),
    ));
    for line in &logical[logical.len() - TAIL..] {
        out.push((Cow::Borrowed(*line), theme::dim()));
    }
    out
}
