//! Disk layer for saved conversations.
//!
//! `SessionStore` reads and writes `SessionRecord` values as one JSON file
//! per session, named `<session-id>.json`, under a sessions directory.

use std::io;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use super::{SessionId, SessionMeta, SessionRecord};
use crate::error::{HarnessError, Result};

const SESSIONS_SUBDIR: &str = ".deepseek/sessions";

/// The disk layer for saved conversations. Rooted at a sessions directory
/// that the caller supplies, so tests can point it at a temp directory
/// instead of the real project root.
pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    /// Create a store rooted directly at `sessions_dir`. Use this in tests.
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    /// Create a store rooted at the production location under
    /// `project_root`: `<project_root>/.deepseek/sessions/`.
    pub fn for_project(project_root: &Path) -> Self {
        Self::new(project_root.join(SESSIONS_SUBDIR))
    }

    fn record_path(&self, id: &SessionId) -> PathBuf {
        self.sessions_dir.join(format!("{}.json", id.as_str()))
    }

    /// Write `record` to disk, creating the sessions directory if needed.
    ///
    /// Guarantees: the write goes through a temporary file in the same
    /// directory, then renames over the target, so a crash or a power cut
    /// partway through a write cannot leave a truncated or half-written
    /// file at the real path. A second save of the same id fully replaces
    /// the first.
    pub fn save(&self, record: &SessionRecord) -> Result<()> {
        std::fs::create_dir_all(&self.sessions_dir)?;

        let target = self.record_path(&record.meta.id);
        let tmp_path = self
            .sessions_dir
            .join(format!("{}.json.tmp", record.meta.id.as_str()));

        let json = serde_json::to_string_pretty(record)
            .map_err(|e| HarnessError::Parse(format!("could not serialize session: {e}")))?;
        let bytes = json.len();
        std::fs::write(&tmp_path, json)?;
        std::fs::rename(&tmp_path, &target)?;

        debug!(
            session_id = record.meta.id.as_str(),
            bytes,
            path = %target.display(),
            "session store: wrote record"
        );
        Ok(())
    }

    /// Read and parse one session record.
    ///
    /// Guarantees: a missing file and a corrupt file come back as
    /// distinguishable errors. A missing file yields `HarnessError::Io`
    /// with `ErrorKind::NotFound`. A present but unparseable file yields
    /// `HarnessError::Parse`.
    pub fn load(&self, id: &SessionId) -> Result<SessionRecord> {
        let path = self.record_path(id);
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content).map_err(|e| {
            HarnessError::Parse(format!("could not parse session {}: {e}", path.display()))
        })
    }

    /// List the metadata for every session in the directory, highest
    /// number first.
    ///
    /// The order is by `seq`, not by `updated_at`. A saved conversation
    /// gets its number when it opens and keeps it, so a row holds its place
    /// in the list no matter how many times a running turn saves it. Sorting
    /// by `updated_at` moved the running conversation to the top on every
    /// autosave, which is once a turn and once every 15 seconds inside a
    /// long turn. `created_at` breaks a tie between two records that share a
    /// number, which only happens for records written before numbering
    /// existed and not yet renumbered.
    ///
    /// Guarantees: a file that cannot be read or parsed is skipped with a
    /// `warn!` log rather than failing the whole listing. A missing
    /// sessions directory yields an empty list, not an error.
    pub fn list(&self) -> Vec<SessionMeta> {
        let entries = match std::fs::read_dir(&self.sessions_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                warn!(
                    "session store: could not read sessions directory {}: {e}",
                    self.sessions_dir.display()
                );
                return Vec::new();
            }
        };

        let mut metas: Vec<SessionMeta> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&path)
                .map_err(HarnessError::from)
                .and_then(|content| {
                    serde_json::from_str::<SessionRecord>(&content).map_err(|e| {
                        HarnessError::Parse(format!(
                            "could not parse session {}: {e}",
                            path.display()
                        ))
                    })
                }) {
                Ok(record) => metas.push(record.meta),
                Err(e) => {
                    warn!("session store: skipping {}: {e}", path.display());
                }
            }
        }

        metas.sort_by(|a, b| {
            b.seq
                .cmp(&a.seq)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        metas
    }

    /// Give a number to every saved conversation that has none, oldest
    /// first, and write each one back. Returns how many were numbered.
    ///
    /// Numbering shipped after these files were written, so they carry
    /// `seq: 0` from `serde`'s default. This runs once at startup, before
    /// the first `list()`, so the Sessions tab never shows a 0. Oldest first
    /// by `created_at`, so the numbers agree with the order the
    /// conversations actually happened in.
    ///
    /// A record that will not load or will not save is skipped with a
    /// `warn!`: a numbering problem must not stop the app from opening.
    pub fn number_old_sessions(&self) -> usize {
        let mut metas = self.list();
        let mut next = metas.iter().map(|meta| meta.seq).max().unwrap_or(0) + 1;
        metas.retain(|meta| meta.seq == 0);
        metas.sort_by_key(|meta| meta.created_at);

        let mut numbered = 0;
        for meta in metas {
            let Ok(mut record) = self.load(&meta.id) else {
                warn!(
                    "session store: could not number {}: record will not load",
                    meta.id.as_str()
                );
                continue;
            };
            record.meta.seq = next;
            match self.save(&record) {
                Ok(()) => {
                    next += 1;
                    numbered += 1;
                }
                Err(e) => warn!("session store: could not number {}: {e}", meta.id.as_str()),
            }
        }
        if numbered > 0 {
            debug!("session store: numbered {numbered} older session(s)");
        }
        numbered
    }

    /// Remove a session's file. Deleting a session that is not there is
    /// not an error.
    pub fn delete(&self, id: &SessionId) -> Result<()> {
        let path = self.record_path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
