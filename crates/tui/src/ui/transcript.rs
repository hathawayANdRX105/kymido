//! ui/transcript — `UiState` 消息 → 若干行（route §2 纯函数管线，D10）。
//!
//! 只读快照渲染：user 行带 brand 前缀、assistant 正文按 `parts` 顺序出
//! 文本/工具行（工具 part 交 [`super::tool_card`] 卡片化，T3）。按列宽
//! 自己断行，行数可数 → 底部对齐只需截尾，不依赖 Paragraph wrap 的不可见
//! 行数。模型内容先过 [`sanitize`]：裸控制字节不许进 cell（同 linear 零
//! ESC 约束）。
//! 思考块（`reasoning`，T21）经 [`super::thinking`] 在该消息的文本/工具 part
//! **之前**出，计数与物化同源（分叉由 `tests/transcript_window.rs` 钉）。
//!
//! **T6 窗口化**：渲染不再物化全部历史行——第一遍 [`total_lines`] 只
//! **计数**（与物化共用 [`wrap_with`] 同一断行核心，历史行不生成
//! `Line`/`Span`），第二遍 [`window`] 只物化与视口窗口
//! `[view_top, view_top + height)` 相交的消息。视口首行来自
//! [`ScrollModel`](crate::scroll::ScrollModel)（翻页 / 脱钩 / 跟尾滑动都
//! 在模型里，渲染只读）。

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use web_state::types::{ChatMessage, MessagePart};

use crate::app::App;
use crate::theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 渲染 transcript 到 `area`：窗口化——只物化可见窗口（route §3 T6 契约
/// 种子），历史行不重复物化。
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width.max(1);
    let height = area.height as usize;
    let total = total_lines(app, width);
    let top = app.viewport().view_top(total, height);
    frame.render_widget(
        Paragraph::new(window(app, width as usize, top, height)),
        area,
    );
}

/// 内容总行数（第一遍：**只计数不物化**——与 [`window`] 共用断行核心，
/// 分叉由 `tests/scroll_follow.rs::window_render_matches_line_count` 钉）。
pub(super) fn total_lines(app: &App, width: u16) -> usize {
    let width = width.max(1) as usize;
    app.messages()
        .iter()
        .map(|msg| message_rows(msg, app.tools_expanded(), width))
        .sum()
}

/// 一条消息的行数（与 [`push_message`] 逐分支同源，分叉即测试红）。行数的
/// **单一出处**：渲染侧 [`total_lines`] / [`window`] 与检索侧
/// [`crate::ui::search_overlay::line_offset`] 同吃本函数，两边不可能各数各的。
/// T21：思考块行数先经 [`super::thinking::rows`]（在文本/工具 part 之前，
/// 与 [`push_message`] 物化侧同序）。
pub(super) fn message_rows(msg: &ChatMessage, tools_expanded: bool, width: usize) -> usize {
    if msg.role == "user" {
        return count_wrapped(&msg.content, width, "❯ ");
    }
    // T21：思考块（reasoning）在文本/工具 part 之前出——parts 空与非空
    // 两个分支都算。历史回填消息 reasoning 为空，`thinking::rows` 对它
    // 归 0 是零虚构契约（不出裸卡头）。
    let mut rows = super::thinking::rows(&msg.reasoning, width);
    if msg.parts.is_empty() {
        rows += count_wrapped(&msg.content, width, "");
        return rows;
    }
    rows += msg
        .parts
        .iter()
        .map(|part| match part {
            MessagePart::Text(text) => count_wrapped(text, width, ""),
            MessagePart::Tool(tc) => super::tool_card::rows(tc, tools_expanded, width),
        })
        .sum::<usize>();
    rows
}

/// 可见窗口的行（第二遍）：只物化与 `[top, top + height)` 相交的消息，
/// 再切片到窗口——其上（更旧）与其下（更新）的消息一行为都不生成。
fn window(app: &App, width: usize, top: usize, height: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(height);
    let mut start = 0usize;
    for msg in app.messages() {
        let rows = message_rows(msg, app.tools_expanded(), width);
        let end = start + rows;
        if end <= top {
            // 整条在窗口之上（更旧）：跳过，不物化。
            start = end;
            continue;
        }
        if start >= top + height {
            break; // 整条在窗口之下（更新）：其后只会更靠下，收工。
        }
        if rows > 0 {
            let mut buf: Vec<Line<'static>> = Vec::with_capacity(rows);
            push_message(&mut buf, app, msg, width);
            let from = top.saturating_sub(start);
            let to = (top + height).min(end) - start;
            out.extend(buf.into_iter().skip(from).take(to - from));
        }
        start = end;
        if out.len() >= height {
            break;
        }
    }
    out
}

/// 一条消息 → 若干行（与 [`message_rows`] 同一数据路径的物化侧）。
fn push_message(out: &mut Vec<Line<'static>>, app: &App, msg: &ChatMessage, width: usize) {
    if msg.role == "user" {
        push_wrapped(out, &msg.content, width, "❯ ", theme::brand_bold());
        return;
    }
    // T21：思考块在文本/工具 part 之前（与 message_rows 逐分支同序——计数
    // 侧与物化侧分叉即测试红）。
    out.extend(super::thinking::lines(&msg.reasoning, width));
    if msg.parts.is_empty() {
        push_wrapped(out, &msg.content, width, "", theme::base());
        return;
    }
    for part in &msg.parts {
        match part {
            MessagePart::Text(text) => push_wrapped(out, text, width, "", theme::base()),
            // T3：一个 Tool part = 一张卡（序列折叠与三态都在 tool_card）。
            MessagePart::Tool(tc) => {
                out.extend(super::tool_card::lines(tc, app.tools_expanded(), width));
            }
        }
    }
}

/// 一段文本 → 若干行：首行带 `prefix`，续行补同样宽的空白；样式统一。
/// （`pub(super)`：`tool_card` 的标题/结果区复用同一断行与清洗。）
pub(super) fn push_wrapped(
    out: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    prefix: &str,
    style: Style,
) {
    let clean = sanitize(text);
    let indent = " ".repeat(UnicodeWidthStr::width(prefix));
    let avail = width.saturating_sub(UnicodeWidthStr::width(prefix)).max(1);
    let head = prefix;
    let mut line = 0usize;
    wrap_with(&clean, avail, |seg| {
        // 首行前缀 / 续行空白由调用次序决定（第 0 段 = 首行）。
        let head = if line == 0 { head } else { indent.as_str() };
        line += 1;
        out.push(Line::from(Span::styled(format!("{head}{seg}"), style)));
    });
}

/// 一段文本 → 行数（[`push_wrapped`] 的计数侧，断行核心同一份——窗口化
/// 第一遍只数不物化）。`pub(super)`：`tool_card` 的标题/结果区计数复用。
pub(super) fn count_wrapped(text: &str, width: usize, prefix: &str) -> usize {
    let clean = sanitize(text);
    let avail = width.saturating_sub(UnicodeWidthStr::width(prefix)).max(1);
    let mut rows = 0usize;
    wrap_with(&clean, avail, |_| rows += 1);
    rows
}

/// 断行核心：按空白贪心断行、超宽单词按列宽硬切，逐段回调（不持有段、
/// 不物化 `Line`——计数侧与物化侧共用这一份，杜绝两边算法漂移）。
///
/// **宽度一律按显示格宽，不按字符数**（T23）。按 `chars().count()` 断行
/// 会把一个 CJK 字或 emoji 算成 1 格，而它在终端里占 2 格，于是渲出来的
/// 行宽翻倍、`count_wrapped` 数出的行数与画面对不上，`ScrollModel::
/// max_offset` / `view_top` 整体错位。组合符宽度为 0，归 0 格。
fn wrap_with(text: &str, width: usize, mut f: impl FnMut(&str)) {
    let width = width.max(1);
    for raw in text.split('\n') {
        let mut line = String::new();
        for word in raw.split(' ') {
            if !line.is_empty()
                && UnicodeWidthStr::width(line.as_str()) + 1 + UnicodeWidthStr::width(word) <= width
            {
                line.push(' ');
                line.push_str(word);
                continue;
            }
            if !line.is_empty() {
                let taken = std::mem::take(&mut line);
                f(&taken);
            }
            let mut rest = word;
            while UnicodeWidthStr::width(rest) > width {
                let cut = cell_boundary(rest, width);
                // 切点必在字符边界（`cell_boundary` 保证），因此
                // `rest[..cut]` 不会劈开多字节 UTF-8 序列。零宽字符
                // 独占（`cut == 0`）时整词让出，避免死循环。
                if cut == 0 {
                    break;
                }
                f(&rest[..cut]);
                rest = &rest[cut..];
            }
            line.push_str(rest);
        }
        f(&line);
    }
}

/// 从 `s` 开头切出不超过 `cells` 个显示格的那一段，返回**字符边界**上的
/// 字节偏移。跨格字符（占 2 格）只在完整放得下时才被切进来，所以返回的
/// 切片宽度 ≤ `cells` 且不碎掉任何码位。首字符本身就超宽时返回 0，
/// 调用方需保证 `width >= 1` 且据此退出。
fn cell_boundary(s: &str, cells: usize) -> usize {
    let mut used = 0usize;
    for (offset, ch) in s.char_indices() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0) as usize;
        if used + w > cells {
            return offset;
        }
        used += w;
    }
    s.len()
}

/// 滤 C0 控制符：制表并为空格、其余（含 ESC 字节）丢掉，保留换行。
/// 按字符过滤不会劈开 UTF-8 多字节序列；控制字节进 cell 会被后端原样
/// 写回终端，必须在渲染前掐掉（同 `linear.rs` 的零 ESC 契约）。
pub(super) fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_ascii_control() || *c == '\n')
        .collect()
}
