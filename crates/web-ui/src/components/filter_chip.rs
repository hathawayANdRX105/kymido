//! 过滤 chip（自 taskpanel.rs 拆出）：任务看板顶部的小型状态过滤按钮。

use dioxus::prelude::*;

/// 过滤 chip 样式：active 态（bg-accent 高亮）/ 默认态。
const FILTER_CHIP_ACTIVE: &str = concat!(
    "h-[26px] px-[7px] rounded-[7px] role-caption bg-accent ",
    "text-foreground cursor-pointer transition-colors border-none",
);
const FILTER_CHIP_INACTIVE: &str = concat!(
    "h-[26px] px-[7px] rounded-[7px] role-caption hover:bg-muted ",
    "hover:text-foreground cursor-pointer transition-colors border-none ",
    "bg-transparent",
);

/// 单 FilterChip：小型 tab 按钮，active 时底 bg-accent 高亮。
#[component]
pub(crate) fn FilterChip(
    label: String,
    active: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let class = if active {
        FILTER_CHIP_ACTIVE
    } else {
        FILTER_CHIP_INACTIVE
    };
    rsx! {
        button { r#type: "button", class: "{class}", onclick: onclick, "{label}" }
    }
}
