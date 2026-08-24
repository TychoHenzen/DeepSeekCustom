//! JSON storage for inspectable procedure run reports.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use thiserror::Error;
use tracing::debug;

use super::{
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureTerminalDisposition,
};
use crate::error::{HarnessError, Result};

const PROCEDURE_RUNS_SUBDIR: &str = ".deepseek/procedure-runs";
static REVIEW_DECISION_LOCK: Mutex<()> = Mutex::new(());

/// Stores one complete `ProcedureRun` per JSON file.
pub struct ProcedureReportStore {
    reports_dir: PathBuf,
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
        Self { reports_dir }
    }

    /// Create a store at `<project_root>/.deepseek/procedure-runs/`.
    pub fn for_project(project_root: &Path) -> Self {
        Self::new(project_root.join(PROCEDURE_RUNS_SUBDIR))
    }

    /// Path used by one run report.
    pub fn report_path(&self, id: &ProcedureRunId) -> PathBuf {
        self.reports_dir.join(format!("{}.json", id.as_str()))
    }

    /// Save a complete run report, creating the report directory as needed.
    pub fn save(&self, report: &ProcedureRun) -> Result<()> {
        std::fs::create_dir_all(&self.reports_dir)?;

        let target = self.report_path(&report.id);
        let temporary = self
            .reports_dir
            .join(format!("{}.json.tmp", report.id.as_str()));
        let json = serde_json::to_string_pretty(report).map_err(|error| {
            HarnessError::Parse(format!("could not serialize procedure report: {error}"))
        })?;

        std::fs::write(&temporary, &json)?;
        replace_file(&temporary, &target)?;
        debug!(
            run_id = report.id.as_str(),
            bytes = json.len(),
            path = %target.display(),
            "procedure report store: wrote report"
        );
        Ok(())
    }

    /// Load one run report.
    ///
    /// A missing report returns an IO error with `ErrorKind::NotFound`. A
    /// present report with invalid JSON returns a parse error naming its path.
    pub fn load(&self, id: &ProcedureRunId) -> Result<ProcedureRun> {
        let path = self.report_path(id);
        let json = std::fs::read_to_string(&path)?;
        serde_json::from_str(&json).map_err(|error| {
            HarnessError::Parse(format!(
                "could not parse procedure report {}: {error}",
                path.display()
            ))
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
        let mut report = self
            .load(id)
            .map_err(|source| ProcedureReviewError::ReportLoad {
                run_id: run_id.clone(),
                source,
            })?;

        if report.terminal_disposition != Some(ProcedureTerminalDisposition::AwaitingReview) {
            return Err(ProcedureReviewError::NotAwaitingReview {
                run_id,
                requested,
                actual: terminal_disposition_name(report.terminal_disposition.as_ref()),
            });
        }
        if report.review_disposition == requested {
            return Ok(report);
        }
        if report.review_disposition != ProcedureReviewDisposition::Pending {
            return Err(ProcedureReviewError::DecisionConflict {
                run_id,
                requested,
                actual: report.review_disposition,
            });
        }

        report.review_disposition = requested;
        self.save(&report)
            .map_err(|source| ProcedureReviewError::ReportSave {
                run_id: report.id.as_str(),
                source,
            })?;
        Ok(report)
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
