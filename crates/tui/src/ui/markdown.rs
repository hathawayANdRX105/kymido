//! ui/markdown — assistant 正文（`MessagePart::Text`）的 markdown 渲染（T26）。
//!
//! 解析走 `pulldown-cmark`，产出**段列表**；[`rows`]（计数侧）与 [`lines`]
//! （物化侧）对**同一份**段列表各折叠一次，断行复用 [`transcript`] 的
//! `count_wrapped` / `push_wrapped` 同一核心。count == render 因此是结构性的：
//! 两侧不可能各走各的算法，分叉只剩"段列表两次构建不一致"这一种可能。
//!
//! **流式半截输入是常态而非异常**：assistant 正文逐 delta 累加、每帧重算，
//! `text` 经常是未闭合的代码围栏、半截的 `**`、写到一半的列表。pulldown 对
//! 这些形态的产出是**确定的**（未闭合围栏 = 代码块到 EOF；未闭合 `**` 按
//! 字面量走 `Event::Text`），所以每种半截形态都有确定行数——不 panic、不丢
//! 内容、不等它写完。`markdown_handles_unterminated_fence` /
//! `markdown_handles_unterminated_emphasis` 各钉一条。
//!
//! **代码块有行数 / 字节双上限**：超限截断并显式提示，不许把整屏刷成一个
//! 巨型代码块（grok `EDIT_HL_MAX_LINES` / `EDIT_HL_MAX_BYTES` 同思路）。
//!
//! **表格不假装对齐**（主控裁决⑨）：本批不开 `ENABLE_TABLES`，表格语法
//! 以纯文本穿过，列宽不保证——宁可丑，不做半吊子的假对齐。
//!
//! **用户消息不走这里**（`transcript.rs` 的 user 早返回分支保持不变）：
//! 用户输入的 `*` 和 `_` 是字面量，不是格式。
//!
//! **不做**：脚注 / 任务列表 / 定义列表等一切 pulldown 扩展（不开对应
//! option，一律当普通段落文本）；语法高亮（G11，独立立项）；OSC-8 链接
//! （URL 丢弃，只留文字）。

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

use super::transcript::{count_wrapped, push_wrapped};

/// 代码块行数上限。
const CODE_MAX_LINES: usize = 200;
/// 代码块字节上限。
const CODE_MAX_BYTES: usize = 16 * 1024;
/// 代码块超限提示（显式，不静默截断）。
const CODE_TRUNCATED: &str = "… code block truncated";

/// 一个逻辑段：断行前的一条「源文本 + 首行前缀 + 样式」。
struct Seg {
    text: String,
    prefix: &'static str,
    style: Style,
    /// 分隔线：不参与断行，`rows` 记 1 行，`lines` 画一行占满列宽的 `─`。
    rule: bool,
}

impl Seg {
    fn new(text: impl Into<String>, prefix: &'static str, style: Style) -> Self {
        Self {
            text: text.into(),
            prefix,
            style,
            rule: false,
        }
    }

    fn rule() -> Self {
        Self {
            text: String::new(),
            prefix: "",
            style: theme::dim(),
            rule: true,
        }
    }
}

/// markdown 源文本 → 段列表。[`rows`] / [`lines`] 唯一的数据来源。
///
/// 空文本 / 纯空白 → 空段列表（两侧都归 0，不凭空出行）。
fn segments(text: &str) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    // 当前块累积器：`Start(BlockQuote)` / `Start(Item)` 先把带前缀与样式的
    // 空段放进来，后续 `Event::Text` 直接往里追加——文本事件拿不到块上下文，
    // 靠预播种解决。块外的裸 Text（pulldown 不产，防御）落普通段落，不丢。
    let mut para: Option<Seg> = None;
    // 列表深度与有序列表当前编号。
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    // 是否处于代码块内（代码文本另收，吃双上限）。
    let mut in_code = false;
    let mut code = String::new();
    // 标题层级，`Start(Heading)` 记、`End(Heading)` 用。
    let mut heading_level: Option<u8> = None;

    let flush_para = |out: &mut Vec<Seg>, para: &mut Option<Seg>| {
        if let Some(seg) = para.take() {
            if !seg.text.trim().is_empty() {
                out.push(seg);
            }
        }
    };

    for event in Parser::new(text) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    flush_para(&mut out, &mut para);
                    heading_level = Some(heading_depth(level));
                }
                Tag::BlockQuote(_) => {
                    flush_para(&mut out, &mut para);
                    // 引用块整段走 `│ ` 前缀 + dim（左条形态）。
                    para = Some(Seg::new(String::new(), "│ ", theme::dim()));
                }
                Tag::List(start) => {
                    flush_para(&mut out, &mut para);
                    list_stack.push(start);
                }
                Tag::Item => {
                    flush_para(&mut out, &mut para);
                    let mut text = String::new();
                    // 缩进随深度增长（每层 2 格），标记随后。
                    for _ in 1..list_stack.len() {
                        text.push_str("  ");
                    }
                    match list_stack.last_mut() {
                        // 有序列表：编号进文本，编号本身不占 `&'static`。
                        Some(Some(n)) => {
                            text.push_str(&n.to_string());
                            text.push_str(". ");
                            *n += 1;
                        }
                        _ => text.push_str("- "),
                    }
                    para = Some(Seg::new(text, "", theme::dim()));
                }
                Tag::CodeBlock(_) => {
                    flush_para(&mut out, &mut para);
                    in_code = true;
                    code.clear();
                }
                Tag::Link { .. } => {
                    // 链接只保留文本，URL 丢弃（本批不做 OSC-8）。
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    if let Some(level) = heading_level.take() {
                        if let Some(mut seg) = para.take() {
                            let hashes: &'static str = match level {
                                1 => "# ",
                                2 => "## ",
                                3 => "### ",
                                4 => "#### ",
                                5 => "##### ",
                                _ => "###### ",
                            };
                            seg.text.insert_str(0, hashes);
                            seg.prefix = "";
                            // 标题加粗由角色色 + 字重共同承担。
                            seg.style = theme::heading().add_modifier(Modifier::BOLD);
                            if !seg.text.trim().is_empty() {
                                out.push(seg);
                            }
                        }
                    }
                }
                TagEnd::Paragraph | TagEnd::BlockQuote(_) | TagEnd::Item => {
                    flush_para(&mut out, &mut para);
                }
                TagEnd::List(_) => {
                    flush_para(&mut out, &mut para);
                    list_stack.pop();
                }
                TagEnd::CodeBlock => {
                    in_code = false;
                    out.push(code_segment(&code));
                }
                _ => {}
            },
            Event::Text(t) => {
                if in_code {
                    code.push_str(&t);
                } else {
                    let seg = para.get_or_insert_with(|| Seg::new("", "", theme::base()));
                    seg.text.push_str(&t);
                }
            }
            Event::Code(c) => {
                // 行内 code 反引号包裹。
                let seg = para.get_or_insert_with(|| Seg::new("", "", theme::base()));
                seg.text.push('`');
                seg.text.push_str(&c);
                seg.text.push('`');
            }
            Event::SoftBreak => {
                if let Some(seg) = para.as_mut() {
                    seg.text.push(' ');
                }
            }
            Event::HardBreak => {
                if let Some(seg) = para.as_mut() {
                    seg.text.push('\n');
                }
            }
            Event::Rule => {
                flush_para(&mut out, &mut para);
                out.push(Seg::rule());
            }
            // HTML 按文本原样显示（终端不执行，也不会假装渲染过）。
            Event::Html(h) | Event::InlineHtml(h) => {
                let seg = para.get_or_insert_with(|| Seg::new("", "", theme::base()));
                seg.text.push_str(&h);
            }
            _ => {}
        }
    }
    flush_para(&mut out, &mut para);
    out
}

/// 标题层级 → `#` 个数（`HeadingLevel` 无直接数值接口）。
fn heading_depth(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// 代码块 → 段：`▌ ` 前缀 + `code_fg` 样式，吃行数 / 字节双上限。
///
/// 截断发生在**段构造**阶段，计数侧与物化侧看到的是同一份（可能已截断的）
/// 文本——上限不会造成两侧分叉。
fn code_segment(code: &str) -> Seg {
    let mut body: Vec<&str> = code.lines().collect();
    let mut truncated = false;
    if body.len() > CODE_MAX_LINES {
        body.truncate(CODE_MAX_LINES);
        truncated = true;
    }
    let mut text = body.join("\n");
    if text.len() > CODE_MAX_BYTES {
        // 按字节切会劈开 UTF-8：回落到字符边界。
        let mut cut = CODE_MAX_BYTES;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        truncated = true;
    }
    if truncated {
        text.push('\n');
        text.push_str(CODE_TRUNCATED);
    }
    Seg::new(text, "▌ ", theme::code_fg())
}

/// markdown 源文本 → 行数（计数侧，`message_rows` 的对偶）。
pub fn rows(text: &str, width: usize) -> usize {
    segments(text)
        .iter()
        .map(|seg| {
            if seg.rule {
                1
            } else {
                count_wrapped(&seg.text, width, seg.prefix)
            }
        })
        .sum()
}

/// markdown 源文本 → 行（物化侧，`push_message` 的对偶）。
pub fn lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for seg in segments(text) {
        if seg.rule {
            out.push(Line::from(Span::styled(
                "─".repeat(width.max(1)),
                seg.style,
            )));
        } else {
            push_wrapped(&mut out, &seg.text, width, seg.prefix, seg.style);
        }
    }
    out
}
