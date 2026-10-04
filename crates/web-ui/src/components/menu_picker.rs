//! composer 下拉选择器（自 chat.rs 拆出）：ui-kit DropdownMenu 的数据驱动封装
//! （胶囊触发器 + 多选项 + 选中高亮），自底部向上弹出。仅被 `Chat` 使用。

use dioxus::prelude::*;

use ui_kit::icons::IconCheck;
use ui_kit::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuPosition,
    DropdownMenuTrigger,
};

/// 触发器胶囊样式（成组样式抽常量）。
const MENU_PICKER_TRIGGER: &str = concat!(
    "flex h-8 items-center gap-1.5 rounded-full px-3 role-caption ",
    "hover:bg-secondary-hover transition-colors cursor-pointer ",
    "border-none bg-transparent",
);

/// composer 下拉选择器：ui-kit DropdownMenu 的数据驱动封装（label + header +
/// items + 选中高亮），自底部向上弹出。
///
/// 结构 = kit 的「触发器 + 面板 + 多选项」：面板首行是 `header`（`DropdownMenuLabel`，
/// 空串则不渲染），其后是多选项段；行距（`px-2 py-1.5 gap-2`）、左对齐与选中项
/// IconCheck 的 tick 动效全部由 kit 的 `DropdownMenuItem` 基串承担，调用方不覆写
/// ——面板宽度只有一项定制。选中即关走 kit 契约：条目挂 `data-dropdown-close`，
/// 面板 JS 据此收关。
#[component]
pub(crate) fn MenuPicker(
    label: String,
    /// 面板首行标题（空串则不渲染标题行）。
    header: String,
    items: Vec<(String, String)>,
    active_value: String,
    #[props(default = false)] mono: bool,
    on_select: EventHandler<String>,
) -> Element {
    let mono_class = if mono { "font-mono" } else { "" };
    rsx! {
        DropdownMenu {
            // as_child：胶囊是 composer 专用外观（无边框、小号、hover 才出底），
            // 让 kit 只挂 data-dropdown-trigger，不与 kit 的 shadcn 触发钮基串争样式。
            DropdownMenuTrigger {
                as_child: true,
                button {
                    r#type: "button",
                    class: "{MENU_PICKER_TRIGGER}",
                    span { class: "{mono_class}", "{label}" }
                }
            }
            DropdownMenuContent {
                position: DropdownMenuPosition::Top,
                class: "min-w-[190px]",
                if !header.is_empty() {
                    DropdownMenuLabel { "{header}" }
                }
                for (item_label, item_value) in items.iter() {
                    {
                        let item_label = item_label.clone();
                        let item_value = item_value.clone();
                        let is_active = item_value == active_value;
                        rsx! {
                            DropdownMenuItem {
                                key: "{item_value}",
                                // ui-anim-scope 让 hover 整行驱动选中项 IconCheck 的
                                // tick 动效；行宽与左对齐显式写死，间距与 padding
                                // 仍由 kit item 基串管，不在此覆盖。
                                class: "w-full text-left ui-anim-scope",
                                "data-dropdown-close": "true",
                                onclick: move |_| on_select.call(item_value.clone()),
                                span { class: "truncate {mono_class}", "{item_label}" }
                                if is_active {
                                    IconCheck { size: 14, class: "shrink-0 text-foreground" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
