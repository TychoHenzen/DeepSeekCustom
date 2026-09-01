//! Persistence, review, fingerprint, and metrics operations for procedure reports.

use super::{
    LocalizationTraceExport, LocalizationTraceExportRecord, ProcedureAttemptDisposition,
    ProcedureInputFingerprints, ProcedureMetricsSummary, ProcedureReviewDisposition,
    ProcedureReviewError, ProcedureRun, ProcedureRunId, ProcedureRunMetrics,
    ProcedureTerminalDisposition, RepairLadderEvent, VerifierReport, capture_path_fingerprints,
};
use crate::error::{HarnessError, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::debug;

const PROCEDURE_RUNS_SUBDIR: &str = ".deepseek/procedure-runs";
static REVIEW_DECISION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone)]
pub struct ProcedureReportRepository {
    reports_dir: PathBuf,
    project_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProcedureReport {
    pub run: ProcedureRun,
    pub input_fingerprints: ProcedureInputFingerprints,
    pub verification: Option<VerifierReport>,
    pub repair_events: Vec<RepairLadderEvent>,
    pub metrics: Option<ProcedureRunMetrics>,
}

impl ProcedureReportRepository {
    pub fn new(reports_dir: PathBuf) -> Self {
        Self {
            reports_dir,
            project_root: None,
        }
    }
    pub fn for_project(project_root: &Path) -> Self {
        Self {
            reports_dir: project_root.join(PROCEDURE_RUNS_SUBDIR),
            project_root: Some(project_root.to_path_buf()),
        }
    }
    pub fn report_path(&self, id: &ProcedureRunId) -> PathBuf {
        self.reports_dir.join(format!("{}.json", id.as_str()))
    }
    pub fn save(&self, report: &ProcedureRun) -> Result<()> {
        self.save_with_optional_metrics(report, ProcedureRunMetrics::from_terminal_run(report))
    }
    pub fn save_with_metrics(
        &self,
        report: &ProcedureRun,
        metrics: ProcedureRunMetrics,
    ) -> Result<()> {
        self.save_with_optional_metrics(report, Some(metrics))
    }

    fn save_with_optional_metrics(
        &self,
        report: &ProcedureRun,
        metrics: Option<ProcedureRunMetrics>,
    ) -> Result<()> {
        self.save_document(&StoredProcedureReport {
            run: report.clone(),
            input_fingerprints: self.capture_input_fingerprints(report)?,
            verification: None,
            repair_events: Vec::new(),
            metrics,
        })
    }

    pub fn load(&self, id: &ProcedureRunId) -> Result<ProcedureRun> {
        Ok(self.load_with_fingerprints(id)?.run)
    }
    pub fn load_with_fingerprints(&self, id: &ProcedureRunId) -> Result<StoredProcedureReport> {
        load_document(&self.report_path(id))
    }
    pub fn save_verification(
        &self,
        id: &ProcedureRunId,
        verification: &VerifierReport,
    ) -> Result<()> {
        let mut stored = self.load_with_fingerprints(id)?;
        stored.verification = Some(verification.clone());
        self.save_document(&stored)
    }
    pub fn save_repair_events(
        &self,
        id: &ProcedureRunId,
        events: &[RepairLadderEvent],
    ) -> Result<()> {
        let mut stored = self.load_with_fingerprints(id)?;
        stored.repair_events = events.to_vec();
        self.save_document(&stored)
    }
    pub fn replace_metrics(
        &self,
        id: &ProcedureRunId,
        metrics: &ProcedureRunMetrics,
    ) -> Result<()> {
        let mut stored = self.load_with_fingerprints(id)?;
        stored.metrics = Some(metrics.clone());
        self.save_document(&stored)
    }

    pub fn recent_metrics(&self, window_runs: usize) -> Result<ProcedureMetricsSummary> {
        let entries = match std::fs::read_dir(&self.reports_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ProcedureMetricsSummary::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut metrics = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let report = load_document(&path)?;
            if let Some(metric) = report.metrics {
                metrics.push((metric.completed_at_unix_ms, report.run.id.as_str(), metric));
            }
        }
        metrics.sort_unstable_by(|left, right| {
            right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1))
        });
        let mut summary = ProcedureMetricsSummary::default();
        for (_, _, metric) in metrics.into_iter().take(window_runs) {
            summary.record(&metric);
        }
        Ok(summary)
    }

    pub fn export_localization_traces(&self) -> Result<LocalizationTraceExport> {
        let entries = match std::fs::read_dir(&self.reports_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LocalizationTraceExport::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut reports = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            reports.push(load_document(&path)?);
        }
        reports.sort_unstable_by_key(|report| report.run.id.as_str());
        Ok(LocalizationTraceExport {
            records: reports
                .iter()
                .map(LocalizationTraceExportRecord::from)
                .collect(),
        })
    }

    pub fn reject(
        &self,
        id: &ProcedureRunId,
    ) -> std::result::Result<ProcedureRun, ProcedureReviewError> {
        self.record_review(id, ProcedureReviewDisposition::Rejected)
    }
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
        let _guard = REVIEW_DECISION_LOCK
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
                actual: super::report::terminal_disposition_name(
                    report.run.terminal_disposition.as_ref(),
                ),
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
        let Some(root) = &self.project_root else {
            return Ok(ProcedureInputFingerprints::default());
        };
        if !report
            .spec_fingerprint
            .as_deref()
            .is_some_and(|value| value.starts_with("sha256:"))
        {
            return Ok(ProcedureInputFingerprints::default());
        }
        let openspec = capture_path_fingerprints(
            root,
            super::report_repository_io::openspec_artifact_paths(root, report),
        )
        .map_err(|error| HarnessError::Parse(error.to_string()))?;
        let paths = report
            .attempts
            .iter()
            .rev()
            .find(|attempt| attempt.disposition == ProcedureAttemptDisposition::Accepted)
            .map(|attempt| {
                attempt
                    .targets
                    .iter()
                    .map(|target| target.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let targets = capture_path_fingerprints(root, paths)
            .map_err(|error| HarnessError::Parse(error.to_string()))?;
        Ok(ProcedureInputFingerprints { openspec, targets })
    }

    fn save_document(&self, report: &StoredProcedureReport) -> Result<()> {
        std::fs::create_dir_all(&self.reports_dir)?;
        let target = self.report_path(&report.run.id);
        let temporary = self
            .reports_dir
            .join(format!("{}.json.tmp", report.run.id.as_str()));
        let json = super::report_encoding::encode_report(report)?;
        std::fs::write(&temporary, &json)?;
        super::report_repository_io::replace_file(&temporary, &target)?;
        debug!(run_id = report.run.id.as_str(), bytes = json.len(), path = %target.display(), "procedure report repository: wrote report");
        Ok(())
    }
}

fn load_document(path: &Path) -> Result<StoredProcedureReport> {
    super::report_encoding::decode_report(&std::fs::read_to_string(path)?, path)
}
