//! Disk layer for saved conversations.
//!
//! `SessionStore` reads and writes `SessionRecord` values as one JSON file
//! per session, named `<session-id>.json`, under a sessions directory.

use std::io;
use std::path::{Path, PathBuf};

use tracing::warn;

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
        std::fs::write(&tmp_path, json)?;
        std::fs::rename(&tmp_path, &target)?;

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
        serde_json::from_str(&content)
            .map_err(|e| HarnessError::Parse(format!("could not parse session {}: {e}", path.display())))
    }

    /// List the metadata for every session in the directory, sorted by
    /// `updated_at`, newest first.
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
                        HarnessError::Parse(format!("could not parse session {}: {e}", path.display()))
                    })
                }) {
                Ok(record) => metas.push(record.meta),
                Err(e) => {
                    warn!("session store: skipping {}: {e}", path.display());
                }
            }
        }

        metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        metas
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{Message, Role};
    use crate::gui::transcript::Transcript;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dsc-store-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_record(title: &str, updated_at: u64) -> SessionRecord {
        SessionRecord {
            meta: SessionMeta {
                id: SessionId::new(),
                title: title.into(),
                created_at: 1000,
                updated_at,
                backend: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                message_count: 1,
            },
            messages: vec![Message {
                role: Role::User,
                content: Some(title.into()),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            transcript: Transcript::new(),
            claude_session_id: None,
        }
    }

    #[test]
    fn save_then_load_returns_equal_record() {
        let dir = temp_dir("roundtrip");
        let store = SessionStore::new(dir.clone());
        let record = sample_record("hello", 100);

        store.save(&record).unwrap();
        let loaded = store.load(&record.meta.id).unwrap();

        assert_eq!(loaded.meta.id, record.meta.id);
        assert_eq!(loaded.meta.title, record.meta.title);
        assert_eq!(loaded.messages.len(), record.messages.len());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saving_same_id_twice_leaves_second_version_on_disk() {
        let dir = temp_dir("overwrite");
        let store = SessionStore::new(dir.clone());
        let mut record = sample_record("first version", 100);

        store.save(&record).unwrap();
        record.meta.title = "second version".into();
        record.meta.updated_at = 200;
        store.save(&record).unwrap();

        let loaded = store.load(&record.meta.id).unwrap();
        assert_eq!(loaded.meta.title, "second version");
        assert_eq!(loaded.meta.updated_at, 200);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_returns_sessions_sorted_newest_first() {
        let dir = temp_dir("list-sort");
        let store = SessionStore::new(dir.clone());

        let a = sample_record("older", 100);
        let b = sample_record("newest", 300);
        let c = sample_record("middle", 200);
        store.save(&a).unwrap();
        store.save(&b).unwrap();
        store.save(&c).unwrap();

        let metas = store.list();
        assert_eq!(metas.len(), 3);
        assert_eq!(metas[0].title, "newest");
        assert_eq!(metas[1].title, "middle");
        assert_eq!(metas[2].title, "older");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_skips_corrupt_file_and_returns_good_ones() {
        let dir = temp_dir("list-corrupt");
        let store = SessionStore::new(dir.clone());

        let a = sample_record("good one", 100);
        let b = sample_record("good two", 200);
        store.save(&a).unwrap();
        store.save(&b).unwrap();
        std::fs::write(dir.join("garbage.json"), "{ not valid json").unwrap();

        let metas = store.list();
        assert_eq!(metas.len(), 2);
        assert!(metas.iter().any(|m| m.title == "good one"));
        assert!(metas.iter().any(|m| m.title == "good two"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_on_missing_directory_returns_empty_vec() {
        let dir = temp_dir("list-missing");
        std::fs::remove_dir_all(&dir).ok();
        let store = SessionStore::new(dir.join("sessions"));

        assert!(store.list().is_empty());
    }

    #[test]
    fn load_of_never_saved_id_is_distinguishable_from_parse_failure() {
        let dir = temp_dir("load-missing");
        let store = SessionStore::new(dir.clone());
        let missing_id = SessionId::new();

        let missing_err = store.load(&missing_id).unwrap_err();
        assert!(matches!(
            missing_err,
            HarnessError::Io(e) if e.kind() == io::ErrorKind::NotFound
        ));

        std::fs::write(dir.join(format!("{}.json", missing_id.as_str())), "{ bad").unwrap();
        let parse_err = store.load(&missing_id).unwrap_err();
        assert!(matches!(parse_err, HarnessError::Parse(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_removes_file_and_second_delete_is_not_an_error() {
        let dir = temp_dir("delete");
        let store = SessionStore::new(dir.clone());
        let record = sample_record("to delete", 100);
        store.save(&record).unwrap();

        store.delete(&record.meta.id).unwrap();
        assert!(store.load(&record.meta.id).is_err());

        store.delete(&record.meta.id).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
