//! S5: per-session execution-state transitions (moved from the in-src test
//! module per the repo's test-in-`tests/` convention).

use daemon::exec_state::{ExecState, ExecStatus};

#[test]
fn run_slot_is_sticky_across_replacements() {
    let exec = ExecState::new();
    assert_eq!(exec.status(), ExecStatus::Idle);
    assert!(!exec.has_active_run());

    exec.set_running("r1");
    assert_eq!(
        exec.status(),
        ExecStatus::Running {
            run_id: "r1".into()
        }
    );
    assert_eq!(exec.running_run_id(), Some("r1".into()));
    assert!(exec.has_active_run());

    // A later run cannot clear the slot it did not take
    exec.set_running("r2");
    exec.try_clear_run("r1");
    assert_eq!(
        exec.status(),
        ExecStatus::Running {
            run_id: "r2".into()
        }
    );

    exec.try_clear_run("r2");
    assert_eq!(exec.status(), ExecStatus::Idle);

    // Legacy prompt records declare no run; the slot stays open
    exec.set_running("r3");
    exec.set_running("");
    assert_eq!(exec.status(), ExecStatus::Idle);
}

#[test]
fn awaiting_review_keeps_the_session_hot() {
    let exec = ExecState::new();
    exec.awaiting_review("q1");
    assert_eq!(
        exec.status(),
        ExecStatus::AwaitingReview {
            question_id: "q1".into()
        }
    );
    assert!(
        exec.has_active_run(),
        "the reaper must not re-read a session parked on a review"
    );
    exec.idle();
    assert_eq!(exec.status(), ExecStatus::Idle);
    assert!(!exec.has_active_run());
}

#[test]
fn each_session_gets_its_own_plan_runtime() {
    let a = ExecState::new();
    let b = ExecState::new();
    a.plan.prepare_set(true).unwrap().unwrap().commit().unwrap();
    assert!(a.plan.active().unwrap());
    assert!(
        !b.plan.active().unwrap(),
        "one session's /plan must not flip another session's plan mode"
    );
}
