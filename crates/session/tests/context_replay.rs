//! C01 slice 1 — durable context ledger: migration, idempotency, tool-group
//! pairing, and the projection authoritative pointer.
//!
//! These tests pin the four contracts later C01 slices build on:
//!
//! 1. a pre-C01 file opens unchanged and gains the C01 tables on first open;
//!    opening again is idempotent (no duplicate-column error, no data loss);
//! 2. appending the same `idempotency_key` twice writes exactly one row, while
//!    two genuinely repeated messages (same text, different key) both survive;
//! 3. a `tool_call` and its matching `tool_result` read back as a closed pair,
//!    and an unpaired result does not fabricate a group;
//! 4. committing an older `projection_generation` after a newer one leaves the
//!    session-level authoritative pointer on the newer projection.

use std::path::Path;

use serde_json::json;
use session::{
    ContextEvent, ContextEventDraft, ContextEventKind, ContextObjectDraft, ContextObjectKind,
    OutcomeStatus, PayloadKind, ProjectionDraft, RequestOutcomeDraft, SessionDb, SourceKind,
    SourceRef, SourceScope,
};
use tempfile::tempdir;

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn draft(event_id: &str, order: i64, key: &str) -> ContextEventDraft {
    ContextEventDraft {
        event_id: event_id.into(),
        session_id: "s1".into(),
        branch_id: "main".into(),
        epoch: 1,
        turn_id: Some("t1".into()),
        run_id: Some("r1".into()),
        event_order: order,
        role: "user".into(),
        kind: ContextEventKind::Prompt,
        payload: json!({"text": "hello"}),
        source_kind: SourceKind::User,
        tool_call_id: None,
        idempotency_key: key.into(),
    }
}

fn tool_draft(
    event_id: &str,
    order: i64,
    key: &str,
    kind: ContextEventKind,
    tool_call_id: &str,
) -> ContextEventDraft {
    ContextEventDraft {
        event_id: event_id.into(),
        session_id: "s1".into(),
        branch_id: "main".into(),
        epoch: 1,
        turn_id: Some("t1".into()),
        run_id: Some("r1".into()),
        event_order: order,
        role: "tool".into(),
        kind,
        payload: json!({"tool": "shell"}),
        source_kind: SourceKind::Tool,
        tool_call_id: Some(tool_call_id.into()),
        idempotency_key: key.into(),
    }
}

fn projection(projection_id: &str, generation: i64, payload_kind: PayloadKind) -> ProjectionDraft {
    ProjectionDraft {
        projection_id: projection_id.into(),
        session_id: "s1".into(),
        branch_id: "main".into(),
        source_epoch: 1,
        covered_through_event_id: None,
        method_id: "summarize".into(),
        method_version: "1".into(),
        config_revision: "c1".into(),
        focus_revision: "f1".into(),
        projection_generation: generation,
        payload_kind,
        payload_json: Some(json!({"summary": "…"}).to_string()),
        object_id: None,
    }
}

/// Create a pre-C01 database file: only `sessions` and `messages`, with one
/// session row already present. A file created by current code gets the C01
/// tables from `CREATE TABLE`, so this is the only way to reach the
/// migration-gains-tables path.
fn legacy_file(db_path: &Path) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    runtime.block_on(async {
        let db = libsql::Builder::new_local(db_path)
            .build()
            .await
            .expect("open legacy file");
        let conn = db.connect().expect("connect legacy file");
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 turn_log TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO sessions (id, title, created_at, updated_at, turn_log)
             VALUES ('legacy-row', '老库', 1, 2, '');",
        )
        .await
        .expect("write pre-C01 schema");
    });
}

// -----------------------------------------------------------------------------
// 1. Migration
// -----------------------------------------------------------------------------

#[test]
fn legacy_database_gains_context_tables_idempotently() {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    legacy_file(&db_path);

    // First open runs the migration; the legacy session row must survive.
    let db = SessionDb::open(&db_path).expect("open legacy db");
    let summary = db
        .ensure_session("legacy-row", "ignored")
        .expect("ensure legacy session");
    assert_eq!(summary.created_at_ms, 1, "legacy row must not be rewritten");

    // Second open re-runs every migration; a duplicate-column error or a lost
    // row would surface here.
    drop(db);
    let db = SessionDb::open(&db_path).expect("reopen migrated db");

    let (event, inserted) = db
        .append_context_event(draft("ev-1", 1, "k1"))
        .expect("append after reopen");
    assert!(inserted);
    let read = db.context_event("ev-1").expect("read").expect("present");
    assert_eq!(read, event);
}

// -----------------------------------------------------------------------------
// 2. Idempotency
// -----------------------------------------------------------------------------

#[test]
fn same_idempotency_key_writes_once_but_repeated_text_survives() {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    let db = SessionDb::open(&db_path).expect("open");

    let (first, inserted) = db
        .append_context_event(draft("ev-1", 1, "k1"))
        .expect("first");
    assert!(inserted);
    let (second, inserted) = db
        .append_context_event(draft("ev-1", 1, "k1"))
        .expect("replay");
    assert!(!inserted, "replay must not insert");
    assert_eq!(first, second, "replay returns the stored row");

    // Same text, different idempotency key: a legitimate repeat, both kept.
    db.append_context_event(draft("ev-2", 2, "k2"))
        .expect("repeat text");

    let rows = db
        .context_events_in_range("s1", "main", 1, 0, 100, 100)
        .expect("range");
    assert_eq!(rows.len(), 2, "two distinct events, one per key");
}

#[test]
fn colliding_event_id_under_a_new_key_is_rejected() {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    let db = SessionDb::open(&db_path).expect("open");

    db.append_context_event(draft("ev-1", 1, "k1"))
        .expect("first");
    let err = db
        .append_context_event(draft("ev-1", 1, "k-different"))
        .expect_err("must reject id reuse under a new key");
    assert!(
        err.to_string().contains("event_id"),
        "error names the collision: {err}"
    );
}

// -----------------------------------------------------------------------------
// 3. Tool groups
// -----------------------------------------------------------------------------

#[test]
fn closed_tool_groups_pair_call_and_result_only() {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    let db = SessionDb::open(&db_path).expect("open");

    db.append_context_event(tool_draft(
        "c1",
        1,
        "kc1",
        ContextEventKind::ToolCall,
        "call-1",
    ))
    .expect("call");
    db.append_context_event(tool_draft(
        "r1",
        2,
        "kr1",
        ContextEventKind::ToolResult,
        "call-1",
    ))
    .expect("result");
    // A result whose call never arrived must not invent a group.
    db.append_context_event(tool_draft(
        "r2",
        3,
        "kr2",
        ContextEventKind::ToolResult,
        "call-2",
    ))
    .expect("orphan result");

    let groups = db.closed_tool_groups("s1", "main", 1).expect("groups");
    assert_eq!(groups.len(), 1, "only the paired call/result is closed");
    assert_eq!(groups[0].tool_call_id, "call-1");
    assert_eq!(groups[0].call.event_id, "c1");
    assert_eq!(groups[0].call.kind, ContextEventKind::ToolCall);
    assert_eq!(groups[0].result.event_id, "r1");
    assert_eq!(groups[0].result.kind, ContextEventKind::ToolResult);

    // Run query sees all three, in order — pairing does not hide the orphan.
    let run = db
        .context_events_for_run("s1", "main", 1, "r1")
        .expect("run");
    let order: Vec<&str> = run
        .iter()
        .map(|e: &ContextEvent| e.event_id.as_str())
        .collect();
    assert_eq!(order, vec!["c1", "r1", "r2"]);
}

// -----------------------------------------------------------------------------
// 4. Authoritative pointer
// -----------------------------------------------------------------------------

#[test]
fn older_generation_never_overwrites_the_pointer() {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    let db = SessionDb::open(&db_path).expect("open");

    let (p5, authoritative) = db
        .commit_projection(projection("p5", 5, PayloadKind::InlineJson))
        .expect("commit gen 5");
    assert!(authoritative, "first commit takes the pointer");

    // A late/duplicate producer commits an older generation: stored, but the
    // pointer must not move back.
    let (p3, authoritative) = db
        .commit_projection(projection("p3", 3, PayloadKind::InlineJson))
        .expect("commit gen 3");
    assert!(!authoritative, "older generation must not take the pointer");
    assert_eq!(p3.status.as_str(), "committed");

    let current = db
        .authoritative_projection("s1", "main")
        .expect("pointer")
        .expect("some");
    assert_eq!(current.projection_id, p5.projection_id);
    assert_eq!(current.projection_generation, 5);

    // A strictly newer generation does move it and supersedes the old holder.
    let (p7, authoritative) = db
        .commit_projection(projection("p7", 7, PayloadKind::InlineJson))
        .expect("commit gen 7");
    assert!(authoritative);
    let current = db
        .authoritative_projection("s1", "main")
        .expect("pointer")
        .expect("some");
    assert_eq!(current.projection_id, p7.projection_id);

    let all = db.projections_for_session("s1", 100).expect("list");
    assert_eq!(all.len(), 3, "superseded and stale rows are retained");
    let old = all.iter().find(|p| p.projection_id == "p5").expect("p5");
    assert_eq!(old.status.as_str(), "superseded");
}

// -----------------------------------------------------------------------------
// 5. SourceRef scope
// -----------------------------------------------------------------------------

#[test]
fn source_ref_round_trips_and_is_scope_checked() {
    let scope = SourceScope::new("ws", "s1", "main", 1);
    let reference = SourceRef::new(&scope, "ev-1");
    let encoded = reference.encode();

    let decoded = SourceRef::decode_in_scope(&encoded, &scope).expect("in scope");
    assert_eq!(decoded, reference);

    let other = SourceScope::new("ws", "s1", "main", 2);
    let err = SourceRef::decode_in_scope(&encoded, &other).expect_err("epoch mismatch");
    assert!(err.to_string().contains("scope"), "scope error: {err}");

    let other_session = SourceScope::new("ws", "s2", "main", 1);
    assert!(SourceRef::decode_in_scope(&encoded, &other_session).is_err());
}

// -----------------------------------------------------------------------------
// 6. Objects
// -----------------------------------------------------------------------------

#[test]
fn objects_round_trip_and_sweep_reclaims_orphans() {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    let db = SessionDb::open(&db_path).expect("open");

    let (object, inserted) = db
        .store_context_object(ContextObjectDraft {
            object_id: "obj-1".into(),
            kind: ContextObjectKind::Blob,
            bytes: b"durable bytes".to_vec(),
        })
        .expect("store");
    assert!(inserted);

    // Identical replay is a no-op.
    let (again, inserted) = db
        .store_context_object(ContextObjectDraft {
            object_id: "obj-1".into(),
            kind: ContextObjectKind::Blob,
            bytes: b"durable bytes".to_vec(),
        })
        .expect("replay");
    assert!(!inserted);
    assert_eq!(again.sha256, object.sha256);

    assert_eq!(
        db.read_context_object("obj-1").expect("read"),
        b"durable bytes".to_vec()
    );

    // Corrupting the file must fail the read-time integrity check.
    let object_dir = db_path.parent().expect("parent").join(format!(
        "{}.context-objects",
        db_path.file_stem().expect("stem").to_string_lossy()
    ));
    std::fs::write(object_dir.join("obj-1"), b"tampered").expect("tamper");
    assert!(db.read_context_object("obj-1").is_err(), "hash mismatch");

    // An unreferenced file (crash between temp write and commit) is reclaimed.
    std::fs::write(object_dir.join("orphan"), b"left behind").expect("orphan");
    let removed = db.sweep_orphan_object_files().expect("sweep");
    assert_eq!(removed, 1, "only the unreferenced file goes away");
    assert!(object_dir.join("obj-1").exists(), "referenced file stays");
}

/// The manifest hash must be the real SHA-256, not just a self-consistent
/// digest: consumers compare it against hashes computed outside this crate.
/// Pinned to the canonical NIST vector for `b"abc"`.
#[test]
fn object_hash_matches_canonical_sha256_vector() {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    let db = SessionDb::open(&db_path).expect("open");

    let (object, _) = db
        .store_context_object(ContextObjectDraft {
            object_id: "obj-abc".into(),
            kind: ContextObjectKind::Blob,
            bytes: b"abc".to_vec(),
        })
        .expect("store");

    assert_eq!(
        object.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "stored hash must equal SHA-256(\"abc\")"
    );
}

// -----------------------------------------------------------------------------
// 7. Request outcomes
// -----------------------------------------------------------------------------

#[test]
fn request_outcome_attempts_are_immutable() {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    let db = SessionDb::open(&db_path).expect("open");

    let outcome = RequestOutcomeDraft {
        request_id: "req-1".into(),
        attempt: 1,
        session_id: "s1".into(),
        source_epoch: 1,
        projection_generation: 5,
        request_body_object_id: None,
        request_body_hash: Some("abc".into()),
        route: "chat".into(),
        model: "m".into(),
        outcome_status: OutcomeStatus::SendFailed,
        usage: None,
        error_kind: Some("timeout".into()),
        sent_at: 10,
        completed_at: Some(11),
    };
    let (first, inserted) = db.record_request_outcome(outcome.clone()).expect("record");
    assert!(inserted);

    // Replaying the same attempt returns the stored row — no overwrite of the
    // recorded failure.
    let (second, inserted) = db.record_request_outcome(outcome).expect("replay");
    assert!(!inserted);
    assert_eq!(first, second);
    assert_eq!(second.outcome_status, OutcomeStatus::SendFailed);

    // A new attempt is a separate row.
    let (_, inserted) = db
        .record_request_outcome(RequestOutcomeDraft {
            request_id: "req-1".into(),
            attempt: 2,
            session_id: "s1".into(),
            source_epoch: 1,
            projection_generation: 5,
            request_body_object_id: None,
            request_body_hash: Some("abc".into()),
            route: "chat".into(),
            model: "m".into(),
            outcome_status: OutcomeStatus::Completed,
            usage: None,
            error_kind: None,
            sent_at: 12,
            completed_at: Some(13),
        })
        .expect("attempt 2");
    assert!(inserted);
    assert_eq!(
        db.request_outcomes_for_session("s1", 100)
            .expect("list")
            .len(),
        2
    );
}
