// Integration tests for the declarative scenario runner (`src/script.rs`).
//
// Each test drives a real pty through the public entry point and checks the
// report and the artifacts on disk. The cases are the failures a test harness
// most badly hides: a key name nobody can spell being skipped instead of
// failing, a settle-wait that leaves no trace when it times out, a child that
// dies mid-scenario being driven on into a green report, and a runner that
// litters its artifact directory.

use std::path::{Path, PathBuf};

use terminal::TerminalRegistry;
use terminal::script::{
    RunReport, RunStatus, Scenario, Step, StepOutcome, StepStatus, TerminalConfig, run_scenario,
};

/// A scratch artifact directory, emptied first so a leftover from a previous
/// run can never make a "nothing left behind" assertion vacuously pass.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("kymido-tui-t0c")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn outcome<'a>(report: &'a RunReport, step: usize) -> &'a StepOutcome {
    &report.steps[step]
}

/// The false-green a test harness must never produce: an unrecognized key
/// name gets silently skipped, so a scenario "runs through" while the key
/// that would have driven the screen was never sent. The step must be
/// `Failed` with the bad name in the note, and the steps after it skipped.
#[test]
fn unknown_key_name_fails_the_step() {
    let registry = TerminalRegistry::new();
    let dir = fresh_dir("unknown-key");
    let scenario = Scenario {
        name: "unknown-key".into(),
        command: "sh".into(),
        cwd: None,
        terminal: TerminalConfig { rows: 20, cols: 80 },
        steps: vec![
            Step::SendKeys {
                keys: "Frobnicate".into(),
            },
            Step::WaitForText {
                text: "SHOULD-NEVER-RUN".into(),
                timeout_ms: 500,
            },
        ],
    };

    // A failed *step* is not an Err: the report must still come back so the
    // failure can be read next to the artifacts.
    let report = run_scenario(&registry, &scenario, &dir)
        .expect("only setup problems are Err; a step failure is a Failed report");

    assert_eq!(report.status, RunStatus::Failed);
    let first = outcome(&report, 0);
    assert_eq!(first.status, StepStatus::Failed, "note: {first:?}");
    assert!(
        first.note.contains("Frobnicate"),
        "the note must name the key that is unknown, not just say it failed: {first:?}"
    );
    assert_eq!(outcome(&report, 1).status, StepStatus::Skipped);
}

/// A `wait_for_text` timeout must leave behind the ASCII frame it gave up
/// on, with the path recorded in the failing step's note — otherwise whoever
/// reads the failure cannot tell which frame the screen was stuck on.
#[test]
fn wait_for_text_timeout_records_snapshot() {
    let registry = TerminalRegistry::new();
    let dir = fresh_dir("wait-timeout");
    let scenario = Scenario {
        name: "wait-timeout".into(),
        // `sleep` prints nothing, so the settle-wait can only ever time out.
        command: "sleep 5".into(),
        cwd: None,
        terminal: TerminalConfig { rows: 20, cols: 80 },
        steps: vec![
            Step::WaitForText {
                text: "NEVER".into(),
                timeout_ms: 250,
            },
            Step::Screenshot {
                name: Some("after".into()),
            },
        ],
    };

    let report =
        run_scenario(&registry, &scenario, &dir).expect("a step failure reports, not Errs");

    assert_eq!(report.status, RunStatus::Failed);
    let failed = outcome(&report, 0);
    assert_eq!(failed.status, StepStatus::Failed);
    let expected = dir.join("wait-timeout").join("000-wait.txt");
    assert!(
        failed.note.contains(&expected.display().to_string()),
        "the failing step's note must name the snapshot: {failed:?}"
    );
    assert!(
        expected.is_file(),
        "the snapshot must exist at the path the note claims"
    );
    let body = std::fs::read_to_string(&expected).expect("snapshot is readable");
    assert!(
        body.contains('┌'),
        "the snapshot is a bordered ASCII frame of the stuck screen, not raw bytes: {body}"
    );
    assert_eq!(outcome(&report, 1).status, StepStatus::Skipped);
}

/// A child that dies mid-scenario must stop the run cleanly: one `Failed`
/// step whose note spells out the exit code, every step after it `Skipped`,
/// and the report itself `Failed` — the exact opposite of the run that keeps
/// writing keys into a corpse and comes back green.
#[test]
fn crash_mid_scenario_stops_cleanly() {
    let registry = TerminalRegistry::new();
    let dir = fresh_dir("crash");
    let scenario = Scenario {
        name: "crash".into(),
        command: "sh".into(),
        cwd: None,
        terminal: TerminalConfig { rows: 20, cols: 80 },
        steps: vec![
            Step::Type {
                text: "exit 3\n".into(),
            },
            Step::Wait { ms: 100 },
            Step::WaitForText {
                text: "NEVER".into(),
                timeout_ms: 2000,
            },
            Step::Screenshot {
                name: Some("after".into()),
            },
        ],
    };

    let report =
        run_scenario(&registry, &scenario, &dir).expect("a crashed child reports, not Errs");

    assert_eq!(report.status, RunStatus::Failed);
    let faileds: Vec<&StepOutcome> = report
        .steps
        .iter()
        .filter(|o| o.status == StepStatus::Failed)
        .collect();
    assert_eq!(
        faileds.len(),
        1,
        "exactly the step that saw the child die is Failed: {report:?}"
    );
    let last_failed = faileds[0].step;
    for step in &report.steps {
        if step.step > last_failed {
            assert_eq!(
                step.status,
                StepStatus::Skipped,
                "steps after the death cannot run: {step:?}"
            );
        }
    }
    let exit_noted = report.steps.iter().any(|o| o.note.contains("exit code 3"));
    assert!(
        exit_noted,
        "the exit code is not swallowed; it must appear in a step note: {report:?}"
    );
    // And on disk, where the report is actually read from:
    let body = std::fs::read_to_string(dir.join("crash").join("report.json"))
        .expect("report.json is written even for a failed run");
    assert!(body.contains("exit code 3"));
}

/// The artifact directory is a place people read, so a run must not leave
/// half-written temporaries in it: the on-disk file set is asserted exactly,
/// which is what a stray `.tmp` would show up in.
#[test]
fn no_temp_artifacts_left_behind() {
    let registry = TerminalRegistry::new();
    let dir = fresh_dir("clean-run");
    let scenario = Scenario {
        name: "clean-run".into(),
        command: "sh".into(),
        cwd: None,
        terminal: TerminalConfig { rows: 20, cols: 80 },
        steps: vec![
            // Assemble the sentinel from pieces so the matched string is the
            // command's *output*, not the echoed command line.
            Step::Type {
                text: "printf '\\nT_%s_T\\n' DONE\n".into(),
            },
            Step::WaitForText {
                text: "T_DONE_T".into(),
                timeout_ms: 5000,
            },
            Step::Screenshot {
                name: Some("clean".into()),
            },
        ],
    };

    let report = run_scenario(&registry, &scenario, &dir).expect("a green run cannot Err");

    assert_eq!(report.status, RunStatus::Passed, "{report:?}");
    let scenario_dir = dir.join("clean-run");
    let mut entries: Vec<String> = std::fs::read_dir(&scenario_dir)
        .expect("the artifact directory exists")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    entries.sort();
    let expected = vec![
        "002-clean.json".to_string(),
        "002-clean.svg".to_string(),
        "002-clean.txt".to_string(),
        "report.json".to_string(),
    ];
    assert_eq!(
        entries, expected,
        "exact file set; a leftover .tmp or half-written frame would show up here"
    );
    // The report on disk is the same report the caller got, and it is green.
    let stored: RunReport = serde_json::from_str(
        &std::fs::read_to_string(scenario_dir.join("report.json"))
            .expect("report.json is readable"),
    )
    .expect("report.json deserializes");
    assert_eq!(stored.scenario, "clean-run");
    assert_eq!(stored.status, RunStatus::Passed);
    assert_eq!(stored.steps.len(), 3);
}

/// The shipped scenario files stay valid: they parse, and the green one
/// actually runs green through the public entry point. Running the TUI
/// scenario needs `kymido` on a pty — that is the smoke's job (§6), not the
/// test's, so it is only checked for parseability here.
#[test]
fn shipped_scenarios_are_runnable() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));

    let scenario = Scenario::from_path(&manifest.join("scenarios/echo-sentinel.json"))
        .expect("the shipped echo scenario parses");
    let registry = TerminalRegistry::new();
    let dir = fresh_dir("shipped-echo");
    let report =
        run_scenario(&registry, &scenario, &dir).expect("the shipped echo scenario runs green");
    assert_eq!(report.status, RunStatus::Passed, "{report:?}");
    assert!(
        dir.join("echo-sentinel")
            .join("003-echo-ready.txt")
            .is_file(),
        "the scenario's screenshot step produced its artifact"
    );

    for name in ["slash-open.json", "fail-missing-text.json"] {
        Scenario::from_path(&manifest.join("scenarios").join(name))
            .unwrap_or_else(|e| panic!("{name} must parse: {e}"));
    }
}
