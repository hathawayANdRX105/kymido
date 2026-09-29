//! 会话映射（`space → sessions`）的纯变换助手。
//!
//! 从 `views/workspace.rs` 下沉——这些是「数据上行」里的纯函数：读
//! `Signal<HashMap<…>>` / `&[Session]`，产出变换后的集合或单一布尔判定，
//! 不接 RPC、不发请求（io 在 `actions.rs` / `subscriptions.rs`）。

use std::collections::HashMap;
use std::collections::HashSet;

use dioxus::prelude::*;
use web_state::types::{Session, SessionStatus};

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 新建会话（内存版；Daemon 模式下调用方再追加 daemon 持久化）。
/// 独立成自由函数：Signal 是 Copy，任意闭包都可以直接调用，避免处理器
/// 闭包被多处 move。返回 (会话 id, 标题) 供 daemon 侧 create 使用。
/// 标题是 `会话 <ts>` 时间戳占位——侧栏新建瞬间先占位，首条用户消息
/// 发出后由 `on_send` 换成 `title_from_first_message` 的截词标题。
#[allow(clippy::too_many_arguments)]
pub fn create_session_in(
    space_path: String,
    model: String,
    mut space_sessions: Signal<HashMap<String, Vec<Session>>>,
    mut session_messages: Signal<HashMap<String, Vec<web_state::types::ChatMessage>>>,
    mut active_space_path: Signal<String>,
    mut active_session_id: Signal<String>,
) -> (String, String) {
    let ts = now_ms();
    let new_id = format!("s-{}", ts);
    let title = format!("会话 {}", ts % 1_000_000);
    let new_session = Session {
        id: new_id.clone(),
        title: title.clone(),
        last_active: "刚刚".into(),
        model,
        status: SessionStatus::Idle,
        last_active_epoch: ts,
        parent_id: None,
    };
    let mut map = space_sessions.read().clone();
    let mut list = map.get(&space_path).cloned().unwrap_or_default();
    list.insert(0, new_session);
    map.insert(space_path.clone(), list);
    space_sessions.set(map);
    session_messages.write().insert(new_id.clone(), Vec::new());
    active_space_path.set(space_path);
    active_session_id.set(new_id.clone());
    (new_id, title)
}

/// 会话是否仍存在于任一 space。流式期间会话可能被用户删除
/// （`on_delete_session` 会同时清掉消息），孤儿 entry 不能写回。
pub fn session_exists_in(space_sessions: Signal<HashMap<String, Vec<Session>>>, sid: &str) -> bool {
    space_sessions
        .read()
        .values()
        .flatten()
        .any(|s| s.id == sid)
}

/// 当前会话是否在运行（会话级状态：composer 的停止钮/运行中标识/
/// 输入门禁都由此驱动，切会话自然切换）。
pub fn active_session_running(
    space_sessions: Signal<HashMap<String, Vec<Session>>>,
    active_session_id: Signal<String>,
) -> bool {
    let sid = active_session_id();
    !sid.is_empty()
        && space_sessions
            .read()
            .values()
            .flatten()
            .any(|s| s.id == sid && s.status == SessionStatus::Active)
}

/// 会话列表侧的有效状态：页面在飞（Active，由事件流实时写）优先于
/// run 记录推断出的缓存状态（WP-C），缓存缺失时保持会话自身状态。
fn effective_status(current: &SessionStatus, inferred: Option<&SessionStatus>) -> SessionStatus {
    match (current, inferred) {
        // 页面正在跑：以实时态为准（推断缓存是加载时的快照，会滞后）
        (SessionStatus::Active, _) => SessionStatus::Active,
        (_, Some(inferred)) => inferred.clone(),
        (other, None) => other.clone(),
    }
}

/// 把推断状态就地合并进侧栏用的 space → 会话列表映射。
pub fn merge_run_statuses(
    mut map: HashMap<String, Vec<Session>>,
    cache: &HashMap<String, SessionStatus>,
) -> HashMap<String, Vec<Session>> {
    for list in map.values_mut() {
        apply_run_statuses(list, cache);
    }
    map
}

/// 把推断状态就地合并进一个会话列表（⌘K 快速切换用）。
pub fn apply_run_statuses(list: &mut [Session], cache: &HashMap<String, SessionStatus>) {
    for s in list.iter_mut() {
        s.status = effective_status(&s.status, cache.get(&s.id));
    }
}

/// 会话 id 集合（看板/状态推断 effect 的依赖集）。
pub fn session_id_set(space_sessions: Signal<HashMap<String, Vec<Session>>>) -> HashSet<String> {
    space_sessions
        .read()
        .values()
        .flatten()
        .map(|s| s.id.clone())
        .collect()
}
