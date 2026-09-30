//! 页面操作（R3 抽离）：`workspace.rs` 视图层闭包内的 spawn 线程 + RPC +
//! 多信号写回，提为具名 fn。
//!
//! 视图里的 `on_*` 闭包只做「读入参 → 调本层具名 fn」；一切并发、线程
//! 边界、Signal 写回收敛于此。Signal 是 `Copy`，打包进 [`WorkspaceSignals`]
//! 一次传入，不靠闭包逐捕获。

use std::collections::HashMap;
use std::time::Duration;

use dioxus::prelude::*;
use web_client::{ClientError, QuestionAnswer};
use web_state::title_from_first_message;
use web_state::types::{ChatMessage, PendingAttachment, Session, SessionStatus, StatusLine};

use crate::state::backend::DataBackend;
use crate::state::readiness::ReadinessGate;
use crate::state::session::{create_session_in, now_ms};

/// workspace 共享信号集：把视图层的十几个 `Signal` 打包传给动作 fn，
/// 避免每个 handler 闭包逐个 `move` 捕获。Signal 是 Copy，整个结构可复制。
#[derive(Clone, Copy)]
pub struct WorkspaceSignals {
    pub spaces: Signal<Vec<web_state::types::WorkspaceSpace>>,
    pub active_space_path: Signal<String>,
    pub space_sessions: Signal<HashMap<String, Vec<Session>>>,
    pub active_session_id: Signal<String>,
    pub session_messages: Signal<HashMap<String, Vec<ChatMessage>>>,
    pub pending_titles: Signal<HashMap<String, String>>,
    pub statusline: Signal<StatusLine>,
    pub pending_question: Signal<Option<web_client::QuestionItem>>,
    pub show_quick_switcher: Signal<bool>,
    pub show_settings: Signal<bool>,
    pub show_tasks: Signal<bool>,
    pub view: Signal<super::super::views::workspace::View>,
    pub backend: Signal<DataBackend>,
    pub readiness: Signal<ReadinessGate>,
    pub run_target_sid: Signal<String>,
    pub live_run_id: Signal<String>,
    pub board_version: Signal<u64>,
}

/// 标题更新最多尝试两次；仅 `database_missing` 可重试。
pub fn retry_update<F>(mut update: F) -> Result<(), ClientError>
where
    F: FnMut() -> Result<(), ClientError>,
{
    for attempt in 0..2 {
        match update() {
            Ok(()) => return Ok(()),
            Err(ClientError::Server { code, message: _ })
                if code == "database_missing" && attempt == 0 =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("two-attempt retry loop always returns")
}

/// 返回仍需落库的标题。首条消息使用本次派生标题；此前落库失败时，
/// 只要本地标题仍是那次派生结果就继续重试。本地标题已变则不覆盖。
pub fn title_to_persist(
    pending_title: Option<&str>,
    current_title: &str,
    derived_title: &str,
    is_first_message: bool,
) -> Option<String> {
    if is_first_message {
        return Some(derived_title.to_string());
    }
    match pending_title {
        Some(pending) if pending == current_title => Some(pending.to_string()),
        _ => None,
    }
}

/// 会话选中：切会话 + 回到聊天视图。
pub fn select_session(mut sig: WorkspaceSignals, id: String) {
    (sig.active_session_id).set(id);
    (sig.view).set(super::super::views::workspace::View::Chat);
}

/// 空间（项目）选中：仅切 active space。
pub fn select_space(mut sig: WorkspaceSignals, path: String) {
    (sig.active_space_path).set(path);
}

/// 新建会话（内存即时 + Daemon 持久化）。
pub fn create_session(mut sig: WorkspaceSignals, model: String, space_path: String) {
    let (new_id, title) = create_session_in(
        space_path,
        model,
        sig.space_sessions,
        sig.session_messages,
        sig.active_space_path,
        sig.active_session_id,
    );
    if let DataBackend::Daemon(d) = (sig.backend)() {
        std::thread::spawn(move || {
            let _ = d.create_session(&new_id, &title);
        });
    }
    (sig.view).set(super::super::views::workspace::View::Chat);
}

/// 删除会话：Daemon 侧删除（线程内，连带消息），内存清理照旧；
/// 删除当前会话时回落到该 space 的头部会话。
pub fn delete_session(mut sig: WorkspaceSignals, id: String) {
    if let DataBackend::Daemon(d) = (sig.backend)() {
        let id_daemon = id.clone();
        std::thread::spawn(move || {
            let _ = d.delete_session(&id_daemon);
        });
    }
    let mut map = (sig.space_sessions).read().clone();
    for list in map.values_mut() {
        list.retain(|s| s.id != id);
    }
    (sig.space_sessions).set(map);
    (sig.session_messages).write().remove(&id);
    if (sig.active_session_id)() == id {
        let next = sig
            .space_sessions
            .read()
            .get(&(sig.active_space_path)())
            .and_then(|list| list.first().map(|s| s.id.clone()))
            .unwrap_or_default();
        (sig.active_session_id).set(next);
    }
}

/// 删除项目（空间）：内存移除 + 回落。
pub fn delete_space(mut sig: WorkspaceSignals, space_path: String) {
    let mut sp = (sig.spaces)();
    sp.retain(|s| s.path != space_path);
    (sig.spaces).set(sp);
    (sig.space_sessions).write().remove(&space_path);
    if (sig.active_space_path)() == space_path {
        if let Some(first) = (sig.spaces)().first() {
            (sig.active_space_path).set(first.path.clone());
        } else {
            (sig.active_space_path).set(String::new());
            (sig.active_session_id).set(String::new());
        }
    }
}

/// 切换模型：状态行 + 运行配置写回。
pub fn change_model(
    mut sig: WorkspaceSignals,
    config: web_client::llm::LlmRuntimeConfig,
    m: String,
    on_update_config: EventHandler<web_client::llm::LlmRuntimeConfig>,
) {
    let mut st = (sig.statusline)();
    st.model = m.clone();
    (sig.statusline).set(st);
    let mut cfg = config.clone();
    cfg.model = m;
    on_update_config.call(cfg);
}

/// 切换思考强度（off ↔ 8k）。
pub fn toggle_thinking(mut sig: WorkspaceSignals) {
    let mut st = (sig.statusline)();
    st.thinking = if st.thinking == "off" {
        "8k".into()
    } else {
        "off".into()
    };
    (sig.statusline).set(st);
}

/// 中止当前 run：daemon abort（线程内）+ 内存即时复位 + 计时结算。
pub fn abort_run(mut sig: WorkspaceSignals) {
    if let DataBackend::Daemon(d) = (sig.backend)() {
        let sid = (sig.active_session_id)();
        std::thread::spawn(move || {
            let _ = d.abort_worker(&sid);
        });
    }
    // 内存即时复位：会话回 Idle；事件流里的残余事件由孤儿守卫兜底。
    // 在飞 run id 也清空：中断后列表状态以 run ledger 的持久记录为准（WP-C）
    (sig.live_run_id).set(String::new());
    // 计时结算（5.6）：中断后不会再有 TurnEnd，此处不结算耗时会一直
    // 按「在飞」实时增长。中断点即该 run 的终点。
    let mut st = (sig.statusline)();
    st.finish_run(now_ms());
    (sig.statusline).set(st);
    let sid = (sig.active_session_id)();
    let mut map = (sig.space_sessions).read().clone();
    for list in map.values_mut() {
        for s in list.iter_mut() {
            if s.id == sid {
                s.status = SessionStatus::Idle;
            }
        }
    }
    (sig.space_sessions).set(map);
}

/// 回答 plan-mode review 问题卡：RPC 在线程里跑，结果经 channel 回消费
/// 侧清卡片。终局错误（question_not_found / question_already_answered）
/// 也清卡片；只有传输类错误保留等重试。
pub fn answer_question(sig: WorkspaceSignals, qid: String, answer: QuestionAnswer) {
    if let DataBackend::Daemon(d) = (sig.backend)() {
        let mut pending_question = sig.pending_question;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<bool>>();
        std::thread::spawn(move || {
            let outcome = match d.answer_question(&qid, &answer) {
                Ok(()) => Some(true),
                Err(ClientError::Server { code, .. })
                    if code == "question_not_found" || code == "question_already_answered" =>
                {
                    // 问题已没了（别处答过 / 已清理）：卡片是 stale 的
                    eprintln!("[web] question {qid} gone ({code}); dropping card");
                    Some(false)
                }
                Err(e) => {
                    // 传输类错误：保留卡片等重试
                    eprintln!("[web] user.answer failed (question stays pending): {e}");
                    None
                }
            };
            let _ = tx.send(outcome);
        });
        spawn(async move {
            match rx.recv().await {
                Some(Some(true)) => pending_question.set(None), // 答成功
                Some(Some(false)) => pending_question.set(None), // 问题已消失
                _ => {}                                         // 传输错误：保留
            }
        });
    }
}

/// 发送消息：内存即时上屏 + Daemon 持久化（R3：从视图闭包抽为具名 fn）。
///
/// 流程：必要时先建会话 → Daemon 模式起订阅就绪线程（幂等 ensure +
/// append + title 持久化 + worker prompt）→ 用户消息上屏 + 会话置
/// Active → Disconnected 时复位回 Idle（不造回复）。
///
/// 复杂性说明：本函数线程/闭包边界多（spawn 线程非 Send 不能碰 Signal），
/// 故 Signal 只在上屏与结算段写，RPC 全在线程内、靠 channel 回消费侧。
pub fn send_message(
    sig: WorkspaceSignals,
    config: web_client::llm::LlmRuntimeConfig,
    text: String,
    attachments: Vec<PendingAttachment>,
) {
    let mut space_sessions = sig.space_sessions;
    let mut session_messages = sig.session_messages;
    let mut statusline = sig.statusline;
    let mut pending_titles = sig.pending_titles;
    let mut run_target_sid = sig.run_target_sid;
    let mut live_run_id = sig.live_run_id;

    // 无会话时先建一个；发送线程会再次幂等确保 daemon 行存在。
    if (sig.active_session_id)().is_empty()
        || !sig
            .space_sessions
            .read()
            .values()
            .any(|list| list.iter().any(|s| s.id == (sig.active_session_id)()))
    {
        create_session_in(
            (sig.active_space_path)(),
            config.model.clone(),
            sig.space_sessions,
            sig.session_messages,
            sig.active_space_path,
            sig.active_session_id,
        );
    }
    let sid = (sig.active_session_id)();

    // 首条消息把时间戳占位换成确定性截词标题。落库失败后，后续发送
    // 继续重试同一标题；本地标题若已改变则不覆盖。
    let is_first_message = session_messages.read().get(&sid).is_none_or(Vec::is_empty);
    let derived_title = title_from_first_message(&text);
    let session = sig
        .space_sessions
        .read()
        .values()
        .flatten()
        .find(|session| session.id == sid)
        .cloned();
    let persisted_title = session.as_ref().and_then(|session| {
        title_to_persist(
            pending_titles.read().get(&sid).map(String::as_str),
            &session.title,
            &derived_title,
            is_first_message,
        )
    });
    if persisted_title.is_some() {
        pending_titles
            .write()
            .insert(sid.clone(), persisted_title.clone().unwrap_or_default());
    } else {
        pending_titles.write().remove(&sid);
    }
    let parent_id = session.and_then(|session| session.parent_id);

    // Daemon 模式：真运行。用户消息持久化（线程内，刚自建的会话在同
    // 一线程先 create 再 append 保证顺序）；assistant 事件全走订阅
    // 管线，prompt 返回值（worker 原始 rpc 响应）忽略。run_target_sid 在
    // spawn 前落定，消费端据此写回本会话。
    if let DataBackend::Daemon(d) = (sig.backend)() {
        let sid_daemon = sid.clone();
        let text_daemon = text.clone();
        let attachments_daemon = attachments.clone();
        let d_prompt = d.clone();
        // 订阅事件不带会话归属：消费端以 run_target_sid 为写回目标，
        // 必须在 prompt 发出前落定
        run_target_sid.set(sid.clone());
        let run_id = format!("r-{}", now_ms());
        live_run_id.set(run_id.clone());
        // 5.6 计时起点：run 开始时落定 started_at，状态行的耗时段在飞
        // 期间随流式事件重渲染实时算，TurnEnd 时结算成总耗时。
        let mut st_start = statusline();
        st_start.start_run(now_ms());
        statusline.set(st_start);
        eprintln!("[web] send sid={} len={}", sid, text.len());
        // known-issue 1：失败分两类回传——订阅就绪门超时（daemon 冷启动 /
        // 重启窗口，订阅还没活）与 prompt RPC 错误。两类都要在状态行
        // 可见（last_error），不再只落 eprintln 让用户对着空 turn 猜。
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum SendFailure {
            NotReady,
            Prompt,
        }
        let (fail_tx, fail_rx) = tokio::sync::oneshot::channel::<SendFailure>();
        let run_id_prompt = run_id.clone();
        let title_for_daemon = persisted_title.clone();
        let (title_tx, title_rx) = tokio::sync::oneshot::channel::<bool>();
        let ready = (sig.readiness)();
        std::thread::spawn(move || {
            // 15s 门：订阅重连退避最坏 1+2+4+5s 一轮（BACKOFF 封顶 5s），
            // 门比最坏一轮留足余量，daemon 重启窗口内的首条消息不用手动预热。
            match ready.run_when_ready(Duration::from_secs(15), || {
                // 与侧栏创建线程并发时，在同一发送线程再次幂等确保会话存在。
                // 子会话必须携带 parent_id，避免竞态下退化为根会话。
                let ensured = if let Some(parent_id) = parent_id.as_deref() {
                    d.create_session_with_parent(&sid_daemon, &derived_title, Some(parent_id))
                } else {
                    d.create_session(&sid_daemon, &derived_title)
                };
                if let Err(e) = ensured {
                    eprintln!("[web] session ensure failed: {e}");
                }
                let _ = d.append_message(&sid_daemon, true, &text_daemon, &attachments_daemon, &[]);
                if let Some(title) = title_for_daemon {
                    let persisted = retry_update(|| d.update_session_title(&sid_daemon, &title));
                    if let Err(e) = &persisted {
                        eprintln!("[web] session title update failed after retries: {e}");
                    }
                    let _ = title_tx.send(persisted.is_ok());
                }
                d_prompt.worker_prompt_run(
                    &sid_daemon,
                    &run_id_prompt,
                    &text_daemon,
                    &attachments_daemon,
                )
            }) {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    eprintln!("[web] worker prompt failed: {e}");
                    let _ = fail_tx.send(SendFailure::Prompt);
                }
                Err(()) => {
                    eprintln!("[web] worker subscription not ready in 15s, prompt dropped");
                    let _ = fail_tx.send(SendFailure::NotReady);
                }
            }
        });
        let sid_title = sid.clone();
        spawn(async move {
            if title_rx.await == Ok(true) {
                pending_titles.write().remove(&sid_title);
            }
        });
        let sid_fail = sid.clone();
        let run_id_fail = run_id.clone();
        spawn(async move {
            match fail_rx.await {
                Ok(kind) => {
                    {
                        let mut map = space_sessions.write();
                        for list in map.values_mut() {
                            for s in list.iter_mut() {
                                if s.id == sid_fail {
                                    s.status = SessionStatus::Idle;
                                }
                            }
                        }
                    }
                    if live_run_id() == run_id_fail {
                        live_run_id.set(String::new());
                        run_target_sid.set(String::new());
                    }
                    // 计时结算（5.6）：prompt 直接失败时事件路径不会有
                    // TurnEnd，不结算耗时会一直按「在飞」实时增长
                    let mut st = statusline();
                    st.finish_run(now_ms());
                    // known-issue 1：失败在状态行可见（此前只有 eprintln，
                    // 用户看到 32ms 空 turn 却无从知晓原因）
                    st.last_error = match kind {
                        SendFailure::NotReady => "daemon 未就绪，消息未发送".into(),
                        SendFailure::Prompt => "run 启动失败，请重发".into(),
                    };
                    statusline.set(st);
                }
                Err(_) => {}
            }
        });
    }

    let now = now_ms();
    let user_msg = ChatMessage {
        id: format!("{}-user-{}", sid, now),
        role: "user".into(),
        content: text.clone(),
        reasoning: String::new(),
        reasoning_started_ms: None,
        reasoning_ms: None,
        tool_calls: vec![],
        parts: vec![],
        timestamp: "刚刚".into(),
        ts_epoch_ms: now,
        // The picked images ride the message itself, so a refresh (which
        // rebuilds the transcript from `session.messages`) still shows
        // them instead of a text-only ghost of the turn.
        attachments,
    };
    session_messages
        .write()
        .entry(sid.clone())
        .or_default()
        .push(user_msg);

    // 会话进入运行态
    let mut map = space_sessions.read().clone();
    for list in map.values_mut() {
        for s in list.iter_mut() {
            if s.id == sid {
                s.status = SessionStatus::Active;
                s.last_active = "刚刚".into();
                s.last_active_epoch = now;
                if is_first_message {
                    s.title = title_from_first_message(&text);
                }
            }
        }
    }
    space_sessions.set(map);

    match (sig.backend)() {
        DataBackend::Disconnected => {
            // 无 daemon = 无 agent 可跑：用户消息已上屏（本地即时反馈），
            // 但不产生任何 assistant 回复——旧的 mock 模拟流已删除，这里
            // 绝不编造回复。只把会话从上文刚置的 Active 复位回 Idle，
            // 否则 composer 会永久卡在「运行中」（停止钮亮、输入被门禁）。
            // known-issue 1：无 daemon 时消息只有本地回显、不产生 run——
            // 状态行给出可见提示（此前静默丢弃）。
            let mut st = statusline();
            st.last_error = "daemon 不可用，消息未发送".into();
            statusline.set(st);
            let mut map = space_sessions.read().clone();
            for list in map.values_mut() {
                for s in list.iter_mut() {
                    if s.id == sid {
                        s.status = SessionStatus::Idle;
                    }
                }
            }
            space_sessions.set(map);
        }
        DataBackend::Daemon(_) => {
            // 事件由订阅管线驱动，TurnEnd 收尾在订阅消费端完成；
            // prompt 失败兜底已在上文挂接。
        }
    }
}
