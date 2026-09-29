//! web-ui 页面级状态与数据层下沉（自 `views/workspace.rs` 抽出）。
//!
//! 视图（`views/`）只负责「取数 + 编排 + 组装」，数据后端的探测、订阅管线与
//! 长时回调动作（spawn 线程 + RPC + 多信号写回）收进本层——web-spec R3：
//! rsx/闭包只做写状态与调提取函数，`spawn`+RPC+多行业务提为具名 fn。
//!
//! - [`backend`]`DataBackend` / `daemon_space`：唯一真实数据源是本机 daemon。
//! - [`readiness`]`ReadinessGate`：worker 订阅就绪门。
//! - [`subscriptions`]`worker_event_loop`/`question_event_loop`：推送读线程。
//! - [`actions`]：页面操作（发送/中止/回答/建删会话）的具名 fn。
//!
//! 单向：`state` 依赖 `web-client`/`web-state` 的 DTO 与 dioxus `Signal`，
//! 不依赖 `views`/`components`（web_layering：views → components/layouts/utils，state 并列于最底层）。

pub mod actions;
pub mod backend;
pub mod readiness;
pub mod session;
pub mod subscriptions;
