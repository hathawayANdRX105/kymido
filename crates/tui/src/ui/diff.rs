//! ui/diff — edit 类工具结果的统一 diff 渲染（T27）。
//!
//! `diffy` 解析 → ± 装订线 + 双侧行号 + hunk 空隙标记。与 markdown 同一
//! 结构性对偶：先构造**行列表**，[`rows`]（计数侧）与 [`lines`]（物化侧）
//! 对同一份列表各折叠一次，断行走 [`transcript`] 的同一核心——count ==
//! render 不可能各走各的算法。
//!
//! **判据宁严勿宽**：`looks_like_diff` 只认统一 diff 的行首形态
//!（`@@ ` / `--- `+`+++ `）。误判的代价是把普通工具输出画成 diff——
//! 比漏判严重得多，所以判不过就回落纯文本。
//!
//! **解析失败回落纯文本**：头部形态像 diff 但 `diffy` 拒收（畸形 patch、
//! 二进制），一律走纯文本路径——不吞内容、不 panic。
//!
//! **CRLF**：细节先过 `sanitize`，`\r` 作为 ASCII 控制字节被统一掐掉，
//! 行号装订线不会错位。
//!
//! **行数上限**：渲染行数超限截断 + 显式提示，不许几千行 diff 刷屏。

use diffy::{Line as DiffLine, Patch};
use ratatui::style::Style;
use ratatui::text::Line;

use crate::theme;

use super::transcript::{count_wrapped, push_wrapped};

/// 相邻 hunk 之间隔超过这个行数的上下文时插空隙标记。
const GAP_THRESHOLD: usize = 3;
/// 渲染行数上限（超限截断 + 显式提示）。
const DIFF_MAX_ROWS: usize = 200;
/// 超限提示（显式，不静默截断）。
const DIFF_TRUNCATED: &str = "… diff truncated";

/// 一行渲染规格：装订线（首行前缀）、续行缩进、内容、样式。
struct Row {
    gutter: String,
    text: String,
    style: Style,
}

/// 统一 diff 行首形态判据。
///
/// 认两种形态：hunk 头 `@@ -a,b +c,d @@`，或成对出现的 `--- `/`+++ ` 文件头。
/// 单独一个 `--- `（例如 YAML 文档分隔、表格分隔线）**不**判成 diff。
pub fn looks_like_diff(detail: &str) -> bool {
    let hunk = detail.starts_with("@@ ") || detail.contains("\n@@ ");
    let del = detail.starts_with("--- ") || detail.contains("\n--- ");
    let add = detail.starts_with("+++ ") || detail.contains("\n+++ ");
    hunk || (del && add)
}

/// `diffy` 能否解析。`looks_like_diff` 为真但这里为假 = 头部像 diff 的
/// 畸形文本，调用方应回落纯文本路径。
fn parseable(detail: &str) -> bool {
    Patch::from_str(detail).is_ok()
}

/// 进入渲染前的总闸：形态像、且解析得动。两项任一不满足，调用方走纯文本。
pub(crate) fn renderable(detail: &str) -> bool {
    looks_like_diff(detail) && parseable(detail)
}

/// 解析后的 patch → 行列表。行数上限与空隙标记都在这里生效，计数侧与
/// 物化侧看到的是同一份。
fn build_rows(detail: &str) -> Vec<Row> {
    let patch = match Patch::from_str(detail) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    // 装订线宽度由最大行号决定（两侧各自取），一次算定整份一致。
    let mut max_old = 0usize;
    let mut max_new = 0usize;
    for hunk in patch.hunks() {
        for line in hunk.lines() {
            match line {
                DiffLine::Context(_) => {
                    max_old += 1;
                    max_new += 1;
                }
                DiffLine::Delete(_) => max_old += 1,
                DiffLine::Insert(_) => max_new += 1,
            }
        }
        max_old = max_old.max(hunk.old_range().start());
        max_new = max_new.max(hunk.new_range().start());
    }
    let width = [max_old, max_new]
        .iter()
        .map(|n| n.to_string().len())
        .max()
        .unwrap_or(1);

    let mut out: Vec<Row> = Vec::new();
    let mut overflow = false;
    // 上一 hunk 的删除侧结束行号（0 表示还没有 hunk），用于空隙判定。
    let mut prev_old_end: Option<usize> = None;

    'hunks: for hunk in patch.hunks() {
        // 空隙标记：两个 hunk 之间隔了 > GAP_THRESHOLD 行删除侧上下文。
        if let Some(end) = prev_old_end {
            let gap = hunk.old_range().start().saturating_sub(end);
            if gap > GAP_THRESHOLD {
                if out.len() >= DIFF_MAX_ROWS {
                    overflow = true;
                    break 'hunks;
                }
                out.push(Row {
                    gutter: " ".repeat(width * 2 + 2),
                    text: format!("… {gap} lines skipped"),
                    style: theme::dim(),
                });
            }
        }

        let mut old = hunk.old_range().start();
        let mut new = hunk.new_range().start();
        for line in hunk.lines() {
            if out.len() >= DIFF_MAX_ROWS {
                overflow = true;
                break 'hunks;
            }
            let (sign, text, old_disp, new_disp) = match line {
                DiffLine::Context(t) => {
                    let row = (" ", *t, old.to_string(), new.to_string());
                    old += 1;
                    new += 1;
                    row
                }
                DiffLine::Delete(t) => {
                    let row = ("-", *t, old.to_string(), String::new());
                    old += 1;
                    row
                }
                DiffLine::Insert(t) => {
                    let row = ("+", *t, String::new(), new.to_string());
                    new += 1;
                    row
                }
            };
            // diffy 的行内容带尾部 \n（仅 "\ No newline" 特例被剥掉）；
            // 不剥的话 push_wrapped 把每行折成两物理行（200 逻辑行渲染
            // 401 物理行）。顺手过 sanitize：零 ESC 契约不依赖调用方。
            let mut content = super::transcript::sanitize(text);
            if content.ends_with('\n') {
                content.pop();
            }
            let gutter = format!("  {:>w$} {:>w$} {} ", old_disp, new_disp, sign, w = width);
            out.push(Row {
                gutter,
                text: content,
                style: match line {
                    DiffLine::Insert(_) => theme::diff_add(),
                    DiffLine::Delete(_) => theme::diff_del(),
                    DiffLine::Context(_) => theme::dim(),
                },
            });
        }
        prev_old_end = Some(old.saturating_sub(1));
    }

    if overflow {
        out.push(Row {
            gutter: " ".repeat(width * 2 + 2),
            text: DIFF_TRUNCATED.to_string(),
            style: theme::warn(),
        });
    }
    out
}

/// 纯文本回落：与 `tool_card` 结果区全显分支同形（`  ` 前缀 + base 样式），
/// 保证「像 diff 但解析不动」的内容一字不少地显示。
fn plain_rows(detail: &str) -> Vec<Row> {
    super::transcript::sanitize(detail)
        .lines()
        .map(|line| Row {
            gutter: "  ".to_string(),
            text: line.to_string(),
            style: theme::base(),
        })
        .collect()
}

/// detail → 行数（计数侧，`result_rows` 的对偶）。
pub fn rows(detail: &str, width: usize) -> usize {
    let rows = if renderable(detail) {
        build_rows(detail)
    } else {
        plain_rows(detail)
    };
    rows.iter()
        .map(|row| count_wrapped(&row.text, width, &row.gutter))
        .sum()
}

/// detail → 行（物化侧，`result_lines` 的对偶）。
pub fn lines(detail: &str, width: usize) -> Vec<Line<'static>> {
    let rows = if renderable(detail) {
        build_rows(detail)
    } else {
        plain_rows(detail)
    };
    let mut out: Vec<Line<'static>> = Vec::new();
    for row in rows {
        push_wrapped(&mut out, &row.text, width, &row.gutter, row.style);
    }
    out
}
