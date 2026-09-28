//! `load_messages` tail-window semantics — the contract every caller leans on
//! (TUI history backfill, web open, daemon resume): the window is the LAST
//! `limit` messages returned in ascending `seq` order, and `validate_limit`
//! bounds are enforced before any query.
//!
//! Pinned against `lib.rs`: `validate_limit` (0 and > MAX_LIMIT rejected),
//! the `ORDER BY seq DESC LIMIT ?` tail walk flipped to ascending on return,
//! and the blank-id guard. Nothing here touches the archive machinery —
//! those contracts live in `archive.rs`.

use session::{SessionDb, SessionRole};
use tempfile::tempdir;

const MAX_LIMIT: u32 = 10_000;

/// Seed `n` messages, alternating roles, and return the db path owner.
struct Fixture {
    _dir: tempfile::TempDir,
    db: SessionDb,
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let db = SessionDb::open(dir.path().join("sessions.db")).expect("open");
    Fixture { _dir: dir, db }
}

fn seed(db: &SessionDb, id: &str, n: i64) {
    db.ensure_session(id, "window").expect("ensure");
    for i in 1..=n {
        let role = if i % 2 == 0 {
            SessionRole::Assistant
        } else {
            SessionRole::User
        };
        db.append_message(id, role, &format!("m{i:02}"), &[], &[])
            .expect("append");
    }
}

#[test]
fn limit_returns_the_tail_in_ascending_order() {
    let f = fixture();
    seed(&f.db, "s", 10);

    let rows = f.db.load_messages("s", 3).expect("load");
    let seqs: Vec<i64> = rows.iter().map(|m| m.seq).collect();
    assert_eq!(seqs, vec![8, 9, 10], "tail window, ascending");
    assert_eq!(rows[0].text, "m08");
    assert_eq!(rows[2].text, "m10");
}

#[test]
fn limit_above_row_count_returns_everything_ascending() {
    let f = fixture();
    seed(&f.db, "s", 4);

    let rows = f.db.load_messages("s", 50).expect("load");
    let seqs: Vec<i64> = rows.iter().map(|m| m.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
}

#[test]
fn limit_zero_is_rejected() {
    let f = fixture();
    seed(&f.db, "s", 2);

    let err = f.db.load_messages("s", 0).expect_err("0 must reject");
    assert!(
        matches!(err, session::SessionError::InvalidLimit(0)),
        "got {err:?}"
    );
}

#[test]
fn limit_over_max_is_rejected_and_max_is_accepted() {
    let f = fixture();
    seed(&f.db, "s", 1);

    let err =
        f.db.load_messages("s", MAX_LIMIT + 1)
            .expect_err("over-max must reject");
    assert!(
        matches!(err, session::SessionError::InvalidLimit(n) if n == MAX_LIMIT + 1),
        "got {err:?}"
    );

    // MAX_LIMIT itself is the inclusive upper bound.
    f.db.load_messages("s", MAX_LIMIT).expect("max accepted");
}

#[test]
fn empty_session_loads_an_empty_vec() {
    let f = fixture();
    f.db.ensure_session("s", "empty").expect("ensure");

    let rows = f.db.load_messages("s", 10).expect("load");
    assert!(rows.is_empty(), "no rows in, no rows out");
}

#[test]
fn blank_session_id_is_rejected() {
    let f = fixture();

    let err = f.db.load_messages("   ", 5).expect_err("blank must reject");
    assert!(
        matches!(err, session::SessionError::InvalidSessionId),
        "got {err:?}"
    );
}
