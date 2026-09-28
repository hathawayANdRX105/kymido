//! `session.load_messages` e2e — the RPC the TUI history backfill, web open
//! and daemon resume all ride on. Pins the tail-N window, the ascending
//! row order and the limit validation *through the wire*, where a field
//! rename or param regression would silently break every frontend.
//!
//! All tests run against a real daemon on a temp socket with a real sqlite
//! file, exactly like `session_title.rs`. The mock OpenAI backend the shared
//! `common` fixture wires up is never called — no prompt is ever sent.

use daemon::protocol::{Command, Response};
use daemon::{Daemon, DaemonClient};
use serde_json::{Value, json};
use tempfile::tempdir;

use common::{MockOpenAi, daemon_cfg, one_text_turn};

mod common;

fn start_daemon(dir: &std::path::Path) -> (Daemon, DaemonClient) {
    let mock = MockOpenAi::start(vec![one_text_turn("unused")]);
    let cfg = daemon_cfg(dir, &mock, 8);
    let daemon = Daemon::start(cfg).expect("daemon start");
    let client = DaemonClient::connect_to(daemon.socket_addr().path());
    (daemon, client)
}

fn call(client: &DaemonClient, command: Command, params: Value) -> Response {
    client
        .call_raw(command, params)
        .expect("rpc round trip over the daemon socket")
}

fn rows(client: &DaemonClient, sid: &str, limit: u32) -> Vec<Value> {
    let resp = call(
        client,
        Command::SessionLoadMessages,
        json!({ "session_id": sid, "limit": limit }),
    );
    assert!(resp.success, "load failed: {:?}", resp.error);
    serde_json::from_value(resp.data.expect("rows payload")).expect("rows array")
}

fn seqs(rows: &[Value]) -> Vec<i64> {
    rows.iter()
        .filter_map(|r| r.get("seq").and_then(Value::as_i64))
        .collect()
}

#[test]
fn load_returns_the_tail_window_in_ascending_order() {
    let dir = tempdir().expect("temp dir");
    let sid = "s-load-tail";
    let (mut daemon, client) = start_daemon(dir.path());

    let created = call(
        &client,
        Command::SessionCreate,
        json!({ "session_id": sid, "title": "load" }),
    );
    assert!(created.success, "create failed: {:?}", created.error);
    for i in 1..=5 {
        let resp = call(
            &client,
            Command::SessionAppend,
            json!({ "session_id": sid, "role": "user", "text": format!("m{i}") }),
        );
        assert!(resp.success, "append {i} failed: {:?}", resp.error);
    }

    let tail = rows(&client, sid, 3);
    assert_eq!(seqs(&tail), vec![3, 4, 5], "tail window, ascending");
    assert_eq!(tail[0].get("text").and_then(Value::as_str), Some("m3"));

    let all = rows(&client, sid, 50);
    assert_eq!(seqs(&all), vec![1, 2, 3, 4, 5], "limit above row count");

    daemon.shutdown();
}

#[test]
fn load_rejects_zero_limit_through_the_wire() {
    let dir = tempdir().expect("temp dir");
    let sid = "s-load-zero";
    let (mut daemon, client) = start_daemon(dir.path());

    let created = call(
        &client,
        Command::SessionCreate,
        json!({ "session_id": sid, "title": "load" }),
    );
    assert!(created.success, "create failed: {:?}", created.error);

    let resp = call(
        &client,
        Command::SessionLoadMessages,
        json!({ "session_id": sid, "limit": 0 }),
    );
    assert!(!resp.success, "limit=0 must be an error envelope");
    let code = resp.error.as_ref().map(|e| e.code.as_str()).unwrap_or("");
    assert_eq!(code, "invalid_limit", "error code: {:?}", resp.error);

    daemon.shutdown();
}
