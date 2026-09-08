//! Atomic persistence for recovery-run records.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use super::state::{
    AttemptStatus, RecoveryRun, RecoveryRunId, RecoveryRunRecord, RecoveryStateError,
};
use crate::error::{HarnessError, Result};

const RECOVERY_SUBDIR: &str = ".deepseek/recovery";

/// Disk storage for recovery runs. The directory is separate from
/// conversation sessions so recovery state can be retained independently.
#[derive(Debug, Clone)]
pub struct RecoveryStore {
    recovery_dir: PathBuf,
}

pub(crate) struct RecoveryLock {
    _file: File,
}

impl RecoveryStore {
    pub fn new(recovery_dir: PathBuf) -> Self {
        Self { recovery_dir }
    }

    pub fn for_project(project_root: &Path) -> Self {
        Self::new(project_root.join(RECOVERY_SUBDIR))
    }

    fn record_path(&self, id: &RecoveryRunId) -> PathBuf {
        self.recovery_dir.join(format!("{}.json", id.as_str()))
    }

    pub(crate) fn lock_run(&self, id: &RecoveryRunId) -> Result<RecoveryLock> {
        std::fs::create_dir_all(&self.recovery_dir)?;
        let path = self.recovery_dir.join(format!("{}.lock", id.as_str()));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        acquire_file_lock(&file).map_err(|error| {
            if is_lock_contended(&error) {
                HarnessError::Tool(format!("recovery run {id} is already claimed"))
            } else {
                HarnessError::Io(error)
            }
        })?;
        Ok(RecoveryLock { _file: file })
    }

    pub fn save(&self, run: &RecoveryRun) -> Result<()> {
        std::fs::create_dir_all(&self.recovery_dir)?;
        let target = self.record_path(&run.id());
        let temporary = self
            .recovery_dir
            .join(format!("{}.json.tmp", run.id().as_str()));
        let json = serde_json::to_string_pretty(run.record()).map_err(|error| {
            HarnessError::Parse(format!("could not serialize recovery run: {error}"))
        })?;
        std::fs::write(&temporary, &json)?;
        std::fs::rename(&temporary, &target)?;
        debug!(
            recovery_run = %run.id(),
            bytes = json.len(),
            path = %target.display(),
            "recovery store: wrote run"
        );
        Ok(())
    }

    pub fn load(&self, id: &RecoveryRunId) -> Result<RecoveryRun> {
        let path = self.record_path(id);
        let content = std::fs::read_to_string(&path)?;
        let record: RecoveryRunRecord = serde_json::from_str(&content).map_err(|error| {
            HarnessError::Parse(format!(
                "could not parse recovery run {}: {error}",
                path.display()
            ))
        })?;
        let should_persist_recovery = record.status == super::state::RecoveryStatus::Diagnosing
            || record
                .attempted_actions
                .iter()
                .any(|action| action.status == AttemptStatus::Pending);
        let run = RecoveryRun::from_record(record).map_err(|error| {
            HarnessError::Parse(format!("invalid recovery run {}: {error}", path.display()))
        })?;
        if should_persist_recovery {
            self.save(&run)?;
        }
        Ok(run)
    }

    pub fn list(&self) -> Vec<RecoveryRunRecord> {
        let entries = match std::fs::read_dir(&self.recovery_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                warn!(
                    path = %self.recovery_dir.display(),
                    "recovery store: could not read directory: {error}"
                );
                return Vec::new();
            }
        };

        entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension().and_then(|extension| extension.to_str()) == Some("json"))
                    .then(|| std::fs::read_to_string(&path).ok())
                    .flatten()
                    .and_then(|content| match serde_json::from_str::<RecoveryRunRecord>(&content) {
                        Ok(record) => Some(record),
                        Err(error) => {
                            warn!(path = %path.display(), "recovery store: skipping invalid record: {error}");
                            None
                        }
                    })
            })
            .collect()
    }

    pub fn delete(&self, id: &RecoveryRunId) -> Result<()> {
        match std::fs::remove_file(self.record_path(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Validate a record loaded by another boundary before it is accepted.
    pub fn validate_record(record: RecoveryRunRecord) -> Result<RecoveryRun> {
        RecoveryRun::from_record(record).map_err(|error: RecoveryStateError| {
            HarnessError::Parse(format!("invalid recovery run: {error}"))
        })
    }
}

fn is_lock_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == Some(32)
        || error.raw_os_error() == Some(33)
}

fn acquire_file_lock(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
        };
        use windows::Win32::System::IO::OVERLAPPED;

        let mut overlapped = OVERLAPPED::default();
        unsafe {
            LockFileEx(
                HANDLE(file.as_raw_handle()),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
            .map_err(|error| io::Error::new(io::ErrorKind::WouldBlock, error))
        }
    }

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Ok(())
    }
}
