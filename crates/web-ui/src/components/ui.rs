//! 通用模态壳 —— ui-kit 尚无的组件，样式取自 kit 的 `ui-modal-*` 类
//! 语义（bg-card / border-border / shadow-xl / blur 遮罩）。
//!
//! 其余 atoms 已全部换成 ui-kit 对应物，本 crate 不再自研：
//! - `Button` / `IconButton` → `ui_kit::Button`（variant + IconSm 尺寸即原 28px 圆形钮）
//! - `Spinner` → `ui_kit::Spinner`（SVG animate-spin，替代自绘 .spinner-ring）
//! - `Dropdown` → `ui_kit::DropdownMenu` 族（composer 用 chat.rs 的 MenuPicker 封装）
//! - `Badge` → 无调用点，直接删除（需要时用 `ui_kit::Badge`）
//! - `ModalHeader` → 无调用点，随换壳一起删除

use dioxus::prelude::*;

// ── Modal ───────────────────────────────────────────────────────────────────
// kit `Dialog` 是固定「取消/确认」两钮的确认弹窗，不是通用壳；kymido 的设置
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
    // 顶部对齐换用 ui-modal-backdrop-top 变体类——旧的 replace("items-center")
    // 是对 Rust 常量做字符串手术，样式搬进 CSS 后这条路已经断了。
    let backdrop = if top_aligned {
        "ui-modal-backdrop-top"
    } else {
        "ui-modal-backdrop"
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
