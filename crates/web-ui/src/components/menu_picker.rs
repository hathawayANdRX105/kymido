//! composer 下拉选择器（自 chat.rs 拆出）：ui-kit DropdownMenu 的数据驱动封装
//! （胶囊触发器 + items + 选中高亮），自底部向上弹出。仅被 `Chat` 使用。

use dioxus::prelude::*;

use ui_kit::icons::IconCheck;
use ui_kit::{DropdownMenu, DropdownMenuItem};

/// composer 下拉选择器：ui-kit DropdownMenu 的数据驱动封装（label + items +
/// 选中高亮），自底部向上弹出。菜单无内部标题：每行即一个选项，行距与左对齐
/// 交给 kit 的 item 基串。kit 菜单项不设「选中即关」，这里由 `close_req` 请求
/// 收关——Dioxus signal 每次 set 都标脏，重复 set(true) 仍会触发收关 effect，
/// 可反复选用。
#[component]
pub(crate) fn MenuPicker(
    label: String,
    items: Vec<(String, String)>,
    active_value: String,
    #[props(default = false)] mono: bool,
    on_select: EventHandler<String>,
) -> Element {
    let mut close_req = use_signal(|| false);
    let mono_class = if mono { "font-mono" } else { "" };
    rsx! {
        // DropdownMenu 根是 display:contents（无定位上下文），absolute 面板需要
        // 调用方提供 relative 锚点，否则会上浮到整个布局容器而漂位。
        div { class: "relative",
            DropdownMenu {
            content_class: "bottom-full left-0 mb-1 min-w-[190px]",
            close_signal: close_req,
            trigger: rsx! {
                button {
                    r#type: "button",
                    class: "flex h-8 items-center gap-1.5 rounded-full px-3 text-[12.5px] text-label-2 hover:bg-selector transition-colors cursor-pointer border-none bg-transparent",
                    span { class: "{mono_class}", "{label}" }
                }
            },
            content: rsx! {
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
                                onclick: move |_| {
                                    on_select.call(item_value.clone());
                                    close_req.set(true);
                                },
                                span { class: "truncate {mono_class}", "{item_label}" }
                                if is_active {
                                    IconCheck { size: 14, class: "shrink-0 text-foreground" }
                                }
                            }
                        }
                    }
                }
            },
            }
        }
    }
}
