use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use tracing::warn;

use super::{WorkflowRun, WorkflowRunId, WorkflowRunRecord, WorkflowStateError};
use crate::error::HarnessError;

const WORKFLOW_SUBDIR: &str = ".deepseek/workflows";
const REGISTRY_FILE: &str = "registry.json";

#[derive(Debug, Clone)]
pub struct WorkflowStore {
    workflow_dir: PathBuf,
}

pub(crate) struct WorkflowLock {
    file: Option<File>,
    path: PathBuf,
}

impl Drop for WorkflowLock {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

impl WorkflowStore {
    pub fn for_project(project_root: &Path) -> Self {
        Self {
            workflow_dir: project_root.join(WORKFLOW_SUBDIR),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.workflow_dir
    }

    fn record_path(&self, id: &WorkflowRunId) -> PathBuf {
        self.workflow_dir.join(format!("{}.json", id.as_str()))
    }

    fn registry_path(&self) -> PathBuf {
        self.workflow_dir.join(REGISTRY_FILE)
    }

    pub(crate) fn lock_run(&self, id: &WorkflowRunId) -> Result<WorkflowLock, HarnessError> {
        std::fs::create_dir_all(&self.workflow_dir)?;
        let path = self.workflow_dir.join(format!("{}.lock", id.as_str()));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        acquire_file_lock(&file).map_err(|error| {
            if is_lock_contended(&error) {
                HarnessError::Tool(format!("workflow run {id} is already claimed"))
            } else {
                HarnessError::Io(error)
            }
        })?;
        Ok(WorkflowLock {
            file: Some(file),
            path,
        })
    }

    pub(crate) fn lock_registry(&self) -> Result<WorkflowLock, HarnessError> {
        std::fs::create_dir_all(&self.workflow_dir)?;
        let path = self.workflow_dir.join("registry.lock");
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        acquire_file_lock(&file).map_err(|error| {
            if is_lock_contended(&error) {
                HarnessError::Tool("workflow registry is already claimed".to_string())
            } else {
                HarnessError::Io(error)
            }
        })?;
        Ok(WorkflowLock {
            file: Some(file),
            path,
        })
    }

    pub fn save(&self, run: &WorkflowRun) -> Result<(), HarnessError> {
        std::fs::create_dir_all(&self.workflow_dir)?;
        let target = self.record_path(&run.id());
        let temporary = self
            .workflow_dir
            .join(format!("{}.json.tmp", run.id().as_str()));
        let json = serde_json::to_string_pretty(run.record()).map_err(|error| {
            HarnessError::Parse(format!("could not serialize workflow: {error}"))
        })?;
        std::fs::write(&temporary, json)?;
        std::fs::rename(&temporary, &target)?;
        Ok(())
    }

    pub fn load(&self, id: &WorkflowRunId) -> Result<WorkflowRun, HarnessError> {
        let path = self.record_path(id);
        let content = std::fs::read_to_string(&path)?;
        let record: WorkflowRunRecord = serde_json::from_str(&content).map_err(|error| {
            HarnessError::Parse(format!(
                "could not parse workflow {}: {error}",
                path.display()
            ))
        })?;
        WorkflowRun::from_record(record).map_err(|error| HarnessError::Parse(error.to_string()))
    }

    pub fn load_after_restart(&self, id: &WorkflowRunId) -> Result<WorkflowRun, HarnessError> {
        let mut run = self.load(id)?;
        run.normalize_after_restart();
        Ok(run)
    }

    pub fn recover_inflight(&self) -> Result<usize, HarnessError> {
        let _lock = self.lock_registry()?;
        let mut recovered = 0;
        for record in self.list() {
            if matches!(
                record.state,
                super::WorkflowRunState::Claimed | super::WorkflowRunState::Running
            ) {
                let mut run = WorkflowRun::from_record(record)
                    .map_err(|error| HarnessError::Parse(error.to_string()))?;
                run.normalize_after_restart();
                self.save(&run)?;
                recovered += 1;
            }
        }
        self.prune_terminal(128)?;
        Ok(recovered)
    }

    pub(crate) fn prune_terminal(&self, keep: usize) -> Result<(), HarnessError> {
        let protected = self.load_registry()?.selected_run;
        let mut terminal = self
            .list()
            .into_iter()
            .filter(|record| record.state.is_terminal() && Some(record.id) != protected)
            .collect::<Vec<_>>();
        if terminal.len() <= keep {
            return Ok(());
        }
        terminal.sort_by_key(|record| record.updated_at);
        let remove_count = terminal.len() - keep;
        for record in terminal.into_iter().take(remove_count) {
            self.delete(&record.id)?;
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<WorkflowRunRecord> {
        let entries = match std::fs::read_dir(&self.workflow_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                warn!(path = %self.workflow_dir.display(), "workflow store: could not read directory: {error}");
                return Vec::new();
            }
        };

        let mut records = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension().and_then(|extension| extension.to_str()) == Some("json")
                    && path.file_name().and_then(|name| name.to_str()) != Some(REGISTRY_FILE))
                    .then(|| std::fs::read_to_string(&path).ok())
                    .flatten()
                    .and_then(|content| match serde_json::from_str::<WorkflowRunRecord>(&content) {
                        Ok(record) => Some(record),
                        Err(error) => {
                            warn!(path = %path.display(), "workflow store: skipping invalid record: {error}");
                            None
                        }
                    })
            })
            .collect::<Vec<_>>();
        records.sort_by_key(|record| record.created_at);
        records
    }

    pub fn delete(&self, id: &WorkflowRunId) -> Result<(), HarnessError> {
        match std::fs::remove_file(self.record_path(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn load_registry(&self) -> Result<WorkflowRegistryRecord, HarnessError> {
        let path = self.registry_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).map_err(|error| {
                HarnessError::Parse(format!("could not parse workflow registry: {error}"))
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(WorkflowRegistryRecord::default())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn save_registry(
        &self,
        registry: &WorkflowRegistryRecord,
    ) -> Result<(), HarnessError> {
        std::fs::create_dir_all(&self.workflow_dir)?;
        let temporary = self.workflow_dir.join("registry.json.tmp");
        let json = serde_json::to_string_pretty(registry).map_err(|error| {
            HarnessError::Parse(format!("could not serialize workflow registry: {error}"))
        })?;
        std::fs::write(&temporary, json)?;
        std::fs::rename(&temporary, self.registry_path())?;
        Ok(())
    }

    pub fn validate_record(record: WorkflowRunRecord) -> Result<WorkflowRun, HarnessError> {
        WorkflowRun::from_record(record)
            .map_err(|error: WorkflowStateError| HarnessError::Parse(error.to_string()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkflowRegistryRecord {
    pub selected_run: Option<WorkflowRunId>,
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
