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
    orbit_setup: Option<OrbitSetup>,
}

impl SessionRegistry {
    /// Build a registry; the first `handle()` call lazily constructs the
    /// legacy worker (same construction as the pre-registry single handle).
    pub fn new(omp_path: String, orbit_setup: Option<OrbitSetup>) -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            omp_path,
            orbit_setup,
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
                    let handle = Arc::new(Mutex::new(WorkerHandle::new(
                        &self.omp_path,
                        self.orbit_setup.clone(),
                        key.clone(),
                    )));
                    map.insert(key.clone(), Arc::clone(&handle));
                    handle
                }
            }
        };
        let mut guard = arc.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Legacy (no orbit setup) collapses every target to the shared entry:
    /// session-less traffic must never multiply the single worker.
    #[test]
    fn legacy_mode_routes_everything_to_the_shared_entry() {
        let registry = SessionRegistry::new("/nonexistent/omp".into(), None);
        assert_eq!(registry.target("s1"), "");
        assert_eq!(registry.target(""), "");

        registry.with_session("s1", |_| {});
        registry.with_session("s2", |_| {});
        assert!(
            registry.session_ids().is_empty(),
            "legacy mode must not register per-session ids"
        );
        assert!(
            registry.has(""),
            "the shared entry serves session-less traffic"
        );
    }
}
