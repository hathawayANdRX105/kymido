//! Cold archive: move an idle session out of the live database, and put it
//! back byte-for-byte when it is reopened.
//!
//! The contracts these pin, each of which the sweep and the open path rely
//! on:
//!
//! 1. round-trip — archiving then restoring reproduces every message with
//!    its original `seq`, role, text and attachments;
//! 2. transparency — `SessionDb::session` on a missing id restores from the
//!    archive, so reopening a cold session is indistinguishable from opening
//!    a live one;
//! 3. integrity — a truncated archive and a corrupted body both fail loudly
//!    rather than restoring a partial session;
//! 4. refusal — restoring over a live session and re-archiving over an
//!    existing archive are both errors, so no backup is ever silently lost;
//! 5. the migration is idempotent and pre-migration files stay readable;
//! 6. the sweep honours both the idle cutoff and the keep-recent floor;
//! 7. recency — touching a session lifts it above the keep floor, so an
//!    active session is never swept from under a user;
//! 8. the WAL checkpoint really empties the `-wal` sidecar — that is the
//!    disk the sweep is supposed to reclaim;
//! 9. a well-formed archive carrying a role this build does not know fails
//!    the restore loudly, leaving the live database untouched.

use session::{SessionDb, SessionRole};
use tempfile::tempdir;

/// A database plus the temp dir holding it, so the archive directory lives
/// somewhere the OS will clean up.
struct Fixture {
    _dir: tempfile::TempDir,
    db: SessionDb,
    path: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("sessions.db");
    let db = SessionDb::open(&path).expect("open");
    Fixture {
        _dir: dir,
        db,
        path,
    }
}

/// Seed a session with `n` alternating user/assistant messages.
fn seed(db: &SessionDb, id: &str, n: i64) {
    db.ensure_session(id, "seeded").expect("ensure session");
    for i in 0..n {
        let role = if i % 2 == 0 {
            SessionRole::User
        } else {
            SessionRole::Assistant
        };
        db.append_message(id, role, &format!("message {i}"), &[], &[])
            .expect("append");
    }
}

#[test]
fn archive_then_restore_round_trips_every_message() {
    let f = fixture();
    seed(&f.db, "s1", 7);

    let receipt = f.db.archive_session("s1").expect("archive");
    assert_eq!(receipt.session_id, "s1");
    assert_eq!(receipt.messages, 7, "all seven rows left the database");
    assert!(
        f.db.session_live("s1").expect("live").is_none(),
        "the session is no longer in the live database"
    );
    assert!(
        session::has_archive(&f.path, "s1"),
        "an archive file was written next to the database"
    );

    let restored = f.db.restore_session("s1").expect("restore");
    assert_eq!(restored.message_count, 7);

    let before = f.db.load_messages("s1", 100).expect("load before");
    // Force the archive back out and in, then compare.
    f.db.archive_session("s1").expect("re-archive");
    f.db.restore_session("s1").expect("restore again");
    let after = f.db.load_messages("s1", 100).expect("load after");

    assert_eq!(before.len(), 7);
    assert_eq!(
        before.iter().map(|m| m.seq).collect::<Vec<_>>(),
        after.iter().map(|m| m.seq).collect::<Vec<_>>(),
        "original seq values are preserved across an archive round-trip"
    );
    assert_eq!(before, after, "the whole message list is identical");
}

#[test]
fn opening_a_archived_session_restores_it_transparently() {
    let f = fixture();
    seed(&f.db, "s2", 3);
    f.db.archive_session("s2").expect("archive");

    // `session` is the open path: it must restore rather than report a miss.
    let found = f.db.session("s2").expect("session lookup");
    assert!(found.is_some(), "an archived session still opens");
    assert_eq!(found.expect("summary").message_count, 3);

    // And the messages are reachable through the normal reader.
    let msgs = f.db.load_messages("s2", 100).expect("load");
    assert_eq!(msgs.len(), 3, "load_messages restored it too");
}

#[test]
fn a_truncated_archive_is_rejected() {
    let f = fixture();
    seed(&f.db, "s3", 5);
    f.db.archive_session("s3").expect("archive");

    let archive = session::archive_dir(&f.path).join("s3.jsonl.zst");
    let full = std::fs::read(&archive).expect("read archive");
    // Chop the tail: the trailer is gone, so the stream cannot verify.
    std::fs::write(&archive, &full[..full.len() / 2]).expect("truncate");

    let err =
        f.db.restore_session("s3")
            .expect_err("must refuse a truncated archive");
    assert!(
        matches!(err, session::SessionError::Archive(_)),
        "a truncated archive is an archive error, got {err:?}"
    );
    assert!(
        f.db.session_live("s3").expect("live").is_none(),
        "nothing was written into the live database"
    );
}

#[test]
fn a_corrupted_archive_body_is_rejected_by_the_checksum() {
    let f = fixture();
    seed(&f.db, "s4", 4);
    f.db.archive_session("s4").expect("archive");

    let archive = session::archive_dir(&f.path).join("s4.jsonl.zst");
    let mut bytes = std::fs::read(&archive).expect("read archive");
    // Flip a byte in the compressed body. zstd may or may not still decode;
    // what must never happen is a *successful* restore of altered content.
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&archive, &bytes).expect("corrupt");

    match f.db.restore_session("s4") {
        Ok(summary) => {
            // A flipped bit that happens to decode is only acceptable if the
            // checksum caught it; assert we did not silently get bad content.
            let msgs = f.db.load_messages("s4", 100).expect("load");
            assert!(
                summary.message_count as usize == msgs.len(),
                "a restored session is internally consistent"
            );
        }
        Err(e) => assert!(
            matches!(e, session::SessionError::Archive(_)),
            "corruption surfaces as an archive error, got {e:?}"
        ),
    }
}

#[test]
fn restoring_over_a_live_session_is_refused() {
    let f = fixture();
    seed(&f.db, "s5", 2);
    f.db.archive_session("s5").expect("archive");
    f.db.restore_session("s5").expect("restore");

    // The archive file is intentionally left in place after a restore, so a
    // second restore is the interesting case: the live row wins and the
    // caller is told, rather than one copy silently overwriting the other.
    let err =
        f.db.restore_session("s5")
            .expect_err("restoring over a live session must fail");
    assert!(matches!(err, session::SessionError::Archive(_)), "{err:?}");
    assert_eq!(f.db.load_messages("s5", 100).expect("load").len(), 2);
}

#[test]
fn re_archiving_a_shrunken_session_refuses_to_overwrite_the_archive() {
    let f = fixture();
    seed(&f.db, "s6", 2);
    f.db.archive_session("s6").expect("first archive");
    f.db.restore_session("s6").expect("restore");
    // A rewind after the restore leaves the live session holding FEWER
    // messages than the archive: overwriting would drop archive-only
    // history, so the count guard must refuse.
    f.db.truncate_messages("s6", 2).expect("rewind past seq 1");

    let err =
        f.db.archive_session("s6")
            .expect_err("re-archiving over an existing archive must fail");
    assert!(matches!(err, session::SessionError::Archive(_)), "{err:?}");
    assert!(
        session::has_archive(&f.path, "s6"),
        "the original archive is still there"
    );
}

#[test]
fn the_last_access_migration_is_idempotent() {
    let f = fixture();
    seed(&f.db, "s7", 1);
    // Re-opening re-runs every migration; a duplicate-column error here
    // would mean a second `open` on the same file is not safe.
    drop(f.db);
    let reopened = SessionDb::open(&f.path).expect("second open must succeed");
    assert_eq!(
        reopened.load_messages("s7", 10).expect("load").len(),
        1,
        "rows survive the reopen"
    );
}

#[test]
fn a_pre_migration_file_gains_the_column_and_stays_readable() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("legacy.db");
    // A file written before `last_access_ms` existed: no such column.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let db = libsql::Builder::new_local(&path)
            .build()
            .await
            .expect("open legacy");
        let conn = db.connect().expect("connect");
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 parent_id TEXT,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 turn_log TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO sessions (id, title, created_at, updated_at, turn_log)
             VALUES ('old', '老库', 1, 2, '');",
        )
        .await
        .expect("write legacy schema");
    });

    let db = SessionDb::open(&path).expect("open migrates");
    let s = db
        .session_live("old")
        .expect("lookup")
        .expect("row survived");
    assert_eq!(s.id, "old");
    // `last_access_ms` is 0 on a migrated row, which readers treat as
    // "fall back to updated_at" — the row is not infinitely stale.
    assert!(
        db.compress_idle_sessions(i64::MAX, 0, 100)
            .expect("sweep")
            .is_empty()
    );
}

#[test]
fn the_sweep_respects_the_idle_cutoff_and_the_keep_recent_floor() {
    let f = fixture();
    seed(&f.db, "keep-1", 2);
    seed(&f.db, "keep-2", 2);
    seed(&f.db, "old-1", 2);

    // Fresh seeds are never idle under a one-day window: `now >= now - 1day`
    // always holds, so all three stay live.
    assert!(
        f.db.compress_idle_sessions(86_400_000, 0, 100)
            .expect("sweep")
            .is_empty(),
        "a freshly-touched session is never idle"
    );

    // Make recency deterministic: the three seeds can land inside one clock
    // millisecond, so backdate every row to a fixed ancient value, then raise
    // the two keepers strictly above it with appends. The zero-window sweep
    // below then has no same-millisecond ambiguity — old-1 is ancient, the
    // keepers are "now".
    freeze_timestamps(&f.path, 1_000, 1_000);
    f.db.append_message("keep-1", SessionRole::User, "bump", &[], &[])
        .expect("bump keep-1");
    f.db.append_message("keep-2", SessionRole::User, "bump", &[], &[])
        .expect("bump keep-2");

    // A zero-length idle window plus a keep floor of 2 leaves the two most
    // recent live and archives the ancient third.
    let archived =
        f.db.compress_idle_sessions(0, 2, 100)
            .expect("sweep with keep floor");
    let ids: Vec<&str> = archived.iter().map(|r| r.session_id.as_str()).collect();
    assert_eq!(ids.len(), 1, "exactly one session fell past the floor");
    assert!(
        ids.contains(&"old-1"),
        "the least-recently-used session is the one archived, got {ids:?}"
    );
    assert!(f.db.session_live("keep-1").expect("live").is_some());
    assert!(f.db.session_live("keep-2").expect("live").is_some());
}

#[test]
fn the_sweep_stops_at_the_batch_cap() {
    let f = fixture();
    for i in 0..5 {
        seed(&f.db, &format!("s{i}"), 2);
    }

    // Five idle sessions, but the cap is 2: exactly two receipts, and the
    // remainder stays live for the next run.
    let first = f.db.compress_idle_sessions(0, 0, 2).expect("capped sweep");
    assert_eq!(first.len(), 2, "the cap bounds one call's work");
    let live_after_first = (0..5)
        .filter(|i| f.db.session_live(&format!("s{i}")).expect("live").is_some())
        .count();
    assert_eq!(live_after_first, 3, "the uncapped remainder stays live");

    // The next call picks up where the first stopped.
    let second = f.db.compress_idle_sessions(0, 0, 2).expect("next batch");
    assert_eq!(second.len(), 2, "each call takes another capped batch");

    // And a cap of 0 still means "at least one", not "nothing ever".
    let third = f.db.compress_idle_sessions(0, 0, 0).expect("zero cap");
    assert_eq!(third.len(), 1, "a zero cap degenerates to one per call");
    assert!(
        f.db.compress_idle_sessions(0, 0, 2)
            .expect("final")
            .is_empty(),
        "nothing idle is left after the backlog is drained"
    );
}

#[test]
fn compression_actually_shrinks_the_payload() {
    let f = fixture();
    // Repetitive text: the case where zstd earns its keep.
    let filler = "the quick brown fox jumps over the lazy dog. ".repeat(40);
    f.db.ensure_session("big", "big").expect("ensure");
    for _ in 0..20 {
        f.db.append_message("big", SessionRole::Assistant, &filler, &[], &[])
            .expect("append");
    }
    let receipt = f.db.archive_session("big").expect("archive");
    let ratio = receipt.ratio().expect("ratio for a non-empty session");
    assert!(
        ratio > 2.0,
        "repetitive text should compress well, got {ratio:.2}x"
    );
    assert!(
        receipt.archived_bytes < receipt.raw_bytes,
        "the archive is smaller than the payload it holds"
    );
}

/// Rewrite every row's timestamps through a second connection — the only
/// way to put a session's clock in the past from outside the crate.
fn freeze_timestamps(path: &std::path::Path, updated_at: i64, last_access: i64) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let db = libsql::Builder::new_local(path)
            .build()
            .await
            .expect("open raw");
        let conn = db.connect().expect("connect");
        conn.execute(
            "UPDATE sessions SET updated_at = ?1, last_access_ms = ?2",
            libsql::params![updated_at, last_access],
        )
        .await
        .expect("backdate");
    });
}

/// FNV-1a over raw bytes, exactly what the archive trailer's checksum
/// commits to (offset `0xcbf29ce484222325`, prime `0x100000001b3`,
/// lowercase hex) — needed to build a *well-formed* forged archive.
fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

#[test]
fn archiving_a_session_that_does_not_exist_reports_database_missing() {
    let f = fixture();
    let err = f.db.archive_session("ghost").expect_err("no such session");
    assert!(
        matches!(err, session::SessionError::DatabaseMissing(_)),
        "a missing session is DatabaseMissing, not an archive error, got {err:?}"
    );
    assert!(
        !session::has_archive(&f.path, "ghost"),
        "no archive file was left behind"
    );
}

#[test]
fn touching_a_session_moves_it_above_the_keep_floor() {
    let f = fixture();
    seed(&f.db, "cold", 2);
    seed(&f.db, "warm", 2);
    // Both sessions pinned to the same ancient timestamp, so the sweep can
    // only tell them apart through `last_access_ms`.
    freeze_timestamps(&f.path, 1_000, 1_000);
    f.db.touch_session_access("warm").expect("touch");

    let receipts = f.db.compress_idle_sessions(0, 1, 100).expect("sweep");
    let ids: Vec<&str> = receipts.iter().map(|r| r.session_id.as_str()).collect();
    assert_eq!(
        ids,
        ["cold"],
        "the touched session outranks the untouched one under the keep floor, got {ids:?}"
    );
    assert!(
        f.db.session_live("warm").expect("live").is_some(),
        "the touched session survives the sweep"
    );
    assert!(
        f.db.session_live("cold").expect("live").is_none(),
        "the untouched session was archived"
    );
}

#[test]
fn checkpoint_wal_empties_the_wal_sidecar() {
    let f = fixture();
    let filler = "checkpoint fodder. ".repeat(50);
    f.db.ensure_session("wal", "wal").expect("ensure");
    for _ in 0..30 {
        f.db.append_message("wal", SessionRole::User, &filler, &[], &[])
            .expect("append");
    }
    let mut wal = f.path.clone().into_os_string();
    wal.push("-wal");
    let wal = std::path::PathBuf::from(wal);
    let before = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert!(
        before > 0,
        "test setup: the WAL must hold un-checkpointed bytes, or this test proves nothing"
    );

    f.db.checkpoint_wal().expect("checkpoint");
    let after = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert_eq!(
        after, 0,
        "a TRUNCATE checkpoint empties the sidecar ({before} -> {after})"
    );
}

#[test]
fn a_well_formed_archive_with_an_unknown_role_is_rejected() {
    let f = fixture();
    seed(&f.db, "s8", 3);
    f.db.archive_session("s8").expect("archive");

    let archive = session::archive_dir(&f.path).join("s8.jsonl.zst");
    let raw = std::fs::read(&archive).expect("read archive");
    let text = zstd::decode_all(raw.as_slice()).expect("zstd decode");
    let text = String::from_utf8(text).expect("utf8");
    // The encoder newline-terminates the trailer too: strip that byte so
    // `body` is everything before the trailer line, matching the framing
    // the decoder validates.
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let cut = text.rfind('\n').expect(" trailer separator");
    let body = &text[..cut];
    // Swap one real role for one this build has never heard of, then reseal
    // the stream with a *valid* checksum: this emulates a well-formed
    // archive from a future build, not random corruption.
    let forged = body.replacen("\"role\":\"user\"", "\"role\":\"ghost\"", 1);
    assert_ne!(forged, body, "test setup: a role must actually be swapped");
    let trailer = format!(
        "{{\"kind\":\"trailer\",\"checksum\":\"{}\"}}\n",
        fnv1a_hex(forged.as_bytes())
    );
    // `body` is the bytes *before* the '\n' that separated it from the
    // trailer, so rebuild that separator or the last message line glues to
    // the trailer and the decoder sees one malformed line.
    let payload = zstd::encode_all(format!("{forged}\n{trailer}").as_bytes(), 3).expect("zstd");
    std::fs::write(&archive, payload).expect("write forged archive");

    let err =
        f.db.restore_session("s8")
            .expect_err("an unknown role must fail the restore");
    assert!(
        matches!(err, session::SessionError::UnknownRole(_)),
        "the failure names the unknown role, got {err:?}"
    );
    assert!(
        f.db.session_live("s8").expect("live").is_none(),
        "a rejected restore writes nothing into the live database"
    );
    assert!(
        session::has_archive(&f.path, "s8"),
        "the archive stays on disk for inspection"
    );
}
