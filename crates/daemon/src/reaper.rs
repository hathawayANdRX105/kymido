//! S3: connection attach bookkeeping + 30s tree-gated session unloading.
//!
//! A session engine is *live* while it has at least one:
//!
//! - attached client connection (`AttachTracker` count > 0) — a connection
//!   attaches to a session by prompting it or subscribing to it, and
//!   detaches when the connection closes;
//! - in-flight run (the handle's sticky `active_run` slot, or an open run
//!   in the ledger);
//! - **live descendant**: any registry session whose `parent_id` chain in
//!   the session store reaches it. Tree gating: a parent stays hot while
//!   any child is still alive, whatever the parent's own attach count.
//!
//! When all three are absent the session becomes *eligible*; the reaper
//! keeps it hot for a 3-minute grace window and then unloads the engine
//! (context is already in the session store; re-attach lazily resumes).
//! The scan runs on a 30s tick, so unloading lags eligibility by at most
//! one tick.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::registry::SessionRegistry;
use crate::state::{RunLedger, SessionState};

/// Grace a fully-idle client session stays hot after its last activity.
pub const UNLOAD_GRACE: Duration = Duration::from_secs(180);

/// Reaper tick cadence.
pub const REAPER_TICK: Duration = Duration::from_secs(30);

/// Per-connection attach bookkeeping (S3). A connection "attaches" to the
/// sessions it prompts or subscribes to; the daemon counts live attachments
/// per session and the reaper only unloads sessions with zero of them.
#[derive(Clone, Default)]
pub struct AttachTracker {
    counts: Arc<Mutex<HashMap<String, u64>>>,
}

impl AttachTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count this connection as attached to `session_id`. Legacy (empty)
    /// sessions are never attached: they have no owner to keep alive.
    pub fn attach(&self, session_id: &str) {
        if session_id.is_empty() {
            return;
        }
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry(session_id.to_string()).or_insert(0) += 1;
    }

    /// Drop one attachment. Underflow floors at zero so a double-detach
    /// (e.g. connection teardown racing a target switch) can never make a
    /// session negative — it only loses the ghost count, never a real one.
    pub fn detach(&self, session_id: &str) {
        if session_id.is_empty() {
            return;
        }
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        match counts.get_mut(session_id) {
            Some(count) => {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    counts.remove(session_id);
                }
            }
            None => {}
        }
    }

    /// Whether any connection is currently attached to `session_id`.
    pub fn attached(&self, session_id: &str) -> bool {
        let counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        counts.get(session_id).is_some_and(|c| *c > 0)
    }
}

/// The 30s tree-gated unloader (S3). Cheaply cloneable; the tick thread
/// and the daemon share one instance.
#[derive(Clone)]
pub struct SessionReaper {
    registry: SessionRegistry,
    attach: AttachTracker,
    sessions: SessionState,
    runs: RunLedger,
    /// First time each session observed eligible; the grace clock runs only
    /// while eligibility is continuous — any attach / run / descendant
    /// re-arms it from scratch.
    eligible_since: Arc<Mutex<HashMap<String, Instant>>>,
}

impl SessionReaper {
    pub fn new(
        registry: SessionRegistry,
        attach: AttachTracker,
        sessions: SessionState,
        runs: RunLedger,
    ) -> Self {
        Self {
            registry,
            attach,
            sessions,
            runs,
            eligible_since: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The shared attach bookkeeping (the connection loop attaches/detaches
    /// through it).
    pub fn attach_tracker(&self) -> &AttachTracker {
        &self.attach
    }

    /// One 30s scan pass: evaluate every live session and unload the ones
    /// whose grace elapsed. Public so tests can drive ticks deterministically.
    pub fn tick(&self) {
        let now = Instant::now();
        let live: Vec<String> = self.registry.session_ids();
        let mut eligible_since = self
            .eligible_since
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for sid in &live {
            let eligible = !self.attach.attached(sid)
                && !self.runs.has_open_run(sid)
                && !self.registry.with_session(sid, |h| h.has_active_run())
                && !self.has_live_descendant(sid);
            if eligible {
                let first = *eligible_since.entry(sid.clone()).or_insert(now);
                if now.duration_since(first) >= UNLOAD_GRACE {
                    self.registry.unload(sid);
                    eligible_since.remove(sid);
                }
            } else {
                eligible_since.remove(sid);
            }
        }
        // Prune bookkeeping for sessions that left the registry meanwhile.
        eligible_since.retain(|sid, _| self.registry.has(sid));
    }

    /// Tree gating (S3): `sid` stays alive while any **live** session has
    /// `sid` on its `parent_id` chain (subagent children). The walk
    /// uses the session store (the lineage survives unloading) with a
    /// visited-set cycle guard.
    fn has_live_descendant(&self, sid: &str) -> bool {
        self.registry
            .session_ids()
            .into_iter()
            .filter(|d| d != sid)
            .any(|descendant| self.is_ancestor_in_store(sid, &descendant))
    }

    /// Does the store's `parent_id` chain of `descendant` reach `anc`?
    fn is_ancestor_in_store(&self, anc: &str, descendant: &str) -> bool {
        let mut visited = HashSet::new();
        let mut current = Some(descendant.to_string());
        while let Some(id) = current {
            if !visited.insert(id.clone()) {
                break; // cycle guard: a corrupt chain must not loop the reaper
            }
            let Some(summary) = self.sessions.session(&id).ok().flatten() else {
                return false; // chain hit a deleted session: no live descent
            };
            let Some(parent) = summary.parent_id else {
                return false; // root of the tree
            };
            if parent == anc {
                return true;
            }
            current = Some(parent);
        }
        false
    }

    /// Spawn the 30s tick thread. Exits on `shutdown`; the thread is
    /// detached — it only touches shared state, nothing outlives the daemon.
    pub fn spawn_tick_thread(
        reaper: Arc<Self>,
        shutdown: &Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<thread::JoinHandle<()>, std::io::Error> {
        let shutdown = Arc::clone(shutdown);
        thread::Builder::new()
            .name("kymido-session-reaper".into())
            .spawn(move || {
                loop {
                    if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    reaper.tick();
                    // Sleep in 1s slices so shutdown (or a reconfigured
                    // cadence later) is honored without a long stall.
                    for _ in 0..REAPER_TICK.as_secs() {
                        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                            return;
                        }
                        thread::sleep(Duration::from_secs(1));
                    }
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::worker::{OrbitConfig, OrbitSetup};
    use agent_loop::orbit::LlmBackend;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;
    use tempfile::tempdir;

    /// The engine is never run in these tests: handles are lazy and the
    /// reaper only reads registry / attach / run state.
    struct NoBackend;

    impl LlmBackend for NoBackend {
        fn stream_cb(
            &self,
            _model: &llm::Model,
            _context: &llm::Context,
            _tools: &[llm::ToolDef],
            _signal: &AtomicBool,
            _emit: &mut dyn FnMut(&llm::StreamEvent),
        ) {
        }
    }

    fn stub_setup() -> OrbitSetup {
        OrbitSetup {
            model: llm::Model {
                api_key: "k".into(),
                model: "test".into(),
                base_url: None,
                max_tokens: None,
            },
            backend: Arc::new(NoBackend),
            config: OrbitConfig {
                cwd: None,
                max_turns: 1,
                compaction: Arc::new(agent_loop::compaction::CharBudgetPolicy::default()),
                catalog: Arc::new(tools::default_catalog()),
                mcp_tools: Arc::new(Vec::new()),
                session_tools: Arc::new(Vec::new()),
                plan_policy_section: None,
                aside_queue: Arc::new(std::sync::Mutex::new(VecDeque::new())),
            },
        }
    }

    struct Fixture {
        registry: SessionRegistry,
        reaper: SessionReaper,
        sessions: SessionState,
        runs: RunLedger,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempdir().expect("temp dir");
        let sessions = SessionState::new(
            session::SessionDb::open(dir.path().join("sessions.db")).expect("open session db"),
        );
        let runs =
            RunLedger::open_for_socket(&dir.path().join("daemon.sock")).expect("open ledger");
        let registry = SessionRegistry::new("/nonexistent/omp".into(), Some(stub_setup()));
        let reaper = SessionReaper::new(
            registry.clone(),
            AttachTracker::new(),
            sessions.clone(),
            runs.clone(),
        );
        Fixture {
            registry,
            reaper,
            sessions,
            runs,
            _dir: dir,
        }
    }

    /// Push the session's grace clock past `UNLOAD_GRACE` so the next tick
    /// unloads without sleeping the real 3-minute window.
    fn age_grace(reaper: &SessionReaper, sid: &str) {
        let mut map = reaper.eligible_since.lock().expect("eligible map poisoned");
        map.insert(
            sid.to_string(),
            Instant::now() - UNLOAD_GRACE - Duration::from_millis(1),
        );
    }

    #[test]
    fn idle_session_unloads_after_grace() {
        let f = fixture();
        f.registry.with_session("a", |_| {});
        assert!(f.registry.has("a"));

        f.reaper.tick(); // first observation: the grace clock starts now
        age_grace(&f.reaper, "a");
        f.reaper.tick(); // clock elapsed: the engine is dropped

        assert!(
            !f.registry.has("a"),
            "grace elapsed: the idle engine must unload"
        );
    }

    #[test]
    fn attached_session_stays_hot_and_detach_resumes_the_clock() {
        let f = fixture();
        f.registry.with_session("a", |_| {});
        f.reaper.attach_tracker().attach("a");

        f.reaper.tick();
        f.reaper.tick();
        assert!(
            f.reaper.eligible_since.lock().expect("map").is_empty(),
            "an attached session is never eligible, so no grace clock may start"
        );
        assert!(f.registry.has("a"));

        f.reaper.attach_tracker().detach("a");
        f.reaper.tick(); // eligibility (and the clock) start here
        age_grace(&f.reaper, "a");
        f.reaper.tick();
        assert!(!f.registry.has("a"), "detach must resume the grace clock");
    }

    #[test]
    fn open_run_defers_unload_until_the_run_closes() {
        let f = fixture();
        f.registry.with_session("a", |_| {});
        f.runs.start("r1", "a", 1_000).expect("ledger start");

        f.reaper.tick();
        assert!(
            f.reaper.eligible_since.lock().expect("map").is_empty(),
            "an open run keeps the session ineligible"
        );

        f.runs.finish("r1", 2_000, "ok").expect("ledger finish");
        f.reaper.tick(); // the clock starts when the run closes
        age_grace(&f.reaper, "a");
        f.reaper.tick();
        assert!(
            !f.registry.has("a"),
            "the grace clock runs from run close, not run start"
        );
    }

    #[test]
    fn live_descendant_keeps_its_parent_hot() {
        let f = fixture();
        f.sessions
            .ensure_session_with_parent("child", "c", Some("parent"))
            .expect("child row");
        f.sessions
            .ensure_session("parent", "p")
            .expect("parent row");
        f.registry.with_session("parent", |_| {});
        f.registry.with_session("child", |_| {});

        f.reaper.tick();
        {
            let map = f.reaper.eligible_since.lock().expect("map");
            assert!(
                !map.contains_key("parent"),
                "a live child must keep the parent hot"
            );
            assert!(map.contains_key("child"), "the child itself is eligible");
        }

        age_grace(&f.reaper, "child");
        f.reaper.tick(); // the child unloads; the parent only now becomes eligible
        assert!(!f.registry.has("child"));
        assert!(f.registry.has("parent"));

        age_grace(&f.reaper, "parent");
        f.reaper.tick();
        assert!(
            !f.registry.has("parent"),
            "with no live descendants left, the parent unloads"
        );
    }
}
