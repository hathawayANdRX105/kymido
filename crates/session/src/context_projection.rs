//! C01 durable context: content-addressed object storage, committed
//! projections with a session-level authoritative pointer, and immutable
//! request outcomes.
//!
//! ## Objects
//!
//! Attachments and large outputs are stored as files beside the database and
//! referenced by a manifest row in `context_objects`. The path is derived by
//! the host from a validated `object_id` — a model-supplied string can never
//! become a filesystem path. Commit order is fixed:
//!
//! 1. write a temp file,
//! 2. verify its sha256 and byte length,
//! 3. write the manifest row in a transaction,
//! 4. remove the temp/residual file.
//!
//! A failure before commit leaves no manifest; a failure after leaves a file
//! no row points at, which [`SessionDb::sweep_orphan_object_files`] reclaims.
//! Integrity (hash/length) is checked on read too — it proves the bytes, it
//! does not grant access.
//!
//! ## Projections
//!
//! A projection is derived state, never user speech. Each commit records the
//! source revision it covered (`source_epoch`, `covered_through_event_id`,
//! `config_revision`, `focus_revision`) and a monotonically compared
//! `projection_generation`. The session-level authoritative pointer only moves
//! forward: committing a *lower* generation inserts the row but leaves the
//! pointer on the newer projection, so a late/stale result cannot overwrite
//! it.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::context_events::sha256_hex;
use crate::{SessionDb, SessionError};

// -----------------------------------------------------------------------------
// Enums
// -----------------------------------------------------------------------------

/// What kind of bytes an object holds. The store never guesses: the caller
/// declares it and the value is validated against its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextObjectKind {
    /// Opaque attachment bytes (image, document, …).
    Blob,
    /// A stored projection payload.
    ProjectionPayload,
    /// The exact bytes of a provider request body.
    RequestBody,
    /// A cold-archive package.
    Archive,
}

impl ContextObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::ProjectionPayload => "projection_payload",
            Self::RequestBody => "request_body",
            Self::Archive => "archive",
        }
    }

    pub fn parse(value: &str) -> Result<Self, SessionError> {
        Ok(match value {
            "blob" => Self::Blob,
            "projection_payload" => Self::ProjectionPayload,
            "request_body" => Self::RequestBody,
            "archive" => Self::Archive,
            other => {
                return Err(SessionError::Context(format!(
                    "unknown context object kind `{other}`"
                )));
            }
        })
    }
}

/// Lifecycle of a projection row. `Authoritative` marks the one projection per
/// `(session, branch)` the host currently serves; at most one row holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStatus {
    Committed,
    Authoritative,
    Superseded,
    Failed,
}

impl ProjectionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Authoritative => "authoritative",
            Self::Superseded => "superseded",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, SessionError> {
        Ok(match value {
            "committed" => Self::Committed,
            "authoritative" => Self::Authoritative,
            "superseded" => Self::Superseded,
            "failed" => Self::Failed,
            other => {
                return Err(SessionError::Context(format!(
                    "unknown projection status `{other}`"
                )));
            }
        })
    }
}

/// Where a projection's payload lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    InlineJson,
    ObjectRef,
}

impl PayloadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InlineJson => "inline_json",
            Self::ObjectRef => "object_ref",
        }
    }

    pub fn parse(value: &str) -> Result<Self, SessionError> {
        Ok(match value {
            "inline_json" => Self::InlineJson,
            "object_ref" => Self::ObjectRef,
            other => {
                return Err(SessionError::Context(format!(
                    "unknown projection payload kind `{other}`"
                )));
            }
        })
    }
}

/// How a provider request ended. A `SendFailed` attempt is a real attempt and
/// is never upgraded to success by a later retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Sent,
    Completed,
    SendFailed,
    Aborted,
}

impl OutcomeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Completed => "completed",
            Self::SendFailed => "send_failed",
            Self::Aborted => "aborted",
        }
    }

    pub fn parse(value: &str) -> Result<Self, SessionError> {
        Ok(match value {
            "sent" => Self::Sent,
            "completed" => Self::Completed,
            "send_failed" => Self::SendFailed,
            "aborted" => Self::Aborted,
            other => {
                return Err(SessionError::Context(format!(
                    "unknown outcome status `{other}`"
                )));
            }
        })
    }
}

// -----------------------------------------------------------------------------
// Records
// -----------------------------------------------------------------------------

/// One object manifest row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextObject {
    pub object_id: String,
    pub sha256: String,
    pub kind: ContextObjectKind,
    pub byte_length: i64,
    pub storage_path: String,
    pub created_at: i64,
    pub deleted_at: Option<i64>,
}

/// Bytes handed to [`SessionDb::store_context_object`]. The store computes the
/// hash/length, derives the path, and stamps `created_at`.
#[derive(Debug, Clone)]
pub struct ContextObjectDraft {
    pub object_id: String,
    pub kind: ContextObjectKind,
    pub bytes: Vec<u8>,
}

/// One committed projection row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextProjection {
    pub projection_id: String,
    pub session_id: String,
    pub branch_id: String,
    pub source_epoch: i64,
    pub covered_through_event_id: Option<String>,
    pub method_id: String,
    pub method_version: String,
    pub config_revision: String,
    pub focus_revision: String,
    pub projection_generation: i64,
    pub payload_kind: PayloadKind,
    pub payload_json: Option<String>,
    pub object_id: Option<String>,
    pub status: ProjectionStatus,
    pub committed_at: i64,
}

/// Caller-supplied fields for [`SessionDb::commit_projection`].
#[derive(Debug, Clone)]
pub struct ProjectionDraft {
    pub projection_id: String,
    pub session_id: String,
    pub branch_id: String,
    pub source_epoch: i64,
    pub covered_through_event_id: Option<String>,
    pub method_id: String,
    pub method_version: String,
    pub config_revision: String,
    pub focus_revision: String,
    pub projection_generation: i64,
    pub payload_kind: PayloadKind,
    pub payload_json: Option<String>,
    pub object_id: Option<String>,
}

/// One provider-send attempt, exactly as recorded. `usage_json` is the raw
/// provider usage object (never a secret).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestOutcome {
    pub request_id: String,
    pub attempt: i64,
    pub session_id: String,
    pub source_epoch: i64,
    pub projection_generation: i64,
    pub request_body_object_id: Option<String>,
    pub request_body_hash: Option<String>,
    pub route: String,
    pub model: String,
    pub outcome_status: OutcomeStatus,
    pub usage_json: Option<String>,
    pub error_kind: Option<String>,
    pub sent_at: i64,
    pub completed_at: Option<i64>,
}

/// Caller-supplied fields for [`SessionDb::record_request_outcome`]. Exactly
/// one of `request_body_object_id` / `request_body_hash` must be present.
#[derive(Debug, Clone)]
pub struct RequestOutcomeDraft {
    pub request_id: String,
    pub attempt: i64,
    pub session_id: String,
    pub source_epoch: i64,
    pub projection_generation: i64,
    pub request_body_object_id: Option<String>,
    pub request_body_hash: Option<String>,
    pub route: String,
    pub model: String,
    pub outcome_status: OutcomeStatus,
    pub usage: Option<serde_json::Value>,
    pub error_kind: Option<String>,
    pub sent_at: i64,
    pub completed_at: Option<i64>,
}

// -----------------------------------------------------------------------------
// Migration
// -----------------------------------------------------------------------------

/// DDL for objects, projections and request outcomes. `IF NOT EXISTS`
/// throughout; a pre-C01 file gains the tables with no data movement.
pub(crate) const CONTEXT_PROJECTION_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS context_objects (
        object_id TEXT PRIMARY KEY,
        sha256 TEXT NOT NULL,
        kind TEXT NOT NULL,
        byte_length INTEGER NOT NULL,
        storage_path TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        deleted_at INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_context_objects_sha256
        ON context_objects(sha256);

    CREATE TABLE IF NOT EXISTS context_projections (
        projection_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        branch_id TEXT NOT NULL,
        source_epoch INTEGER NOT NULL,
        covered_through_event_id TEXT,
        method_id TEXT NOT NULL,
        method_version TEXT NOT NULL,
        config_revision TEXT NOT NULL,
        focus_revision TEXT NOT NULL,
        projection_generation INTEGER NOT NULL,
        payload_kind TEXT NOT NULL,
        payload_json TEXT,
        object_id TEXT,
        status TEXT NOT NULL,
        committed_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_context_projections_scope
        ON context_projections(session_id, branch_id, status);
    CREATE INDEX IF NOT EXISTS idx_context_projections_generation
        ON context_projections(session_id, branch_id, projection_generation);

    CREATE TABLE IF NOT EXISTS request_outcomes (
        request_id TEXT NOT NULL,
        attempt INTEGER NOT NULL,
        session_id TEXT NOT NULL,
        source_epoch INTEGER NOT NULL,
        projection_generation INTEGER NOT NULL,
        request_body_object_id TEXT,
        request_body_hash TEXT,
        route TEXT NOT NULL,
        model TEXT NOT NULL,
        outcome_status TEXT NOT NULL,
        usage_json TEXT,
        error_kind TEXT,
        sent_at INTEGER NOT NULL,
        completed_at INTEGER,
        PRIMARY KEY (request_id, attempt)
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_request_outcomes_request_attempt
        ON request_outcomes(request_id, attempt);
";

// -----------------------------------------------------------------------------
// Row decoding
// -----------------------------------------------------------------------------

const OBJECT_COLUMNS: &str =
    "object_id, sha256, kind, byte_length, storage_path, created_at, deleted_at";

const PROJECTION_COLUMNS: &str = "projection_id, session_id, branch_id, source_epoch, \
     covered_through_event_id, method_id, method_version, config_revision, focus_revision, \
     projection_generation, payload_kind, payload_json, object_id, status, committed_at";

const OUTCOME_COLUMNS: &str = "request_id, attempt, session_id, source_epoch, \
     projection_generation, request_body_object_id, request_body_hash, route, model, \
     outcome_status, usage_json, error_kind, sent_at, completed_at";

fn row_to_object(row: &libsql::Row) -> Result<ContextObject, SessionError> {
    Ok(ContextObject {
        object_id: row.get::<String>(0)?,
        sha256: row.get::<String>(1)?,
        kind: ContextObjectKind::parse(&row.get::<String>(2)?)?,
        byte_length: row.get::<i64>(3)?,
        storage_path: row.get::<String>(4)?,
        created_at: row.get::<i64>(5)?,
        deleted_at: row.get::<Option<i64>>(6)?,
    })
}

fn row_to_projection(row: &libsql::Row) -> Result<ContextProjection, SessionError> {
    Ok(ContextProjection {
        projection_id: row.get::<String>(0)?,
        session_id: row.get::<String>(1)?,
        branch_id: row.get::<String>(2)?,
        source_epoch: row.get::<i64>(3)?,
        covered_through_event_id: row.get::<Option<String>>(4)?,
        method_id: row.get::<String>(5)?,
        method_version: row.get::<String>(6)?,
        config_revision: row.get::<String>(7)?,
        focus_revision: row.get::<String>(8)?,
        projection_generation: row.get::<i64>(9)?,
        payload_kind: PayloadKind::parse(&row.get::<String>(10)?)?,
        payload_json: row.get::<Option<String>>(11)?,
        object_id: row.get::<Option<String>>(12)?,
        status: ProjectionStatus::parse(&row.get::<String>(13)?)?,
        committed_at: row.get::<i64>(14)?,
    })
}

fn row_to_outcome(row: &libsql::Row) -> Result<RequestOutcome, SessionError> {
    Ok(RequestOutcome {
        request_id: row.get::<String>(0)?,
        attempt: row.get::<i64>(1)?,
        session_id: row.get::<String>(2)?,
        source_epoch: row.get::<i64>(3)?,
        projection_generation: row.get::<i64>(4)?,
        request_body_object_id: row.get::<Option<String>>(5)?,
        request_body_hash: row.get::<Option<String>>(6)?,
        route: row.get::<String>(7)?,
        model: row.get::<String>(8)?,
        outcome_status: OutcomeStatus::parse(&row.get::<String>(9)?)?,
        usage_json: row.get::<Option<String>>(10)?,
        error_kind: row.get::<Option<String>>(11)?,
        sent_at: row.get::<i64>(12)?,
        completed_at: row.get::<Option<i64>>(13)?,
    })
}

/// An object id becomes a file name, so it must be a plain token: letters,
/// digits, `-`, `_`, `.` — and never `.` / `..` themselves.
fn validate_object_id(object_id: &str) -> Result<(), SessionError> {
    let ok = !object_id.is_empty()
        && object_id != "."
        && object_id != ".."
        && object_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(SessionError::Context(format!(
            "invalid object_id `{object_id}`: must be a plain file-name token"
        )))
    }
}

// -----------------------------------------------------------------------------
// Store API: objects
// -----------------------------------------------------------------------------

impl SessionDb {
    /// Directory the host derives for this database's objects. Never taken
    /// from caller input.
    fn context_object_dir(&self) -> PathBuf {
        let path = self.path();
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "session".to_string());
        let name = format!("{stem}.context-objects");
        match parent {
            Some(parent) => parent.join(name),
            None => PathBuf::from(name),
        }
    }

    /// Store `draft.bytes` as a content-verified object and commit its manifest
    /// row. Returns `(object, inserted)`; re-storing an identical object is a
    /// no-op that returns the existing row. Storing the *same id* with
    /// different bytes is refused (an id names one immutable object).
    pub fn store_context_object(
        &self,
        draft: ContextObjectDraft,
    ) -> Result<(ContextObject, bool), SessionError> {
        validate_object_id(&draft.object_id)?;
        let sha256 = sha256_hex(&draft.bytes);
        let byte_length = i64::try_from(draft.bytes.len())
            .map_err(|_| SessionError::Context("object too large for i64 length".into()))?;

        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;

            // Existing manifest for this id: idempotent replay when identical,
            // an id-immutability violation otherwise.
            if let Some(existing) = select_object(conn, &draft.object_id).await? {
                if existing.sha256 == sha256
                    && existing.byte_length == byte_length
                    && existing.kind == draft.kind
                {
                    return Ok((existing, false));
                }
                return Err(SessionError::Context(format!(
                    "object_id `{}` already exists with different content",
                    draft.object_id
                )));
            }

            let dir = self.context_object_dir();
            fs::create_dir_all(&dir)?;
            let final_path = dir.join(&draft.object_id);
            let tmp_path = dir.join(format!(".{}.{}.tmp", draft.object_id, crate::now_ms()));
            let pre_existing = final_path.exists();

            // 1. temp write, 2. verify length + hash by reading back.
            fs::write(&tmp_path, &draft.bytes)?;
            let on_disk = fs::read(&tmp_path)?;
            if on_disk.len() != draft.bytes.len() || sha256_hex(&on_disk) != sha256 {
                let _ = fs::remove_file(&tmp_path);
                return Err(SessionError::Context(format!(
                    "temp object `{}` failed integrity verification",
                    draft.object_id
                )));
            }
            fs::rename(&tmp_path, &final_path)?;

            let created_at = crate::now_ms();
            let storage_path = final_path.to_string_lossy().into_owned();

            // 3. commit the manifest row; roll the file back on failure.
            let tx = conn
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await?;
            let inserted = tx
                .execute(
                    "INSERT INTO context_objects \
                     (object_id, sha256, kind, byte_length, storage_path, created_at, deleted_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                    libsql::params![
                        draft.object_id.as_str(),
                        sha256.as_str(),
                        draft.kind.as_str(),
                        byte_length,
                        storage_path.as_str(),
                        created_at,
                    ],
                )
                .await;
            let inserted = match inserted {
                Ok(n) => n,
                Err(e) => {
                    if !pre_existing {
                        let _ = fs::remove_file(&final_path);
                    }
                    return Err(e.into());
                }
            };
            tx.commit().await?;

            // 4. nothing left to reclaim: the temp file was renamed into place.
            Ok((
                ContextObject {
                    object_id: draft.object_id,
                    sha256,
                    kind: draft.kind,
                    byte_length,
                    storage_path,
                    created_at,
                    deleted_at: None,
                },
                inserted > 0,
            ))
        })
    }

    /// Look up an object manifest by id.
    pub fn context_object(&self, object_id: &str) -> Result<Option<ContextObject>, SessionError> {
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            select_object(conn, object_id).await
        })
    }

    /// Read an object's bytes back, re-verifying its sha256 and length. A
    /// soft-deleted object, a missing file, or bytes that no longer match the
    /// manifest are all hard errors — integrity is checked on read, not
    /// assumed from the row.
    pub fn read_context_object(&self, object_id: &str) -> Result<Vec<u8>, SessionError> {
        let row = self.context_object(object_id)?.ok_or_else(|| {
            SessionError::Context(format!("context object `{object_id}` does not exist"))
        })?;
        if row.deleted_at.is_some() {
            return Err(SessionError::Context(format!(
                "context object `{object_id}` is deleted"
            )));
        }
        let bytes = fs::read(&row.storage_path)?;
        if i64::try_from(bytes.len()).ok() != Some(row.byte_length)
            || sha256_hex(&bytes) != row.sha256
        {
            return Err(SessionError::Context(format!(
                "context object `{object_id}` failed integrity verification"
            )));
        }
        Ok(bytes)
    }

    /// Delete object files no manifest row references, plus stale `.tmp`
    /// writes. Returns how many files were reclaimed. This is the "clear
    /// residue" pass for a crash between the temp write and the commit.
    pub fn sweep_orphan_object_files(&self) -> Result<u64, SessionError> {
        let dir = self.context_object_dir();
        if !dir.exists() {
            return Ok(0);
        }
        let inner = self.inner();
        let guard = inner.conn.lock();
        let referenced: Vec<String> = inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query("SELECT object_id FROM context_objects", ())
                .await?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next().await? {
                ids.push(row.get::<String>(0)?);
            }
            Ok::<Vec<String>, SessionError>(ids)
        })?;

        let mut removed = 0u64;
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_tmp = name.starts_with('.') && name.ends_with(".tmp");
            let unreferenced = !referenced.iter().any(|id| id == &name);
            if is_tmp || unreferenced {
                // Never delete a directory through this path; only files.
                if entry.file_type()?.is_file() {
                    fs::remove_file(entry.path())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

async fn select_object(
    conn: &libsql::Connection,
    object_id: &str,
) -> Result<Option<ContextObject>, SessionError> {
    let mut rows = conn
        .query(
            &format!("SELECT {OBJECT_COLUMNS} FROM context_objects WHERE object_id = ?1"),
            libsql::params![object_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row_to_object(&row)?)),
        None => Ok(None),
    }
}

async fn select_object_in_tx(
    tx: &libsql::Transaction,
    object_id: &str,
) -> Result<Option<ContextObject>, SessionError> {
    let mut rows = tx
        .query(
            &format!("SELECT {OBJECT_COLUMNS} FROM context_objects WHERE object_id = ?1"),
            libsql::params![object_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row_to_object(&row)?)),
        None => Ok(None),
    }
}

// -----------------------------------------------------------------------------
// Store API: projections
// -----------------------------------------------------------------------------

impl SessionDb {
    /// Commit a projection and, when its generation is newer, move the
    /// session-level authoritative pointer to it.
    ///
    /// Returns `(projection, authoritative)`. A projection whose generation is
    /// **not** greater than the current authoritative one is still stored
    /// (`status = committed`) but the pointer does not move — the stale
    /// result is preserved for audit and cannot overwrite the newer one.
    /// Re-committing the same `projection_id` returns the stored row.
    pub fn commit_projection(
        &self,
        draft: ProjectionDraft,
    ) -> Result<(ContextProjection, bool), SessionError> {
        if draft.projection_id.trim().is_empty() {
            return Err(SessionError::Context(
                "projection_id must be non-empty".into(),
            ));
        }
        for (field, value) in [
            ("session_id", &draft.session_id),
            ("branch_id", &draft.branch_id),
            ("method_id", &draft.method_id),
            ("method_version", &draft.method_version),
            ("config_revision", &draft.config_revision),
            ("focus_revision", &draft.focus_revision),
        ] {
            if value.trim().is_empty() {
                return Err(SessionError::Context(format!("{field} must be non-empty")));
            }
        }
        match draft.payload_kind {
            PayloadKind::InlineJson if draft.payload_json.is_none() => {
                return Err(SessionError::Context(
                    "inline_json projection requires payload_json".into(),
                ));
            }
            PayloadKind::ObjectRef if draft.object_id.is_none() => {
                return Err(SessionError::Context(
                    "object_ref projection requires object_id".into(),
                ));
            }
            _ => {}
        }

        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;

            if let Some(existing) = select_projection(conn, &draft.projection_id).await? {
                let authoritative = existing.status == ProjectionStatus::Authoritative;
                return Ok((existing, authoritative));
            }

            let tx = conn
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await?;

            // An object_ref payload must point at a live object.
            if let Some(object_id) = &draft.object_id {
                let obj = select_object_in_tx(&tx, object_id).await?;
                let ok = matches!(
                    obj.as_ref(),
                    Some(o) if o.deleted_at.is_none() && o.kind == ContextObjectKind::ProjectionPayload
                );
                if !ok {
                    return Err(SessionError::Context(format!(
                        "projection object `{object_id}` is missing, deleted or not a \
                         projection payload"
                    )));
                }
            }

            let committed_at = crate::now_ms();
            tx.execute(
                "INSERT INTO context_projections \
                 (projection_id, session_id, branch_id, source_epoch, covered_through_event_id, \
                  method_id, method_version, config_revision, focus_revision, \
                  projection_generation, payload_kind, payload_json, object_id, status, \
                  committed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                libsql::params![
                    draft.projection_id.as_str(),
                    draft.session_id.as_str(),
                    draft.branch_id.as_str(),
                    draft.source_epoch,
                    draft.covered_through_event_id.clone(),
                    draft.method_id.as_str(),
                    draft.method_version.as_str(),
                    draft.config_revision.as_str(),
                    draft.focus_revision.as_str(),
                    draft.projection_generation,
                    draft.payload_kind.as_str(),
                    draft.payload_json.clone(),
                    draft.object_id.clone(),
                    ProjectionStatus::Committed.as_str(),
                    committed_at,
                ],
            )
            .await?;

            // Compare-and-set the authoritative pointer: only a strictly newer
            // generation may take it. Read the current holder inside the same
            // write transaction so two racing commits cannot both promote.
            let current = select_authoritative_in_tx(&tx, &draft.session_id, &draft.branch_id).await?;
            let promote = match &current {
                None => true,
                Some(cur) => draft.projection_generation > cur.projection_generation,
            };
            let mut status = ProjectionStatus::Committed;
            if promote {
                if let Some(cur) = &current {
                    tx.execute(
                        "UPDATE context_projections SET status = ?1 WHERE projection_id = ?2",
                        libsql::params![ProjectionStatus::Superseded.as_str(), cur.projection_id.as_str()],
                    )
                    .await?;
                }
                tx.execute(
                    "UPDATE context_projections SET status = ?1 WHERE projection_id = ?2",
                    libsql::params![
                        ProjectionStatus::Authoritative.as_str(),
                        draft.projection_id.as_str()
                    ],
                )
                .await?;
                status = ProjectionStatus::Authoritative;
            }
            tx.commit().await?;

            Ok((
                ContextProjection {
                    projection_id: draft.projection_id,
                    session_id: draft.session_id,
                    branch_id: draft.branch_id,
                    source_epoch: draft.source_epoch,
                    covered_through_event_id: draft.covered_through_event_id,
                    method_id: draft.method_id,
                    method_version: draft.method_version,
                    config_revision: draft.config_revision,
                    focus_revision: draft.focus_revision,
                    projection_generation: draft.projection_generation,
                    payload_kind: draft.payload_kind,
                    payload_json: draft.payload_json,
                    object_id: draft.object_id,
                    status,
                    committed_at,
                },
                promote,
            ))
        })
    }

    /// The projection the host currently serves for `(session, branch)`, or
    /// `None` when nothing has been committed yet.
    pub fn authoritative_projection(
        &self,
        session_id: &str,
        branch_id: &str,
    ) -> Result<Option<ContextProjection>, SessionError> {
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    &format!(
                        "SELECT {PROJECTION_COLUMNS} FROM context_projections \
                         WHERE session_id = ?1 AND branch_id = ?2 AND status = ?3 \
                         LIMIT 1"
                    ),
                    libsql::params![
                        session_id,
                        branch_id,
                        ProjectionStatus::Authoritative.as_str()
                    ],
                )
                .await?;
            match rows.next().await? {
                Some(row) => Ok(Some(row_to_projection(&row)?)),
                None => Ok(None),
            }
        })
    }

    /// Every projection committed for a session, newest generation first.
    pub fn projections_for_session(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<ContextProjection>, SessionError> {
        let limit = crate::validate_limit(limit)?;
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    &format!(
                        "SELECT {PROJECTION_COLUMNS} FROM context_projections \
                         WHERE session_id = ?1 \
                         ORDER BY projection_generation DESC, committed_at DESC LIMIT ?2"
                    ),
                    libsql::params![session_id, limit],
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(row_to_projection(&row)?);
            }
            Ok(out)
        })
    }
}

async fn select_projection(
    conn: &libsql::Connection,
    projection_id: &str,
) -> Result<Option<ContextProjection>, SessionError> {
    let mut rows = conn
        .query(
            &format!(
                "SELECT {PROJECTION_COLUMNS} FROM context_projections WHERE projection_id = ?1"
            ),
            libsql::params![projection_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row_to_projection(&row)?)),
        None => Ok(None),
    }
}

async fn select_authoritative_in_tx(
    tx: &libsql::Transaction,
    session_id: &str,
    branch_id: &str,
) -> Result<Option<ContextProjection>, SessionError> {
    let mut rows = tx
        .query(
            &format!(
                "SELECT {PROJECTION_COLUMNS} FROM context_projections \
                 WHERE session_id = ?1 AND branch_id = ?2 AND status = ?3 LIMIT 1"
            ),
            libsql::params![
                session_id,
                branch_id,
                ProjectionStatus::Authoritative.as_str()
            ],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row_to_projection(&row)?)),
        None => Ok(None),
    }
}

// -----------------------------------------------------------------------------
// Store API: request outcomes
// -----------------------------------------------------------------------------

impl SessionDb {
    /// Record one provider-send attempt. `(request_id, attempt)` is the
    /// primary key, so replaying the same attempt returns the stored row and
    /// never overwrites it — a later attempt is a new `attempt` value, and a
    /// `send_failed` row is never upgraded by retrying the same attempt.
    pub fn record_request_outcome(
        &self,
        draft: RequestOutcomeDraft,
    ) -> Result<(RequestOutcome, bool), SessionError> {
        if draft.request_id.trim().is_empty() {
            return Err(SessionError::Context("request_id must be non-empty".into()));
        }
        if draft.attempt < 1 {
            return Err(SessionError::Context("request attempt must be >= 1".into()));
        }
        match (&draft.request_body_object_id, &draft.request_body_hash) {
            (Some(_), None) | (None, Some(_)) => {}
            _ => {
                return Err(SessionError::Context(
                    "request outcome needs exactly one of request_body_object_id / \
                     request_body_hash"
                        .into(),
                ));
            }
        }
        let usage_json = match &draft.usage {
            Some(value) => Some(
                serde_json::to_string(value)
                    .map_err(|e| SessionError::Context(format!("usage is not JSON: {e}")))?,
            ),
            None => None,
        };

        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;

            if let Some(existing) = select_outcome(conn, &draft.request_id, draft.attempt).await? {
                return Ok((existing, false));
            }

            let tx = conn
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await?;
            // Validate the body object reference against a live manifest.
            if let Some(object_id) = &draft.request_body_object_id {
                let obj = select_object_in_tx(&tx, object_id).await?;
                let ok = matches!(obj.as_ref(), Some(o) if o.deleted_at.is_none());
                if !ok {
                    return Err(SessionError::Context(format!(
                        "request body object `{object_id}` is missing or deleted"
                    )));
                }
            }
            tx.execute(
                "INSERT INTO request_outcomes \
                 (request_id, attempt, session_id, source_epoch, projection_generation, \
                  request_body_object_id, request_body_hash, route, model, outcome_status, \
                  usage_json, error_kind, sent_at, completed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                libsql::params![
                    draft.request_id.as_str(),
                    draft.attempt,
                    draft.session_id.as_str(),
                    draft.source_epoch,
                    draft.projection_generation,
                    draft.request_body_object_id.clone(),
                    draft.request_body_hash.clone(),
                    draft.route.as_str(),
                    draft.model.as_str(),
                    draft.outcome_status.as_str(),
                    usage_json.clone(),
                    draft.error_kind.clone(),
                    draft.sent_at,
                    draft.completed_at,
                ],
            )
            .await?;
            tx.commit().await?;

            Ok((
                RequestOutcome {
                    request_id: draft.request_id,
                    attempt: draft.attempt,
                    session_id: draft.session_id,
                    source_epoch: draft.source_epoch,
                    projection_generation: draft.projection_generation,
                    request_body_object_id: draft.request_body_object_id,
                    request_body_hash: draft.request_body_hash,
                    route: draft.route,
                    model: draft.model,
                    outcome_status: draft.outcome_status,
                    usage_json,
                    error_kind: draft.error_kind,
                    sent_at: draft.sent_at,
                    completed_at: draft.completed_at,
                },
                true,
            ))
        })
    }

    /// One recorded attempt, or `None`.
    pub fn request_outcome(
        &self,
        request_id: &str,
        attempt: i64,
    ) -> Result<Option<RequestOutcome>, SessionError> {
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            select_outcome(conn, request_id, attempt).await
        })
    }

    /// Every attempt of a session, newest first.
    pub fn request_outcomes_for_session(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<RequestOutcome>, SessionError> {
        let limit = crate::validate_limit(limit)?;
        let inner = self.inner();
        let guard = inner.conn.lock();
        inner.runtime.block_on(async move {
            let conn = &*guard;
            let mut rows = conn
                .query(
                    &format!(
                        "SELECT {OUTCOME_COLUMNS} FROM request_outcomes \
                         WHERE session_id = ?1 ORDER BY sent_at DESC, attempt DESC LIMIT ?2"
                    ),
                    libsql::params![session_id, limit],
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(row_to_outcome(&row)?);
            }
            Ok(out)
        })
    }
}

async fn select_outcome(
    conn: &libsql::Connection,
    request_id: &str,
    attempt: i64,
) -> Result<Option<RequestOutcome>, SessionError> {
    let mut rows = conn
        .query(
            &format!(
                "SELECT {OUTCOME_COLUMNS} FROM request_outcomes \
                 WHERE request_id = ?1 AND attempt = ?2"
            ),
            libsql::params![request_id, attempt],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row_to_outcome(&row)?)),
        None => Ok(None),
    }
}
