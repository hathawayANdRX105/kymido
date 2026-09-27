//! End-to-end CLI tests for the session commands.
//!
//! Each test:
//! 1. Picks a unique socket + DB path in a tempdir
//! 2. Spawns a daemon with `KYMIDO_DAEMON_SOCKET` / `KYMIDO_SESSION_DB` set
//! 3. Runs `kymido session <cmd>` against the daemon
//! 4. Asserts on the parsed JSON output

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use daemon::{Daemon, DaemonConfig};
use tempfile::TempDir;

/// Spawn the test binary's daemon against a tempdir. `KYMIDO_OMP_PATH` is
/// pointed at a non-existent binary so the daemon never tries to launch a
/// worker — only `daemon.*` / `session.*` commands are exercised.
fn start_daemon(dir: &Path, tag: &str) -> Daemon {
    let cfg = DaemonConfig {
        socket_path: Some(dir.join(format!("{tag}.sock"))),
        omp_path: "omp-not-installed-for-cli-tests".to_string(),
        session_db_path: Some(dir.join(format!("{tag}.db"))),
        orbit_model: None,
        cwd: dir.to_path_buf(),
        max_turns: 64,
        ..Default::default()
    };
    Daemon::start(cfg).expect("start daemon")
}

/// Wait until the socket is reachable, up to ~4s. Cold start + tmpfs bind
/// can miss the first poll; 200 × 20 ms gives enough slack on CI.
fn wait_for_socket(path: &Path) {
    // Two-phase wait: first try fast retries (the daemon's accept loop polls
    // every 50 ms), then a generous slow window. We also dump the parent dir
    // when we give up so the failure mode is obvious in the test log.
    for _ in 0..500 {
        if UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    eprintln!("wait_for_socket: giving up; parent dir contents:");
    if let Some(parent) = path.parent()
        && let Ok(rd) = std::fs::read_dir(parent)
    {
        for e in rd.flatten() {
            eprintln!("  {:?}", e.path());
        }
    }
    panic!("daemon socket never came up: {path:?}");
}

/// Run the test binary (`kymido`) against the daemon's socket and return
/// `(stdout, stderr, success)`. `data_dir` is set so the CLI's Config::load
/// picks up the same socket via the same env vars as the daemon.
fn run_oi(data_dir: &Path, socket: &Path, db: &Path, args: &[&str]) -> (String, String, bool) {
    let exe = env!("CARGO_BIN_EXE_kymido");
    let output = Command::new(exe)
        .args(args)
        .env("KYMIDO_DATA_DIR", data_dir)
        .env("KYMIDO_DAEMON_SOCKET", socket)
        .env("KYMIDO_SESSION_DB", db)
        .env("KYMIDO_OMP_PATH", "omp-not-installed-for-cli-tests")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn kymido");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (stdout, stderr, output.status.success())
}

/// Unwrap helper for tests: prints the client error before panicking so a
/// setup failure isn't a silent panic in the test runner.
macro_rules! must {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => panic!("client call failed: {e}"),
        }
    };
}

#[test]
fn session_list_json_round_trip() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("list.sock");
    let db = dir.path().join("cli.db");
    let _daemon = start_daemon(dir.path(), "list");
    wait_for_socket(&sock);

    // Seed two sessions through the daemon directly.
    let client = daemon::DaemonClient::connect_to(&sock);
    must!(client.session_create("alpha", "first"));
    must!(client.session_create("beta", "second"));

    let (stdout, stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &["--json", "session", "list", "alpha"],
    );
    assert!(ok, "stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse JSON");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "alpha");
    assert_eq!(arr[0]["title"], "first");
}

#[test]
fn session_get_missing_id_returns_error() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("missing.sock");
    let db = dir.path().join("missing.db");
    let _daemon = start_daemon(dir.path(), "missing");
    wait_for_socket(&sock);

    let (stdout, stderr, ok) = run_oi(dir.path(), &sock, &db, &["session", "get", "ghost"]);
    assert!(!ok, "should fail");
    assert!(
        stderr.contains("not found") || stdout.contains("not found"),
        "stderr={stderr} stdout={stdout}"
    );
}

#[test]
fn session_create_get_delete_through_cli() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("crud.sock");
    let db = dir.path().join("crud.db");
    let _daemon = start_daemon(dir.path(), "crud");
    wait_for_socket(&sock);

    // Get on missing id → nonzero exit.
    let (_stdout, _stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &["--json", "session", "get", "nope"],
    );
    assert!(!ok);

    // Delete on missing id → exits 0, JSON `{deleted:false}`.
    let (stdout, _stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &["--json", "session", "delete", "nope"],
    );
    assert!(ok, "delete of missing is non-fatal");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse");
    assert_eq!(v["deleted"], serde_json::json!(false));
}

#[test]
fn session_search_returns_messages_in_json() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("search.sock");
    let db = dir.path().join("search.db");
    let _daemon = start_daemon(dir.path(), "search");
    wait_for_socket(&sock);

    let client = daemon::DaemonClient::connect_to(&sock);
    must!(client.session_create("s1", "t"));
    must!(client.session_append("s1", session::SessionRole::User, "hello world", &[]));

    let (stdout, _stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &["--json", "session", "search", "hello", "--limit", "10"],
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["text"], "hello world");
    assert_eq!(arr[0]["role"], "user");
}

#[test]
fn session_query_dispatch_matches_tool_def_shape() {
    // The CLI's `session query` accepts the same args shape as the
    // daemon's `session_query` ToolDef (`kind` + extras). Round-trip both
    // forms end-to-end through a live daemon.
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("query.sock");
    let db = dir.path().join("query.db");
    let _daemon = start_daemon(dir.path(), "query");
    wait_for_socket(&sock);

    let client = daemon::DaemonClient::connect_to(&sock);
    must!(client.session_create("a", "alpha"));
    must!(client.session_create("b", "beta"));

    // kind=list via CLI.
    let (stdout, _stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &[
            "--json", "session", "query", "--kind", "list", "--query", "alpha", "--limit", "10",
        ],
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "a");

    // kind=delete via CLI.
    let (stdout, _stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &[
            "--json",
            "session",
            "query",
            "--kind",
            "delete",
            "--session-id",
            "b",
        ],
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse");
    assert_eq!(v["deleted"], serde_json::json!(true));

    // kind=get → null after delete.
    let (stdout, _stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &[
            "--json",
            "session",
            "query",
            "--kind",
            "get",
            "--session-id",
            "b",
        ],
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse");
    assert!(v.is_null());
}

#[test]
fn session_query_without_daemon_fails_cleanly() {
    // No daemon → connect failure surfaces as a non-zero exit, not a panic.
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("absent.sock");
    let db = dir.path().join("absent.db");

    let (_stdout, stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &["--json", "session", "list", "anything"],
    );
    assert!(!ok);
    assert!(!stderr.is_empty(), "expected an error message");
}

#[test]
fn session_query_rejects_unknown_kind_in_cli() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("bad.sock");
    let db = dir.path().join("bad.db");
    let _daemon = start_daemon(dir.path(), "bad");
    wait_for_socket(&sock);

    let (_stdout, stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &["--json", "session", "query", "--kind", "nuke"],
    );
    assert!(!ok);
    // Daemon rejects unknown kinds with `unknown session_query kind …`.
    assert!(
        stderr.contains("unknown session_query kind") || stderr.contains("protocol"),
        "stderr={stderr}"
    );
}

#[test]
fn session_compress_archives_idle_sessions_without_a_daemon() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("compress.sock");
    let db = dir.path().join("compress.db");

    // Seed through the daemon, then shut it down: compress is offline
    // maintenance and must work with no daemon running.
    let mut daemon = start_daemon(dir.path(), "compress");
    wait_for_socket(&sock);
    let client = daemon::DaemonClient::connect_to(&sock);
    must!(client.session_create("cold-1", "first cold"));
    daemon.shutdown();
    drop(daemon);

    let (stdout, stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &[
            "--json",
            "session",
            "compress",
            "--idle-days",
            "0",
            "--keep",
            "0",
        ],
    );
    assert!(ok, "compress failed, stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse JSON");
    assert_eq!(v["archived"], 1, "exactly the seeded session, got {v}");
    assert_eq!(v["receipts"][0]["session_id"], "cold-1");
    assert!(
        dir.path()
            .join("archive")
            .join("cold-1.jsonl.zst")
            .is_file(),
        "the archive landed next to the database"
    );

    // A second run over the same database is a no-op, not an error.
    let (stdout, stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &[
            "--json",
            "session",
            "compress",
            "--idle-days",
            "0",
            "--keep",
            "0",
        ],
    );
    assert!(ok, "second compress failed, stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse JSON");
    assert_eq!(v["archived"], 0, "nothing left idle, got {v}");
}

#[test]
fn a_corrupt_archive_surfaces_as_session_io_through_the_daemon() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("corrupt.sock");
    let db = dir.path().join("corrupt.db");

    let mut daemon = start_daemon(dir.path(), "corrupt");
    wait_for_socket(&sock);
    let client = daemon::DaemonClient::connect_to(&sock);
    must!(client.session_create("cold-2", "second cold"));
    daemon.shutdown();
    drop(daemon);

    let (stdout, stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &[
            "--json",
            "session",
            "compress",
            "--idle-days",
            "0",
            "--keep",
            "0",
        ],
    );
    assert!(ok, "compress failed, stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse JSON");
    assert_eq!(v["archived"], 1, "seeded session archived, got {v}");

    let archive = dir.path().join("archive").join("cold-2.jsonl.zst");
    std::fs::write(&archive, b"this is not a zstd frame").expect("corrupt");

    // Reopening the session through the daemon must surface the archive
    // failure with the `session_io` code from session_error_response, not a
    // crash or a silent miss.
    let mut daemon = start_daemon(dir.path(), "corrupt");
    wait_for_socket(&sock);
    let (_stdout, stderr, ok) = run_oi(
        dir.path(),
        &sock,
        &db,
        &["--json", "session", "get", "cold-2"],
    );
    assert!(!ok, "a corrupt archive must not open cleanly");
    assert!(
        stderr.contains("session_io"),
        "the daemon maps archive failures to session_io, stderr={stderr}"
    );
    daemon.shutdown();
}
