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

use std::cell::RefCell;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use web_state::types::{ChatMessage, MessagePart};

use crate::app::App;
use crate::theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::fold::THINKING_PART;
use super::hit::{HitAction, HitMap};

/// 行内命中标记：物化时记下「本消息第 `row` 行是某卡头/思考头/stub」，
/// `window` 把它换算成屏幕坐标推进 [`HitMap`]——行号只可能来自物化侧
/// （count 侧没有屏幕坐标），命中与绘制天然同帧同序。
struct HeaderMark {
    /// 在本消息产出行内的 0-based 行号。
    row: usize,
    /// 命中动作。
    action: HitAction,
}

/// `push_message` 的产物：物化行 + 行内命中标记（扁平 `Vec<Line>` 丢了「哪
/// 行是卡头」的信息，单独回传标记而不是往 `Line` 上打洞）。
struct MessageOut {
    /// 物化行。
    lines: Vec<Line<'static>>,
    /// 命中标记（row 相对 `lines` 下标）。
    marks: Vec<HeaderMark>,
}

/// 渲染 transcript 到 `area`：窗口化——只物化可见窗口（route §3 T6 契约
/// 种子），历史行不重复物化。`hit` 由 [`ui::draw`] 在每帧 `clear` 后传入，
/// 本帧画到屏上的卡头/思考头/stub 行推进命中表。
pub fn render(frame: &mut Frame, area: Rect, app: &App, hit: &RefCell<HitMap>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width.max(1);
    let height = area.height as usize;
    let total = total_lines(app, width);
    let top = app.viewport().view_top(total, height);
    let (lines, marks) = window(app, width as usize, top, height);
    frame.render_widget(Paragraph::new(lines), area);
    // 命中登记：物化行内相对行号 → 屏幕绝对行 = area.y + (相对行 - 视口top)。
    // 只登记落在本帧窗口内的行（`window` 已裁过，marks 与 lines 同界）。
    for mark in marks {
        let y = area.y + mark.row as u16;
        hit.borrow_mut().push(area.x, y, area.width, 1, mark.action);
    }
}

/// 内容总行数（第一遍：**只计数不物化**——与 [`window`] 共用断行核心，
/// 分叉由 `tests/scroll_follow.rs::window_render_matches_line_count` 钉）。
/// `fold` 查询走 [`App::fold_for`]（per-part 三态，Tab 全局开关作默认）。
pub(super) fn total_lines(app: &App, width: u16) -> usize {
    let width = width.max(1) as usize;
    app.messages()
        .iter()
        .enumerate()
        .map(|(msg_idx, msg)| message_rows(app, msg, msg_idx, width))
        .sum()
}

/// 一条消息的行数（与 [`push_message`] 逐分支同源，分叉即测试红）。行数的
/// **单一出处**：渲染侧 [`total_lines`] / [`window`] 与检索侧
/// [`crate::ui::search_overlay::line_offset`] 同吃本函数，两边不可能各数各的。
/// T21：思考块行数先经 [`super::thinking::rows`]（在文本/工具 part 之前，
/// 与 [`push_message`] 物化侧同序）。`fold` 查询走 `app.fold_for`，per-part
/// override 覆盖 `tools_expanded` 默认值——计数与物化共用同一查询，对偶不破。
pub(super) fn message_rows(app: &App, msg: &ChatMessage, msg_idx: usize, width: usize) -> usize {
    if msg.role == "user" {
        return count_wrapped(&msg.content, width, "❯ ");
    }
    // T21：思考块（reasoning）在文本/工具 part 之前出——parts 空与非空
    // 两个分支都算。历史回填消息 reasoning 为空，`thinking::rows` 对它
    // 归 0 是零虚构契约（不出裸卡头）。
    let mut rows =
        super::thinking::rows(&msg.reasoning, app.fold_for(msg_idx, THINKING_PART), width);
    if msg.parts.is_empty() {
        rows += count_wrapped(&msg.content, width, "");
        return rows;
    }
    rows += msg
        .parts
        .iter()
        .enumerate()
        .map(|(part_idx, part)| match part {
            // T26：assistant 正文走 markdown（标题/列表/围栏代码块/引用块）。
            MessagePart::Text(text) => super::markdown::rows(text, width),
            MessagePart::Tool(tc) => {
                super::tool_card::rows(tc, app.fold_for(msg_idx, part_idx), width)
            }
        })
        .sum::<usize>();
    rows
}

/// 可见窗口的行 + 命中标记（第二遍）：只物化与 `[top, top + height)` 相交的
/// 消息，再切片到窗口——其上（更旧）与其下（更新）的消息一行为都不生成。
/// `marks` 的 `row` 已换算成**窗口内**相对行号（`render` 再加 `area.y` 得
/// 屏幕绝对行）。
fn window(
    app: &App,
    width: usize,
    top: usize,
    height: usize,
) -> (Vec<Line<'static>>, Vec<HeaderMark>) {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(height);
    let mut marks: Vec<HeaderMark> = Vec::new();
    let mut start = 0usize;
    for (msg_idx, msg) in app.messages().iter().enumerate() {
        let rows = message_rows(app, msg, msg_idx, width);
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
            let mut buf = MessageOut {
                lines: Vec::with_capacity(rows),
                marks: Vec::new(),
            };
            push_message(&mut buf, app, msg, msg_idx, width);
            let from = top.saturating_sub(start);
            let to = (top + height).min(end) - start;
            // 命中标记随切片同步偏移：原 row（消息内）→ 窗口内相对行。
            for mark in buf.marks.drain(..) {
                if mark.row >= from && mark.row < to {
                    marks.push(HeaderMark {
                        row: out.len() + (mark.row - from),
                        action: mark.action,
                    });
                }
            }
            out.extend(buf.lines.into_iter().skip(from).take(to - from));
        }
        start = end;
        if out.len() >= height {
            break;
        }
    }
    (out, marks)
}

/// 一条消息 → 物化行 + 命中标记（与 [`message_rows`] 同一数据路径）。
fn push_message(out: &mut MessageOut, app: &App, msg: &ChatMessage, msg_idx: usize, width: usize) {
    if msg.role == "user" {
        push_wrapped(
            &mut out.lines,
            &msg.content,
            width,
            "❯ ",
            theme::brand_bold(),
        );
        return;
    }
    // T21：思考块在文本/工具 part 之前（与 message_rows 逐分支同序——计数
    // 侧与物化侧分叉即测试红）。命中标记记卡头/stub 那行（行号 = 当前长度）。
    let think = app.fold_for(msg_idx, THINKING_PART);
    if !msg.reasoning.trim().is_empty() {
        out.marks.push(HeaderMark {
            row: out.lines.len(),
            action: HitAction::ToggleThinking { msg_idx },
        });
    }
    out.lines
        .extend(super::thinking::lines(&msg.reasoning, think, width));
    if msg.parts.is_empty() {
        push_wrapped(&mut out.lines, &msg.content, width, "", theme::base());
        return;
    }
    for (part_idx, part) in msg.parts.iter().enumerate() {
        match part {
            // T26：与 message_rows 同分支同源——assistant 正文走 markdown，
            // 用户消息与历史回填（parts 为空）保持纯文本字面量。
            MessagePart::Text(text) => out.lines.extend(super::markdown::lines(text, width)),
            // T3：一个 Tool part = 一张卡（序列折叠与三态都在 tool_card）。
            // 命中标记记卡头/stub 那行。
            MessagePart::Tool(tc) => {
                out.marks.push(HeaderMark {
                    row: out.lines.len(),
                    action: HitAction::ToggleToolCard { msg_idx, part_idx },
                });
                out.lines.extend(super::tool_card::lines(
                    tc,
                    app.fold_for(msg_idx, part_idx),
                    width,
                ));
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
