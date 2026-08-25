//! JSON storage for inspectable procedure run reports.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use super::{
    ProcedureInputFingerprints, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureTerminalDisposition, capture_path_fingerprints,
};
use crate::error::{HarnessError, Result};

const PROCEDURE_RUNS_SUBDIR: &str = ".deepseek/procedure-runs";
static REVIEW_DECISION_LOCK: Mutex<()> = Mutex::new(());

/// Stores one complete `ProcedureRun` per JSON file.
#[derive(Debug, Clone)]
pub struct ProcedureReportStore {
    reports_dir: PathBuf,
    project_root: Option<PathBuf>,
}

/// One persisted run plus the immutable inputs captured at localization time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProcedureReport {
    pub run: ProcedureRun,
    pub input_fingerprints: ProcedureInputFingerprints,
}

#[derive(Serialize, Deserialize)]
struct ProcedureReportDocument {
    #[serde(flatten)]
    run: ProcedureRun,
    #[serde(default, skip_serializing_if = "ProcedureInputFingerprints::is_empty")]
    input_fingerprints: ProcedureInputFingerprints,
}

/// Failure to record a semantic review decision for one saved run.
#[derive(Debug, Error)]
pub enum ProcedureReviewError {
    #[error("could not load procedure run {run_id} for review: {source}")]
    ReportLoad {
        run_id: String,
        #[source]
        source: HarnessError,
    },
    #[error(
        "cannot record {requested} review for procedure run {run_id}: terminal disposition is {actual}; expected awaiting_review"
    )]
    NotAwaitingReview {
        run_id: String,
        requested: ProcedureReviewDisposition,
        actual: String,
    },
    #[error(
        "cannot record {requested} review for procedure run {run_id}: review disposition is already {actual}; first terminal review decision wins"
    )]
    DecisionConflict {
        run_id: String,
        requested: ProcedureReviewDisposition,
        actual: ProcedureReviewDisposition,
    },
    #[error("could not save reviewed procedure run {run_id}: {source}")]
    ReportSave {
        run_id: String,
        #[source]
        source: HarnessError,
    },
}

/// A localization report has not received the approval required downstream.
#[derive(Debug, Error, PartialEq, Eq)]
#[error(
    "procedure run {run_id} cannot enter a downstream procedure stage: review disposition is {disposition}"
)]
pub struct ProcedureApprovedReportError {
    pub run_id: String,
    pub disposition: ProcedureReviewDisposition,
}

/// Require the explicit approval that downstream procedure stages must consume.
pub fn require_approved_report(
    report: &ProcedureRun,
) -> std::result::Result<&ProcedureRun, ProcedureApprovedReportError> {
    if report.review_disposition == ProcedureReviewDisposition::Approved {
        return Ok(report);
    }

    Err(ProcedureApprovedReportError {
        run_id: report.id.as_str(),
        disposition: report.review_disposition,
    })
}

impl ProcedureReportStore {
    /// Create a store rooted directly at `reports_dir`.
    pub fn new(reports_dir: PathBuf) -> Self {
        Self {
            reports_dir,
            project_root: None,
        }
    }

    /// Create a store at `<project_root>/.deepseek/procedure-runs/`.
    pub fn for_project(project_root: &Path) -> Self {
        Self {
            reports_dir: project_root.join(PROCEDURE_RUNS_SUBDIR),
            project_root: Some(project_root.to_path_buf()),
        }
    }

    /// Path used by one run report.
    pub fn report_path(&self, id: &ProcedureRunId) -> PathBuf {
        self.reports_dir.join(format!("{}.json", id.as_str()))
    }

    /// Save a complete run report, creating the report directory as needed.
    pub fn save(&self, report: &ProcedureRun) -> Result<()> {
        let document = StoredProcedureReport {
            run: report.clone(),
            input_fingerprints: self.capture_input_fingerprints(report)?,
        };
        self.save_document(&document)
    }

    /// Load one run report.
    ///
    /// A missing report returns an IO error with `ErrorKind::NotFound`. A
    /// present report with invalid JSON returns a parse error naming its path.
    pub fn load(&self, id: &ProcedureRunId) -> Result<ProcedureRun> {
        Ok(self.load_with_fingerprints(id)?.run)
    }

    /// Load one run together with its captured preview-input fingerprints.
    ///
    /// Reports written before fingerprint capture deserialize with an empty
    /// fingerprint set. They remain inspectable but cannot pass the preview gate.
    pub fn load_with_fingerprints(&self, id: &ProcedureRunId) -> Result<StoredProcedureReport> {
        let path = self.report_path(id);
        let json = std::fs::read_to_string(&path)?;
        let document: ProcedureReportDocument = serde_json::from_str(&json).map_err(|error| {
            HarnessError::Parse(format!(
                "could not parse procedure report {}: {error}",
                path.display()
            ))
        })?;
        Ok(StoredProcedureReport {
            run: document.run,
            input_fingerprints: document.input_fingerprints,
        })
    }

    /// Reject one structurally valid report by its immutable run identifier.
    pub fn reject(
        &self,
        id: &ProcedureRunId,
    ) -> std::result::Result<ProcedureRun, ProcedureReviewError> {
        self.record_review(id, ProcedureReviewDisposition::Rejected)
    }

    /// Approve one structurally valid report by its immutable run identifier.
    pub fn approve(
        &self,
        id: &ProcedureRunId,
    ) -> std::result::Result<ProcedureRun, ProcedureReviewError> {
        self.record_review(id, ProcedureReviewDisposition::Approved)
    }

    fn record_review(
        &self,
        id: &ProcedureRunId,
        requested: ProcedureReviewDisposition,
    ) -> std::result::Result<ProcedureRun, ProcedureReviewError> {
        let _decision_guard = REVIEW_DECISION_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let run_id = id.as_str();
        let mut report =
            self.load_with_fingerprints(id)
                .map_err(|source| ProcedureReviewError::ReportLoad {
                    run_id: run_id.clone(),
                    source,
                })?;

        if report.run.terminal_disposition != Some(ProcedureTerminalDisposition::AwaitingReview) {
            return Err(ProcedureReviewError::NotAwaitingReview {
                run_id,
                requested,
                actual: terminal_disposition_name(report.run.terminal_disposition.as_ref()),
            });
        }
        if report.run.review_disposition == requested {
            return Ok(report.run);
        }
        if report.run.review_disposition != ProcedureReviewDisposition::Pending {
            return Err(ProcedureReviewError::DecisionConflict {
                run_id,
                requested,
                actual: report.run.review_disposition,
            });
        }

        report.run.review_disposition = requested;
        self.save_document(&report)
            .map_err(|source| ProcedureReviewError::ReportSave {
                run_id: report.run.id.as_str(),
                source,
            })?;
        Ok(report.run)
    }

    fn capture_input_fingerprints(
        &self,
        report: &ProcedureRun,
    ) -> Result<ProcedureInputFingerprints> {
        let Some(project_root) = &self.project_root else {
            return Ok(ProcedureInputFingerprints::default());
        };
        if !report
            .spec_fingerprint
            .as_deref()
            .is_some_and(|value| value.starts_with("sha256:"))
        {
            return Ok(ProcedureInputFingerprints::default());
        }

        let openspec =
            capture_path_fingerprints(project_root, openspec_artifact_paths(project_root, report))
                .map_err(|error| HarnessError::Parse(error.to_string()))?;
        let target_paths = report
            .attempts
            .iter()
            .rev()
            .find(|attempt| attempt.disposition == super::ProcedureAttemptDisposition::Accepted)
            .map(|attempt| {
                attempt
                    .targets
                    .iter()
                    .map(|target| target.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let targets = capture_path_fingerprints(project_root, target_paths)
            .map_err(|error| HarnessError::Parse(error.to_string()))?;
        Ok(ProcedureInputFingerprints { openspec, targets })
    }

    fn save_document(&self, report: &StoredProcedureReport) -> Result<()> {
        std::fs::create_dir_all(&self.reports_dir)?;
        let target = self.report_path(&report.run.id);
        let temporary = self
            .reports_dir
            .join(format!("{}.json.tmp", report.run.id.as_str()));
        let document = ProcedureReportDocument {
            run: report.run.clone(),
            input_fingerprints: report.input_fingerprints.clone(),
        };
        let json = serde_json::to_string_pretty(&document).map_err(|error| {
            HarnessError::Parse(format!("could not serialize procedure report: {error}"))
        })?;
        std::fs::write(&temporary, &json)?;
        replace_file(&temporary, &target)?;
        debug!(
            run_id = report.run.id.as_str(),
            bytes = json.len(),
            path = %target.display(),
            "procedure report store: wrote report"
        );
        Ok(())
    }
}

fn openspec_artifact_paths(project_root: &Path, report: &ProcedureRun) -> Vec<String> {
    let prefix = format!("openspec/changes/{}", report.change_id);
    let mut paths = vec![
        format!("{prefix}/proposal.md"),
        format!("{prefix}/tasks.md"),
    ];
    if let Some(binding) = report.selected_task.covers.as_deref() {
        if let Some(capability) = binding.split("::").next().map(str::trim)
            && !capability.is_empty()
        {
            paths.push(format!("{prefix}/specs/{capability}/spec.md"));
        }
    } else {
        collect_spec_paths(
            project_root,
            &project_root.join(&prefix).join("specs"),
            &mut paths,
        );
    }
    paths
}

fn collect_spec_paths(project_root: &Path, directory: &Path, paths: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries
        .filter_map(std::result::Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_spec_paths(project_root, &path, paths);
        } else if path.file_name().is_some_and(|name| name == "spec.md")
            && let Ok(relative) = path.strip_prefix(project_root)
        {
            paths.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn terminal_disposition_name(disposition: Option<&ProcedureTerminalDisposition>) -> String {
    match disposition {
        Some(ProcedureTerminalDisposition::Succeeded) => "succeeded".to_string(),
        Some(ProcedureTerminalDisposition::AwaitingReview) => "awaiting_review".to_string(),
        Some(ProcedureTerminalDisposition::Failed { .. }) => "failed".to_string(),
        Some(ProcedureTerminalDisposition::Interrupted) => "interrupted".to_string(),
        None => "none".to_string(),
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both pointers reference null-terminated UTF-16 buffers that
    // remain alive for the duration of this synchronous Windows API call.
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
