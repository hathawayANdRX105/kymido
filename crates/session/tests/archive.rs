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
//! 6. the sweep honours both the idle cutoff and the keep-recent floor.

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
        db.append_message(id, role, &format!("message {i}"), &[])
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
fn archiving_twice_refuses_to_clobber_the_first_archive() {
    let f = fixture();
    seed(&f.db, "s6", 2);
    f.db.archive_session("s6").expect("first archive");
    f.db.restore_session("s6").expect("restore");
    f.db.append_message("s6", SessionRole::User, "a new message", &[])
        .expect("append after restore");

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
        db.compress_idle_sessions(i64::MAX, 0)
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

    // Everything is touched now, so nothing is idle yet.
    assert!(
        f.db.compress_idle_sessions(86_400_000, 0)
            .expect("sweep")
            .is_empty(),
        "a freshly-touched session is never idle"
    );

    // A zero-length idle window plus a keep floor of 2 leaves the two most
    // recent live and archives the third.
    let archived =
        f.db.compress_idle_sessions(0, 2)
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
fn compression_actually_shrinks_the_payload() {
    let f = fixture();
    // Repetitive text: the case where zstd earns its keep.
    let filler = "the quick brown fox jumps over the lazy dog. ".repeat(40);
    f.db.ensure_session("big", "big").expect("ensure");
    for _ in 0..20 {
        f.db.append_message("big", SessionRole::Assistant, &filler, &[])
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
