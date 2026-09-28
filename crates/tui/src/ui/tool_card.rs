//! ui/tool_card — 工具调用卡片（route §3 T3）。
//!
//! 数据源是投影好的 `UiState.messages[].parts[].Tool`（web-state 的
//! `ToolCall{title,kind,status}`）：一次 `ToolCall→ToolStart→ToolResult`
//! 序列在 `UiState::apply` 里折成**一个** `MessagePart::Tool`，本模块按
//! 「一个 part = 一张卡」渲染，**不重新实现归一化**——未知工具名的标题
//! 兜底由 `tool_call_from_rpc` 负责（`web/state/tests/ui_state.rs` 已钉）。
//!
//! 卡三态：running → done / failed（`ToolResult` 翻同一张卡的状态，不另开
//! 行）。长结果默认折叠成「头尾预览 + 隐藏行数」；展开状态由
//! `App::tools_expanded` 持有、一个键（Tab）全部展开/再按折回。
//!
//! T22：卡头字形双通道——kind 字形（覆盖代码库现有 11 个 kind，未知
//! 回落 `⚙`）+ 三态字形（`~` / `✓` / `✗`，未知态 `?`）：形状是主通道、
//! 颜色是辅助——NO_COLOR 终端 / 分不出红绿的读者不靠颜色也读得出三态。
//! 字形全部单格（unicode-width 1），不影响行数（钉 `tests/tool_card.rs`）。

use ratatui::text::{Line, Span};

use web_state::types::ToolCall;

use crate::theme;

use super::transcript::push_wrapped;

/// 折叠阈值：结果超过该行数即默认折叠（route §3「长结果折叠」）。
pub const COLLAPSE_AFTER_LINES: usize = 6;
/// 折叠时保留的头部行数。
const HEAD_LINES: usize = 3;
/// 折叠时保留的尾部行数。
const TAIL_LINES: usize = 2;

/// 一张工具卡的全部行：卡头（态字形 · kind 字形 · kind · 三态文案）+ 标题 + 结果区。
///
/// `expanded` 由 `App::tools_expanded` 传入（一个键全部展开/折叠）；
/// `width` 是列宽，标题与结果按列宽断行（同 transcript 的处理）。
pub fn lines(tc: &ToolCall, expanded: bool, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(header(tc));
    push_wrapped(&mut out, &tc.title, width, "  ", theme::base());
    if tc.status == "running" {
        // 运行中 detail 还是入参 JSON、没有结果可看；结果由 ToolResult 落进
        // 这同一张卡——三态只在卡头翻，不新开行（route §3 一张卡契约）。
        return out;
    }
    out.extend(result_lines(tc, expanded, width));
    out
}

/// 一张卡的行数（T6 窗口化：与 [`lines`] 同源计数，历史卡不物化）。
///
/// 分支与 [`lines`] / [`result_lines`] 一一镜像：卡头恒 1 行、running 无
/// 结果区、折叠态 = 头 + 隐藏行数 + 尾；断行走 `transcript::count_wrapped`
/// （与 `push_wrapped` 同一 `wrap_with` 核心，分叉由 `tests/scroll_follow.rs`
/// 的 count/render 一致性测试钉；glyph 组合由
/// `tests/tool_card.rs::glyphs_do_not_change_row_count` 钉）。
///
/// T22：`pub` 出给测试直接钉计数/物化对偶（`rows == lines().len()`）。
pub fn rows(tc: &ToolCall, expanded: bool, width: usize) -> usize {
    let mut rows = 1 + super::transcript::count_wrapped(&tc.title, width, "  ");
    if tc.status == "running" {
        return rows;
    }
    rows += result_rows(tc, expanded, width);
    rows
}

/// 卡头：`▸ <态字形> <kind字形> <kind> · <三态文案>`，样式随状态
/// （running=brand、failed=danger、done=dim）。T22：三态与 kind 各有一个
/// 字形做主通道（无颜色可读），颜色只做辅助——见 [`state_glyph`] /
/// [`kind_glyph`]。
fn header(tc: &ToolCall) -> Line<'static> {
    let style = match tc.status.as_str() {
        "running" => theme::brand_bold(),
        "error" => theme::danger(),
        _ => theme::dim(),
    };
    Line::from(Span::styled(
        format!(
            "▸ {} {} {} · {}",
            state_glyph(&tc.status),
            kind_glyph(&tc.kind),
            tc.kind,
            status_label(&tc.status)
        ),
        style,
    ))
}

/// 三态文案：success→done、error→failed；未知状态原样显示（只显示数据，
/// 不编造状态）。
fn status_label(status: &str) -> &str {
    match status {
        "success" => "done",
        "error" => "failed",
        other => other,
    }
}

/// T22：三态字形（不靠颜色区分三态的形状通道——NO_COLOR 终端 / 分不出
/// 红绿的读者不能只靠卡头颜色；形状 + 文案为主通道，颜色为辅助）：
/// `~` running / `✓` done / `✗` failed；未知状态 `?`——不编造状态。
fn state_glyph(status: &str) -> char {
    match status {
        "running" => '~',
        "success" => '✓',
        "error" => '✗',
        _ => '?',
    }
}

/// T22：kind 字形。表覆盖代码库现有 11 个 kind 值（`web-state::ui_state`
/// `tool_call_from_rpc` 归一化表：bash / edit / read / write / delete /
/// grep / glob / job / terminal / subagent + "tool" 兜底桶）；任意未知
/// kind 回落 `⚙`（永不空串、不 panic）。字形全单格（unicode-width 1，
/// 与已在用的 `▸` / `❯` 同宽类），不参与断行与行数。
fn kind_glyph(kind: &str) -> char {
    match kind {
        "bash" => '$',
        "job" => '&',
        "terminal" => '⌗',
        "read" => '≡',
        "write" => '⇓',
        "edit" => '✎',
        "delete" => '⌫',
        "grep" | "glob" => '⌕',
        "subagent" => '↗',
        _ => '⚙',
    }
}

/// 结果区：短结果全显；长结果默认「头尾预览 + 隐藏行数」，`expanded` 时
/// 全显（一个键全部展开/折叠）。
fn result_lines(tc: &ToolCall, expanded: bool, width: usize) -> Vec<Line<'static>> {
    let text = super::transcript::sanitize(&tc.detail);
    let logical: Vec<&str> = text.lines().collect();
    if logical.is_empty() {
        return Vec::new();
    }
    if expanded || logical.len() <= COLLAPSE_AFTER_LINES {
        let mut out = Vec::new();
        for line in logical {
            push_wrapped(&mut out, line, width, "  ", theme::base());
        }
        return out;
    }
    // 折叠：头 HEAD 行 + 隐藏行数 + 尾 TAIL 行（阈值恒大于头尾之和，
    // 切片不会越界）。
    let hidden = logical.len().saturating_sub(HEAD_LINES + TAIL_LINES);
    let mut out = Vec::new();
    for line in logical.iter().take(HEAD_LINES) {
        push_wrapped(&mut out, line, width, "  ", theme::base());
    }
    out.push(Line::from(Span::styled(
        format!("  … {hidden} lines hidden"),
        theme::dim(),
    )));
    for line in &logical[logical.len() - TAIL_LINES..] {
        push_wrapped(&mut out, line, width, "  ", theme::base());
    }
    out
}

/// 结果区行数（[`result_lines`] 的镜像：分支、折叠头尾切片逐一对应）。
fn result_rows(tc: &ToolCall, expanded: bool, width: usize) -> usize {
    let text = super::transcript::sanitize(&tc.detail);
    let logical: Vec<&str> = text.lines().collect();
    if logical.is_empty() {
        return 0;
    }
    let rows = |line: &str| super::transcript::count_wrapped(line, width, "  ");
    if expanded || logical.len() <= COLLAPSE_AFTER_LINES {
        return logical.iter().map(|line| rows(line)).sum();
    }
    let head: usize = logical.iter().take(HEAD_LINES).map(|line| rows(line)).sum();
    let tail: usize = logical[logical.len() - TAIL_LINES..]
        .iter()
        .map(|line| rows(line))
        .sum();
    head + 1 + tail // 中间那 1 行是「… N lines hidden」
}
