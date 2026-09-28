//! The one-shot idle-session sweep a daemon start kicks off.
//!
//! The sweep *decision* — whether to run at all, and exactly which command
//! to run — is factored out of the daemon binary's `main` so it is testable
//! without spawning a process. The binary keeps only the spawn and the
//! wait-thread glue.

use std::path::{Path, PathBuf};

/// What a daemon start should run for the sweep: the sibling `kymido` CLI
/// doing `session compress`.
///
/// The work happens in a child process so a long backlog can never stall
/// the serve loop; the child opens the database file directly and
/// serializes against the daemon through SQLite's own locking, and the
/// `--max-sessions` cap bounds how long it holds the write lock per batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepCommand {
    /// Absolute path of the sibling `kymido` binary.
    pub program: PathBuf,
    /// Fixed argument vector: conservative defaults (`--idle-days 30`,
    /// `--max-sessions 200`) and machine-readable output for the log.
    pub args: [&'static str; 7],
}

/// Why a startup sweep was skipped. Carries enough context for the caller
/// to log a useful line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepSkip {
    /// No session database on disk yet (fresh install): a sweep child would
    /// only start, report "no session database", and exit.
    NoDatabase,
    /// The sibling `kymido` binary was not found next to the daemon
    /// executable (e.g. the daemon running alone from a cargo target dir).
    CliMissing(PathBuf),
}

/// Decide the startup sweep from two on-disk facts: the session database
/// path and the directory the daemon executable lives in.
pub fn startup_sweep_command(db_path: &Path, exe_dir: &Path) -> Result<SweepCommand, SweepSkip> {
    if !db_path.is_file() {
        return Err(SweepSkip::NoDatabase);
    }
    let cli = exe_dir.join("kymido");
    if !cli.is_file() {
        return Err(SweepSkip::CliMissing(cli));
    }
    Ok(SweepCommand {
        program: cli,
        args: [
            "session",
            "compress",
            "--idle-days",
            "30",
            "--max-sessions",
            "200",
            "--json",
        ],
    })
}
