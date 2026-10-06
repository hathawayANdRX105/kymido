//! C01 durable context ledger: the canonical append-only event store and the
//! versioned host handle used to cite an event's original text.
//!
//! ## Why events are not messages
//!
//! `messages` is the UI transcript: it has no branch/epoch identity, no run
//! boundary, and no tool-call pairing. C01 needs all three to make context
//! projection reproducible:
//!
//! * `branch_id` + `epoch` identify *which* history an event belongs to.
//!   A rewind/truncate advances the history epoch (a logical change) instead
//!   of reusing row ids, so a stale projection can be rejected rather than
//!   silently applied to a different history.
//! * `run_id` / `turn_id` are the real run boundaries; a steering message or
//!   a tool result shares the run of the turn it belongs to and never opens a
//!   new turn just because its role happens to be `user`.
//! * `tool_call_id` lets a [`ToolGroup`] be read back as a closed pair, so a
//!   projection method can never cut a call away from its result.
//!
//! ## Idempotency
//!
//! Every event carries an `idempotency_key` (unique). Appending with a key
//! that is already present returns the stored row and writes nothing — the
//! one legitimate replay path. Identical *text* is **not** a dedup key: two
//! genuinely repeated user messages are two events and both survive.
//!
//! ## Source references
//!
//! [`SourceRef`] is a versioned host handle:
//! `schema_version + workspace_id + session_id + branch_id + epoch + event_id`.
//! Only the host creates and parses it. Parsing re-checks the handle against
//! the caller's current [`SourceScope`] — the encoding itself is not an
//! authorization, an out-of-scope or truncated handle is refused at read
//! time.

use serde::{Deserialize, Serialize};

use crate::{SessionDb, SessionError};

// -----------------------------------------------------------------------------
// SHA-256 (self-contained; the crate has no hashing dependency)
// -----------------------------------------------------------------------------

/// Hex-encoded SHA-256 of `bytes`.
///
/// Implemented here rather than pulled in as a dependency: the store needs a
/// content address for object manifests and an integrity hash for events, and
/// a dependency-free implementation keeps the crate's frozen dependency set
/// intact. Correctness is pinned by the object round-trip / tamper tests in
/// `tests/context_replay.rs`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for byte in sha256(bytes) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut message = bytes.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h = [
            h[0].wrapping_add(a),
            h[1].wrapping_add(b),
            h[2].wrapping_add(c),
            h[3].wrapping_add(d),
            h[4].wrapping_add(e),
            h[5].wrapping_add(f),
            h[6].wrapping_add(g),
            h[7].wrapping_add(hh),
        ];
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

// -----------------------------------------------------------------------------
// Enums
// -----------------------------------------------------------------------------

/// What an event *is*, independent of who produced it. A tool result is a
/// `ToolResult` whether the tool is local or remote; `Imported` marks content
/// that arrived through `session.append` from an external source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEventKind {
    Prompt,
    Assistant,
    ToolCall,
    ToolResult,
    Steering,
    Aside,
    TurnEnd,
    Abort,
    Imported,
}

impl ContextEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Assistant => "assistant",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Steering => "steering",
            Self::Aside => "aside",
            Self::TurnEnd => "turn_end",
            Self::Abort => "abort",
            Self::Imported => "imported",
        }
    }

    pub fn parse(value: &str) -> Result<Self, SessionError> {
        Ok(match value {
            "prompt" => Self::Prompt,
            "assistant" => Self::Assistant,
            "tool_call" => Self::ToolCall,
            "tool_result" => Self::ToolResult,
            "steering" => Self::Steering,
            "aside" => Self::Aside,
            "turn_end" => Self::TurnEnd,
            "abort" => Self::Abort,
            "imported" => Self::Imported,
            other => {
                return Err(SessionError::Context(format!(
                    "unknown context event kind `{other}`"
                )));
            }
        })
    }
}

/// Where the event actually came from. `Synthetic` is host-generated text
/// (e.g. a recovery marker) and must never be presented as user speech.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    User,
    Model,
    Tool,
    System,
    Synthetic,
    Import,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Model => "model",
            Self::Tool => "tool",
            Self::System => "system",
            Self::Synthetic => "synthetic",
            Self::Import => "import",
        }
    }

    pub fn parse(value: &str) -> Result<Self, SessionError> {
        Ok(match value {
            "user" => Self::User,
            "model" => Self::Model,
            "tool" => Self::Tool,
            "system" => Self::System,
            "synthetic" => Self::Synthetic,
            "import" => Self::Import,
            other => {
                return Err(SessionError::Context(format!(
                    "unknown source kind `{other}`"
                )));
            }
        })
    }
}

// -----------------------------------------------------------------------------
// Records
// -----------------------------------------------------------------------------

/// One canonical ledger event, exactly as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextEvent {
    pub event_id: String,
    pub session_id: String,
    pub branch_id: String,
    pub epoch: i64,
    pub turn_id: Option<String>,
    pub run_id: Option<String>,
    pub event_order: i64,
    pub role: String,
    pub kind: ContextEventKind,
    pub payload_json: String,
    pub source_kind: SourceKind,
    pub tool_call_id: Option<String>,
    pub idempotency_key: String,
    pub created_at: i64,
    pub integrity_hash: String,
}

impl ContextEvent {
    /// Decoded payload. Parsing is deferred to the caller so a row can be
    /// scanned (e.g. by `run_id`) without paying JSON decode for every event.
    pub fn payload(&self) -> Result<serde_json::Value, SessionError> {
        serde_json::from_str(&self.payload_json)
            .map_err(|e| SessionError::Context(format!("event payload is not JSON: {e}")))
    }
}

/// Caller-supplied fields for [`SessionDb::append_context_event`]. The store
/// fills in `created_at` and `integrity_hash`.
#[derive(Debug, Clone)]
pub struct ContextEventDraft {
    pub event_id: String,
    pub session_id: String,
    pub branch_id: String,
    pub epoch: i64,
    pub turn_id: Option<String>,
    pub run_id: Option<String>,
    pub event_order: i64,
    pub role: String,
    pub kind: ContextEventKind,
    pub payload: serde_json::Value,
    pub source_kind: SourceKind,
    pub tool_call_id: Option<String>,
    pub idempotency_key: String,
}

/// A call/result pair located within one scope. Only closed pairs (both the
/// `tool_call` and its matching `tool_result` are present) are returned; a
/// result whose call is missing stays invisible so no method can pair it with
/// the wrong call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolGroup {
    pub tool_call_id: String,
    pub call: ContextEvent,
    pub result: ContextEvent,
}

// -----------------------------------------------------------------------------
// Migration
// -----------------------------------------------------------------------------

/// DDL for the event ledger, spliced into [`crate::apply_schema`]. Every
/// statement is `IF NOT EXISTS`, so it is idempotent by construction and a
/// pre-C01 file gains the tables on first open with no data movement.
pub(crate) const CONTEXT_EVENTS_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS context_events (
        event_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        branch_id TEXT NOT NULL,
        epoch INTEGER NOT NULL,
        turn_id TEXT,
        run_id TEXT,
        event_order INTEGER NOT NULL,
        role TEXT NOT NULL,
        kind TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        source_kind TEXT NOT NULL,
        tool_call_id TEXT,
        idempotency_key TEXT NOT NULL UNIQUE,
        created_at INTEGER NOT NULL,
        integrity_hash TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_context_events_scope_order
        ON context_events(session_id, branch_id, epoch, event_order);
    CREATE INDEX IF NOT EXISTS idx_context_events_run
        ON context_events(run_id);
    CREATE INDEX IF NOT EXISTS idx_context_events_turn
        ON context_events(turn_id);
";

/// Canonical byte string the integrity hash covers. Field order is fixed and
/// fields are separated by U+001F so no payload text can forge a boundary.
pub(crate) fn integrity_input(
    event_id: &str,
    session_id: &str,
    branch_id: &str,
    epoch: i64,
    event_order: i64,
    role: &str,
    kind: &str,
    source_kind: &str,
    tool_call_id: Option<&str>,
    created_at: i64,
    payload_json: &str,
) -> String {
    [
        "context-event-v1",
        event_id,
        session_id,
        branch_id,
        &epoch.to_string(),
        &event_order.to_string(),
        role,
        kind,
        source_kind,
        tool_call_id.unwrap_or(""),
        &created_at.to_string(),
        payload_json,
    ]
    .join("\u{1f}")
}

fn row_to_event(row: &libsql::Row) -> Result<ContextEvent, SessionError> {
    Ok(ContextEvent {
        event_id: row.get::<String>(0)?,
        session_id: row.get::<String>(1)?,
        branch_id: row.get::<String>(2)?,
        epoch: row.get::<i64>(3)?,
        turn_id: row.get::<Option<String>>(4)?,
        run_id: row.get::<Option<String>>(5)?,
        event_order: row.get::<i64>(6)?,
        role: row.get::<String>(7)?,
        kind: ContextEventKind::parse(&row.get::<String>(8)?)?,
        payload_json: row.get::<String>(9)?,
        source_kind: SourceKind::parse(&row.get::<String>(10)?)?,
        tool_call_id: row.get::<Option<String>>(11)?,
        idempotency_key: row.get::<String>(12)?,
        created_at: row.get::<i64>(13)?,
        integrity_hash: row.get::<String>(14)?,
    })
}

const EVENT_COLUMNS: &str = "event_id, session_id, branch_id, epoch, turn_id, run_id, \
     event_order, role, kind, payload_json, source_kind, tool_call_id, \
     idempotency_key, created_at, integrity_hash";

// -----------------------------------------------------------------------------
// Store API
// -----------------------------------------------------------------------------

impl SessionDb {
    /// Append one event idempotently.
    ///
    /// Returns `(event, inserted)`. If `draft.idempotency_key` already exists
    /// the stored row is returned with `inserted = false` and nothing is
    /// written — this is the replay path, not a content dedup. A colliding
    /// `event_id` under a *different* idempotency key is a caller bug and is
    /// rejected.
    pub fn append_context_event(
        &self,
        draft: ContextEventDraft,
    ) -> Result<(ContextEvent, bool), SessionError> {
        if draft.event_id.trim().is_empty() {
            return Err(SessionError::Context(
                "context event_id must be non-empty".into(),
            ));
        }
        if draft.idempotency_key.trim().is_empty() {
            return Err(SessionError::Context(
                "context idempotency_key must be non-empty".into(),
            ));
        }
        let payload_json = serde_json::to_string(&draft.payload)
            .map_err(|e| SessionError::Context(format!("event payload is not JSON: {e}")))?;
        let created_at = crate::now_ms();
        let integrity_hash = sha256_hex(
            integrity_input(
                &draft.event_id,
                &draft.session_id,
                &draft.branch_id,
                draft.epoch,
                draft.event_order,
                &draft.role,
                draft.kind.as_str(),
                draft.source_kind.as_str(),
                draft.tool_call_id.as_deref(),
                created_at,
                &payload_json,
            )
            .as_bytes(),
        );

        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;

            if let Some(existing) = select_event_by_key(conn, &draft.idempotency_key).await? {
                return Ok((existing, false));
            }

            let tx = conn
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await?;
            // Re-check inside the write transaction: two concurrent appenders
            // with the same key race here, and `idempotency_key UNIQUE` makes
            // the loser's INSERT fail rather than duplicate.
            if let Some(existing) = select_event_by_key_tx(&tx, &draft.idempotency_key).await? {
                return Ok((existing, false));
            }
            // A primary-key collision under a fresh key is a caller bug, not a
            // replay: report it before the INSERT so unrelated SQL errors are
            // not mislabelled as an id collision.
            if select_event_in_tx(&tx, &draft.event_id).await?.is_some() {
                return Err(SessionError::Context(format!(
                    "context event_id `{}` already exists under a different idempotency key",
                    draft.event_id
                )));
            }
            tx.execute(
                "INSERT INTO context_events \
                 (event_id, session_id, branch_id, epoch, turn_id, run_id, event_order, \
                  role, kind, payload_json, source_kind, tool_call_id, idempotency_key, \
                  created_at, integrity_hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                libsql::params![
                    draft.event_id.as_str(),
                    draft.session_id.as_str(),
                    draft.branch_id.as_str(),
                    draft.epoch,
                    draft.turn_id.clone(),
                    draft.run_id.clone(),
                    draft.event_order,
                    draft.role.as_str(),
                    draft.kind.as_str(),
                    payload_json.as_str(),
                    draft.source_kind.as_str(),
                    draft.tool_call_id.clone(),
                    draft.idempotency_key.as_str(),
                    created_at,
                    integrity_hash.as_str(),
                ],
            )
            .await?;
            tx.commit().await?;

            let event = ContextEvent {
                event_id: draft.event_id,
                session_id: draft.session_id,
                branch_id: draft.branch_id,
                epoch: draft.epoch,
                turn_id: draft.turn_id,
                run_id: draft.run_id,
                event_order: draft.event_order,
                role: draft.role,
                kind: draft.kind,
                payload_json,
                source_kind: draft.source_kind,
                tool_call_id: draft.tool_call_id,
                idempotency_key: draft.idempotency_key,
                created_at,
                integrity_hash,
            };
            Ok((event, true))
        })
    }

    /// One event by primary key, or `None`.
    pub fn context_event(&self, event_id: &str) -> Result<Option<ContextEvent>, SessionError> {
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    &format!("SELECT {EVENT_COLUMNS} FROM context_events WHERE event_id = ?1"),
                    libsql::params![event_id],
                )
                .await?;
            match rows.next().await? {
                Some(row) => Ok(Some(row_to_event(&row)?)),
                None => Ok(None),
            }
        })
    }

    /// Every event of one run, in `event_order`. A run id is not unique across
    /// sessions/branches, so the scope is required here.
    pub fn context_events_for_run(
        &self,
        session_id: &str,
        branch_id: &str,
        epoch: i64,
        run_id: &str,
    ) -> Result<Vec<ContextEvent>, SessionError> {
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    &format!(
                        "SELECT {EVENT_COLUMNS} FROM context_events \
                         WHERE session_id = ?1 AND branch_id = ?2 AND epoch = ?3 AND run_id = ?4 \
                         ORDER BY event_order, event_id"
                    ),
                    libsql::params![session_id, branch_id, epoch, run_id],
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(row_to_event(&row)?);
            }
            Ok(out)
        })
    }

    /// Bounded, ordered window of `event_order` in `[from_order, to_order]`.
    /// `limit` obeys the same `1..=MAX_LIMIT` ceiling as every other list API.
    pub fn context_events_in_range(
        &self,
        session_id: &str,
        branch_id: &str,
        epoch: i64,
        from_order: i64,
        to_order: i64,
        limit: u32,
    ) -> Result<Vec<ContextEvent>, SessionError> {
        let limit = crate::validate_limit(limit)?;
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    &format!(
                        "SELECT {EVENT_COLUMNS} FROM context_events \
                         WHERE session_id = ?1 AND branch_id = ?2 AND epoch = ?3 \
                           AND event_order >= ?4 AND event_order <= ?5 \
                         ORDER BY event_order, event_id LIMIT ?6"
                    ),
                    libsql::params![session_id, branch_id, epoch, from_order, to_order, limit],
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(row_to_event(&row)?);
            }
            Ok(out)
        })
    }

    /// Closed tool groups of a scope, in call order. A group is closed only
    /// when both the call and its matching result are present; unpaired
    /// results are omitted (see [`ToolGroup`]).
    pub fn closed_tool_groups(
        &self,
        session_id: &str,
        branch_id: &str,
        epoch: i64,
    ) -> Result<Vec<ToolGroup>, SessionError> {
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            // Join the call to its result by tool_call_id inside one scope.
            // MIN(...) picks the earliest of a duplicated side rather than
            // inventing a cross product.
            let call_cols = qualified_columns("c");
            let result_cols = qualified_columns("r");
            let mut rows = conn
                .query(
                    &format!(
                        "SELECT c.tool_call_id, {call_cols}, {result_cols} \
                         FROM context_events c \
                         JOIN context_events r \
                           ON r.session_id = c.session_id \
                          AND r.branch_id = c.branch_id \
                          AND r.epoch = c.epoch \
                          AND r.tool_call_id = c.tool_call_id \
                          AND r.kind = 'tool_result' \
                         WHERE c.session_id = ?1 AND c.branch_id = ?2 AND c.epoch = ?3 \
                           AND c.kind = 'tool_call' AND c.tool_call_id IS NOT NULL \
                         GROUP BY c.tool_call_id \
                         ORDER BY MIN(c.event_order), c.tool_call_id"
                    ),
                    libsql::params![session_id, branch_id, epoch],
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(ToolGroup {
                    tool_call_id: row.get::<String>(0)?,
                    call: row_to_event_at(&row, 1)?,
                    result: row_to_event_at(&row, 1 + EVENT_COLUMN_COUNT)?,
                });
            }
            Ok(out)
        })
    }

    /// Run ids of a session that reached a terminal boundary (`turn_end` or
    /// `abort`), newest first. These are the "recent complete runs" a
    /// projection can safely cover; a run still in flight is not returned.
    pub fn recent_complete_runs(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<String>, SessionError> {
        let limit = crate::validate_limit(limit)?;
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    "SELECT run_id FROM context_events \
                     WHERE session_id = ?1 AND run_id IS NOT NULL \
                       AND kind IN ('turn_end', 'abort') \
                     GROUP BY run_id \
                     ORDER BY MAX(event_order) DESC LIMIT ?2",
                    libsql::params![session_id, limit],
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(row.get::<String>(0)?);
            }
            Ok(out)
        })
    }
}

/// Column count of [`EVENT_COLUMNS`], used to index into a joined row that
/// carries two event column bundles back to back.
const EVENT_COLUMN_COUNT: i32 = 15;

/// Read one event starting at `base` in a row that stores columns flattened
/// (used by the tool-group join, which selects two bundles side by side).
fn row_to_event_at(row: &libsql::Row, base: i32) -> Result<ContextEvent, SessionError> {
    Ok(ContextEvent {
        event_id: row.get::<String>(base)?,
        session_id: row.get::<String>(base + 1)?,
        branch_id: row.get::<String>(base + 2)?,
        epoch: row.get::<i64>(base + 3)?,
        turn_id: row.get::<Option<String>>(base + 4)?,
        run_id: row.get::<Option<String>>(base + 5)?,
        event_order: row.get::<i64>(base + 6)?,
        role: row.get::<String>(base + 7)?,
        kind: ContextEventKind::parse(&row.get::<String>(base + 8)?)?,
        payload_json: row.get::<String>(base + 9)?,
        source_kind: SourceKind::parse(&row.get::<String>(base + 10)?)?,
        tool_call_id: row.get::<Option<String>>(base + 11)?,
        idempotency_key: row.get::<String>(base + 12)?,
        created_at: row.get::<i64>(base + 13)?,
        integrity_hash: row.get::<String>(base + 14)?,
    })
}

async fn select_event_by_key(
    conn: &libsql::Connection,
    key: &str,
) -> Result<Option<ContextEvent>, SessionError> {
    let mut rows = conn
        .query(
            &format!("SELECT {EVENT_COLUMNS} FROM context_events WHERE idempotency_key = ?1"),
            libsql::params![key],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row_to_event(&row)?)),
        None => Ok(None),
    }
}

/// [`select_event_by_key`] against an open transaction — the re-check that
/// runs once the write lock is held.
async fn select_event_by_key_tx(
    tx: &libsql::Transaction,
    key: &str,
) -> Result<Option<ContextEvent>, SessionError> {
    let mut rows = tx
        .query(
            &format!("SELECT {EVENT_COLUMNS} FROM context_events WHERE idempotency_key = ?1"),
            libsql::params![key],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row_to_event(&row)?)),
        None => Ok(None),
    }
}

/// One event by primary key inside a transaction, for the collision check that
/// must run before an INSERT.
async fn select_event_in_tx(
    tx: &libsql::Transaction,
    event_id: &str,
) -> Result<Option<ContextEvent>, SessionError> {
    let mut rows = tx
        .query(
            &format!("SELECT {EVENT_COLUMNS} FROM context_events WHERE event_id = ?1"),
            libsql::params![event_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row_to_event(&row)?)),
        None => Ok(None),
    }
}

/// `a, b, c` → `alias.a, alias.b, alias.c`, for a self-join that must
/// disambiguate two column bundles.
fn qualified_columns(alias: &str) -> String {
    EVENT_COLUMNS
        .split(", ")
        .map(|col| format!("{alias}.{col}"))
        .collect::<Vec<_>>()
        .join(", ")
}

// -----------------------------------------------------------------------------
// Versioned source references
// -----------------------------------------------------------------------------

/// Schema version of the [`SourceRef`] encoding. A handle of any other version
/// is refused rather than best-effort parsed.
pub const SOURCE_REF_SCHEMA_VERSION: u32 = 1;

/// The authorization scope a read is performed under. A [`SourceRef`] only
/// resolves inside an exactly matching scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceScope {
    pub workspace_id: String,
    pub session_id: String,
    pub branch_id: String,
    pub epoch: i64,
}

impl SourceScope {
    pub fn new(
        workspace_id: impl Into<String>,
        session_id: impl Into<String>,
        branch_id: impl Into<String>,
        epoch: i64,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            session_id: session_id.into(),
            branch_id: branch_id.into(),
            epoch,
        }
    }
}

/// A versioned host handle to one event's original text. Only the host creates
/// and parses it; holding the encoded bytes is *not* authorization — reads
/// re-run [`SourceRef::require_scope`] against the caller's current scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub schema_version: u32,
    pub workspace_id: String,
    pub session_id: String,
    pub branch_id: String,
    pub epoch: i64,
    pub event_id: String,
}

impl SourceRef {
    /// Build a handle inside `scope`. The schema version is always the current
    /// one; callers cannot select it.
    pub fn new(scope: &SourceScope, event_id: impl Into<String>) -> Self {
        Self {
            schema_version: SOURCE_REF_SCHEMA_VERSION,
            workspace_id: scope.workspace_id.clone(),
            session_id: scope.session_id.clone(),
            branch_id: scope.branch_id.clone(),
            epoch: scope.epoch,
            event_id: event_id.into(),
        }
    }

    /// Serialize to host-owned bytes. The result is opaque to callers.
    pub fn encode(&self) -> String {
        // Plain JSON-safe data; serialization cannot fail.
        serde_json::to_string(self).expect("SourceRef is JSON-safe")
    }

    /// Parse host-owned bytes, refusing an unknown schema version. Site
    /// identity is *not* checked here — call [`Self::require_scope`] before
    /// using the reference.
    pub fn parse(encoded: &str) -> Result<Self, SessionError> {
        let parsed: SourceRef = serde_json::from_str(encoded)
            .map_err(|e| SessionError::Context(format!("malformed SourceRef: {e}")))?;
        if parsed.schema_version != SOURCE_REF_SCHEMA_VERSION {
            return Err(SessionError::Context(format!(
                "unsupported SourceRef schema_version {} (expected {SOURCE_REF_SCHEMA_VERSION})",
                parsed.schema_version
            )));
        }
        Ok(parsed)
    }

    /// Whether this handle belongs to exactly `scope`.
    pub fn in_scope(&self, scope: &SourceScope) -> bool {
        self.workspace_id == scope.workspace_id
            && self.session_id == scope.session_id
            && self.branch_id == scope.branch_id
            && self.epoch == scope.epoch
    }

    /// [`Self::in_scope`] as a hard gate.
    pub fn require_scope(&self, scope: &SourceScope) -> Result<(), SessionError> {
        if self.in_scope(scope) {
            Ok(())
        } else {
            Err(SessionError::Context(format!(
                "SourceRef for event `{}` is outside the requested scope",
                self.event_id
            )))
        }
    }

    /// Parse *and* scope-check in one step — the read path that must never
    /// trust the encoding on its own.
    pub fn decode_in_scope(encoded: &str, scope: &SourceScope) -> Result<Self, SessionError> {
        let parsed = Self::parse(encoded)?;
        parsed.require_scope(scope)?;
        Ok(parsed)
    }
}
