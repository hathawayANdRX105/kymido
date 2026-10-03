//! Shared local session/message store backed by libSQL (SQLite-compatible).
//!
//! Designed for use by the daemon, CLI, and agent tools. A single
//! [`SessionDb`] owns one connection to one local `*.db` file. Cross-thread
//! safety comes from a `parking_lot::Mutex` around the connection and from
//! running every call through one current-thread Tokio runtime created on `open`.
//!
//! ponytail: one connection + one runtime per `SessionDb`. Cross-thread
//! callers (multiple `SessionDb::open`s to the same file) rely on SQLite WAL
//! + `busy_timeout`; per-connection serialization here is to prevent
//!   re-entrant `block_on` on the current-thread runtime, not to provide
//!   cross-process mutual exclusion.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

// ponytail: only the `core` libsql feature is on; this drops the default
// `serde_json`/`reqwest`/HTTP/WebAssembly backends and the C-bundled SQLite
// is still present. Bump if you need remote/replica/sync features.
use libsql::{Connection, TransactionBehavior};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Maximum allowed `limit` for list / load / search calls. Hard cap to keep a
/// runaway caller from forcing the process to materialize a huge result set.
pub const MAX_LIMIT: u32 = 1000;

/// Fixed identity label stamped on every row [`SessionDb::rewind_messages`]
/// backs up (T13 turn rewind). Same pattern as desktop turnrewind's
/// `SNAPSHOT_IDENTITY`: a constant, not a per-call value, so every backup
/// generation is attributable to this feature and auditable as one family —
/// the ledger keeps history, this label says which rows were discarded and
/// preserved.
pub const SNAPSHOT_IDENTITY: &str = "KYMIDO T13 Rewind";

// -----------------------------------------------------------------------------
// Errors
// -----------------------------------------------------------------------------

/// Errors returned by every `SessionDb` operation.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// libsql returned an error from the underlying SQLite engine.
    #[error("libsql error: {0}")]
    Libsql(#[from] libsql::Error),

    /// I/O error (e.g. creating the parent directory of the database file).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Caller asked for `limit == 0` or `limit > MAX_LIMIT`.
    #[error("invalid limit {0}: must be in 1..={MAX_LIMIT}")]
    InvalidLimit(u32),

    /// Caller passed a role string that is not one of the known
    /// [`SessionRole`] variants.
    #[error("unknown role `{0}` (expected one of user, assistant, system, tool)")]
    UnknownRole(String),

    /// Caller passed an empty/whitespace-only session id or title.
    #[error("invalid session id: must be non-empty")]
    InvalidSessionId,

    /// Caller passed an empty message body.
    #[error("invalid message text: must be non-empty")]
    InvalidMessageText,

    /// Caller passed an empty search query.
    #[error("invalid search query: must be non-empty")]
    InvalidSearchQuery,

    /// Caller passed an empty list-sessions query.
    #[error("invalid list query: must be non-empty")]
    InvalidListQuery,

    /// Tokio runtime could not be created. Stored as a string because
    /// `tokio::runtime::Builder::build` returns `io::Error` rather than a
    /// named error type, and we already wrap plain `io::Error` separately.
    #[error("failed to build tokio runtime: {0}")]
    RuntimeBuild(String),

    /// Asked the database to operate on a path that does not exist on disk.
    #[error("database file `{0}` does not exist")]
    DatabaseMissing(PathBuf),

    /// A turn-log line could not be parsed.  Repair refuses to guess: a log
    /// it cannot fully read is refused, never silently rewritten.
    #[error("malformed turn log line {line}: {reason}")]
    MalformedTurnLog { line: usize, reason: String },

    /// A cold archive could not be written, read, or verified. The string is
    /// the reason; a corrupt or truncated archive is reported rather than
    /// partially restored.
    #[error("archive error: {0}")]
    Archive(String),
}

impl SessionError {
    fn invalid_id_if_blank(id: &str) -> Result<(), SessionError> {
        if id.trim().is_empty() {
            Err(SessionError::InvalidSessionId)
        } else {
            Ok(())
        }
    }
}

// -----------------------------------------------------------------------------
// Roles
// -----------------------------------------------------------------------------

/// A message role. Kept small and string-backed so it can round-trip through
/// both the SQL column and serde without an extra enum-mapping layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionRole {
    User,
    Assistant,
    System,
    Tool,
}

impl SessionRole {
    /// Canonical lowercase wire/storage form (`"user"`, `"assistant"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            SessionRole::User => "user",
            SessionRole::Assistant => "assistant",
            SessionRole::System => "system",
            SessionRole::Tool => "tool",
        }
    }

    /// Parse from any case. Returns [`SessionError::UnknownRole`] on miss.
    pub fn parse(s: &str) -> Result<SessionRole, SessionError> {
        match s.to_ascii_lowercase().as_str() {
            "user" => Ok(SessionRole::User),
            "assistant" => Ok(SessionRole::Assistant),
            "system" => Ok(SessionRole::System),
            "tool" => Ok(SessionRole::Tool),
            _ => Err(SessionError::UnknownRole(s.to_string())),
        }
    }
}

impl fmt::Display for SessionRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// -----------------------------------------------------------------------------
// Records
// -----------------------------------------------------------------------------

/// Row from `list_sessions` — the fields the UI needs to render a session
/// list. `message_count` is always populated by list/lookup paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    /// Parent session id — the lineage edge for 5.3/5.5 grouping. `None` for a
    /// root session (a `NULL` column, or a caller that never set one).
    ///
    /// Serde-optional: a client that omits the field deserializes as `None`,
    /// and `None` is emitted as an explicit `null` so every summary the daemon
    /// returns carries the key.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Unix epoch milliseconds. Matches `created_at` / `updated_at` columns.
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub message_count: u64,
}

/// One run recorded in the daemon's run ledger. `finished_at_ms` is `None`
/// while the run is in flight. Lives here, not in `daemon`, because the web
/// UI reads these rows and must not depend on the daemon implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    #[serde(default)]
    pub seq: i64,
    pub run_id: String,
    pub session_id: String,
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// One image attached to a user message, stored alongside the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Original file name, for UI display and diagnostics only.
    pub name: String,
    /// Whitelisted MIME type, e.g. "image/png" — validation happens
    /// upstream of the store.
    pub media_type: String,
    /// Plain base64 payload, no "data:" prefix.
    pub data: String,
}

/// One message row, in storage order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub session_id: String,
    /// Monotonic per-session, starting at 1. Assigned by
    /// [`SessionDb::append_message`] inside a single transaction.
    pub seq: i64,
    pub role: SessionRole,
    pub text: String,
    /// Unix epoch milliseconds — set when the row is appended.
    pub created_at_ms: i64,
    /// Images attached to this message; empty when it carries none.
    /// `#[serde(default)]` keeps payloads written before this field
    /// deserializable as "no attachments".
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// 该消息（assistant 轮）携带的工具调用卡，UI 形状由 web-state 拥有、
    /// 本层按不透明 JSON 数组透传；空 = 纯文本消息。持久化它，历史重载
    /// 才能重建「工作过程」折叠区（C5.2b live 投影的回放底座）。
    #[serde(default)]
    pub tool_calls: Vec<serde_json::Value>,
}

// -----------------------------------------------------------------------------
// Turn log (crash repair)
// -----------------------------------------------------------------------------

mod archive;
pub use archive::*;
mod turn_log;
pub use turn_log::*;
// -----------------------------------------------------------------------------
// SessionDb
// -----------------------------------------------------------------------------

/// One open session database. Cheap to clone (the inner state is `Arc`-shared).
///
/// `Send + Sync` so any thread can call methods. Internal serialization is a
/// `parking_lot::Mutex` over the single `Connection`; this both serializes
/// reads/writes (libSQL/connection methods are `&self` but SQLite itself has
/// only one writer at a time) and prevents re-entrant `block_on` calls on the
/// current-thread runtime owned here.
#[derive(Clone)]
pub struct SessionDb {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    runtime: tokio::runtime::Runtime,
    conn: Mutex<Connection>,
}

// -----------------------------------------------------------------------------
// Construction
// -----------------------------------------------------------------------------

impl SessionDb {
    /// Open (or create) the database at `path`. Creates the parent directory
    /// if missing, applies schema + PRAGMAs, then hands back a ready
    /// `SessionDb`.
    ///
    /// The same file can be opened from multiple threads / processes — SQLite
    /// WAL + the `busy_timeout` PRAGMA handle cross-process contention. We
    /// set `busy_timeout` and `foreign_keys` before any DDL or `journal_mode`
    /// switch so concurrent `SessionDb::open` calls block on the lock instead
    /// of erroring with SQLITE_BUSY.
    pub fn open(path: impl AsRef<Path>) -> Result<SessionDb, SessionError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)?;
        }

        // Build the long-lived runtime first; use it for everything else.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| SessionError::RuntimeBuild(e.to_string()))?;

        let db = runtime.block_on(libsql::Builder::new_local(&path).build())?;
        let conn = db.connect()?;
        runtime.block_on(init_database(&conn))?;

        Ok(SessionDb {
            inner: Arc::new(Inner {
                path,
                runtime,
                conn: Mutex::new(conn),
            }),
        })
    }

    /// Path the database was opened with.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }
}

/// Number of retry rounds when `apply_schema` or `journal_mode` switching
/// hits a transient lock error. Combined with `busy_timeout`, this is a
/// safety net for the rare case where two callers race past the timeout
/// boundary (e.g. one process is mid-switch-to-WAL while another is mid-DDL).
/// Bounded on purpose: a runaway init that retries indefinitely would mask
/// real I/O or schema problems.
const INIT_LOCK_RETRIES: usize = 5;

/// Set the per-connection knobs that don't need a write lock first so any
/// later DDL or journal-mode switch can block on contention instead of
/// returning SQLITE_BUSY immediately.
async fn apply_safe_pragmas(conn: &Connection) -> Result<(), SessionError> {
    run_with_lock_retry(|| async {
        conn.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;")
            .await?;
        Ok(())
    })
    .await
}
async fn apply_schema(conn: &Connection) -> Result<(), SessionError> {
    let sql = "CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            parent_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            turn_log TEXT NOT NULL DEFAULT '',
            -- 0 = never touched since the column existed; readers fall back
            -- to `updated_at`, so a pre-migration file is not all-stale.
            last_access_ms INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS messages (
            session_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            role TEXT NOT NULL,
            text TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            attachments TEXT,
            tool_calls TEXT,
            PRIMARY KEY (session_id, seq),
            FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
         CREATE TABLE IF NOT EXISTS message_snapshots (
            id INTEGER PRIMARY KEY,
            session_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            role TEXT NOT NULL,
            text TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            attachments TEXT,
            tool_calls TEXT,
            identity TEXT NOT NULL,
            snapshotted_at INTEGER NOT NULL,
            FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS idx_message_snapshots_session
            ON message_snapshots(session_id);";
    run_with_lock_retry(|| async {
        conn.execute_batch(sql).await?;
        Ok(())
    })
    .await
}

/// Add the `turn_log` column to `sessions` when the file predates it.
///
/// SQLite has no `ADD COLUMN IF NOT EXISTS`, so the column list is read
/// first; the `ALTER` runs only when the column is missing. Idempotent —
/// every `open` runs it, and a database that already has the column costs
/// one `PRAGMA table_info` round trip.
async fn apply_turn_log_column(conn: &Connection) -> Result<(), SessionError> {
    let mut rows = conn.query("PRAGMA table_info(sessions)", ()).await?;
    let mut has_turn_log = false;
    while let Some(row) = rows.next().await? {
        // Column 1 of `table_info` is the name (0 = cid).
        if row.get_str(1).is_ok_and(|name| name == "turn_log") {
            has_turn_log = true;
        }
    }
    drop(rows);
    if has_turn_log {
        return Ok(());
    }
    run_with_lock_retry(|| async {
        conn.execute_batch("ALTER TABLE sessions ADD COLUMN turn_log TEXT NOT NULL DEFAULT '';")
            .await?;
        Ok(())
    })
    .await
}

/// Add the `parent_id` column to `sessions` when the file predates it.
///
/// Same idempotent shape as [`apply_turn_log_column`]: SQLite has no
/// `ADD COLUMN IF NOT EXISTS`, so the column list is read first and the
/// `ALTER` runs only when the column is missing. Every `open` runs this —
/// an up-to-date file costs one `PRAGMA table_info` round trip and no DDL.
/// The race where two processes pass the same check and one `ALTER` loses is
/// absorbed by [`is_duplicate_column_error`] inside [`run_with_lock_retry`].
///
/// `parent_id` is intentionally a bare nullable `TEXT` with no `FOREIGN KEY`
/// constraint (unlike `messages.session_id`): lineage is written by the
/// `session.create` caller and a child may legitimately be recorded before
/// its parent row exists (client-side grouping, out-of-order replay).
/// Grouping (5.3/5.5) treats a missing-or-null parent as a root.
async fn apply_parent_id_column(conn: &Connection) -> Result<(), SessionError> {
    let mut rows = conn.query("PRAGMA table_info(sessions)", ()).await?;
    let mut has_parent_id = false;
    while let Some(row) = rows.next().await? {
        // Column 1 of `table_info` is the name (0 = cid).
        if row.get_str(1).is_ok_and(|name| name == "parent_id") {
            has_parent_id = true;
        }
    }
    drop(rows);
    if has_parent_id {
        return Ok(());
    }
    run_with_lock_retry(|| async {
        conn.execute_batch("ALTER TABLE sessions ADD COLUMN parent_id TEXT;")
            .await?;
        Ok(())
    })
    .await
}

/// Add the `attachments` column to `messages` when the file predates it.
///
/// Same idempotent shape as [`apply_parent_id_column`]: SQLite has no
/// `ADD COLUMN IF NOT EXISTS`, so the column list is read first and the
/// `ALTER` runs only when the column is missing.
///
/// The column is a bare nullable `TEXT` with no default: legacy rows stay
/// `NULL` and surface as "no attachments", so the migration moves no data
/// and old session files open unchanged.
async fn apply_messages_attachments_column(conn: &Connection) -> Result<(), SessionError> {
    let mut rows = conn.query("PRAGMA table_info(messages)", ()).await?;
    let mut has_attachments = false;
    while let Some(row) = rows.next().await? {
        // Column 1 of `table_info` is the name (0 = cid).
        if row.get_str(1).is_ok_and(|name| name == "attachments") {
            has_attachments = true;
        }
    }
    drop(rows);
    if has_attachments {
        return Ok(());
    }
    run_with_lock_retry(|| async {
        conn.execute_batch("ALTER TABLE messages ADD COLUMN attachments TEXT;")
            .await?;
        Ok(())
    })
    .await
}

/// Add `last_access_ms` to `sessions` when the file predates cold archiving.
///
/// Same idempotent shape as [`apply_parent_id_column`]. The column defaults to
/// `0`, and readers treat `0` as "never touched since this column existed",
/// falling back to `updated_at` so a pre-migration database is not suddenly
/// full of infinitely-stale sessions. The sweep backfills nothing: the
/// fallback is the migration.
async fn apply_last_access_column(conn: &Connection) -> Result<(), SessionError> {
    let mut rows = conn.query("PRAGMA table_info(sessions)", ()).await?;
    let mut has_last_access = false;
    while let Some(row) = rows.next().await? {
        // Column 1 of `table_info` is the name (0 = cid).
        if row.get_str(1).is_ok_and(|name| name == "last_access_ms") {
            has_last_access = true;
        }
    }
    drop(rows);
    if has_last_access {
        return Ok(());
    }
    run_with_lock_retry(|| async {
        conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN last_access_ms INTEGER NOT NULL DEFAULT 0;",
        )
        .await?;
        Ok(())
    })
    .await
}

/// Same idempotent shape as [`apply_messages_attachments_column`], for the
/// `tool_calls` UI-card passthrough column on both `messages` and its rewind
/// backup `message_snapshots`（两表列集必须同步，rewind 的 INSERT..SELECT 才不炸）。
async fn apply_tool_calls_column(conn: &Connection) -> Result<(), SessionError> {
    for table in ["messages", "message_snapshots"] {
        let mut rows = conn
            .query(&format!("PRAGMA table_info({table})"), ())
            .await?;
        let mut has_column = false;
        while let Some(row) = rows.next().await? {
            if row.get_str(1).is_ok_and(|name| name == "tool_calls") {
                has_column = true;
            }
        }
        drop(rows);
        if has_column {
            continue;
        }
        run_with_lock_retry(|| async {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN tool_calls TEXT;"))
                .await?;
            Ok(())
        })
        .await?;
    }
    Ok(())
}

async fn apply_pragmas(conn: &Connection) -> Result<(), SessionError> {
    conn.execute_batch("PRAGMA synchronous = NORMAL;").await?;
    Ok(())
}

async fn enable_wal_if_needed(conn: &Connection) -> Result<(), SessionError> {
    // `query` returns a `Rows` future that holds a live prepared statement.
    // We must fully drain it (or `None`) before issuing another statement
    // — otherwise libsql leaves the connection with an open statement,
    // and the next `PRAGMA journal_mode = WAL` rejects it as "from within a
    // transaction".
    let mut rows = conn.query("PRAGMA journal_mode", libsql::params![]).await?;
    let current = loop {
        match rows.next().await? {
            Some(row) => {
                if let Ok(s) = row.get_str(0) {
                    break s.to_string();
                }
            }
            None => break String::new(),
        }
    };
    drop(rows);
    if current.eq_ignore_ascii_case("wal") {
        return Ok(());
    }
    // Switching to WAL requires a write lock; retry on transient lock errors.
    run_with_lock_retry(|| async {
        conn.execute_batch("PRAGMA journal_mode = WAL;").await?;
        Ok(())
    })
    .await
}

async fn init_database(conn: &Connection) -> Result<(), SessionError> {
    apply_safe_pragmas(conn).await?;
    enable_wal_if_needed(conn).await?;
    apply_pragmas(conn).await?;
    apply_schema(conn).await?;
    apply_turn_log_column(conn).await?;
    apply_parent_id_column(conn).await?;
    apply_messages_attachments_column(conn).await?;
    apply_last_access_column(conn).await?;
    apply_tool_calls_column(conn).await?;
    Ok(())
}

fn is_lock_error(err: &SessionError) -> bool {
    let SessionError::Libsql(libsql::Error::SqliteFailure(code, _)) = err else {
        return false;
    };
    // Both SQLITE_BUSY (5) and SQLITE_LOCKED (6) and their extended codes
    // share the primary code in the low 8 bits.
    matches!(*code & 0xFF, 5 | 6)
}

/// Whether `err` means the `turn_log` column already exists — the race case
/// where two processes passed the same `PRAGMA table_info` check and the
/// second `ALTER TABLE` lost. Treated as success because the end state (the
/// column is present) is exactly what the migration wanted.
fn is_duplicate_column_error(err: &SessionError) -> bool {
    let SessionError::Libsql(libsql::Error::SqliteFailure(code, msg)) = err else {
        return false;
    };
    // SQLITE_ERROR (1) carries "duplicate column name" in the message.
    *code & 0xFF == 1 && msg.contains("duplicate column")
}

async fn run_with_lock_retry<F, Fut>(op: F) -> Result<(), SessionError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<(), SessionError>>,
{
    let mut attempt = 0usize;
    loop {
        match op().await {
            Ok(()) => return Ok(()),
            Err(e) if is_duplicate_column_error(&e) => {
                // Lost the `PRAGMA table_info` race against another process:
                // the column is there, which is all this migration wanted.
                return Ok(());
            }
            Err(e) if attempt < INIT_LOCK_RETRIES && is_lock_error(&e) => {
                attempt += 1;
                // No sleep: `busy_timeout` already drives the actual wait
                // inside SQLite; this loop is a hedge against the rare case
                // where the contention window is longer than the timeout.
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

fn now_ms() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

fn validate_limit(limit: u32) -> Result<u32, SessionError> {
    if limit == 0 || limit > MAX_LIMIT {
        Err(SessionError::InvalidLimit(limit))
    } else {
        Ok(limit)
    }
}

// -----------------------------------------------------------------------------
// API: sessions
// -----------------------------------------------------------------------------

impl SessionDb {
    /// Create a brand-new session, or return the existing row (with its
    /// original timestamps) if `id` is already present.
    ///
    /// `id` and `title` must be non-empty (after trimming). The new row is a
    /// root (no parent); use [`Self::ensure_session_with_parent`] to record a
    /// lineage edge.
    pub fn ensure_session(&self, id: &str, title: &str) -> Result<SessionSummary, SessionError> {
        self.ensure_session_with_parent(id, title, None)
    }

    /// [`Self::ensure_session`] with an explicit `parent_id`. `None` (or a
    /// blank string) records a root session.
    ///
    /// The parent is written only on insert — an existing row keeps whatever
    /// lineage edge it already has, mirroring how `ensure_session` preserves
    /// `title` and `created_at`. A parent id is not validated against the
    /// `sessions` table (see [`apply_parent_id_column`]: a child may be
    /// recorded before its parent).
    pub fn ensure_session_with_parent(
        &self,
        id: &str,
        title: &str,
        parent_id: Option<&str>,
    ) -> Result<SessionSummary, SessionError> {
        SessionError::invalid_id_if_blank(id)?;
        if title.trim().is_empty() {
            return Err(SessionError::InvalidSessionId);
        }
        // A blank parent is the same as no parent — keeps a stray "" out of
        // the lineage column.
        let parent_id = parent_id.filter(|p| !p.trim().is_empty());
        let id_owned = id.to_string();
        let title_owned = title.to_string();
        let parent_owned = parent_id.map(str::to_string);

        let guard = self.inner.conn.lock();
        // ponytail: scope the guard so the lock is released the instant the
        // async block returns — no explicit drop call needed and no chance
        // of using the connection after a different call gets in.
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let now = now_ms();
            // INSERT OR IGNORE leaves existing rows alone (so created_at and
            // parent_id are preserved); we then UPDATE updated_at so the row
            // moves to the top of list_sessions.
            conn.execute(
                "INSERT OR IGNORE INTO sessions (id, title, parent_id, created_at, updated_at) \
                  VALUES (?1, ?2, ?3, ?4, ?4)",
                libsql::params![
                    id_owned.as_str(),
                    title_owned.as_str(),
                    parent_owned.as_deref(),
                    now
                ],
            )
            .await?;
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                libsql::params![now, id_owned.as_str()],
            )
            .await?;
            load_session_row(conn, id_owned.as_str())
                .await
                .ok_or_else(|| SessionError::DatabaseMissing(PathBuf::from(id_owned)))
        })
    }

    /// Convenience wrapper: always create. Returns the existing row if the id
    /// was already taken — same as [`Self::ensure_session`] but with a
    /// shorter name for the "I just want to make sure this exists" call site.
    pub fn create_session(&self, id: &str, title: &str) -> Result<SessionSummary, SessionError> {
        self.create_session_with_parent(id, title, None)
    }

    /// [`Self::create_session`] with an explicit `parent_id`; see
    /// [`Self::ensure_session_with_parent`] for the parent semantics.
    pub fn create_session_with_parent(
        &self,
        id: &str,
        title: &str,
        parent_id: Option<&str>,
    ) -> Result<SessionSummary, SessionError> {
        self.ensure_session_with_parent(id, title, parent_id)
    }

    /// Rename an existing session: UPDATE `title` + `updated_at` only, and
    /// read the row back so the returned summary is exactly what is on disk.
    ///
    /// This is an UPDATE, not an upsert: a row that does not exist yields
    /// [`SessionError::DatabaseMissing`] and nothing is inserted — callers
    /// use this to persist a derived title for a session they know exists,
    /// and silently creating the row would hide their bug. A blank id is
    /// rejected up front with [`SessionError::InvalidSessionId`] (like every
    /// other mutator here): letting it reach the UPDATE would report a
    /// missing *database file* for what is really an invalid id.
    ///
    /// An empty `title` is accepted: the deterministic stand-in title the
    /// web UI sends is legitimate. This is deliberately looser than
    /// [`Self::ensure_session`], which rejects a blank title on insert.
    /// `created_at`, `parent_id`, and the turn log are never touched.
    ///
    /// Known imprecision (reviewed, kept): the read-back `None` is the
    /// missing-row signal, and `load_session_row` maps *every* failure to
    /// `None`, so a committed UPDATE whose SELECT fails is also reported as
    /// `DatabaseMissing`. Splitting that case out would need a new
    /// `SessionError` variant for a window this rare.
    pub fn update_title(&self, id: &str, title: &str) -> Result<SessionSummary, SessionError> {
        SessionError::invalid_id_if_blank(id)?;
        let id_owned = id.to_string();
        let title_owned = title.to_string();

        let guard = self.inner.conn.lock();
        // ponytail: scope the guard so the lock is released the instant the
        // async block returns — same shape as ensure_session_with_parent.
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let now = now_ms();
            conn.execute(
                "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
                libsql::params![title_owned.as_str(), now, id_owned.as_str()],
            )
            .await?;
            load_session_row(conn, id_owned.as_str())
                .await
                .ok_or_else(|| SessionError::DatabaseMissing(PathBuf::from(id_owned)))
        })
    }

    /// Delete a session and all of its messages (via `ON DELETE CASCADE`).
    /// Returns true if a row was removed.
    pub fn delete_session(&self, id: &str) -> Result<bool, SessionError> {
        SessionError::invalid_id_if_blank(id)?;
        let id_owned = id.to_string();

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let n = conn
                .execute(
                    "DELETE FROM sessions WHERE id = ?1",
                    libsql::params![id_owned.as_str()],
                )
                .await?;
            Ok(n > 0)
        })
    }

    /// List sessions whose id or title contains `query` (case-insensitive
    /// `LIKE`), ordered by `updated_at DESC, id ASC`. `limit` is hard-capped
    /// at [`MAX_LIMIT`]; 0 is rejected.
    pub fn list_sessions(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        let limit = validate_limit(limit)?;
        if query.is_empty() {
            return Err(SessionError::InvalidListQuery);
        }
        let like = format!("%{}%", query);

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    "SELECT s.id, s.title, s.parent_id, s.created_at, s.updated_at, \
                            (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) \
                     FROM sessions s \
                     WHERE s.id LIKE ?1 COLLATE NOCASE OR s.title LIKE ?1 COLLATE NOCASE \
                     ORDER BY s.updated_at DESC, s.id ASC \
                     LIMIT ?2",
                    libsql::params![like, limit],
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(SessionSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    parent_id: row.get::<Option<String>>(2)?,
                    created_at_ms: row.get(3)?,
                    updated_at_ms: row.get(4)?,
                    message_count: row.get::<i64>(5)?.max(0) as u64,
                });
            }
            Ok(out)
        })
    }

    /// Look up a single session by id, restoring it from the cold archive
    /// when it is no longer in the live database. Returns `None` only when
    /// the session exists nowhere.
    ///
    /// This is the transparency seam: a session compressed by
    /// [`Self::compress_idle_sessions`] reopens as if it had never been
    /// moved. A successful lookup counts as use and refreshes the idle
    /// clock.
    pub fn session(&self, id: &str) -> Result<Option<SessionSummary>, SessionError> {
        SessionError::invalid_id_if_blank(id)?;

        if let Some(found) = self.session_live(id)? {
            self.touch_session_access(id)?;
            return Ok(Some(found));
        }
        if !archive::has_archive(self.path(), id) {
            return Ok(None);
        }
        let restored = self.restore_session(id)?;
        Ok(Some(restored))
    }

    /// The read-only half of [`Self::session`]: no archive lookup, no access
    /// touch. `list_sessions` and the sweep use this so browsing the picker
    /// does not resurrect every cold session it walks past.
    pub fn session_live(&self, id: &str) -> Result<Option<SessionSummary>, SessionError> {
        SessionError::invalid_id_if_blank(id)?;
        let id_owned = id.to_string();

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            Ok(load_session_row(conn, id_owned.as_str()).await)
        })
    }

    /// `true` if no session rows exist. Used by tests / callers that want to
    /// skip a "fresh DB" fast path.
    pub fn is_empty(&self) -> Result<bool, SessionError> {
        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn.query("SELECT 1 FROM sessions LIMIT 1", ()).await?;
            Ok(rows.next().await?.is_none())
        })
    }

    // ---------------------------------------------------------------------
    // Cold archive
    // ---------------------------------------------------------------------

    /// Record that `session_id` was just used. Drives the idle cutoff in
    /// [`Self::compress_idle_sessions`].
    ///
    /// This is an explicit call rather than a hook buried inside the read
    /// path: a read that silently issues a write would turn every page of the
    /// TUI into a write transaction, and it would also mean a read on a
    /// read-only database fails. The open paths call this; `list_sessions`
    /// deliberately does not, since browsing the picker is not using a
    /// session.
    pub fn touch_session_access(&self, session_id: &str) -> Result<(), SessionError> {
        SessionError::invalid_id_if_blank(session_id)?;
        let id_owned = session_id.to_string();
        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            conn.execute(
                "UPDATE sessions SET last_access_ms = ?1 WHERE id = ?2",
                libsql::params![now_ms(), id_owned.as_str()],
            )
            .await?;
            Ok(())
        })
    }

    /// Move `session_id` out of the live database into a compressed archive
    /// file, returning what it cost and what it freed.
    ///
    /// The rows are deleted from the database, but **the database file does
    /// not shrink** — SQLite recycles freed pages internally and stays at its
    /// high-water mark. The bytes only actually leave the disk when the file
    /// is compacted by the caller (see [`Self::checkpoint_wal`]) or a
    /// `VACUUM`; the archive itself is what makes them reclaimable.
    ///
    /// Refuses when an archive already exists: re-archiving would silently
    /// replace a backup with a newer copy of a session whose rows have since
    /// diverged.
    pub fn archive_session(&self, session_id: &str) -> Result<ArchiveReceipt, SessionError> {
        SessionError::invalid_id_if_blank(session_id)?;
        let id_owned = session_id.to_string();

        // Read the whole session out first, outside the write lock. The id
        // is cloned into the block because `async move` takes ownership of
        // everything it mentions, and the archive write below still needs it.
        let read_id = id_owned.clone();
        let (archived_session, messages): (ArchivedSession, Vec<ArchivedMessage>) = {
            let guard = self.inner.conn.lock();
            self.inner.runtime.block_on(async move {
                let conn = &*guard;
                let id = read_id.as_str();
                let session = load_archived_session(conn, id)
                    .await?
                    .ok_or_else(|| SessionError::DatabaseMissing(std::path::PathBuf::from(id)))?;
                let messages = load_archived_messages(conn, id).await?;
                Ok::<(ArchivedSession, Vec<ArchivedMessage>), SessionError>((session, messages))
            })
        }?;

        let path = archive_path(self.path(), &id_owned);
        if path.exists() {
            // An existing archive may be replaced only when the live rows
            // subsume it: restore-then-append leaves the live session a
            // superset, so the new archive loses nothing. If the live
            // session holds FEWER messages than the archive — a rewind
            // after restore, or a recycled session id — overwriting would
            // silently drop archive-only history, so refuse. A corrupt
            // archive cannot prove it holds anything the live rows lack,
            // so it may be replaced.
            let raw = std::fs::read(&path).map_err(SessionError::Io)?;
            if let Ok(old) = archive::decode_archive(&raw)
                && old.messages.len() > messages.len()
            {
                return Err(SessionError::Archive(format!(
                    "an archive at {} holds {} messages but the live session has {}; \
                         refusing to shrink it — rewind less, or delete the archive by hand",
                    path.display(),
                    old.messages.len(),
                    messages.len()
                )));
            }
        }

        let (payload, raw_len) = archive::encode_archive(&archived_session, &messages);
        let written = archive::write_archive_file(&path, &payload)?;

        // Only now drop the rows. If the delete fails the archive is still on
        // disk and `restore_session` refuses to double-restore, so the safe
        // failure mode is "both exist" rather than "gone from both".
        let delete_id = id_owned.clone();
        let guard = self.inner.conn.lock();
        let removed = self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let id = delete_id.as_str();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;
            let n = tx
                .execute(
                    "DELETE FROM messages WHERE session_id = ?1",
                    libsql::params![id],
                )
                .await?;
            tx.execute("DELETE FROM sessions WHERE id = ?1", libsql::params![id])
                .await?;
            tx.commit().await?;
            Ok::<u64, SessionError>(n)
        })?;

        Ok(ArchiveReceipt {
            session_id: id_owned,
            messages: removed as usize,
            raw_bytes: raw_len as u64,
            archived_bytes: written,
        })
    }

    /// Put an archived session back into the live database. The session row
    /// and every message are inserted in one transaction, so a failure
    /// leaves the database untouched rather than half-restored. Original
    /// `seq` values are preserved.
    ///
    /// Refuses when the id already exists in the database — restoring over a
    /// live session would have to pick a winner, and silently discarding one
    /// of them is the worse failure.
    pub fn restore_session(&self, session_id: &str) -> Result<SessionSummary, SessionError> {
        SessionError::invalid_id_if_blank(session_id)?;
        let id_owned = session_id.to_string();
        let path = archive_path(self.path(), &id_owned);
        let raw = std::fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SessionError::DatabaseMissing(path.clone())
            } else {
                SessionError::Io(e)
            }
        })?;
        let decoded = archive::decode_archive(&raw)?;
        if decoded.session.id != id_owned {
            return Err(SessionError::Archive(format!(
                "archive at {} holds session `{}`, not `{id_owned}`",
                path.display(),
                decoded.session.id
            )));
        }

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;

            let mut existing = tx
                .query(
                    "SELECT 1 FROM sessions WHERE id = ?1",
                    libsql::params![id_owned.as_str()],
                )
                .await?;
            if existing.next().await?.is_some() {
                return Err(SessionError::Archive(format!(
                    "session `{id_owned}` is already live; refusing to restore over it"
                )));
            }

            let s = &decoded.session;
            tx.execute(
                "INSERT INTO sessions \
                 (id, title, parent_id, created_at, updated_at, turn_log, last_access_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                libsql::params![
                    s.id.as_str(),
                    s.title.as_str(),
                    s.parent_id.as_deref(),
                    s.created_at,
                    s.updated_at,
                    s.turn_log.as_str(),
                    now_ms(),
                ],
            )
            .await?;
            for m in &decoded.messages {
                // Validate before the insert rather than trusting the string:
                // an archive written by a build that knew a role this one
                // does not must fail the restore, not be written through.
                let role = archive::role_from_str(&m.role)?;
                tx.execute(
                    "INSERT INTO messages \
                     (session_id, seq, role, text, created_at, attachments) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    libsql::params![
                        s.id.as_str(),
                        m.seq,
                        role.as_str(),
                        m.text.as_str(),
                        m.created_at,
                        m.attachments.as_deref(),
                    ],
                )
                .await?;
            }
            tx.commit().await?;
            Ok(archive::summary_of(s, decoded.messages.len()))
        })
    }

    /// Restore `session_id` if it is missing from the database but present in
    /// the archive. Returns the summary when a restore happened, `None` when
    /// the session is live or has no archive.
    ///
    /// Called on the read paths so reopening a cold session is transparent.
    /// The archive is left in place after a restore: it is the only backup of
    /// a session that is no longer in the live file, and deleting it here
    /// would make a second restore impossible if the session were archived
    /// again by mistake.
    pub fn restore_if_archived(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionSummary>, SessionError> {
        if self.session(session_id)?.is_some() {
            return Ok(None);
        }
        if !archive::has_archive(self.path(), session_id) {
            return Ok(None);
        }
        self.restore_session(session_id).map(Some)
    }

    /// Archive every session idle for longer than `idle_ms`, skipping the
    /// `keep_recent` most-recently-used ones. Returns one receipt per session
    /// actually moved.
    ///
    /// "Idle" means `last_access_ms` when it is non-zero, else `updated_at`
    /// (the fallback for rows written before the column existed). `keep_recent`
    /// is a floor on top of the idle cutoff: even a very old session stays
    /// live if it is one of the N most recent, so a machine that is only ever
    /// idle for a long time never archives everything. `max_sessions` bounds
    /// one call's write-lock footprint: the sweep returns after archiving at
    /// most that many sessions even if more qualify.
    pub fn compress_idle_sessions(
        &self,
        idle_ms: i64,
        keep_recent: u32,
        // Batch cap: the daemon-startup sweep must not sit on the write lock
        // for a whole backlog at once — archive at most this many sessions
        // per call; the next run (or the next daemon start) takes the rest.
        max_sessions: u32,
    ) -> Result<Vec<ArchiveReceipt>, SessionError> {
        let cutoff = now_ms().saturating_sub(idle_ms.max(0));
        // Candidates, most-recent first, so the keep_recent floor is a slice.
        let candidates: Vec<(String, i64)> = {
            let guard = self.inner.conn.lock();
            self.inner.runtime.block_on(async move {
                let conn = &*guard;
                let mut rows = conn
                    .query(
                        "SELECT id, MAX(updated_at, last_access_ms) AS touched \
                         FROM sessions ORDER BY touched DESC, id ASC",
                        (),
                    )
                    .await?;
                let mut out = Vec::new();
                while let Some(row) = rows.next().await? {
                    out.push((row.get(0)?, row.get::<i64>(1).unwrap_or(0)));
                }
                Ok::<Vec<(String, i64)>, SessionError>(out)
            })
        }?;

        let mut receipts = Vec::new();
        for (rank, (id, touched)) in candidates.into_iter().enumerate() {
            if (rank as u32) < keep_recent {
                continue;
            }
            if touched >= cutoff {
                continue;
            }
            if receipts.len() >= max_sessions.max(1) as usize {
                break;
            }
            match self.archive_session(&id) {
                Ok(r) => receipts.push(r),
                // A concurrent sweep or a daemon touch can archive it first.
                // Skipping is correct: the other writer did the work.
                Err(SessionError::DatabaseMissing(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(receipts)
    }

    /// Truncate the write-ahead log so it stops growing alongside the
    /// database file. Cheap and bounded; it does **not** compact the
    /// database file itself (see [`Self::archive_session`] for why).
    pub fn checkpoint_wal(&self) -> Result<(), SessionError> {
        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            // PRAGMA returns a row; drain it or the statement stays open and
            // the next write is rejected as "from within a transaction".
            let mut rows = conn.query("PRAGMA wal_checkpoint(TRUNCATE)", ()).await?;
            while rows.next().await?.is_some() {}
            Ok(())
        })
    }
}

/// Read the full `sessions` row in the shape the archive needs, including the
/// columns [`load_session_row`] does not surface.
async fn load_archived_session(
    conn: &Connection,
    id: &str,
) -> Result<Option<ArchivedSession>, SessionError> {
    let mut rows = conn
        .query(
            "SELECT id, title, parent_id, created_at, updated_at, turn_log, last_access_ms \
             FROM sessions WHERE id = ?1",
            libsql::params![id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(ArchivedSession {
        id: row.get(0)?,
        title: row.get(1)?,
        parent_id: row.get::<Option<String>>(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        turn_log: row.get::<Option<String>>(5)?.unwrap_or_default(),
        last_access_ms: row.get::<Option<i64>>(6)?.unwrap_or(0),
    }))
}

/// Every message of `id` in ascending `seq` order, in archive shape.
async fn load_archived_messages(
    conn: &Connection,
    id: &str,
) -> Result<Vec<ArchivedMessage>, SessionError> {
    let mut rows = conn
        .query(
            "SELECT seq, role, text, created_at, attachments \
             FROM messages WHERE session_id = ?1 ORDER BY seq ASC",
            libsql::params![id],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(ArchivedMessage {
            seq: row.get(0)?,
            role: row.get(1)?,
            text: row.get(2)?,
            created_at: row.get(3)?,
            attachments: row.get::<Option<String>>(4)?,
        });
    }
    Ok(out)
}

async fn load_session_row(conn: &Connection, id: &str) -> Option<SessionSummary> {
    let mut rows = match conn
        .query(
            "SELECT id, title, parent_id, created_at, updated_at, \
                    (SELECT COUNT(*) FROM messages WHERE session_id = s.id) \
             FROM sessions s WHERE id = ?1",
            libsql::params![id],
        )
        .await
    {
        Ok(r) => r,
        Err(_) => return None,
    };
    let row = rows.next().await.ok().flatten()?;
    Some(SessionSummary {
        id: row.get(0).ok()?,
        title: row.get(1).ok()?,
        parent_id: row.get::<Option<String>>(2).ok()?,
        created_at_ms: row.get(3).ok()?,
        updated_at_ms: row.get(4).ok()?,
        message_count: row.get::<i64>(5).ok()?.max(0) as u64,
    })
}

// -----------------------------------------------------------------------------
// API: messages
// -----------------------------------------------------------------------------

/// Decode the `messages.attachments` column.
///
/// `NULL` (a row written before the column migration, or one with no
/// attachments) and a malformed payload both degrade to an empty vec —
/// one corrupted row must never fail the whole session read.
fn parse_attachments(raw: Option<&str>) -> Vec<Attachment> {
    raw.and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

/// `tool_calls` 列是 UI 侧工具卡 JSON 数组的透传（对 session 层不透明）。
fn parse_tool_calls(raw: Option<&str>) -> Vec<serde_json::Value> {
    raw.and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

impl SessionDb {
    /// Append a message to a session inside an `Immediate` transaction.
    /// Returns the assigned `seq` (1-based, monotonic per session) and the
    /// row's `created_at_ms`.
    ///
    /// Creates the session row if it does not yet exist (caller may have
    /// forgotten to `ensure_session` first; the id doubles as the fallback
    /// title).
    ///
    /// `attachments` are stored in the nullable `messages.attachments`
    /// column as a JSON array; an empty slice binds `NULL` so a legacy
    /// reader sees no attachments.
    pub fn append_message(
        &self,
        session_id: &str,
        role: SessionRole,
        text: &str,
        attachments: &[Attachment],
        tool_calls: &[serde_json::Value],
    ) -> Result<(i64, i64), SessionError> {
        SessionError::invalid_id_if_blank(session_id)?;
        if text.is_empty() {
            return Err(SessionError::InvalidMessageText);
        }
        let id_owned = session_id.to_string();
        let text_owned = text.to_string();
        let attachments_json: Option<String> = if attachments.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(attachments)
                    .expect("Attachment holds plain strings; serialization cannot fail"),
            )
        };
        let tool_calls_json: Option<String> = if tool_calls.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(tool_calls)
                    .expect("tool calls are pre-parsed JSON values; serialization cannot fail"),
            )
        };

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let now = now_ms();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;

            // Make sure the session row exists. We never overwrite an
            // existing title — that would clobber user-set data inside the
            // same transaction.
            let mut title_row = tx
                .query(
                    "SELECT title FROM sessions WHERE id = ?1",
                    libsql::params![id_owned.as_str()],
                )
                .await?;
            let have_session = title_row.next().await?.is_some();
            if !have_session {
                tx.execute(
                    "INSERT INTO sessions (id, title, created_at, updated_at) \
                     VALUES (?1, ?1, ?2, ?2)",
                    libsql::params![id_owned.as_str(), now],
                )
                .await?;
            }

            // MAX(seq) + 1, atomically inside the tx.
            let mut seq_row = tx
                .query(
                    "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE session_id = ?1",
                    libsql::params![id_owned.as_str()],
                )
                .await?;
            let next_seq: i64 = seq_row
                .next()
                .await?
                .map(|r| r.get::<i64>(0).unwrap_or(0))
                .unwrap_or(0)
                + 1;

            tx.execute(
                "INSERT INTO messages \
                 (session_id, seq, role, text, created_at, attachments, tool_calls) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                libsql::params![
                    id_owned.as_str(),
                    next_seq,
                    role.as_str(),
                    text_owned.as_str(),
                    now,
                    attachments_json,
                    tool_calls_json,
                ],
            )
            .await?;

            // Bump session.updated_at so it surfaces to the top of list.
            tx.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                libsql::params![now, id_owned.as_str()],
            )
            .await?;

            // commit() consumes `tx`; on any error along the way, the
            // libsql Drop impl rolls back, so the tx is never left open.
            tx.commit().await?;
            Ok((next_seq, now))
        })
    }

    /// Drop every message with `seq >= from_seq` for `session_id` and
    /// return how many rows actually went away.
    ///
    /// This is the rewind primitive behind `session.truncate` (T12 retry /
    /// edit): the caller truncates first, then appends the replacement, so
    /// the tail is rewritten rather than copied. `from_seq <= 0` empties the
    /// session; a `from_seq` past the tail deletes nothing and reports `0`
    /// (idempotent — truncating twice is the same as truncating once).
    /// Deleting from a session id that does not exist likewise reports `0`:
    /// the returned count is the truth, never a fabricated success.
    pub fn truncate_messages(&self, session_id: &str, from_seq: i64) -> Result<u64, SessionError> {
        SessionError::invalid_id_if_blank(session_id)?;
        let id_owned = session_id.to_string();

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let n = conn
                .execute(
                    "DELETE FROM messages WHERE session_id = ?1 AND seq >= ?2",
                    libsql::params![id_owned.as_str(), from_seq],
                )
                .await?;
            Ok(n)
        })
    }

    /// Snapshot every message with `seq >= from_seq` into
    /// `message_snapshots` (stamped [`SNAPSHOT_IDENTITY`]), then delete it —
    /// both halves inside one transaction, so a snapshot insert that fails
    /// rolls the delete back with it: no snapshot, no truncation.
    ///
    /// This is the primitive behind `session.rewind` (T13 turn rewind).
    /// Boundary semantics are [`Self::truncate_messages`]`'` verbatim
    /// (`from_seq <= 0` empties the session; a `from_seq` past the tail
    /// touches nothing and reports `0`) — the TUI validates the rewind point
    /// before calling, the store stays permissive. Returns how many rows were
    /// backed up; that equals the rows dropped, since both statements read
    /// the same range with no writes in between.
    ///
    /// Snapshot rows are append-only (`id` is a bare rowid): rewinding the
    /// same range twice stacks a second backup instead of colliding with the
    /// first, so the discarded generation stays auditable.
    pub fn rewind_messages(&self, session_id: &str, from_seq: i64) -> Result<u64, SessionError> {
        SessionError::invalid_id_if_blank(session_id)?;
        let id_owned = session_id.to_string();

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;
            let snapshotted = tx
                .execute(
                    "INSERT INTO message_snapshots \
                     (session_id, seq, role, text, created_at, attachments, tool_calls, identity, snapshotted_at) \
                     SELECT session_id, seq, role, text, created_at, attachments, tool_calls, ?2, ?3 \
                     FROM messages WHERE session_id = ?1 AND seq >= ?4",
                    libsql::params![id_owned.as_str(), SNAPSHOT_IDENTITY, now_ms(), from_seq],
                )
                .await?;
            tx.execute(
                "DELETE FROM messages WHERE session_id = ?1 AND seq >= ?2",
                libsql::params![id_owned.as_str(), from_seq],
            )
            .await?;
            // commit() consumes `tx`; on any error along the way the libsql
            // Drop impl rolls the whole transaction back, so the backup and
            // the delete can never land half-done.
            tx.commit().await?;
            Ok(snapshotted)
        })
    }

    /// Rows the rewind backup holds for `session_id` **carrying
    /// [`SNAPSHOT_IDENTITY`]**. Read-only audit seam: the write half of
    /// [`Self::rewind_messages`]`'` transaction is the DELETE, so tests and
    /// callers prove the backup happened through this count without reaching
    /// into the database by hand.
    pub fn rewind_snapshot_count(&self, session_id: &str) -> Result<u64, SessionError> {
        SessionError::invalid_id_if_blank(session_id)?;
        let id_owned = session_id.to_string();

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM message_snapshots \
                     WHERE session_id = ?1 AND identity = ?2",
                    libsql::params![id_owned.as_str(), SNAPSHOT_IDENTITY],
                )
                .await?;
            let n = match rows.next().await? {
                Some(row) => row.get::<i64>(0).unwrap_or(0),
                None => 0,
            };
            drop(rows);
            Ok(n.max(0) as u64)
        })
    }

    /// Load the most recent `limit` messages for `session_id`, returned in
    /// ascending `seq` order.
    ///
    /// The window is anchored at the tail of the session (highest `seq`
    /// wins), which is the context resume / "last N turns" semantics call
    /// sites want.  The query fetches those rows with `ORDER BY seq DESC`
    /// so the SQL itself picks the tail window, then the rows are flipped
    /// back to ascending `seq` before return so the public API order is
    /// unchanged.
    pub fn load_messages(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<SessionMessage>, SessionError> {
        SessionError::invalid_id_if_blank(session_id)?;
        let limit = validate_limit(limit)?;
        // A cold session is restored before the tail window is taken, so the
        // window is computed against the whole transcript rather than
        // whatever happens to be left in the live file.
        self.restore_if_archived(session_id)?;
        self.touch_session_access(session_id)?;
        let id_owned = session_id.to_string();

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    "SELECT session_id, seq, role, text, created_at, attachments, tool_calls \
                     FROM messages WHERE session_id = ?1 \
                     ORDER BY seq DESC LIMIT ?2",
                    libsql::params![id_owned.as_str(), limit],
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                let role_str: String = row.get(2)?;
                let role = SessionRole::parse(&role_str)?;
                out.push(SessionMessage {
                    session_id: row.get(0)?,
                    seq: row.get(1)?,
                    role,
                    text: row.get(3)?,
                    created_at_ms: row.get(4)?,
                    attachments: parse_attachments(row.get::<Option<String>>(5)?.as_deref()),
                    tool_calls: parse_tool_calls(row.get::<Option<String>>(6)?.as_deref()),
                });
            }
            // SQL walked the tail window backwards (DESC); flip it so
            // callers see ascending `seq` order, unchanged from the prior
            // contract.
            out.reverse();
            Ok(out)
        })
    }

    /// Substring search across message text, optionally scoped to one
    /// session. Returns matches ordered by `created_at ASC, session_id ASC,
    /// seq ASC` so successive pages of the same time window are stable.
    pub fn search_messages(
        &self,
        query: &str,
        session_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SessionMessage>, SessionError> {
        if query.is_empty() {
            return Err(SessionError::InvalidSearchQuery);
        }
        let limit = validate_limit(limit)?;
        let like = format!("%{}%", query);
        // Validate scoped id up front so the error is reported eagerly.
        let scoped_owned: Option<String> = match session_id {
            Some(s) => {
                SessionError::invalid_id_if_blank(s)?;
                Some(s.to_string())
            }
            None => None,
        };

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            // ponytail: a single `session_id IS NULL OR = ?` keeps the query
            // shape identical whether the caller scoped or not, so we only
            // need one prepared path.
            let mut rows = match scoped_owned {
                Some(sid) => {
                    conn.query(
                        "SELECT session_id, seq, role, text, created_at, attachments, tool_calls \
                         FROM messages \
                         WHERE session_id = ?1 AND text LIKE ?2 COLLATE NOCASE \
                         ORDER BY created_at ASC, session_id ASC, seq ASC \
                         LIMIT ?3",
                        libsql::params![sid.as_str(), like, limit],
                    )
                    .await?
                }
                None => {
                    conn.query(
                        "SELECT session_id, seq, role, text, created_at, attachments, tool_calls \
                         FROM messages \
                         WHERE text LIKE ?1 COLLATE NOCASE \
                         ORDER BY created_at ASC, session_id ASC, seq ASC \
                         LIMIT ?2",
                        libsql::params![like, limit],
                    )
                    .await?
                }
            };

            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                let role_str: String = row.get(2)?;
                let role = SessionRole::parse(&role_str)?;
                out.push(SessionMessage {
                    session_id: row.get(0)?,
                    seq: row.get(1)?,
                    role,
                    text: row.get(3)?,
                    created_at_ms: row.get(4)?,
                    attachments: parse_attachments(row.get::<Option<String>>(5)?.as_deref()),
                    tool_calls: parse_tool_calls(row.get::<Option<String>>(6)?.as_deref()),
                });
            }
            Ok(out)
        })
    }
}

// -----------------------------------------------------------------------------
// API: turn log
// -----------------------------------------------------------------------------

impl SessionDb {
    /// Every session id, in id order. Used by the daemon's startup
    /// crash-repair pass, which must walk each session's turn log;
    /// [`SessionDb::list_sessions`] requires a query and is the wrong tool
    /// for an unconditional scan.
    pub fn session_ids(&self) -> Result<Vec<String>, SessionError> {
        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query("SELECT id FROM sessions ORDER BY id ASC", ())
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(row.get(0)?);
            }
            Ok(out)
        })
    }

    /// Append `records` to the durable turn log of `session_id`. The existing
    /// column is decoded first, so a half-written or corrupt store fails
    /// loudly here instead of silently growing mixed-encoding garbage. A
    /// session row that does not exist is a silent no-op: turn logging is
    /// best-effort, and a caller may hand an id the client never created.
    pub fn append_turn_log(
        &self,
        session_id: &str,
        records: &[TurnRecord],
    ) -> Result<(), SessionError> {
        if records.is_empty() {
            return Ok(());
        }
        SessionError::invalid_id_if_blank(session_id)?;
        let id_owned = session_id.to_string();
        let appended = encode_turn_log(records);

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;
            let mut row = tx
                .query(
                    "SELECT turn_log FROM sessions WHERE id = ?1",
                    libsql::params![id_owned.as_str()],
                )
                .await?;
            let existing: String = match row.next().await? {
                Some(r) => r.get(0)?,
                None => return Ok(()), // no session row — best-effort no-op
            };
            drop(row);
            // A log that fails to decode is kept verbatim — the caller still
            // gets its records appended — but the unreadable prefix is
            // quarantined so crash repair can keep making progress on this
            // session instead of failing every append forever.
            let prefix = if existing.is_empty() {
                String::new()
            } else {
                match decode_turn_log(&existing) {
                    Ok(_) => existing.clone(),
                    Err(_) => {
                        eprintln!(
                            "session: turn_log of {id_owned} failed to decode; \
                             quarantining {} bytes and continuing",
                            existing.len()
                        );
                        String::new()
                    }
                }
            };
            let mut text = prefix;
            text.push_str(&appended);
            tx.execute(
                "UPDATE sessions SET turn_log = ?1 WHERE id = ?2",
                libsql::params![text, id_owned.as_str()],
            )
            .await?;
            tx.commit().await?;
            Ok(())
        })
    }

    /// Read the durable turn log of `session_id` (empty when the session has
    /// no recorded run boundaries yet, or does not exist).
    pub fn load_turn_log(&self, session_id: &str) -> Result<Vec<TurnRecord>, SessionError> {
        SessionError::invalid_id_if_blank(session_id)?;
        let id_owned = session_id.to_string();

        let guard = self.inner.conn.lock();
        self.inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    "SELECT turn_log FROM sessions WHERE id = ?1",
                    libsql::params![id_owned.as_str()],
                )
                .await?;
            let text: String = match rows.next().await? {
                Some(row) => row.get(0)?,
                None => return Ok(Vec::new()),
            };
            decode_turn_log(&text)
        })
    }
}

// -----------------------------------------------------------------------------
