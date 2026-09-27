//! Startup-sweep decision: when a daemon start should kick off a cold
//! compress, and with exactly which command.
//!
//! The contracts these pin:
//!
//! 1. a fresh install (no session database on disk) never spawns a sweep;
//! 2. a daemon binary without its sibling `kymido` CLI next to it skips the
//!    sweep *and* says where it looked;
//! 3. with both present, the plan points at `<exe_dir>/kymido` and carries
//!    the bounded argument vector the daemon log expects.

use daemon::startup_sweep::{SweepSkip, startup_sweep_command};

#[test]
fn a_fresh_install_skips_the_sweep() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing_db = dir.path().join("none.db");
    assert_eq!(
        startup_sweep_command(&missing_db, dir.path()),
        Err(SweepSkip::NoDatabase),
        "no database on disk means nothing to sweep"
    );
}

#[test]
fn a_missing_cli_skips_with_the_path_it_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("sessions.db");
    std::fs::write(&db, "").expect("db file");
    let exe_dir = dir.path().join("bin");
    std::fs::create_dir(&exe_dir).expect("exe dir");

    match startup_sweep_command(&db, &exe_dir) {
        Err(SweepSkip::CliMissing(path)) => {
            assert_eq!(path, exe_dir.join("kymido"));
        }
        other => panic!("expected CliMissing, got {other:?}"),
    }
}

#[test]
fn a_ready_install_plans_the_bounded_compress_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("sessions.db");
    std::fs::write(&db, "").expect("db file");
    let exe_dir = dir.path().join("bin");
    std::fs::create_dir(&exe_dir).expect("exe dir");
    let cli = exe_dir.join("kymido");
    std::fs::write(&cli, "").expect("cli file");

    let cmd = startup_sweep_command(&db, &exe_dir).expect("plan");
    assert_eq!(cmd.program, cli, "the sweep runs the sibling CLI");
    assert_eq!(
        cmd.args,
        [
            "session",
            "compress",
            "--idle-days",
            "30",
            "--max-sessions",
            "200",
            "--json"
        ],
        "conservative defaults, a batch cap, and machine-readable output"
    );
}
