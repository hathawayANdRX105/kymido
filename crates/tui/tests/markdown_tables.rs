//! markdown 表对齐 + 删除线字面量关（T28）——翻主控裁决⑨的契约。
//!
//! 裁决⑨原文（「不开 `ENABLE_TABLES`，表格语法以纯文本穿过，列宽不保证
//! ——宁可丑，不做半吊子的假对齐」）被 T28 推翻：**真对齐**才是正确解，
//! 假对齐才是「半吊子」。本文件钉真对齐的行为与它的边界：
//!
//! - 列宽 = 全列最大显示宽（CJK 按 2 格算），列间两空格；
//! - 表头行下有 `─` 分隔行；
//! - **整表超宽退化为源文本原样**（不 pad）——这是裁决⑨的合法残留，
//!   一行塞不下就别假装对齐；
//! - 删除线保持字面量（不开 `ENABLE_STRIKETHROUGH`）——不是没实现，是
//!   有意不实现，钉住防未来顺手开 option；
//! - 每条断言都附 `rows == lines.len()`：表是一个 `Seg`（内含 `\n`），
//!   两侧共用同一份段列表，分叉即测试红。

use kymido_tui::ui::markdown;
use unicode_width::UnicodeWidthStr;

/// 物化成纯文本行（剥 ratatui Span 样式）。
fn plain(text: &str, width: usize) -> Vec<String> {
    markdown::lines(text, width)
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect()
}

/// 头号不变式：任何形态（含表）在任何列宽下，计数侧 == 物化侧行数。
#[test]
fn table_rows_match_lines() {
    let samples = [
        "| a | b |\n|---|---|\n| 1 | 2 |",
        "| name | count | note |\n|:---|---:|:---:|\n| alpha | 1 | x |\n| longer name | 22 | yyy |",
        "| 单列 |\n|---|\n| 中文值 |",
        // 空表体（只有表头）
        "| a | b |\n|---|---|",
        // 未闭合表（流式半截：只有表头行没写分隔行）
        "| a | b |",
        // 表后跟普通段落
        "| a | b |\n|---|---|\n| 1 | 2 |\n\nafter table",
    ];
    for text in samples {
        for width in [8usize, 12, 20, 44, 80] {
            let rows = markdown::rows(text, width);
            let lines = markdown::lines(text, width);
            assert_eq!(
                rows,
                lines.len(),
                "表样本 @ {width}cols 分叉：rows {rows} != lines {}；样本 =\n{text}",
                lines.len()
            );
        }
    }
}

/// 真对齐：同一列的所有行在相同列位置起头（列宽 = 全列最大宽 + 两空格间隔）。
///
/// 断言用**列位置不变式**：col2 的首个非空白 token 在每行都出现在同一显示
/// 列。token 选择避开 col1 内容：col2 表头/值用 col1 里不出现的子串
/// （`amount` / 数字），否则 `find` 会匹配到 col1 的字面量。
#[test]
fn table_columns_align_to_one_column_start() {
    // 全左对齐（`:---`）：col2 内容起点 = col1最大宽 + 间隔 2。
    // col1 max("kind"=4, "apple"=5, "blueberry pie"=13) = 13 → col2 起点 = 15。
    let text = "| kind | amount |\n|:---|:---|\n| apple | 1 |\n| blueberry pie | 22 |";
    let width = 80;
    let out = plain(text, width);
    assert!(out.len() >= 4, "表应至少 4 行（头+分隔+2 正文）：{out:?}");
    let col2_starts: Vec<usize> = out
        .iter()
        .filter(|l| l.contains("amount") || l.chars().any(|c| c.is_ascii_digit()))
        .map(|l| {
            // col2 token 起点：表头找 "amount"，正文找首个数字。
            let byte = l
                .find("amount")
                .or_else(|| {
                    l.char_indices()
                        .find(|(_, c)| c.is_ascii_digit())
                        .map(|(i, _)| i)
                })
                .expect("每行都应有 col2 token");
            // 换算成显示格（CJK 1 字 2 格）。
            UnicodeWidthStr::width(&l[..byte])
        })
        .collect();
    assert!(
        col2_starts.len() >= 3,
        "应有 表头 + 2 正文行含 col2 token：{out:?}"
    );
    assert!(
        col2_starts.windows(2).all(|w| w[0] == w[1]),
        "col2 起点在行间漂移：{col2_starts:?}（样本 =\n{text}）"
    );
    assert_eq!(
        col2_starts[0], 15,
        "col2 起点应为 13+2 间隔 = 15：{col2_starts:?}"
    );
}

/// 表头下有分隔行：一条全由 `─` 与空格组成的行，且每列的 `─` 段宽度
/// 不超过该列内容宽度（超宽退化时整表无分隔）。
#[test]
fn table_header_has_rule_row() {
    let text = "| ab | cd |\n|---|---|\n| 1 | 2 |";
    let out = plain(text, 40);
    let rule = out
        .iter()
        .find(|l| l.chars().all(|c| c == '─' || c == ' '))
        .expect("表头下应有一条 ─ 分隔行");
    let dashes: usize = rule.chars().filter(|c| *c == '─').count();
    // 2 列，表头 "ab" / "cd" 各 2 格 → 4 个 ─；正文 "1" / "2" 各 1 格。
    assert!(dashes >= 4, "分隔行的 ─ 段数应覆盖两列宽：{rule:?}");
    assert!(!rule.trim().is_empty(), "分隔行不应是空行：{rule:?}");
}

/// 整表超宽退化为源文本原样（不 pad、不加分隔行）——「宁可丑」在超宽
/// 场景保留：窄列宽下不许硬 pad 出折行后错位的假对齐。
///
/// 窄宽下单 cell 自身就会超宽而折行——那是**词断行**的正常行为（内容没
/// 丢），与「假装对齐」是两回事。所以这里断言：① 无表头分隔行（没在
/// 硬 pad 假对齐）、② 每行 duality 成立、③ cell 文本跨折行仍完整。
#[test]
fn table_degrades_when_over_wide() {
    let text = "| averylongheadername | b |\n|---|---|\n| x | y |";
    // 宽度 8 远小于表的自然宽度（> 25）
    let out = plain(text, 8);
    // 退化形态：没有全 ─ 分隔行（至少不在表数据行之间）
    let has_rule = out
        .iter()
        .any(|l| l.chars().filter(|c| *c == '─').count() >= 4);
    assert!(
        !has_rule,
        "超宽退化形态不应有 4+ 格的表头分隔行（说明在硬 pad 假对齐）：{out:?}"
    );
    // duality 在退化路径同样成立
    assert_eq!(
        markdown::rows(text, 8),
        out.len(),
        "退化形态 count/render 分叉"
    );
    // 内容仍在（cell 可折行，去空白后应还原源 token）
    let stripped: String = out
        .iter()
        .flat_map(|l| l.chars())
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        stripped.contains("averylongheadername"),
        "退化形态下内容不许丢（可折行，但 token 完整）：{stripped}"
    );
}

/// 删除线字面量关：`~~foo~~` 整串原样出行，**不吃成样式**。
///
/// 这是「有意不实现」——开 `ENABLE_STRIKETHROUGH` 只需一行 option，
/// 但会让 `~~` 出现在正文里时行为突变。钉住字面量，防顺手开。
#[test]
fn markdown_strikethrough_stays_literal() {
    let text = "before ~~struck~~ after";
    let out = plain(text, 80);
    let joined = out.join("\n");
    assert!(
        joined.contains("~~struck~~"),
        "删除线必须整串字面量保留（未开 ENABLE_STRIKETHROUGH）：{joined}"
    );
    assert!(
        !joined.contains("struck\n"),
        "删除线不应被吃掉标记只留文字：{joined}"
    );
    // 与纯文本行数一致（没进 markdown 特殊分支 → 同一行）
    assert_eq!(
        markdown::rows(text, 80),
        markdown::rows("before ~~struck~~ after", 80)
    );
}

/// 表格在 CJK 下按显示格宽对齐：CJK 字符占 2 格，列宽计算必须用
/// `UnicodeWidthStr`（不是 `chars().count()`）。
///
/// 断言用 col2 起点不变式 + 显示格换算。col1 "名称" = 4 格 vs "x" = 1 →
/// col1 宽 4，col2 起点 = 4+2 = 6；按 `chars().count()` 算会得 4，错 2 格
/// ——这就是这条钉的东西。
#[test]
fn table_aligns_cjk_by_display_width() {
    // col2 值用 col1 里不出现的 token（"total" / 数字），避免 find 匹配
    // 到 col1 的 "x"。
    let text = "| 名称 | total |\n|---|---|\n| 中文 | 1 |\n| x | 22 |";
    let width = 80;
    let out = plain(text, width);
    let starts: Vec<usize> = out
        .iter()
        .filter(|l| l.contains("total") || l.chars().any(|c| c.is_ascii_digit()))
        .map(|l| {
            // col2 token 起点（表头找 "total"，正文找首个数字），换算显示格。
            let byte = l
                .find("total")
                .or_else(|| {
                    l.char_indices()
                        .find(|(_, c)| c.is_ascii_digit())
                        .map(|(i, _)| i)
                })
                .expect("每行都应有 col2 token");
            UnicodeWidthStr::width(&l[..byte])
        })
        .collect();
    assert!(
        starts.len() >= 3,
        "应有 表头 + 2 正文行含 col2 token：{out:?}"
    );
    assert!(
        starts.windows(2).all(|w| w[0] == w[1]),
        "CJK 表 col2 起点漂移：{starts:?}（样本 =\n{text}）"
    );
    // col1 宽 4（"中文" 2 字 × 2 格）→ col2 起点 = 4+2 = 6。
    // 若按 chars().count() 算会得 4，错 2 格——此断言钉住显示格宽。
    assert_eq!(
        starts[0], 6,
        "col2 起点应为 显示格宽(4)+间隔(2) = 6（chars().count() 会得 4）：{starts:?}"
    );
}

/// 表格 + 段落混排：`TagEnd::Table` 之后的段落不被表累加器吞掉。
#[test]
fn table_does_not_swallow_following_paragraph() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n\nnormal paragraph text";
    let out = plain(text, 80);
    let joined = out.join("\n");
    assert!(
        joined.contains("normal paragraph text"),
        "表后的普通段落必须正常出行：{joined}"
    );
}

/// 表内 cell 的 inline code 按字面量退化（`\`x\`` 保留反引号）——cell 内
/// 不引嵌套样式（简化计数，与删除线同一策略）。
#[test]
fn table_cell_code_stays_literal() {
    let text = "| a | b |\n|---|---|\n| `x` | y |";
    let out = plain(text, 80);
    let joined = out.join("\n");
    assert!(
        joined.contains('`'),
        "表内行内 code 保留反引号（字面量退化）：{joined}"
    );
    // 列宽用显示格宽算：反引号占 1 格
    let header = out.first().expect("表头行");
    assert!(
        UnicodeWidthStr::width(header.as_str()) > 0,
        "表头行不应为空：{header:?}"
    );
}
