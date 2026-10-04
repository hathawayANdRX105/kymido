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
//! **表格真对齐**（翻主控裁决⑨，T28）：开 `ENABLE_TABLES`，按列最大显示宽
//! pad、表头下加 `─` 分隔行。裁决⑨反对的是「假对齐」（纯文本穿过、列宽不
//! 顾），本批做真对齐故裁决失效；仅**整表超宽**时退化为源文本原样（一行塞不
//! 下就别假装对齐）——这是裁决⑨的合法残留。
//!
//! **删除线字面量关**（T28）：不开 `ENABLE_STRIKETHROUGH`，`~~x~~` 整串按
//! `Event::Text` 字面量穿行（pulldown 0.13 实测：`Options::empty()` 下就是
//! 如此）。`markdown_strikethrough_stays_literal` 钉这条，防未来顺手开 option。
//!
//! **用户消息不走这里**（`transcript.rs` 的 user 早返回分支保持不变）：
//! 用户输入的 `*` 和 `_` 是字面量，不是格式。
//!
//! **不做**：脚注 / 任务列表 / 定义列表等一切 pulldown 扩展（不开对应
//! option，一律当普通段落文本）；语法高亮（G11，独立立项）；OSC-8 链接
//! （URL 丢弃，只留文字——`theme_lint` D11 禁 `src/` 任何 ESC 字节，且
//! ratatui `set_stringn` 过滤控制符，OSC-8 走 Span 路径必被丢弃；要做得
//! 另立议题：放开 D11 + 绘制后原始发射器 + 终端能力检测）。

use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;

use super::transcript::{count_wrapped, push_wrapped};

/// 代码块行数上限。
const CODE_MAX_LINES: usize = 200;
/// 代码块字节上限。
const CODE_MAX_BYTES: usize = 16 * 1024;
/// 代码块超限提示（显式，不静默截断）。
const CODE_TRUNCATED: &str = "… code block truncated";
/// 表列间隔（无竖线，与 grok pager 同取向）。
const TABLE_GAP: &str = "  ";
/// 表头分隔行填充（`─` U+2500）。
const TABLE_RULE: char = '─';

/// 表格累加器（翻裁决⑨，T28）。表内 cell 按行收集，`TagEnd::Table` 时
/// 定列宽、pad 成一个 `Seg`（含表头分隔行）。
struct TableAcc {
    /// 每列对齐（pulldown `Alignment`，`None` 当左对齐）。
    aligns: Vec<Alignment>,
    /// 全部行 × 列文本（`rows[r][c]`）。行数可以为 0（空表）。
    rows: Vec<Vec<String>>,
    /// 当前正在拼的单元格。
    cell: String,
    /// 当前行（正在拼）。
    row: Vec<String>,
}

impl TableAcc {
    fn new(aligns: Vec<Alignment>) -> Self {
        Self {
            aligns,
            rows: Vec::new(),
            cell: String::new(),
            row: Vec::new(),
        }
    }
}

/// 表 → 段：真对齐（列宽 = 全列最大显示宽，按对齐 pad，列间两空格），
/// 表头行下插 `─` 分隔行。
///
/// **整表超宽退化**：列宽合计 + 间隔超出 `width` 时整表按源文本原样出行
/// （不 pad）——一行塞不下就别假装对齐（裁决⑨的合法残留）。退化形态与
/// 「不开表」时的纯文本不可区分是**有意的**：不做半吊子。
///
/// 产出 `raw = true` 的段：**预格式化**，两侧只做 `\n` 分行、不走词断行
/// ——pad 出的对齐空白若再过 `wrap_with`（按空格贪心的词断行核心）会被
/// 折回单列，列对齐原地蒸发。退化形态返回普通段（文本是啥宽度未知，仍
/// 需要断行兜底）。
fn table_segment(acc: TableAcc, width: usize) -> Seg {
    let n_cols = acc.rows.iter().map(Vec::len).max().unwrap_or(0);
    if n_cols == 0 {
        return Seg::new("", "", theme::base());
    }
    let mut widths = vec![0usize; n_cols];
    for row in &acc.rows {
        for (c, cell) in row.iter().enumerate() {
            widths[c] = widths[c].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    let gap = UnicodeWidthStr::width(TABLE_GAP);
    let total: usize = widths.iter().sum::<usize>() + gap * n_cols.saturating_sub(1);
    if width == 0 || total > width {
        // 超宽：逐行原样（尾部空格 trim，来源无空格时不会凭空多列）。
        let text = acc
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| c.trim_end())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Seg::new(text, "", theme::base());
    }
    let pad = |cell: &str, col: usize| -> String {
        let w = widths[col];
        let cw = UnicodeWidthStr::width(cell);
        let (left, right) = match acc.aligns.get(col).copied().unwrap_or(Alignment::None) {
            Alignment::Right => (w.saturating_sub(cw), 0),
            Alignment::Center => {
                let total_pad = w.saturating_sub(cw);
                (total_pad / 2, total_pad - total_pad / 2)
            }
            _ => (0, w.saturating_sub(cw)),
        };
        format!("{}{}{}", " ".repeat(left), cell, " ".repeat(right))
    };
    let mut lines: Vec<String> = Vec::with_capacity(acc.rows.len() + 1);
    for (r, row) in acc.rows.iter().enumerate() {
        let cells: Vec<String> = (0..n_cols)
            .map(|c| pad(row.get(c).map(String::as_str).unwrap_or(""), c))
            .collect();
        lines.push(cells.join(TABLE_GAP));
        if r == 0 {
            // 表头下分隔行：每列列宽个 `─`，列间同宽间隔。
            lines.push(
                (0..n_cols)
                    .map(|c| TABLE_RULE.to_string().repeat(widths[c]))
                    .collect::<Vec<_>>()
                    .join(TABLE_GAP),
            );
        }
    }
    Seg::raw(lines.join("\n"), theme::base())
}

/// 一个逻辑段：断行前的一条「源文本 + 首行前缀 + 样式」。
struct Seg {
    text: String,
    prefix: &'static str,
    style: Style,
    /// 分隔线：不参与断行，`rows` 记 1 行，`lines` 画一行占满列宽的 `─`。
    rule: bool,
    /// 预格式化（表格对齐产物）：只做 `\n` 分行，不走词断行——pad 出的
    /// 对齐空白再过 `wrap_with` 会被折回单列（`markdown_tables.rs` 钉）。
    /// 与 `rule` 互斥（对齐产物不可能同时是分隔线）。
    raw: bool,
}

impl Seg {
    fn new(text: impl Into<String>, prefix: &'static str, style: Style) -> Self {
        Self {
            text: text.into(),
            prefix,
            style,
            rule: false,
            raw: false,
        }
    }

    /// 预格式化段（表格对齐版）：`rows` / `lines` 按 `\n` 原样分行。
    fn raw(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            prefix: "",
            style,
            rule: false,
            raw: true,
        }
    }

    fn rule() -> Self {
        Self {
            text: String::new(),
            prefix: "",
            style: theme::dim(),
            rule: true,
            raw: false,
        }
    }
}

/// markdown 源文本 → 段列表。[`rows`] / [`lines`] 唯一的数据来源。
///
/// 空文本 / 纯空白 → 空段列表（两侧都归 0，不凭空出行）。
fn segments(text: &str, width: usize) -> Vec<Seg> {
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
    // 表格累加器（翻裁决⑨）。`None` = 不在表格；`Some` 时收集行 + 列对齐。
    let mut table: Option<TableAcc> = None;

    let flush_para = |out: &mut Vec<Seg>, para: &mut Option<Seg>| {
        if let Some(seg) = para.take()
            && !seg.text.trim().is_empty()
        {
            out.push(seg);
        }
    };

    for event in Parser::new_ext(text, Options::ENABLE_TABLES) {
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
                    // 链接只保留文本，URL 丢弃（不做 OSC-8，见头注）。
                }
                Tag::Table(aligns) => {
                    // 翻裁决⑨：开表，收集行 + 列对齐。首行是 `TableHead`，
                    // 与正文行同样收（`TableHead` 本身是 no-op 分支）。
                    flush_para(&mut out, &mut para);
                    table = Some(TableAcc::new(aligns));
                }
                Tag::TableHead | Tag::TableRow => {
                    if let Some(t) = table.as_mut() {
                        t.row.clear();
                    }
                }
                Tag::TableCell => {
                    if let Some(t) = table.as_mut() {
                        t.cell.clear();
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    if let Some(level) = heading_level.take()
                        && let Some(mut seg) = para.take()
                    {
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
                TagEnd::TableCell => {
                    // 单元格落盘。「表头行还是正文行」由 `TagEnd::TableRow`
                    // 收，cell 文本只进当前行，不关心头部——首行按表头对待是
                    // 渲染层的职责（`table_segment` 按行 0 即表头处理）。
                    if let Some(t) = table.as_mut() {
                        let cell = std::mem::take(&mut t.cell);
                        t.row.push(cell);
                    }
                }
                TagEnd::TableRow | TagEnd::TableHead => {
                    if let Some(t) = table.as_mut() {
                        let row = std::mem::take(&mut t.row);
                        if !row.is_empty() {
                            t.rows.push(row);
                        }
                    }
                }
                TagEnd::Table => {
                    if let Some(acc) = table.take() {
                        let seg = table_segment(acc, width);
                        if !seg.text.trim().is_empty() {
                            out.push(seg);
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if in_code {
                    code.push_str(&t);
                } else if let Some(acc) = table.as_mut() {
                    // 表内文本只进当前 cell（inline 语法按纯文本退化：不开
                    // `Tag::Strong` 等分支，cell 只收 `Event::Text` 与
                    // `Event::Code`——见下面 `Event::Code` 的表内分支）。
                    acc.cell.push_str(&t);
                } else {
                    let seg = para.get_or_insert_with(|| Seg::new("", "", theme::base()));
                    seg.text.push_str(&t);
                }
            }
            Event::Code(c) => {
                if in_code {
                    // 代码块里行内 code 本来的反引号吃掉（pulldown 语义）。
                    code.push_str(&c);
                } else if let Some(acc) = table.as_mut() {
                    // 表内行内 code 同样字面量退化（反引号是 cell 内容的一部分）。
                    acc.cell.push('`');
                    acc.cell.push_str(&c);
                    acc.cell.push('`');
                } else {
                    // 行内 code 反引号包裹。
                    let seg = para.get_or_insert_with(|| Seg::new("", "", theme::base()));
                    seg.text.push('`');
                    seg.text.push_str(&c);
                    seg.text.push('`');
                }
            }
            Event::SoftBreak => {
                // 保留为硬换行，而不是折叠成空格：assistant 正文逐 delta
                // 累加、每帧重算，单个 `\n` 在 markdown 语义里是「段落内
                // 软换行」、要等空行出现段落才算闭合——塌成一行会让行数
                // 在流式期间一直漂移（正是任务书禁止的「等它写完」）。
                // 按硬换行处理，行数从第一个 delta 就稳定，纯文本的视觉
                // 行结构与接 markdown 之前一致。
                if let Some(seg) = para.as_mut() {
                    seg.text.push('\n');
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
    segments(text, width)
        .iter()
        .map(|seg| {
            if seg.rule {
                1
            } else if seg.raw {
                // 预格式化（表对齐）：每 `\n` 一行，不走词断行——pad 出的
                // 对齐空白再过 `wrap_with` 会被折回单列。
                seg.text.split('\n').count()
            } else {
                count_wrapped(&seg.text, width, seg.prefix)
            }
        })
        .sum()
}

/// markdown 源文本 → 行（物化侧，`push_message` 的对偶）。
pub fn lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for seg in segments(text, width) {
        if seg.rule {
            out.push(Line::from(Span::styled(
                "─".repeat(width.max(1)),
                seg.style,
            )));
        } else if seg.raw {
            // 预格式化（表对齐）：按 `\n` 原样分行，断行核心一秒破坏 pad。
            for line in seg.text.split('\n') {
                out.push(Line::from(Span::styled(line.to_string(), seg.style)));
            }
        } else {
            push_wrapped(&mut out, &seg.text, width, seg.prefix, seg.style);
        }
    }
    out
}
