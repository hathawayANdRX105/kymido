//! Per-session worker registry (multi-session concurrency, S1).
//!
//! The daemon keeps one [`WorkerHandle`] per session: in orbit mode each
//! engine owns its context, run thread, abort flag, steering queue and
//! event pump, so N sessions can run concurrently. Handles are created
//! lazily on first use and removed by the reaper (S3) or daemon teardown.
//!
//! The legacy entry (empty session id) serves session-less prompts and the
//! whole omp-compat path, which stays single-session by construction: in
//! omp mode every request routes to the legacy entry, so a single external
//! worker process is never multiplied.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::dispatch::WorkerHandle;
use crate::rpc::worker::OrbitSetup;

/// Shared registry of per-session worker handles. Cheaply cloneable (the
/// map sits behind an `Arc`), like every other daemon-wide state handle.
#[derive(Clone)]
pub struct SessionRegistry {
    handles: Arc<Mutex<HashMap<String, Arc<Mutex<WorkerHandle>>>>>,
    omp_path: String,
    /// S5: the shared orbit template (model / backend / catalog / queues).
    /// Per-session stateful faces are re-bound at handle creation — see
    /// [`Self::session_setup`] — so no two sessions share plan or tool
    /// state.
    orbit_setup: Option<OrbitSetup>,
    /// S5: the question broker — per-session review ports are built
    /// against it.
    broker: Arc<crate::questions::QuestionBroker>,
    /// S5: the plan-policy section text (`harness.plan` config, daemon
    /// default when absent) that each session's policy closure injects
    /// while that session's plan mode is active.
    plan_section: String,
}

impl SessionRegistry {
    /// Build a registry; the first `handle()` call lazily constructs the
    /// legacy worker (same construction as the pre-registry single handle).
    pub fn new(
        omp_path: String,
        orbit_setup: Option<OrbitSetup>,
        broker: Arc<crate::questions::QuestionBroker>,
        plan_section: String,
    ) -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            omp_path,
            orbit_setup,
            broker,
            plan_section,
        }
    }

    /// Whether this registry runs the in-process orbit engine. Orbit mode
    /// is the only mode with per-session handles; omp-compat mode routes
    /// everything to the legacy entry.
    pub fn is_orbit(&self) -> bool {
        self.orbit_setup.is_some()
    }

    /// Resolve the routing key for a request: a non-empty session id in
    /// orbit mode, the legacy entry otherwise.
    pub fn target(&self, session_id: &str) -> String {
        if self.is_orbit() && !session_id.is_empty() {
            session_id.to_string()
        } else {
            String::new()
        }
    }

    /// Whether a live handle exists for `session_id` (registry membership).
    pub fn has(&self, session_id: &str) -> bool {
        let key = self.target(session_id);
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&key)
    }

    /// Run `f` on the handle for `session_id`, creating it on first use.
    ///
    /// The map lock is held only long enough to get-or-create the session's
    /// entry; `f` runs under the per-handle lock, so two connections on
    /// *different* sessions proceed in parallel while each session's own
    /// requests are serialized by its handle lock.
    pub fn with_session<R>(&self, session_id: &str, f: impl FnOnce(&mut WorkerHandle) -> R) -> R {
        let key = self.target(session_id);
        let arc = {
            let mut map = self.handles.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(&key) {
                Some(handle) => Arc::clone(handle),
                None => {
                    let (exec, setup) = self.session_setup(&key);
                    let handle = Arc::new(Mutex::new(WorkerHandle::new(
                        &self.omp_path,
                        setup,
                        key.clone(),
                        exec,
                    )));
                    map.insert(key.clone(), Arc::clone(&handle));
                    handle
                }
            }
        };
        let mut guard = arc.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    /// S5: assemble this session's [`OrbitSetup`] + execution state from
    /// the shared template. The two stateful faces are re-bound to the
    /// session's [`ExecState`](crate::exec_state::ExecState):
    ///
    /// 1. the plan-policy closure captures *this* session's
    ///    `PlanModeRuntime`, so a `/plan` flip in one session never
    ///    changes the policy section another session's engine injects;
    /// 2. the session's tool catalog swaps the shared `exit_plan_mode`
    ///    instance for this session's own (bound to this session's
    ///    runtime + scoped review port) — the seam future stateful tools
    ///    plug into, instead of inventing a new daemon-wide singleton.
    fn session_setup(&self, key: &str) -> (crate::exec_state::ExecState, Option<OrbitSetup>) {
        let exec = crate::exec_state::ExecState::new();
        let Some(template) = &self.orbit_setup else {
            // omp-compat: no orbit engine, nothing to re-bind. The
            // session's execution state still exists (uniform ownership)
            // — it is simply never consulted by an engine.
            return (exec, None);
        };
        let catalog = tools::ToolCatalog::new();
        for tool in template.config.catalog.all() {
            if tool.spec().name != "exit_plan_mode" {
                catalog.register(tool);
            }
        }
        catalog.register(std::sync::Arc::new(plan_mode::tools::exit_plan_mode_tool(
            exec.plan.clone(),
            self.broker.review_port_for(key, exec.clone()),
        )));
        let mut setup = template.clone();
        setup.config.catalog = std::sync::Arc::new(catalog);
        let section = self.plan_section.clone();
        let plan = exec.plan.clone();
        setup.config.plan_policy_section = Some(std::sync::Arc::new(move || {
            plan.plan_policy_section(&section)
        }));
        (exec, Some(setup))
    }

    /// Live session ids, excluding the legacy entry.
    pub fn session_ids(&self) -> Vec<String> {
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect()
    }

    /// Drop one session's handle (S3 reaper). Returns whether a live entry
    /// went away. A later prompt re-creates the handle from scratch: the
    /// handle from scratch: the context replays from the session store and
    /// shared tools survive in the container.
    pub fn unload(&self, session_id: &str) -> bool {
        let key = self.target(session_id);
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key)
            .is_some()
    }

    /// Drop every handle (daemon teardown). Workers are dropped: orbit
    /// run threads exit on the closed channels, omp children are killed by
    /// their `Worker` destructor.
    pub fn reset_all(&self) {
        for (_, entry) in self
            .handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            if let Ok(mut w) = entry.try_lock() {
                w.reset();
            }
        }
    }
}
