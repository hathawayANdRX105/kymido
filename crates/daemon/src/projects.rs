//! Project registry (A2): a persisted list of registered working
//! directories.  A project is a directory the user can create sessions in;
//! it is a plain label over a path (sessions themselves stay in the single
//! `sessions.db` and are grouped under the active project — per-project
//! session scoping is a follow-up, not this MVP).
//!
//! Storage: a `projects.json` array of [`ProjectEntry`] in the daemon's data
//! dir.  The first open seeds the default project (the data dir itself), so
//! `project.list` always yields at least the home project.  Mutations write
//! the whole file back; reads serve the in-memory list.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::protocol::ProjectEntry;

const FILE: &str = "projects.json";

fn default_entry(data_dir: &Path) -> ProjectEntry {
    let path = data_dir.to_string_lossy().into_owned();
    let name = data_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "kymido".into());
    ProjectEntry {
        id: path.clone(),
        name,
        path,
    }
}

fn read_file(file: &Path) -> Result<Vec<ProjectEntry>, std::io::Error> {
    let raw = fs::read_to_string(file)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn write_file(file: &Path, entries: &[ProjectEntry]) -> Result<(), std::io::Error> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(entries).unwrap_or_else(|_| "[]".into());
    fs::write(file, raw)
}

/// A registered project failed to create / remove.
#[derive(Debug)]
pub enum ProjectError {
    /// The path is not an existing directory.
    NotADirectory(String),
    /// A project at this path is already registered.
    AlreadyRegistered(String),
    /// File I/O failure.
    Io(std::io::Error),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADirectory(p) => write!(f, "not an existing directory: {p}"),
            Self::AlreadyRegistered(p) => write!(f, "project already registered: {p}"),
            Self::Io(e) => write!(f, "project registry I/O error: {e}"),
        }
    }
}

impl std::error::Error for ProjectError {}

/// Persisted project registry, held behind a `Mutex` so the (single-request)
/// dispatch path can read/write it without re-reading the file each call.
pub struct ProjectStore {
    file: PathBuf,
    entries: Mutex<Vec<ProjectEntry>>,
}

impl ProjectStore {
    /// Open the registry at `data_dir`, seeding the default project on first
    /// run.  A missing / corrupt file degrades to the seeded default, never
    /// a hard error (a fresh install has nothing to preserve).
    pub fn open(data_dir: &Path) -> Self {
        let file = data_dir.join(FILE);
        let mut entries = read_file(&file).unwrap_or_default();
        if entries.is_empty() {
            entries.push(default_entry(data_dir));
            let _ = write_file(&file, &entries);
        }
        Self {
            file,
            entries: Mutex::new(entries),
        }
    }

    /// All registered projects.
    pub fn list(&self) -> Vec<ProjectEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Register `path` as a project.  Validates it is an existing directory
    /// and is not already registered; the path doubles as the project id.
    pub fn create(&self, path: &str) -> Result<ProjectEntry, ProjectError> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(ProjectError::NotADirectory(trimmed.to_string()));
        }
        let meta =
            fs::metadata(trimmed).map_err(|_| ProjectError::NotADirectory(trimmed.to_string()))?;
        if !meta.is_dir() {
            return Err(ProjectError::NotADirectory(trimmed.to_string()));
        }
        let p = Path::new(trimmed);
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| trimmed.to_string());
        let entry = ProjectEntry {
            id: trimmed.to_string(),
            name,
            path: trimmed.to_string(),
        };
        let mut g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if g.iter().any(|e| e.id == entry.id) {
            return Err(ProjectError::AlreadyRegistered(entry.id.clone()));
        }
        g.push(entry.clone());
        write_file(&self.file, &g).map_err(ProjectError::Io)?;
        Ok(entry)
    }

    /// Unregister a project by id.  `true` when a row was actually removed.
    pub fn remove(&self, id: &str) -> Result<bool, ProjectError> {
        let mut g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let before = g.len();
        g.retain(|e| e.id != id);
        let removed = g.len() < before;
        if removed {
            write_file(&self.file, &g).map_err(ProjectError::Io)?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// First open in an empty data dir seeds the default project (the dir
    /// itself), so `project.list` always yields the home project.
    #[test]
    fn open_seeds_default_project() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::open(dir.path());
        let rows = store.list();
        assert_eq!(rows.len(), 1, "exactly the seeded default project");
        assert_eq!(rows[0].path, dir.path().to_string_lossy());
        assert!(!rows[0].name.is_empty());
    }

    /// create validates the path is an existing directory, registers it, and
    /// persists to `projects.json`; a re-opened store sees the same row.
    #[test]
    fn create_list_remove_persist() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("myproject");
        std::fs::create_dir_all(&target).unwrap();

        let store = ProjectStore::open(dir.path());
        let created = store.create(target.to_string_lossy().as_ref()).unwrap();
        assert_eq!(created.id, target.to_string_lossy());
        assert_eq!(created.name, "myproject");
        assert!(store.list().iter().any(|p| p.id == created.id));

        // Persisted: a fresh open (new process) sees the registered project.
        let reloaded = ProjectStore::open(dir.path());
        assert!(reloaded.list().iter().any(|p| p.id == created.id));

        // Remove drops it (returns true) and persists the removal.
        assert!(store.remove(&created.id).unwrap());
        assert!(!store.list().iter().any(|p| p.id == created.id));
        let reloaded2 = ProjectStore::open(dir.path());
        assert!(!reloaded2.list().iter().any(|p| p.id == created.id));
        // Removing an unknown id is a no-op false, not an error.
        assert!(!store.remove(&created.id).unwrap());
    }

    /// create refuses paths that are not existing directories.
    #[test]
    fn create_rejects_non_directory() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::open(dir.path());
        let missing = dir.path().join("does-not-exist");
        match store.create(missing.to_string_lossy().as_ref()) {
            Err(ProjectError::NotADirectory(_)) => {}
            other => panic!("expected NotADirectory, got {other:?}"),
        }
    }

    /// create refuses a path that is already registered (duplicate id).
    #[test]
    fn create_rejects_duplicate() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("dup");
        std::fs::create_dir_all(&target).unwrap();
        let store = ProjectStore::open(dir.path());
        store.create(target.to_string_lossy().as_ref()).unwrap();
        match store.create(target.to_string_lossy().as_ref()) {
            Err(ProjectError::AlreadyRegistered(_)) => {}
            other => panic!("expected AlreadyRegistered, got {other:?}"),
        }
    }

    /// A corrupt / unreadable registry file degrades to the seeded default
    /// rather than hard-erroring (a fresh install has nothing to preserve).
    #[test]
    fn open_tolerates_corrupt_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("projects.json"), "{ not json ").unwrap();
        let store = ProjectStore::open(dir.path());
        assert_eq!(store.list().len(), 1, "corrupt file -> seeded default only");
    }
}
