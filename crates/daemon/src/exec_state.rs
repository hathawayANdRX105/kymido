//! S5: per-session execution state.
//!
//! A session's "what is going on right now" is one object — [`ExecState`] —
//! owned by the session's [`crate::dispatch::WorkerHandle`] (born with it,
//! dead with it, read/written under the handle lock or through the shared
//! status cell). Two facets:
//!
//! - [`ExecStatus`] — the run situation, single source of truth. The
//!   reaper, the event pump's run stamping, and any future read side (TUI
//!   status line, notifications) all look at this one cell instead of
//!   re-deriving it from the sticky slot + the run ledger.
//! - `plan` — the session's own [`plan_mode::PlanModeRuntime`]. `/plan`
//!   dispatch, the engine's plan-policy injection, and the session's
//!   `exit_plan_mode` tool instance all bind to *this* runtime, so plan
//!   state cannot leak across sessions.
//!
//! Future stateful tools plug in the same seam: build their per-session
//! state next to `plan` here, and instantiate the tool against it in
//! `SessionRegistry`'s per-session `OrbitSetup` assembly — never against a
//! daemon-wide singleton.
//!
//! The status cell is `Arc`-shared (cheap) because the scoped review port
//! parks/unparks it from the engine's run thread, outside the handle lock.

use std::sync::{Arc, RwLock};

/// What is going on in this session right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecStatus {
    /// No run in flight, no review parked.
    Idle,
    /// A prompt is in flight; the event stream is stamped with `run_id`.
    Running { run_id: String },
    /// The session's `exit_plan_mode` is parked in a review; the run
    /// thread is blocked until the answer arrives (or the review times
    /// out).
    AwaitingReview { question_id: String },
}

impl ExecStatus {
    /// Whether the session's engine owes a run or a review — i.e. the
    /// reaper must refuse to unload it. `AwaitingReview` counts: the run
    /// thread is blocked inside the scoped review port, so the engine is
    /// very much alive.
    pub fn has_in_flight(&self) -> bool {
        matches!(self, Self::Running { .. } | Self::AwaitingReview { .. })
    }
}

/// Per-session execution state. Clones share the status cell and the plan
/// runtime (both are `Arc`-backed internally) — cloning is the way the
/// registry, the handle, the scoped review port, and the engine's run
/// thread all reach the *same* state.
#[derive(Clone)]
pub struct ExecState {
    status: Arc<RwLock<ExecStatus>>,
    /// This session's plan/review runtime (created with it, dies with it).
    pub plan: plan_mode::PlanModeRuntime,
}

impl ExecState {
    /// A fresh, idle session state with plan mode off (the jcode shape:
    /// per-session, no daemon-wide default state).
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: Arc::new(RwLock::new(ExecStatus::Idle)),
            plan: plan_mode::PlanModeRuntime::new(false),
        }
    }

    /// The run situation right now.
    #[must_use]
    pub fn status(&self) -> ExecStatus {
        self.status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Reaper read surface: a run or a parked review is in flight.
    pub fn has_active_run(&self) -> bool {
        self.status().has_in_flight()
    }

    /// Prompt ack (S5): declare the run this turn's events belong to. An
    /// empty `run_id` (legacy prompt without attribution) declares *no*
    /// run — mirroring the old sticky-slot semantics, where an empty id
    /// cleared the slot so a finished run could not own a later turn.
    pub fn set_running(&self, run_id: &str) {
        let mut guard = self.status.write().unwrap_or_else(|e| e.into_inner());
        *guard = if run_id.is_empty() {
            ExecStatus::Idle
        } else {
            ExecStatus::Running {
                run_id: run_id.to_string(),
            }
        };
    }

    /// Back to idle: the run closed (`AgentEnd`) or a review resolved.
    pub fn idle(&self) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = ExecStatus::Idle;
    }

    /// Park a review: the scoped port submitted the question and the run
    /// thread is now blocked waiting for the answer.
    pub fn awaiting_review(&self, question_id: &str) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = ExecStatus::AwaitingReview {
            question_id: question_id.to_string(),
        };
    }

    /// The event pump's run stamping: which run id, if any, is in flight.
    pub fn running_run_id(&self) -> Option<String> {
        match self.status() {
            ExecStatus::Running { run_id } => Some(run_id),
            _ => None,
        }
    }

    /// Compare-and-reset used by the pump's `AgentEnd` close: only clear
    /// the slot if *this* run still owns it — a concurrent prompt (r2)
    /// that took the slot in the gap must keep its attribution (same
    /// sticky semantics as the old `try_clear_active_run`).
    pub fn try_clear_run(&self, expected_run_id: &str) {
        let mut guard = self.status.write().unwrap_or_else(|e| e.into_inner());
        if matches!(&*guard, ExecStatus::Running { run_id } if run_id == expected_run_id) {
            *guard = ExecStatus::Idle;
        }
    }
}
