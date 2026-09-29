//! ui/fold — 单 part 折叠态（route §3 批 A：per-card 三态）。
//!
//! 三态从 turtle-ui 的 `Ctrl+O 循环 collapsed→expanded→hidden` 收敛成鼠标可
//! 点/找回的形态：[`Fold::Hidden`] **不整卡消失**，渲染为一行 stub
//! （`▸ … ‹hidden›`）——整卡消失让命中的目标本身没了，鼠标无从找回。stub 行
//! 可点回到 Collapsed，自恢复闭环。
//!
//! 折叠态的**单一出处**是 [`crate::app::App::fold_for`]：默认随
//! `App::tools_expanded`（Tab 的全局开关）派生，`App::fold_map` 里的
//! `(msg_idx, part_idx)` override 覆盖默认。渲染侧 `message_rows` /
//! `tool_card` / `thinking` / `line_offset` 不再传 `bool`，改传
//! `&dyn Fn(usize, usize) -> Fold` 同一份查询 —— 行数对偶两侧与检索侧共用
//! 一个判定，分叉由既有的 `tests/scroll_follow.rs` / `transcript_window.rs` /
//! `search_overlay.rs` 对偶钉继续兜住。

/// 单个 part（工具卡 / 思考块）的折叠态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// 折叠：工具卡 = 头 + head/tail 预览 + 隐藏行数；思考块 = 头 + head/tail。
    Collapsed,
    /// 全显（`tools_expanded=true` 的 per-part 版）。
    Expanded,
    /// 隐藏：渲染为一行 stub（`▸ kind ‹hidden›` / `◇ Thought ‹hidden›`），
    /// 不整卡消失——stub 自恢复可点回 Collapsed。
    Hidden,
}

/// 思考块的 part 索引哨兵：reasoning 是消息级字段，不占 `parts[]` 槽位，
/// 用 `usize::MAX` 与真实 `part_idx`（0..n）不撞车（route 批 A 决策）。
pub const THINKING_PART: usize = usize::MAX;

/// 从 `tools_expanded` 全局开关派生默认折叠态（无 override 时的值）。
pub fn default_fold(tools_expanded: bool) -> Fold {
    if tools_expanded {
        Fold::Expanded
    } else {
        Fold::Collapsed
    }
}
