//! 订阅读线程：阻塞消费 daemon 推送帧，断线退避重连。
//!
//! 两个管线（worker 事件 / user 问题）都只拥有 `WebDaemon` 克隆、channel
//! 发送端与本仓 ReadinessGate——不碰任何 `Signal`（use_signal 底层
//! UnsyncStorage 非 Send，不能进 std::thread）。

use std::time::Duration;

use crate::state::readiness::ReadinessGate;
use web_client::QuestionItem;
use web_client::daemon::WebDaemon;
use web_state::convert::WireTranslator;
use web_state::ui_state::AgentEvent;

/// 订阅读线程：阻塞消费 `event.subscribe` 推送帧，断线退避重连
/// （1s/2s/4s/5s 封顶）。只拥有 `WebDaemon` 克隆、`Subscription`、
/// `WireTranslator` 与 `tx`——不碰任何 Signal。退出条件：`tx.send`
/// 失败（消费端随组件卸载而亡）或 keepalive tick 感知 `tx.is_closed`。
/// 在成功 `subscribe_worker` 后标记就绪，断线/退出时复位。
pub fn worker_event_loop(
    d: WebDaemon,
    tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ready: ReadinessGate,
) {
    const BACKOFF: [u64; 4] = [1, 2, 4, 5];
    let mut attempt = 0usize;
    loop {
        if let Ok(mut sub) = d.subscribe_worker() {
            // 订阅成功：标记就绪，允许背景提示线程继续
            ready.mark_ready();
            attempt = 0;
            let mut translator = WireTranslator::new();
            loop {
                // 5s keepalive：None 空转 tick 顺带感知消费端死亡，把组件
                // 卸载后读线程的残留窗口从 30s 压到 5s（空转只是一次
                // syscall，开销可忽略）
                match sub.next_event(Duration::from_secs(5)) {
                    Ok(Some(frame)) => {
                        if let Some(ev) = translator.translate(&frame.event)
                            && tx.send(ev).is_err()
                        {
                            // 消费端已亡：复位就绪并退出
                            ready.mark_not_ready();
                            return;
                        }
                    }
                    Ok(None) => {
                        if tx.is_closed() {
                            // 消费端已亡：复位就绪并退出
                            ready.mark_not_ready();
                            return;
                        }
                    }
                    Err(_) => {
                        // 断线：复位就绪并走重连
                        ready.mark_not_ready();
                        break;
                    }
                }
            }
        }
        if tx.is_closed() {
            ready.mark_not_ready();
            return;
        }
        eprintln!("retry in {}s", BACKOFF[attempt.min(BACKOFF.len() - 1)]);
        std::thread::sleep(Duration::from_secs(BACKOFF[attempt.min(BACKOFF.len() - 1)]));
        attempt += 1;
    }
}

/// 订阅读线程：阻塞消费 `user.question` 推送帧，断线退避重连（策略同
/// [`worker_event_loop`]）。帧的 `event` 字段是序列化的 [`QuestionItem`]；
/// 解析失败的帧跳过并留痕（协议演进的前向兼容），不中断订阅。
pub fn question_event_loop(d: WebDaemon, tx: tokio::sync::mpsc::UnboundedSender<QuestionItem>) {
    const BACKOFF: [u64; 4] = [1, 2, 4, 5];
    let mut attempt = 0usize;
    loop {
        if let Ok(mut sub) = d.subscribe_user_questions() {
            attempt = 0;
            loop {
                match sub.next_event(Duration::from_secs(5)) {
                    Ok(Some(frame)) => {
                        match serde_json::from_value::<QuestionItem>(frame.event) {
                            Ok(item) => {
                                if tx.send(item).is_err() {
                                    return; // 消费端已亡
                                }
                            }
                            Err(e) => {
                                eprintln!("[web] malformed user.question frame: {e}");
                            }
                        }
                    }
                    Ok(None) => {
                        if tx.is_closed() {
                            return; // 消费端已亡：keepalive tick 时感知
                        }
                    }
                    Err(_) => break, // 断线 → 走重连
                }
            }
        }
        if tx.is_closed() {
            return;
        }
        eprintln!("retry in {}s", BACKOFF[attempt.min(BACKOFF.len() - 1)]);
        std::thread::sleep(Duration::from_secs(BACKOFF[attempt.min(BACKOFF.len() - 1)]));
        attempt += 1;
    }
}
