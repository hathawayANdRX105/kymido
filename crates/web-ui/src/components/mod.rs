//! 可复用 UI 组件。原子件（Button/Icon/Spinner/DropdownMenu…）统一用
//! ui-kit（ferrite 家族共享设计系统），本 crate 只留业务组合组件与
//! kit 尚无的 Modal 壳（ui.rs）。

pub mod chat;
pub mod collapsed_rail;
pub mod filter_chip;
pub mod menu_picker;
pub mod message;
pub mod session_row;
pub mod sidebar;
pub mod taskpanel;
pub mod ui;
