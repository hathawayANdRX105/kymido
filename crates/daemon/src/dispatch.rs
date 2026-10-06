//! Command dispatch.
//!
//! Translates a [`Request`] into a [`Response`] by routing to either the
//! [`SessionState`] handle or the [`WorkerHandle`].  Pure function over
//! `&mut WorkerHandle` so the caller (server accept loop) can serialize
//! worker access across concurrent connections.
//!
//! ponytail: dispatch is split out so the server module can stay tiny.  All
//! command logic lives here, all protocol concerns live in `protocol.rs`,
//! and the worker handle is the only piece that knows about `crate::rpc::worker::Worker`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use crate::rpc::worker::WorkerEvent;
use serde_json::{Value, json};
use session::{
    ContextEvent, ContextEventDraft, ContextEventKind, SessionError, SessionMessage, SessionRole,
    SourceKind, ToolGroup, TurnRecord,
};

use crate::protocol::{Command, EventFrame, Request, Response, ResponseError};
use crate::state::{EventBus, RunLedger, SessionState, require_str, require_u32};

/// Topic fed by the `rpc` worker's push event stream (R2 3.3).
pub const WORKER_TOPIC: &str = "worker";

/// How many complete runs a resume replays into the worker's live context
/// on a `worker.prompt` that carries a `session_id` (C01).  Bounded on the
/// run count, not the event count: a run is an atomic unit of work and
/// cutting a run in half would leave a half-paired tool group.  A caller
/// that wants more history can raise this without touching the projection.
const RESUME_RUN_WINDOW: u32 = 50;

/// Branch id of the default (non-branching) session history.  The C01 event
/// ledger requires an explicit scope; the daemon's resume path currently
/// covers only the default branch — branched histories are a follow-up, and
/// replaying the wrong branch would silently corrupt the worker's context.
const DEFAULT_BRANCH: &str = "main";

/// Epoch of the default (non-branching) session history.  Rewind / truncate
/// advance the history epoch, but the resume path currently covers only the
/// current default-branch epoch.
const DEFAULT_EPOCH: i64 = 1;

/// Default `task.list` page size when the client sends no `limit`.  The
/// kanban board renders one screen; anything past the 50 most recently
/// touched tasks is stale backlog the UI can page in later.
const TASK_LIST_DEFAULT_LIMIT: u32 = 50;

/// Generic paged list helper for TaskList / TodoList / GoalList.
/// Extracts `limit` from params (default: `TASK_LIST_DEFAULT_LIMIT`), runs
/// the provided `load` closure, sorts by `updated_at` descending, truncates
/// to `limit`, and serializes the result.
fn paged_list<T, E>(
    id: Option<&str>,
    params: &serde_json::Value,
    load: impl FnOnce(u32) -> Result<Vec<T>, E>,
    sort_by_updated_at: impl FnOnce(&mut [T]),
) -> Response
where
    T: serde::Serialize,
    E: std::fmt::Display,
{
    let limit = params
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32)
        .unwrap_or(TASK_LIST_DEFAULT_LIMIT);
    match load(limit) {
        Ok(mut rows) => {
            sort_by_updated_at(&mut rows);
            rows.truncate(limit as usize);
            match serde_json::to_value(&rows) {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::err(
                    id,
                    ResponseError::new("internal", format!("serialize: {e}")),
                ),
            }
        }
        Err(e) => Response::err(id, ResponseError::new("internal", format!("store: {e}"))),
    }
}

/// Decode the text field of a context event payload.  C01 payloads for
/// `Prompt` / `Assistant` events carry a single `text` string field
/// (see [`ContextEvent::payload`]).  Returns `None` when the JSON is
/// malformed or the field is absent, in which case the event is dropped
/// from the resume projection rather than surfaced as an empty message.
fn context_event_text(payload_json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(payload_json).ok()?;
    v.get("text").and_then(Value::as_str).map(str::to_string)
}

/// Convert one C01 [`ContextEvent`] into a [`SessionMessage`] suitable for
/// [`WorkerHandle::resume_session`].  Returns `None` for events the orbit
/// context does not consume (terminal markers, tool-side records — those
/// travel as `tool_calls` payloads, not separate messages) or when the
/// event's payload does not decode to a text field.
///
/// Never fabricates a success marker: an unpaired tool call keeps
/// `result: null` so the resumed engine can distinguish interrupted work
/// from completed work.  The pairing pass (`pair_tool_results`) fills in
/// the result only for closed groups.
fn event_to_message(session_id: &str, seq: i64, ev: &ContextEvent) -> Option<SessionMessage> {
    let (role, text, tool_calls) = match ev.kind {
        ContextEventKind::Prompt => (
            SessionRole::User,
            context_event_text(&ev.payload_json)?,
            Vec::new(),
        ),
        ContextEventKind::Assistant => {
            let payload: Value = serde_json::from_str(&ev.payload_json).unwrap_or(Value::Null);
            let body = payload
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let tool_calls = ev.tool_call_id.as_deref().map(|id| {
                vec![json!({
                    "id": id,
                    // Tool name lives in the payload (the ledger has no
                    // tool_name column); never expose the raw payload blob.
                    "name": payload.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "result": Value::Null,
                })]
            });
            let tool_calls = tool_calls.unwrap_or_default();
            // An assistant turn that is only a tool-call carrier may have no
            // text; dropping it would lose the call. Drop only when there is
            // neither text nor a tool call.
            if body.is_empty() && tool_calls.is_empty() {
                return None;
            }
            (SessionRole::Assistant, body, tool_calls)
        }
        _ => return None,
    };
    Some(SessionMessage {
        session_id: session_id.to_string(),
        seq,
        role,
        text,
        created_at_ms: ev.created_at,
        attachments: Vec::new(),
        tool_calls,
    })
}

/// Pair each assistant message's `tool_calls` entry with its closed result
/// if a [`ToolGroup`] was found for its `tool_call_id`.  Unmatched calls
/// keep `result: null` — that is the honest interruption marker.
fn pair_tool_results(
    msg: SessionMessage,
    results: &std::collections::HashMap<String, Value>,
) -> SessionMessage {
    if msg.role != SessionRole::Assistant || msg.tool_calls.is_empty() {
        return msg;
    }
    let tool_calls = msg
        .tool_calls
        .into_iter()
        .map(|tc| {
            let id = tc
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut v = tc;
            if !id.is_empty() {
                if let Some(r) = results.get(&id) {
                    v["result"] = r.clone();
                }
            }
            v
        })
        .collect();
    SessionMessage { tool_calls, ..msg }
}

/// Idempotency key for one canonical turn event, derived from the turn
/// identity `(session, role, run)` — never from message text, so two
/// different turns that happen to say the same thing stay two events while a
/// replay of one turn collapses onto one row.
fn turn_event_key(session_id: &str, role: &str, run_id: &str) -> String {
    format!("turn:{session_id}:{role}:{run_id}")
}

/// The caller-supplied half of one ledger event. Everything the writer fills
/// in for itself (`event_id`, `branch_id`, `epoch`, `turn_id`, `event_order`)
/// stays in [`record_context_event`] so those derivations cannot drift apart
/// between call sites.
struct LedgerEvent<'a> {
    session_id: &'a str,
    run_id: Option<&'a str>,
    kind: ContextEventKind,
    source_kind: SourceKind,
    role: &'a str,
    payload: Value,
    tool_call_id: Option<&'a str>,
    idempotency_key: String,
}

/// Write one canonical event into the C01 ledger. Best-effort: a storage
/// failure is logged and swallowed, because the turn must not fail just
/// because its bookkeeping row did — the same policy as [`record_turn`].
/// `event_id` is derived from the idempotency key so a retry of the same
/// logical event reuses the same primary key instead of colliding under a
/// fresh one.
fn record_context_event(sessions: &SessionState, event: LedgerEvent<'_>) -> Option<ContextEvent> {
    if event.session_id.is_empty() {
        return None;
    }
    let draft = ContextEventDraft {
        event_id: format!("ev:{}", event.idempotency_key),
        session_id: event.session_id.to_string(),
        branch_id: DEFAULT_BRANCH.to_string(),
        epoch: DEFAULT_EPOCH,
        turn_id: event.run_id.map(str::to_string),
        run_id: event.run_id.map(str::to_string),
        event_order: crate::state::next_event_order(),
        role: event.role.to_string(),
        kind: event.kind,
        payload: event.payload,
        source_kind: event.source_kind,
        tool_call_id: event.tool_call_id.map(str::to_string),
        idempotency_key: event.idempotency_key,
    };
    match sessions.append_context_event(draft) {
        Ok((event, _inserted)) => Some(event),
        Err(e) => {
            eprintln!("daemon: context ledger write failed: {e}");
            None
        }
    }
}

/// Map a [`SessionRole`] to the canonical ledger `role` string.
fn ledger_role(role: SessionRole) -> &'static str {
    match role {
        SessionRole::User => "user",
        SessionRole::Assistant => "assistant",
        SessionRole::System => "system",
        SessionRole::Tool => "tool",
    }
}

/// Build a resume context by walking complete runs + closed tool groups
/// (C01).  Replaces the old "last 50 raw messages" tail slice with a
/// run-bounded projection that keeps `tool_call_id` pairs intact.
///
/// Missing results are *not* synthesized: a tool call whose result is not
/// yet in the ledger stays in the assistant message's `tool_calls` array
/// with `result: null`, so a resumed engine can tell interrupted work
/// from completed work.  This is the honest projection — inventing a
/// success would poison the next LLM turn.
///
/// Scope: the default branch (`main`) at epoch 1.  Branched histories and
/// rewound epochs are not covered by the current resume path — the caller
/// (`worker.prompt` with a session id) targets one canonical history.
///
/// Besides the complete runs, events that belong to no run are replayed too
/// (bounded by [`session::MAX_LIMIT`]): an explicit `session.append` import
/// or a row written before the session ever ran is canonical history, but it
/// carries no `run_id`, so the run walk alone would silently drop it.  A run
/// still in flight is *not* replayed — only its `turn_end`/`abort` moves it
/// into `recent_complete_runs`, which is exactly the boundary that keeps the
/// just-arrived prompt out of its own resume context.
fn build_resume_context(
    sessions: &SessionState,
    session_id: &str,
) -> Result<Vec<SessionMessage>, SessionError> {
    let runs = sessions.recent_complete_runs(session_id, RESUME_RUN_WINDOW)?;
    let branch_id = DEFAULT_BRANCH;
    let epoch = DEFAULT_EPOCH;

    // Closed call/result pairs, keyed by tool_call_id for O(1) lookup
    // while walking the run events below.  Only closed groups are
    // returned by the store — unpaired results never enter this map, so
    // they cannot leak into a paired call's result field.
    let groups: Vec<ToolGroup> = sessions.closed_tool_groups(session_id, branch_id, epoch)?;
    let mut result_by_call_id: std::collections::HashMap<String, Value> =
        std::collections::HashMap::new();
    for g in groups {
        result_by_call_id.insert(g.tool_call_id, Value::String(g.result.payload_json.clone()));
    }

    // Gather every event the projection covers: all events of the complete
    // runs, plus the run-less imported/legacy rows, then read them back in
    // write order so the resumed context is chronological.
    let mut events: Vec<ContextEvent> = Vec::new();
    for run_id in runs.iter() {
        events.extend(sessions.context_events_for_run(session_id, branch_id, epoch, run_id)?);
    }
    let imported = sessions.context_events_in_range(
        session_id,
        branch_id,
        epoch,
        i64::MIN,
        i64::MAX,
        session::MAX_LIMIT,
    )?;
    events.extend(imported.into_iter().filter(|e| e.run_id.is_none()));
    events.sort_by(|a, b| {
        a.event_order
            .cmp(&b.event_order)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });

    let mut msgs = Vec::new();
    let mut seq = 0i64;
    for ev in &events {
        if let Some(mut m) = event_to_message(session_id, seq, ev) {
            m = pair_tool_results(m, &result_by_call_id);
            seq += 1;
            msgs.push(m);
        }
    }
    Ok(msgs)
}

/// One worker behind a session key in the [`crate::registry::SessionRegistry`]
/// (S1). The dispatch layer takes `&mut`; the registry locks it per request.
pub struct WorkerHandle {
    inner: Option<crate::rpc::worker::Worker>,
    omp_path: String,
    /// Whether the forwarder thread feeding [`WORKER_TOPIC`] is alive.
    /// Cleared by the forwarder itself when the worker's event channel
    /// closes (worker died or was reset), so the next subscribe respawns it.
    pump_active: Arc<AtomicBool>,
    /// S5: this session's execution state — the single source of truth for
    /// "what is going on in this session" (run in flight, review parked)
    /// plus the session's own plan-mode runtime. The reaper and the event
    /// pump's run stamping read the shared status cell; the registry builds
    /// the per-session tool/plan instances against it.
    exec: crate::exec_state::ExecState,
    /// orbit 模式构造包（模型 + 后端 + 容器解析出的 OrbitConfig）；
    /// None = omp 兼容模式。S5 起由注册表按会话装配（plan 闭包 + 本会话
    /// 的 `exit_plan_mode` 进 `session_tools`），不再是 daemon 级共享克隆。
    orbit_setup: Option<crate::rpc::worker::OrbitSetup>,
    /// S2: the session key this handle is bound to ("" = legacy /
    /// session-less entry). The event pump stamps it onto every frame it
    /// pushes so subscribers can filter by session.
    session_id: String,
}

impl WorkerHandle {
    pub fn new(
        omp_path: impl Into<String>,
        orbit_setup: Option<crate::rpc::worker::OrbitSetup>,
        session_id: impl Into<String>,
        exec: crate::exec_state::ExecState,
    ) -> Self {
        WorkerHandle {
            inner: None,
            omp_path: omp_path.into(),
            pump_active: Arc::new(AtomicBool::new(false)),
            exec,
            orbit_setup,
            session_id: session_id.into(),
        }
    }

    /// Record the run served by the prompt about to be sent (G7-B).  The
    /// event pump stamps it onto every frame it pushes while the run is
    /// active.  The slot is *sticky*: `prompt` returns as soon as the
    /// worker acks, but the turn's events keep flowing afterwards (the pump
    /// owns the read loop and fans them out after the response frame is
    /// matched — in orbit mode the whole turn is async), so the run is only
    /// replaced by the next attributed prompt, never cleared on prompt
    /// return. An empty `run_id` (legacy prompt without attribution)
    /// declares no run, so a finished run cannot own a later,
    /// unattributed turn.
    pub fn set_active_run(&self, run_id: &str) {
        self.exec.set_running(run_id);
        // C01: mirror the run slot into the persistence sink so the orbit run
        // thread can attribute the tool events it records to this run.  An
        // empty run id clears the sink's binding.
        if let Some(setup) = self.orbit_setup.as_ref()
            && let Some(sink) = setup.persistence_sink.as_ref()
        {
            sink.set_active_run(run_id);
        }
    }

    /// Forget any active run (worker reset: a respawned worker owes the
    /// previous run nothing).
    pub fn clear_active_run(&self) {
        self.exec.idle();
        if let Some(setup) = self.orbit_setup.as_ref()
            && let Some(sink) = setup.persistence_sink.as_ref()
        {
            sink.set_active_run("");
        }
    }

    /// S3/S5: whether a run (or a parked review) is in flight on this
    /// session. The reaper refuses to unload a handle with one.
    pub fn has_active_run(&self) -> bool {
        self.exec.has_active_run()
    }

    /// S5: this session's plan-mode runtime. The `/plan` dispatch arm, the
    /// session's plan-policy closure, and its `exit_plan_mode` tool all
    /// bind to this instance — plan state cannot leak across sessions.
    pub fn plan(&self) -> &plan_mode::PlanModeRuntime {
        &self.exec.plan
    }

    /// This handle's orbit setup (S5: assembled per session by the
    /// registry — plan closure + session tools bound to this session's
    /// execution state). `None` in omp-compat mode.
    pub fn setup(&self) -> &Option<crate::rpc::worker::OrbitSetup> {
        &self.orbit_setup
    }

    /// Whether this handle drives the kymido orbit engine in-process (`Some`
    /// orbit setup) instead of an external omp worker (G8).
    ///
    /// The split changes run bookkeeping fundamentally: an omp `prompt`
    /// blocks for the whole turn and its return value *is* the turn's
    /// terminal state, so `dispatch` closes the run synchronously.  An orbit
    /// `prompt` only acknowledges the message was queued — the turn runs on
    /// the engine's serial thread and ends later, when the event pump
    /// forwards `WorkerEvent::AgentEnd`.  Closing on the ack would make every
    /// run look finished the instant it started.
    pub fn is_orbit(&self) -> bool {
        self.setup().is_some()
    }

    /// PID of the underlying omp worker (0 if not yet spawned).
    pub fn child_pid(&self) -> u32 {
        self.inner.as_ref().map(|w| w.child_pid()).unwrap_or(0)
    }

    /// Lazy-spawn the worker if it isn't running yet, and register the
    /// daemon-owned `session_query` tool so the agent sees exactly one
    /// daemon-backed entry to the session store.
    fn ensure_started(&mut self) -> Result<(), Response> {
        if self.inner.is_none() {
            let mut w = crate::rpc::worker::Worker::new(&self.omp_path, self.orbit_setup.clone())
                .map_err(|e| {
                Response::err(
                    None,
                    ResponseError::new("worker_spawn_failed", e.to_string()),
                )
            })?;
            // Register session_query as the single daemon-backed tool. A
            // registration failure is non-fatal: omp might not implement
            // the call yet, and we don't want tool negotiation to take
            // the worker down. Log via stderr so operators see it.
            let def = crate::session_query::session_query_def();
            if let Err(e) = w.register_external_tools(vec![def]) {
                eprintln!("daemon: external tool registration failed: {e}");
            }
            self.inner = Some(w);
        }
        Ok(())
    }

    /// Drop the worker entirely; next call lazy-respawns.  The forwarder
    /// thread observes the closed event channel and clears `pump_active`.
    pub fn reset(&mut self) {
        self.clear_active_run();
        self.inner = None;
    }

    /// Replay persisted session history into the live worker before a prompt
    /// (B2a resume).  Delegates to the inner [`crate::rpc::worker::Worker`], which is
    /// a no-op returning 0 in omp mode (there is no in-process engine context
    /// to rebuild) and, in orbit mode, appends the user/assistant rows to the
    /// engine's context — deduped per session id inside the engine, so the
    /// daemon may safely call this on every prompt that carries a
    /// `session_id` without doubling the history.
    ///
    /// The worker must be live first: callers invoke
    /// [`Self::ensure_started`] before this.  A no-op `Ok(0)` on a handle
    /// that was just `reset()` keeps the resume contract total.
    pub fn resume_session(
        &mut self,
        session_id: &str,
        messages: &[SessionMessage],
    ) -> Result<(), Response> {
        if self.inner.is_none() {
            return Ok(());
        }
        let w = self.inner.as_mut().expect("inner checked");
        match w.resume_session(session_id, messages) {
            Ok(_) => Ok(()),
            Err(e) => Err(Response::err(
                None,
                ResponseError::new("worker_resume_failed", e.to_string()),
            )),
        }
    }

    /// Start the worker-side event pump once (R2 3.3): `crate::rpc::worker::Worker::
    /// subscribe` hands the wire read loop to a pump thread; this daemon
    /// side forwarder drains that receiver and broadcasts [`EventFrame`]
    /// lines to every [`WORKER_TOPIC`] subscriber on the [`EventBus`].
    /// Serialized against every other worker use by the server's mutex.
    ///
    /// In orbit mode this thread is also the run's undertaker (G8): a prompt
    /// only acknowledges that the message was queued, so the ledger run and
    /// the session's turn log stay open until the pump forwards the turn's
    /// [`WorkerEvent::AgentEnd`].  `sessions` / `runs` are shared in for that
    /// close.
    pub fn ensure_event_pump(
        &mut self,
        events: &EventBus,
        sessions: &SessionState,
        runs: &RunLedger,
    ) -> Result<(), Response> {
        if self.pump_active.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.ensure_started()?;
        let w = self.inner.as_mut().expect("ensured");
        let rx = w.subscribe(WORKER_TOPIC);
        let active = Arc::clone(&self.pump_active);
        // S5: the session's execution-state cell (shared run slot + plan
        // runtime) — the pump stamps frames from it and releases the run
        // slot on `AgentEnd` by compare-and-reset.
        let exec = self.exec.clone();
        let bus = events.clone();
        let sessions = sessions.clone();
        let runs = runs.clone();
        // ESC (user-requested abort) books the run as paused, not killed.
        let user_abort = std::sync::Arc::clone(&w.user_abort);
        // Orbit runs end here, omp-compat runs end in `dispatch` when the
        // blocking prompt returns — the pump must not second-guess that
        // close (it would append a second TurnEnd per turn).
        let is_orbit = self.is_orbit();
        // S2: stamp frames with this handle's session key; the legacy
        // entry (empty key) stamps nothing, keeping old subscribers intact.
        let session_id = self.session_id.clone();
        active.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            // C01: the pump accumulates the run's streamed assistant text and
            // flushes it as the run's `Assistant` event at AgentEnd.  Tool
            // events are recorded by the orbit run thread, which is the only
            // layer that has the provider tool-call id (the wire frame the
            // pump sees carries none).
            let mut assistant_text = String::new();
            // Ends when the worker dies (rpc pump clears its subscriber
            // table).  Events with no subscribers are dropped by broadcast.
            while let Ok(event) = rx.recv() {
                let Ok(payload) = serde_json::to_value(&event) else {
                    continue;
                };
                let mut frame = EventFrame::new(WORKER_TOPIC, payload);
                // G7-B: attribute the frame to the run the in-flight prompt
                // declared, so subscribers route it to the owning run.
                // Read-only here; a prompt on another connection may be
                // setting the slot concurrently (the server serializes
                // prompts by the worker mutex, but the pump keeps draining
                // this run's events after that prompt returned).
                //
                // The run is snapshotted once and reused below: the finish
                // must close exactly the run this frame was attributed to,
                // not whatever a concurrent prompt left in the slot later.
                let attributed = exec.running_run_id();
                if let Some(run) = attributed.as_deref() {
                    frame = frame.with_run_id(run);
                }
                if !session_id.is_empty() {
                    frame = frame.with_session_id(&session_id);
                }
                // C01: accumulate the run's streamed assistant text under the
                // run this frame belongs to; it is flushed as the `Assistant`
                // event at AgentEnd below.
                if attributed.is_some()
                    && let WorkerEvent::Message { text } = &event
                {
                    assistant_text.push_str(text);
                }
                // G8: an orbit turn's terminal event is AgentEnd on this
                // stream, not the prompt's ack. Close the ledger run and the
                // turn log here, before the frame goes out, so a subscriber
                // reading the end frame sees a run that is already closed.
                // Then release the sticky slot (compare-and-set — see
                // [`try_clear_active_run`]) so later unattributed events do
                // not keep stamping a run that already ended.
                if is_orbit
                    && let WorkerEvent::AgentEnd { stop_reason } = &event
                    && let Some(run) = attributed.as_deref()
                {
                    // Flush the accumulated assistant text as the run's
                    // `Assistant` event before the run closes, keyed by turn
                    // identity so the client's post-turn `session.append`
                    // import of the same message collapses onto this row.
                    if !assistant_text.is_empty() {
                        record_context_event(
                            &sessions,
                            LedgerEvent {
                                session_id: &session_id,
                                run_id: Some(run),
                                kind: ContextEventKind::Assistant,
                                source_kind: SourceKind::Model,
                                role: "assistant",
                                payload: json!({ "text": assistant_text }),
                                tool_call_id: None,
                                idempotency_key: turn_event_key(&session_id, "assistant", run),
                            },
                        );
                    }
                    assistant_text.clear();
                    let user_paused = user_abort.load(std::sync::atomic::Ordering::SeqCst);
                    close_run_on_agent_end(&runs, &sessions, run, stop_reason, user_paused);
                    exec.try_clear_run(run);
                }
                let Ok(line) = serde_json::to_string(&frame) else {
                    continue;
                };
                bus.broadcast(WORKER_TOPIC, &line);
            }
            active.store(false, Ordering::SeqCst);
        });
        Ok(())
    }
}

/// Finish a run and append its `TurnEnd` when the orbit engine signals the
/// end of the turn (G8).  Idempotent: a run already closed (a repeated
/// `AgentEnd` for the same turn) is left alone, so the turn log keeps exactly
/// one `TurnEnd` per `TurnStart` — that start/end balance is exactly what
/// [`session::interrupted_run_closers`] walks at startup.
fn close_run_on_agent_end(
    runs: &RunLedger,
    sessions: &SessionState,
    run_id: &str,
    stop_reason: &str,
    user_paused: bool,
) {
    // Snapshot before finishing: the session id has to survive a concurrent
    // close of the same run, and a run that is already done is not ours to
    // close.
    let Some(record) = runs.get(run_id) else {
        return;
    };
    if record.finished_at_ms.is_some() {
        return;
    }
    // A user-initiated stop (ESC) is resumable, not a kill: book it as
    // "paused" so the board can distinguish it from a crashed/killed run.
    let status = if user_paused && stop_reason == "aborted" {
        "paused"
    } else {
        crate::state::agent_end_status(stop_reason)
    };
    let ts_ms = crate::state::now_ms();
    if let Err(e) = runs.finish(run_id, ts_ms, status) {
        eprintln!("daemon: AgentEnd close failed for run {run_id}: {e}");
    }
    record_turn(
        sessions,
        &record.session_id,
        run_id,
        TurnRecord::TurnEnd {
            run_id: run_id.to_string(),
            ts_ms,
            status: status.into(),
        },
    );
    // C01: the run's terminal ledger event.  `recent_complete_runs` keys on
    // `turn_end`/`abort`, so this row is what makes the run replayable; it
    // MUST carry the run id.  An abort is booked as `Abort`, everything else
    // as `TurnEnd` — one terminal event per run, never both.
    record_terminal_event(sessions, &record.session_id, run_id, stop_reason, status);
}

/// Per-connection dispatch context.  Carries the shared state + the worker
/// handle for this request's target session (S1 routing: the server locks
/// that session's handle from the registry for the single request).
pub struct DispatchCtx<'a> {
    pub sessions: SessionState,
    pub runs: RunLedger,
    pub worker: &'a mut WorkerHandle,
    pub started_at_ms: i64,
    pub shutdown: &'a std::sync::atomic::AtomicBool,
    /// Push-event fan-out table (R2 3.3).
    pub events: EventBus,
    /// Identity of the connection being dispatched, for subscription
    /// teardown on disconnect.
    pub conn_id: u64,
    /// This connection's write channel; subscriptions clone it.
    pub out: Sender<String>,
    /// Where the CLI writes `tasks.jsonl` (the `.kymido` data dir).
    /// `task.list` builds a `store::Store` here per request — the store
    /// is a stateless `PathBuf` wrapper, so there is nothing to cache.
    pub task_data_dir: std::path::PathBuf,
    /// Pending user questions (plan-mode review and friends).
    pub questions: std::sync::Arc<crate::questions::QuestionBroker>,
    /// Project registry (A2): persisted list of registered working dirs.
    pub projects: &'a crate::projects::ProjectStore,
}

/// Dispatch a single request.  Always returns a `Response`; the caller just
pub fn dispatch(ctx: &mut DispatchCtx<'_>, req: Request) -> Response {
    let id = req.id.as_deref();
    match req.command {
        // ---------------- Daemon-level ----------------
        Command::Ping => Response::ok(id, json!({ "pong": true })),

        Command::Shutdown => {
            // Normally short-circuited in `connection_read_loop` before
            // the worker lock (a prompt holds it for the whole turn;
            // answering shutdown late hangs `kymido daemon stop`). This arm is
            // the fallback — keep its payload identical to that path.
            ctx.shutdown
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Response::ok(id, json!({ "shutting_down": true }))
        }

        Command::Info => Response::ok(
            id,
            json!({
                "pid": std::process::id(),
                "started_at_ms": ctx.started_at_ms,
                "uptime_ms": crate::state::now_ms() - ctx.started_at_ms,
                "worker_pid": ctx.worker.child_pid(),
            }),
        ),

        // ---------------- Session ----------------
        Command::SessionCreate => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let title = match require_str(&req.params, "title") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            // G7-A1: optional lineage parent. Absent / null / blank → root,
            // so a client that predates parent_id keeps working untouched.
            let parent_id = req
                .params
                .get("parent_id")
                .and_then(Value::as_str)
                .filter(|p| !p.trim().is_empty());
            match ctx
                .sessions
                .ensure_session_with_parent(sid, title, parent_id)
            {
                Ok(row) => match serde_json::to_value(&row) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(
                        id,
                        ResponseError::new("internal", format!("serialize: {e}")),
                    ),
                },
                Err(e) => session_error_response(id, "session.create", e),
            }
        }

        Command::SessionUpdateTitle => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let title = match require_str(&req.params, "title") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            // UPDATE-only rename: a missing session is a `database_missing`
            // error response (never an insert), same translation as
            // `session.create`'s store errors.
            match ctx.sessions.update_title(sid, title) {
                Ok(row) => match serde_json::to_value(&row) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(
                        id,
                        ResponseError::new("internal", format!("serialize: {e}")),
                    ),
                },
                Err(e) => session_error_response(id, "session.update_title", e),
            }
        }

        Command::SessionGet => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.sessions.session(sid) {
                Ok(Some(row)) => match serde_json::to_value(&row) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(
                        id,
                        ResponseError::new("internal", format!("serialize: {e}")),
                    ),
                },
                Ok(None) => Response::ok(id, Value::Null),
                Err(e) => session_error_response(id, "session.get", e),
            }
        }

        Command::SessionList => {
            let q = match require_str(&req.params, "query") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let limit = match require_u32(&req.params, "limit") {
                Ok(n) => n,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.sessions.list_sessions(q, limit) {
                Ok(rows) => match serde_json::to_value(&rows) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(
                        id,
                        ResponseError::new("internal", format!("serialize: {e}")),
                    ),
                },
                Err(e) => session_error_response(id, "session.list", e),
            }
        }

        Command::SessionDelete => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.sessions.delete_session(sid) {
                Ok(deleted) => Response::ok(id, json!({ "deleted": deleted })),
                Err(e) => session_error_response(id, "session.delete", e),
            }
        }

        // ---------------- Run ----------------
        Command::RunList => {
            let limit = match require_u32(&req.params, "limit") {
                Ok(n) => n,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let mut runs = ctx.runs.list();
            let keep_from = runs.len().saturating_sub(limit as usize);
            runs.drain(..keep_from);
            match serde_json::to_value(&runs) {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::err(
                    id,
                    ResponseError::new("internal", format!("serialize: {e}")),
                ),
            }
        }

        // ---------------- Task ----------------
        Command::TaskList => {
            // `limit` is optional (run.list's is required): the board
            // sends a page size only once it paginates, a bare `{}` is
            // the default page.  A missing / non-number falls back too
            // — a stale client must not break a newer daemon.
            paged_list(
                id,
                &req.params,
                |limit| {
                    let store = store::store::Store::new(&ctx.task_data_dir);
                    let mut tasks = match store.load_all() {
                        Ok(t) => t,
                        Err(e) => return Err(format!("task store: {e}")),
                    };
                    tasks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                    tasks.truncate(limit as usize);
                    Ok(tasks)
                },
                |rows: &mut [store::Task]| {
                    rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                },
            )
        }

        // ---------------- Todo ----------------
        Command::TodoList => {
            // Same contract as `task.list`: `limit` optional, a stale
            // client's missing / non-number value falls back to the
            // default page rather than failing.
            paged_list(
                id,
                &req.params,
                |limit| {
                    let store = store::store::Store::new(&ctx.task_data_dir);
                    let mut todos = match store.load_todos() {
                        Ok(t) => t,
                        Err(e) => return Err(format!("todo store: {e}")),
                    };
                    todos.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                    todos.truncate(limit as usize);
                    Ok(todos)
                },
                |rows: &mut [store::todo::Todo]| {
                    rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                },
            )
        }

        // ---------------- Goal ----------------
        Command::GoalList => {
            // Same contract as `task.list` / `todo.list` (shared page
            // size constant — todos, goals and tasks page alike).
            paged_list(
                id,
                &req.params,
                |limit| {
                    let store = store::store::Store::new(&ctx.task_data_dir);
                    let mut goals = match store.load_goals() {
                        Ok(g) => g,
                        Err(e) => return Err(format!("goal store: {e}")),
                    };
                    goals.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                    goals.truncate(limit as usize);
                    Ok(goals)
                },
                |rows: &mut [store::goal::Goal]| {
                    rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                },
            )
        }

        // ---------------- Stats (G5) ----------------
        Command::StatsSummary => {
            // `range` is optional: absent / unknown falls back to "24h"
            // (see `StatsRange::parse`), so a bare `{}` is a valid call.
            let range = req
                .params
                .get("range")
                .and_then(Value::as_str)
                .unwrap_or("24h");
            let summary = ctx.runs.stats(range, crate::state::now_ms());
            match serde_json::to_value(&summary) {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::err(
                    id,
                    ResponseError::new("internal", format!("serialize: {e}")),
                ),
            }
        }

        Command::SessionAppend => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let role_str = match require_str(&req.params, "role") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let text = match require_str(&req.params, "text") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let role = match SessionRole::parse(role_str) {
                Ok(r) => r,
                Err(_) => {
                    return Response::err(
                        id,
                        ResponseError::new("protocol", format!("unknown role `{role_str}`")),
                    );
                }
            };
            let attachments = match crate::state::optional_attachments(&req.params) {
                Ok(a) => a,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let tool_calls = match crate::state::optional_tool_calls(&req.params) {
                Ok(t) => t,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let run_id = req
                .params
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            match ctx
                .sessions
                .append_message(sid, role, text, &attachments, &tool_calls)
            {
                Ok((seq, ts)) => {
                    // C01: `session.append` stays the UI read model
                    // (`messages`, a different fact store) *and* feeds the
                    // canonical ledger as the explicit import interface.  To
                    // avoid booking the same turn twice, an append that
                    // belongs to a turn reuses that turn's idempotency key:
                    //   * user  → keyed by the assigned `seq`; the daemon's
                    //     prompt-time `Prompt` write looks up the newest
                    //     message and reuses this same key, so one user turn
                    //     is one row (the import lands first and wins);
                    //   * assistant → attributed to the session's most recent
                    //     run, matching the daemon's AgentEnd `Assistant` key;
                    //   * no turn identity (external history) → a seq-keyed
                    //     run-less import replayed by resume as such.
                    let (kind, source_kind) = match role {
                        SessionRole::User => (ContextEventKind::Prompt, SourceKind::User),
                        SessionRole::Assistant => (ContextEventKind::Assistant, SourceKind::Model),
                        SessionRole::System | SessionRole::Tool => {
                            (ContextEventKind::Imported, SourceKind::Import)
                        }
                    };
                    let role_str = ledger_role(role);
                    let attributed_run: Option<String> = if !run_id.is_empty() {
                        Some(run_id.to_string())
                    } else if role == SessionRole::Assistant {
                        ctx.runs.latest_run_id_for_session(sid)
                    } else {
                        None
                    };
                    let key = match attributed_run.as_deref() {
                        Some(run) if !run_id.is_empty() => turn_event_key(sid, role_str, run),
                        Some(run) if role == SessionRole::Assistant => {
                            turn_event_key(sid, "assistant", run)
                        }
                        _ => format!("import:{sid}:{role_str}:{seq}"),
                    };
                    record_context_event(
                        &ctx.sessions,
                        LedgerEvent {
                            session_id: sid,
                            run_id: attributed_run.as_deref(),
                            kind,
                            source_kind,
                            role: role_str,
                            payload: json!({ "text": text }),
                            tool_call_id: None,
                            idempotency_key: key,
                        },
                    );
                    Response::ok(id, json!({ "seq": seq, "created_at_ms": ts }))
                }
                Err(e) => session_error_response(id, "session.append", e),
            }
        }

        Command::SessionLoadMessages => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let limit = match require_u32(&req.params, "limit") {
                Ok(n) => n,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.sessions.load_messages(sid, limit) {
                Ok(rows) => match serde_json::to_value(&rows) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(
                        id,
                        ResponseError::new("internal", format!("serialize: {e}")),
                    ),
                },
                Err(e) => session_error_response(id, "session.load_messages", e),
            }
        }

        Command::SessionSearch => {
            let q = match require_str(&req.params, "query") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let scope = req
                .params
                .get("scope")
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str);
            let limit = match require_u32(&req.params, "limit") {
                Ok(n) => n,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.sessions.search_messages(q, scope, limit) {
                Ok(rows) => match serde_json::to_value(&rows) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(
                        id,
                        ResponseError::new("internal", format!("serialize: {e}")),
                    ),
                },
                Err(e) => session_error_response(id, "session.search", e),
            }
        }

        Command::SessionTruncate => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            // Required, never defaulted: a missing `from_seq` falling back to
            // 0 would silently empty the session.
            let from_seq = match req.params.get("from_seq").and_then(Value::as_i64) {
                Some(n) => n,
                None => {
                    return Response::err(
                        id,
                        ResponseError::new("protocol", "missing required numeric field `from_seq`"),
                    );
                }
            };
            match ctx.sessions.truncate_messages(sid, from_seq) {
                Ok(deleted) => Response::ok(id, json!({ "deleted": deleted })),
                Err(e) => session_error_response(id, "session.truncate", e),
            }
        }

        Command::SessionRewind => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            // Required, never defaulted — same reasoning as
            // `session.truncate`: a missing `from_seq` falling back to 0
            // would silently snapshot-and-empty the session.
            let from_seq = match req.params.get("from_seq").and_then(Value::as_i64) {
                Some(n) => n,
                None => {
                    return Response::err(
                        id,
                        ResponseError::new("protocol", "missing required numeric field `from_seq`"),
                    );
                }
            };
            match ctx.sessions.rewind_messages(sid, from_seq) {
                Ok(snapshotted) => Response::ok(id, json!({ "snapshotted": snapshotted })),
                Err(e) => session_error_response(id, "session.rewind", e),
            }
        }

        Command::SessionReadFromCursor => {
            let cursor = req
                .params
                .get("cursor")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let (runs, next_cursor) = ctx.runs.read_from_cursor(cursor);
            Response::ok(id, json!({ "runs": runs, "cursor": next_cursor }))
        }

        // ---------------- Session archive (soft delete / restore) ----------------
        Command::SessionArchive => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.sessions.archive_session(sid) {
                Ok(receipt) => Response::ok(
                    id,
                    json!({
                        "session_id": receipt.session_id,
                        "messages": receipt.messages,
                        "raw_bytes": receipt.raw_bytes,
                        "archived_bytes": receipt.archived_bytes,
                    }),
                ),
                Err(e) => session_error_response(id, "session.archive", e),
            }
        }

        Command::SessionListArchived => match ctx.sessions.list_archived() {
            Ok(rows) => match serde_json::to_value(&rows) {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::err(
                    id,
                    ResponseError::new("internal", format!("serialize: {e}")),
                ),
            },
            Err(e) => session_error_response(id, "session.list_archived", e),
        },

        Command::SessionRestore => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.sessions.restore_session(sid) {
                Ok(summary) => match serde_json::to_value(&summary) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(
                        id,
                        ResponseError::new("internal", format!("serialize: {e}")),
                    ),
                },
                Err(e) => session_error_response(id, "session.restore", e),
            }
        }

        Command::SessionPurge => {
            let sid = match require_str(&req.params, "session_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.sessions.purge_archive(sid) {
                Ok(purged) => Response::ok(id, json!({ "purged": purged })),
                Err(e) => session_error_response(id, "session.purge", e),
            }
        }

        // ---------------- Project registry (A2) ----------------
        Command::ProjectList => {
            let rows = ctx.projects.list();
            match serde_json::to_value(&rows) {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::err(
                    id,
                    ResponseError::new("internal", format!("serialize: {e}")),
                ),
            }
        }

        Command::ProjectCreate => {
            let path = match require_str(&req.params, "path") {
                Ok(p) => p,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.projects.create(path) {
                Ok(entry) => match serde_json::to_value(&entry) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(
                        id,
                        ResponseError::new("internal", format!("serialize: {e}")),
                    ),
                },
                Err(e) => Response::err(id, ResponseError::new("project", e.to_string())),
            }
        }

        Command::ProjectRemove => {
            let pid = match require_str(&req.params, "project_id") {
                Ok(p) => p,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            match ctx.projects.remove(pid) {
                Ok(removed) => Response::ok(id, json!({ "removed": removed })),
                Err(e) => Response::err(id, ResponseError::new("project", e.to_string())),
            }
        }

        // ---------------- Worker ----------------
        Command::WorkerPing => {
            if let Err(e) = ctx.worker.ensure_started() {
                return e;
            }
            let w = ctx.worker.inner.as_mut().expect("ensured");
            match w.ping() {
                Ok(()) => Response::ok(id, json!({ "pong": true })),
                Err(e) => {
                    Response::err(id, ResponseError::new("worker_ping_failed", e.to_string()))
                }
            }
        }

        Command::WorkerPrompt => {
            let msg = match require_str(&req.params, "message") {
                Ok(s) => s.to_string(),
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            // Turn boundary: land a change parked by an approved exit
            // (`prepare_approved_exit` commits pending=false without
            // touching active). The next prompt is that boundary — without
            // this the exit never applies and plan mode stays active
            // forever. Best-effort: a poisoned mutex cannot be fixed here.
            if let Ok(Some(mutation)) = ctx.worker.plan().prepare_boundary() {
                let _ = mutation.commit();
            }
            // `/plan` family: flip plan-mode state between turns instead of
            // prompting. A message argument enters plan mode first and then
            // falls through, so the text still reaches the model.
            let mut prompt_text: Option<String> = None;
            if let Some(parsed) = ctx.worker.plan().parse_command(&msg) {
                let command = match parsed {
                    Ok(c) => c,
                    Err(e) => {
                        return Response::err(
                            id,
                            ResponseError::new("plan_command_invalid", e.to_string()),
                        );
                    }
                };
                let target = match command {
                    plan_mode::PlanModeCommand::Enter { .. } => true,
                    plan_mode::PlanModeCommand::Off => false,
                };
                if let plan_mode::PlanModeCommand::Enter {
                    message: Some(text),
                } = command
                {
                    prompt_text = Some(text)
                }
                match ctx.worker.plan().prepare_set(target) {
                    Ok(Some(mutation)) => {
                        if let Err(e) = mutation.commit() {
                            return Response::err(
                                id,
                                ResponseError::new("plan_command_failed", e.to_string()),
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        // A pending change exists (mid-turn selection); the
                        // boundary hook lands it later. Report, don't queue.
                        return Response::err(
                            id,
                            ResponseError::new("plan_command_pending", e.to_string()),
                        );
                    }
                }
                if prompt_text.is_none() {
                    return Response::ok(
                        id,
                        json!({
                            "plan_mode": ctx.worker.plan().active().unwrap_or(false),
                            "prompted": false,
                        }),
                    );
                }
            }
            let msg = prompt_text.as_deref().unwrap_or(&msg);
            // Optional session_id + run_id: when provided, we record a
            // run in the ledger so the client can correlate across
            // reconnects.
            let session_id = req
                .params
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let run_id = req
                .params
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let started = crate::state::now_ms();
            if !run_id.is_empty() {
                let _ = ctx.runs.start(run_id, session_id, started);
            }
            record_turn(
                &ctx.sessions,
                session_id,
                run_id,
                TurnRecord::TurnStart {
                    run_id: run_id.to_string(),
                    ts_ms: started,
                },
            );
            // C01: the daemon owns the canonical write for the current run.
            // Record the accepted prompt as the run's opening `Prompt` event.
            // When the client already imported the same user turn through
            // `session.append` (the UI path), this write reuses the import's
            // seq-derived key and is an idempotent no-op, so one turn stays
            // one ledger row.  A prompt with no run id has no turn identity to
            // key on, so it is left to the explicit import path rather than
            // written twice.
            if !run_id.is_empty() {
                // A client that persisted the user message just before
                // prompting (`session.append`, the UI path) already owns the
                // canonical row, keyed by that message's `seq`.  Reuse the
                // same key so one user turn is booked once — the import lands
                // first and this write becomes an idempotent no-op.  With no
                // preceding user append (a bare prompt) the turn identity is
                // the only key available.
                let appended_seq = ctx
                    .sessions
                    .load_messages(session_id, 1)
                    .ok()
                    .and_then(|mut rows| rows.pop())
                    .filter(|m| m.role == SessionRole::User)
                    .map(|m| m.seq);
                let key = match appended_seq {
                    Some(seq) => format!("import:{session_id}:user:{seq}"),
                    None => turn_event_key(session_id, "user", run_id),
                };
                record_context_event(
                    &ctx.sessions,
                    LedgerEvent {
                        session_id,
                        run_id: Some(run_id),
                        kind: ContextEventKind::Prompt,
                        source_kind: SourceKind::User,
                        role: "user",
                        payload: json!({ "text": msg }),
                        tool_call_id: None,
                        idempotency_key: key,
                    },
                );
            }
            // G8: in orbit mode the run's AgentEnd is consumed by the event
            // pump, which was previously started lazily on the first
            // `event.subscribe`. A client that prompts without subscribing
            // would leave every run half-open forever — the exact state
            // this change exists to eliminate. Start the pump before the
            // prompt goes out so the close path is wired regardless of
            // subscriptions. Idempotent (`pump_active` guards the spawn).
            if ctx.worker.is_orbit()
                && let Err(e) = ctx
                    .worker
                    .ensure_event_pump(&ctx.events, &ctx.sessions, &ctx.runs)
            {
                return e;
            }

            if let Err(e) = ctx.worker.ensure_started() {
                if !run_id.is_empty() {
                    let _ = ctx
                        .runs
                        .finish(run_id, crate::state::now_ms(), "spawn_failed");
                }
                record_turn(
                    &ctx.sessions,
                    session_id,
                    run_id,
                    TurnRecord::TurnEnd {
                        run_id: run_id.to_string(),
                        ts_ms: crate::state::now_ms(),
                        status: "failed".into(),
                    },
                );
                record_terminal_event(&ctx.sessions, session_id, run_id, "spawn_failed", "failed");
                return e;
            }
            // G7-B: declare the run this turn's events belong to before the
            // prompt goes out.  The pump stamps it on every frame it pushes
            // while the slot holds it (sticky — see `set_active_run`).
            ctx.worker.set_active_run(run_id);
            // B2a / C01 resume: replay the session's persisted history
            // into the worker's live context before the prompt so a
            // restarted daemon (or a respawned engine) does not start
            // from a blank slate.  Best-effort — a load or replay failure
            // must not block the prompt (the turn still runs, it just
            // misses the history).  In orbit mode the engine dedupes per
            // session id, so calling this on every prompt is safe; in
            // omp mode it is `Ok(0)`.
            //
            // C01: context is rebuilt by walking complete runs and
            // pairing closed tool groups, so `tool_call_id` pairs stay
            // intact and missing results are surfaced as interruptions
            // (never fabricated as successes).  See `build_resume_context`.
            if !session_id.is_empty() {
                match build_resume_context(&ctx.sessions, session_id) {
                    Ok(msgs) => {
                        if let Err(e) = ctx.worker.resume_session(session_id, &msgs) {
                            eprintln!(
                                "daemon: session resume failed for {session_id} (continuing without history): {e:?}"
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "daemon: could not rebuild session {session_id} context for resume (continuing without history): {e}"
                        );
                    }
                }
            }
            let attachments = match crate::state::optional_attachments(&req.params) {
                Ok(a) => a,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            let w = ctx.worker.inner.as_mut().expect("ensured");
            let resp = w.prompt(msg, &attachments);
            let finished = crate::state::now_ms();
            match &resp {
                Ok(v) => {
                    if ctx.worker.is_orbit() {
                        // G8: an orbit prompt only acknowledges that the
                        // message reached the engine's run thread — the turn
                        // itself is still in flight and ends later, when the
                        // event pump forwards `AgentEnd`.  Closing the run
                        // here would make every orbit run look finished the
                        // instant it started: `in_flight_runs` pinned at 0,
                        // the session state machine's three states
                        // unreachable, and the half-open turn-log entry that
                        // `interrupted_run_closers` exists to repair could
                        // never appear in a live log.  Just ack the client;
                        // `ensure_event_pump` owns the close.
                        return Response::ok(id, v.clone());
                    }
                    // omp-compat: `prompt` blocks for the whole turn, so its
                    // return *is* the terminal state — close synchronously.
                    if !run_id.is_empty()
                        && let Err(e) = ctx.runs.finish(run_id, finished, "ok")
                    {
                        eprintln!("daemon: run finish failed for {run_id}: {e}");
                    }
                    record_turn(
                        &ctx.sessions,
                        session_id,
                        run_id,
                        TurnRecord::TurnEnd {
                            run_id: run_id.to_string(),
                            ts_ms: finished,
                            status: "ok".into(),
                        },
                    );
                    record_terminal_event(&ctx.sessions, session_id, run_id, "ok", "ok");
                    Response::ok(id, v.clone())
                }
                Err(e) => {
                    // The message never reached the engine (orbit: run
                    // channel closed; omp: wire error), so the run did not
                    // start at all — closing it as failed here is correct in
                    // either mode.
                    if !run_id.is_empty()
                        && let Err(e) = ctx.runs.finish(run_id, finished, "failed")
                    {
                        eprintln!("daemon: run finish-failed marking failed for {run_id}: {e}");
                    }
                    record_turn(
                        &ctx.sessions,
                        session_id,
                        run_id,
                        TurnRecord::TurnEnd {
                            run_id: run_id.to_string(),
                            ts_ms: finished,
                            status: "failed".into(),
                        },
                    );
                    record_terminal_event(&ctx.sessions, session_id, run_id, "error", "failed");
                    Response::err(
                        id,
                        ResponseError::new("worker_prompt_failed", e.to_string()),
                    )
                }
            }
        }

        Command::WorkerSteer => {
            let msg = match require_str(&req.params, "message") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            if let Err(e) = ctx.worker.ensure_started() {
                return e;
            }
            let w = ctx.worker.inner.as_mut().expect("ensured");
            match w.steer(msg) {
                Ok(v) => Response::ok(id, v),
                Err(e) => {
                    Response::err(id, ResponseError::new("worker_steer_failed", e.to_string()))
                }
            }
        }

        Command::WorkerAbort => {
            if let Err(e) = ctx.worker.ensure_started() {
                return e;
            }
            let w = ctx.worker.inner.as_mut().expect("ensured");
            match w.abort() {
                Ok(v) => Response::ok(id, v),
                Err(e) => {
                    Response::err(id, ResponseError::new("worker_abort_failed", e.to_string()))
                }
            }
        }
        Command::WorkerReadEvent => {
            if let Err(e) = ctx.worker.ensure_started() {
                return e;
            }
            let w = ctx.worker.inner.as_mut().expect("ensured");
            match w.read_event() {
                Ok(ev) => match serde_json::to_value(ev) {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(id, ResponseError::new("internal", e.to_string())),
                },
                Err(e) => Response::err(
                    id,
                    ResponseError::new("worker_read_event_failed", e.to_string()),
                ),
            }
        }

        Command::UserAnswer => {
            let qid = match require_str(&req.params, "question_id") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            if qid.len() > crate::questions::MAX_QUESTION_ID_BYTES {
                return Response::err(id, ResponseError::new("protocol", "question id too long"));
            }
            let answer: crate::questions::QuestionAnswer = match req.params.get("answer") {
                Some(value) => match serde_json::from_value(value.clone()) {
                    Ok(a) => a,
                    Err(e) => {
                        return Response::err(
                            id,
                            ResponseError::new("protocol", format!("malformed answer: {e}")),
                        );
                    }
                },
                None => {
                    return Response::err(id, ResponseError::new("protocol", "answer is required"));
                }
            };
            match ctx.questions.answer(qid, answer) {
                Ok(()) => Response::ok(id, json!({ "answered": true })),
                Err(e) => Response::err(id, e.into()),
            }
        }
        Command::UserQuestionPending => {
            let pending = ctx.questions.pending();
            Response::ok(
                id,
                serde_json::to_value(pending).unwrap_or_else(|_| json!([])),
            )
        }

        // ---------------- Events (R2 3.3) ----------------
        Command::EventSubscribe => {
            let topic = match require_str(&req.params, "topic") {
                Ok(s) => s,
                Err(m) => return Response::err(id, ResponseError::new("protocol", m)),
            };
            if topic == WORKER_TOPIC
                && let Err(e) = ctx
                    .worker
                    .ensure_event_pump(&ctx.events, &ctx.sessions, &ctx.runs)
            {
                return e;
            }
            let sub_id = ctx.events.subscribe(topic, ctx.conn_id, ctx.out.clone());
            Response::ok(id, json!({ "subscription_id": sub_id, "topic": topic }))
        }

        Command::EventUnsubscribe => {
            let Some(sub_id) = req.params.get("subscription_id").and_then(Value::as_u64) else {
                return Response::err(
                    id,
                    ResponseError::new("protocol", "subscription_id (u64) is required"),
                );
            };
            let removed = ctx.events.unsubscribe(sub_id);
            Response::ok(id, json!({ "removed": removed }))
        }
    }
}

/// Record one run boundary in the session's durable turn log. Best-effort
/// and invisible to the protocol: a caller that omits `session_id` / `run_id`
/// gets no turn log. A storage failure is logged rather than propagated (the
/// ledger write next to it already ignores its own errors) — but it stays
/// visible, because the persisted log is what startup crash repair walks, so
/// a silent failure here would later surface as an unrepaired run.
/// See [`crate::server::Daemon::repair_interrupted_runs`].
fn record_turn(sessions: &SessionState, session_id: &str, run_id: &str, record: TurnRecord) {
    if session_id.is_empty() || run_id.is_empty() {
        return;
    }
    if let Err(e) = sessions.append_turn_log(session_id, &[record]) {
        eprintln!("daemon: turn log append failed for session {session_id}: {e}");
    }
}

/// Append the run's terminal C01 ledger event at a synchronous close (the
/// omp-compat prompt path, where the blocking prompt's return *is* the turn
/// end, and the spawn-failed path).  Mirrors [`close_run_on_agent_end`],
/// which owns the same write for orbit runs; both key on `turn_end` so
/// `recent_complete_runs` sees exactly one terminal row per run.  An aborted
/// stop is booked as `Abort`, everything else as `TurnEnd`.
fn record_terminal_event(
    sessions: &SessionState,
    session_id: &str,
    run_id: &str,
    stop_reason: &str,
    status: &str,
) {
    if session_id.is_empty() || run_id.is_empty() {
        return;
    }
    let (kind, key_role) = if stop_reason == "aborted" {
        (ContextEventKind::Abort, "abort")
    } else {
        (ContextEventKind::TurnEnd, "end")
    };
    record_context_event(
        sessions,
        LedgerEvent {
            session_id,
            run_id: Some(run_id),
            kind,
            source_kind: SourceKind::System,
            role: "system",
            payload: json!({ "status": status, "stop_reason": stop_reason }),
            tool_call_id: None,
            idempotency_key: format!("turn:{session_id}:{key_role}:{run_id}"),
        },
    );
}

fn session_error_response(
    id: Option<&str>,
    command: &'static str,
    e: session::SessionError,
) -> Response {
    use session::SessionError;
    let code = match &e {
        SessionError::InvalidSessionId => "invalid_session_id",
        SessionError::InvalidMessageText => "invalid_message_text",
        SessionError::InvalidListQuery => "invalid_list_query",
        SessionError::InvalidSearchQuery => "invalid_search_query",
        SessionError::InvalidLimit(_) => "invalid_limit",
        SessionError::UnknownRole(_) => "unknown_role",
        SessionError::DatabaseMissing(_) => "database_missing",
        SessionError::MalformedTurnLog { .. } => "malformed_turn_log",
        SessionError::Libsql(_)
        | SessionError::Io(_)
        | SessionError::RuntimeBuild(_)
        | SessionError::Archive(_) => "session_io",
        SessionError::Context(_) => "context_error",
    };
    Response::err(id, ResponseError::new(code, format!("{command}: {e}")))
}
