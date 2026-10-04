//! Worker 事件订阅就绪门。
//!
//! `generation` 在断线时递增,使已经等待的发送立即失败；断线后才开始的
//! 发送仍可等待下一次重连成功。

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct ReadinessGate(Arc<(Mutex<ReadinessState>, Condvar)>);

#[derive(Default)]
struct ReadinessState {
    ready: bool,
    generation: u64,
}

impl Default for ReadinessGate {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadinessGate {
    pub fn new() -> Self {
        Self(Arc::new((
            Mutex::new(ReadinessState::default()),
            Condvar::new(),
        )))
    }

    pub fn mark_ready(&self) {
        let (state, cvar) = &*self.0;
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.ready = true;
        cvar.notify_all();
    }

    pub fn mark_not_ready(&self) {
        let (state, cvar) = &*self.0;
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.ready = false;
        state.generation = state.generation.wrapping_add(1);
        cvar.notify_all();
    }

    /// 等待订阅就绪后执行操作。超时或等待期间断线时不执行操作。
    /// 就绪判断结束后释放锁，避免 prompt RPC 堵住断线复位。
    pub fn run_when_ready<T>(
        &self,
        timeout: Duration,
        action: impl FnOnce() -> T,
    ) -> Result<T, ()> {
        let ready = {
            let (state, cvar) = &*self.0;
            let state = state.lock().unwrap_or_else(|e| e.into_inner());
            let generation = state.generation;
            let (state, timed_out) = cvar
                .wait_timeout_while(state, timeout, |state| {
                    !state.ready && state.generation == generation
                })
                .unwrap_or_else(|e| e.into_inner());
            if state.ready {
                true
            } else {
                debug_assert!(timed_out.timed_out() || state.generation != generation);
                false
            }
        };
        if ready { Ok(action()) } else { Err(()) }
    }
}
