//! 通用模态壳 —— ui-kit 尚无的组件，样式取自 kit `panel.rs` 的 MODAL_* 常量
//! 语义（bg-card / border-border / shadow-xl / blur 遮罩）。
//!
//! 其余 atoms 已全部换成 ui-kit 对应物，本 crate 不再自研：
//! - `Button` / `IconButton` → `ui_kit::Button`（variant + IconSm 尺寸即原 28px 圆形钮）
//! - `Spinner` → `ui_kit::Spinner`（SVG animate-spin，替代自绘 .spinner-ring）
//! - `Dropdown` → `ui_kit::DropdownMenu` 族（composer 用 chat.rs 的 MenuPicker 封装）
//! - `Badge` → 无调用点，直接删除（需要时用 `ui_kit::Badge`）
//! - `ModalHeader` → 无调用点，随换壳一起删除

use dioxus::prelude::*;
use ui_kit::MODAL_BACKDROP;

// ── Modal ───────────────────────────────────────────────────────────────────
// kit `Dialog` 是固定「取消/确认」两钮的确认弹窗，不是通用壳；omenic 的设置
// 弹窗 / 快速切换需要的是后者，故保留本薄壳，几何全部收敛到 kit token。

#[component]
pub fn Modal(
    children: Element,
    on_close: EventHandler<()>,
    /// 卡片宽度类（默认 w-[380px]）
    #[props(optional)]
    width_class: Option<String>,
    /// 垂直位置：居中（默认）或顶部 pt-[14vh]
    #[props(default = false)]
    top_aligned: bool,
) -> Element {
    let width = width_class.unwrap_or_else(|| "w-[380px]".to_string());
    // MODAL_BACKDROP 固定 items-center；顶部对齐变体把对齐类替换掉再拼 pt
    let backdrop = if top_aligned {
        MODAL_BACKDROP.replace("items-center", "items-start pt-[14vh]")
    } else {
        MODAL_BACKDROP.to_string()
    };
    rsx! {
        div {
            class: "{backdrop}",
            onclick: move |_| on_close.call(()),
            div {
                class: "{width} max-w-full rounded-2xl border border-border bg-card shadow-xl overflow-hidden",
                onclick: move |e: MouseEvent| e.stop_propagation(),
                {children}
            }
        }
    }
}
