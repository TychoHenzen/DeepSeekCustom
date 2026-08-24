//! JSON storage for inspectable procedure run reports.

use std::path::{Path, PathBuf};

use tracing::debug;

use super::{ProcedureRun, ProcedureRunId};
use crate::error::{HarnessError, Result};

const PROCEDURE_RUNS_SUBDIR: &str = ".deepseek/procedure-runs";

/// Stores one complete `ProcedureRun` per JSON file.
pub struct ProcedureReportStore {
    reports_dir: PathBuf,
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
        std::fs::rename(&temporary, &target)?;
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
}
