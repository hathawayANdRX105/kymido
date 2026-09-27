//! Cold-session archive: move an idle session out of the live database into
//! a compressed file, and put it back on demand.
//!
//! ## Why a separate file rather than a compressed column
//!
//! Deleting rows does **not** shrink a SQLite file. SQLite frees pages into
//! an internal pool and the next inserts recycle them, so the file stays at
//! its high-water mark (jcode's telemetry worker documents this explicitly:
//! *"SQLite files never shrink on DELETE… steady state is file at high-water
//! mark"*). Keeping cold sessions compressed *inside* the database would
//! therefore save nothing on disk. Moving them to a separate zstd file is
//! what actually returns the bytes to the filesystem.
//!
//! ## Wire format
//!
//! One zstd frame wrapping a JSONL stream:
//!
//! ```text
//! {"kind":"header","session":{...},"message_count":N}
//! {"kind":"message","seq":1,"role":"user","text":"…",…}
//! …
//! ```
//!
//! The stream ends with a `{"kind":"trailer","checksum":"<hex>"}` line whose
//! checksum covers the concatenated bytes of every preceding line. A stream
//! without a well-formed trailer is a truncated write and is rejected — a
//! half-restored session is worse than a loud failure.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{SessionError, SessionRole, SessionSummary};
/// One archived message, in the exact shape it is stored in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArchivedMessage {
    pub(crate) seq: i64,
    pub(crate) role: String,
    pub(crate) text: String,
    pub(crate) created_at: i64,
    pub(crate) attachments: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArchivedSession {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) turn_log: String,
    pub(crate) last_access_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ArchiveLine {
    Header {
        session: ArchivedSession,
        message_count: usize,
    },
    Message {
        #[serde(flatten)]
        message: ArchivedMessage,
    },
    Trailer {
        checksum: String,
    },
}

/// What an archive sweep actually moved, and what it reclaimed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ArchiveReceipt {
    pub session_id: String,
    /// Message rows removed from the live database.
    pub messages: usize,
    /// Uncompressed JSONL bytes.
    pub raw_bytes: u64,
    /// Bytes of the `.jsonl.zst` file now on disk.
    pub archived_bytes: u64,
}

impl ArchiveReceipt {
    /// Compression ratio; `None` when the session had no messages.
    pub fn ratio(&self) -> Option<f64> {
        if self.raw_bytes == 0 {
            return None;
        }
        Some(self.raw_bytes as f64 / self.archived_bytes.max(1) as f64)
    }
}

/// Directory holding archives, derived from the database file's location.
pub fn archive_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("archive")
}

pub(crate) fn archive_path(db_path: &Path, id: &str) -> PathBuf {
    archive_dir(db_path).join(format!("{id}.jsonl.zst"))
}

/// `true` when an archive file exists for `id`.
pub fn has_archive(db_path: &Path, id: &str) -> bool {
    archive_path(db_path, id).is_file()
}

/// FNV-1a over the raw line bytes. Not a security primitive — it exists to
/// catch a truncated or corrupted write, not an adversary.
pub(crate) fn checksum_of(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn encode_archive(session: &ArchivedSession, messages: &[ArchivedMessage]) -> Vec<u8> {
    let mut lines = String::new();
    // `std::iter::once(..).chain(..)` rather than an array with a spread:
    // the array form cannot mix a fixed head with a mapped tail.
    let head = ArchiveLine::Header {
        session: session.clone(),
        message_count: messages.len(),
    };
    let tail = messages
        .iter()
        .map(|m| ArchiveLine::Message { message: m.clone() });
    for line in std::iter::once(head).chain(tail) {
        // Each field is plain JSON-safe data; serialization cannot fail.
        lines.push_str(&serde_json::to_string(&line).expect("encode archive line"));
        lines.push('\n');
    }
    let sum = checksum_of(lines.as_bytes());
    lines.push_str(
        &serde_json::to_string(&ArchiveLine::Trailer { checksum: sum }).expect("encode trailer"),
    );
    lines.push('\n');
    zstd::encode_all(lines.as_bytes(), 3).expect("zstd encode")
}

pub(crate) struct DecodedArchive {
    pub session: ArchivedSession,
    pub messages: Vec<ArchivedMessage>,
}

pub(crate) fn decode_archive(raw: &[u8]) -> Result<DecodedArchive, SessionError> {
    let text = zstd::decode_all(raw).map_err(|e| SessionError::Archive(e.to_string()))?;
    let text = String::from_utf8(text).map_err(|e| SessionError::Archive(e.to_string()))?;

    // Split off the trailer: it is the last line and is the integrity witness.
    let Some(cut) = text.rfind('\n') else {
        return Err(SessionError::Archive("archive has no trailer line".into()));
    };
    let (body, trailer_line) = text.split_at(cut);
    let body_bytes = body.as_bytes();
    let trailer_line = trailer_line.strip_prefix('\n').unwrap_or(trailer_line);

    let expected = match serde_json::from_str::<ArchiveLine>(trailer_line)
        .map_err(|e| SessionError::Archive(format!("malformed trailer: {e}")))?
    {
        ArchiveLine::Trailer { checksum } => checksum,
        _ => return Err(SessionError::Archive("last line is not a trailer".into())),
    };
    if checksum_of(body_bytes) != expected {
        return Err(SessionError::Archive(
            "archive checksum mismatch: the file is truncated or corrupt".into(),
        ));
    }

    let mut session = None;
    let mut declared = 0usize;
    let mut messages = Vec::new();
    for line in body_lines(body_bytes) {
        let line =
            String::from_utf8(line.to_vec()).map_err(|e| SessionError::Archive(e.to_string()))?;
        match serde_json::from_str::<ArchiveLine>(&line)
            .map_err(|e| SessionError::Archive(format!("malformed line: {e}")))?
        {
            ArchiveLine::Header {
                session: s,
                message_count,
            } => {
                if session.is_some() {
                    return Err(SessionError::Archive("duplicate header line".into()));
                }
                declared = message_count;
                session = Some(s);
            }
            ArchiveLine::Message { message } => messages.push(message),
            ArchiveLine::Trailer { .. } => {
                return Err(SessionError::Archive("trailer before end of stream".into()));
            }
        }
    }

    let session =
        session.ok_or_else(|| SessionError::Archive("archive has no header line".into()))?;
    if messages.len() != declared {
        return Err(SessionError::Archive(format!(
            "header declares {declared} messages but the stream carries {}",
            messages.len()
        )));
    }
    Ok(DecodedArchive { session, messages })
}

fn body_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty())
}

pub(crate) fn write_archive_file(path: &Path, payload: &[u8]) -> Result<u64, SessionError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(SessionError::Io)?;
    restrict_to_owner(dir, 0o700);
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".into())
    ));
    {
        let mut f = fs::File::create(&tmp).map_err(SessionError::Io)?;
        f.write_all(payload).map_err(SessionError::Io)?;
        f.sync_all().map_err(SessionError::Io)?;
    }
    restrict_to_owner(&tmp, 0o600);
    fs::rename(&tmp, path).map_err(SessionError::Io)?;
    Ok(payload.len() as u64)
}

/// Owner-only permission hardening, best-effort: an archive is the same
/// plaintext a live session is. Non-unix platforms have no POSIX mode bits
/// to set, so there this is a deliberate no-op.
fn restrict_to_owner(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

/// Remove an archive file. `true` when a file was actually removed.
pub fn delete_archive(db_path: &Path, id: &str) -> Result<bool, SessionError> {
    let path = archive_path(db_path, id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(SessionError::Io(e)),
    }
}

/// List archived session ids currently on disk.
pub fn list_archives(db_path: &Path) -> Result<Vec<String>, SessionError> {
    let dir = archive_dir(db_path);
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(SessionError::Io(e)),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(SessionError::Io)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(id) = name.strip_suffix(".jsonl.zst") {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    Ok(ids)
}

/// An archive is untrusted input as far as the store is concerned: a role
/// the current build no longer knows must fail the restore, not be written
/// through as a raw string.
pub(crate) fn role_from_str(s: &str) -> Result<SessionRole, SessionError> {
    SessionRole::parse(s)
}

pub(crate) fn summary_of(s: &ArchivedSession, message_count: usize) -> SessionSummary {
    SessionSummary {
        id: s.id.clone(),
        title: s.title.clone(),
        parent_id: s.parent_id.clone(),
        created_at_ms: s.created_at,
        updated_at_ms: s.updated_at,
        message_count: message_count as u64,
    }
}
