//! S5: SessionRegistry per-session OrbitSetup assembly (moved from the
//! in-src test module per the repo's test-in-`tests/` convention).

use agent_loop::orbit::LlmBackend;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, atomic::AtomicBool};

use daemon::rpc::worker::{OrbitConfig, OrbitSetup};
use daemon::{QuestionBroker, SessionRegistry};

/// The engine is never run in these tests: handles are lazy and only the
/// per-session assembly is exercised.
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
            model: "stub".into(),
            base_url: None,
            max_tokens: None,
            context_window: None,
            input: Vec::new(),
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
            aside_queue: Arc::new(Mutex::new(VecDeque::new())),
        },
    }
}

/// Legacy (no orbit setup) collapses every target to the shared entry:
/// session-less traffic must never multiply the single worker.
#[test]
fn legacy_mode_routes_everything_to_the_shared_entry() {
    let registry = SessionRegistry::new(
        "/nonexistent/omp".into(),
        None,
        Arc::new(QuestionBroker::new()),
        String::new(),
    );
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

/// Orbit mode gives every session its own execution state: plan
/// runtime, plan-policy closure, and `exit_plan_mode` tool instance
/// are all per session and cannot leak across sessions.
#[test]
fn per_session_plan_state_is_isolated() {
    let registry = SessionRegistry::new(
        "/nonexistent/omp".into(),
        Some(stub_setup()),
        Arc::new(QuestionBroker::new()),
        "PLAN SECTION".to_string(),
    );

    registry.with_session("a", |h| {
        h.plan()
            .prepare_set(true)
            .unwrap()
            .unwrap()
            .commit()
            .unwrap();
    });
    let a_active = registry.with_session("a", |h| h.plan().active().unwrap());
    let b_active = registry.with_session("b", |h| h.plan().active().unwrap());
    assert!(
        a_active && !b_active,
        "a's /plan must not flip b's plan mode"
    );

    // Each session's assembled setup carries its own exit_plan_mode
    // instance and a plan closure bound to its own runtime.
    let a_tool = registry.with_session("a", |h| {
        h.setup()
            .as_ref()
            .expect("orbit session setup")
            .config
            .catalog
            .find("exit_plan_mode")
            .expect("per-session exit_plan_mode assembled")
    });
    let b_tool = registry.with_session("b", |h| {
        h.setup()
            .as_ref()
            .expect("orbit session setup")
            .config
            .catalog
            .find("exit_plan_mode")
            .expect("per-session exit_plan_mode assembled")
    });
    assert!(
        !Arc::ptr_eq(&a_tool, &b_tool),
        "tool instances must not be shared"
    );

    let a_section = registry.with_session("a", |h| {
        h.setup()
            .as_ref()
            .expect("orbit session setup")
            .config
            .plan_policy_section
            .as_ref()
            .expect("plan closure assembled")()
    });
    let b_section = registry.with_session("b", |h| {
        h.setup()
            .as_ref()
            .expect("orbit session setup")
            .config
            .plan_policy_section
            .as_ref()
            .expect("plan closure assembled")()
    });
    assert_eq!(a_section, "PLAN SECTION");
    assert_eq!(b_section, "", "b is not in plan mode: no section injected");
}
