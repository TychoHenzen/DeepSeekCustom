//! Pre-dispatch validation for bounded localization agreement sampling.

use std::fmt;
use std::path::PathBuf;

use super::{
    OpenSpecInput, PatchPreviewInputError, PatchPreviewInputGate, PatchPreviewInputRequest,
    ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ValidatedContractInput,
};

/// The explicitly named baseline localization run and OpenSpec task for sampling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplingInputRequest {
    pub baseline_localization_run_id: ProcedureRunId,
    pub change_id: String,
    pub task_id: String,
}

/// Current, approved baseline input that bounded sampling may consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSamplingInput {
    pub report: ProcedureRun,
    pub contract: ValidatedContractInput,
}

/// Refusal before sampling, candidate, patch, verifier, or model work.
#[derive(Debug, PartialEq, Eq)]
pub enum SamplingInputError {
    MissingReport {
        run_id: String,
    },
    ReportLoad {
        run_id: String,
        reason: String,
    },
    ReviewDisposition {
        run_id: String,
        disposition: ProcedureReviewDisposition,
    },
    ChangeMismatch {
        run_id: String,
        requested: String,
        actual: String,
    },
    TaskMismatch {
        run_id: String,
        requested: String,
        actual: String,
    },
    CurrentOpenSpec {
        run_id: String,
        reason: String,
    },
    Stale {
        run_id: String,
        paths: Vec<String>,
    },
}

impl fmt::Display for SamplingInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingReport { run_id } => write!(
                formatter,
                "sampling rejected for localization run {run_id}: report is missing"
            ),
            Self::ReportLoad { run_id, reason } => write!(
                formatter,
                "sampling rejected for localization run {run_id}: could not load report: {reason}"
            ),
            Self::ReviewDisposition {
                run_id,
                disposition,
            } => write!(
                formatter,
                "sampling rejected for localization run {run_id}: review disposition is {disposition}"
            ),
            Self::ChangeMismatch {
                run_id,
                requested,
                actual,
            } => write!(
                formatter,
                "sampling rejected for localization run {run_id}: change mismatch; requested `{requested}`, report belongs to `{actual}`"
            ),
            Self::TaskMismatch {
                run_id,
                requested,
                actual,
            } => write!(
                formatter,
                "sampling rejected for localization run {run_id}: task mismatch; requested `{requested}`, report belongs to `{actual}`"
            ),
            Self::CurrentOpenSpec { run_id, reason } => write!(
                formatter,
                "sampling rejected for localization run {run_id}: could not load current OpenSpec input: {reason}"
            ),
            Self::Stale { run_id, paths } => {
                write!(
                    formatter,
                    "sampling rejected for localization run {run_id}: localization input is stale:"
                )?;
                for path in paths {
                    write!(formatter, "\n- {path}")?;
                }
                write!(formatter, "\nrun localization again before sampling")
            }
        }
    }
}

impl std::error::Error for SamplingInputError {}

/// Reuses the named approved-report guard before agreement sampling starts.
pub struct SamplingInputGate {
    baseline: PatchPreviewInputGate,
}

impl SamplingInputGate {
    pub fn new(input: OpenSpecInput, project_root: PathBuf, reports: ProcedureReportStore) -> Self {
        Self {
            baseline: PatchPreviewInputGate::new(input, project_root, reports),
        }
    }

    /// Return only the current, approved report named by the completed-procedure request.
    /// No downstream sampling seam is reachable until this function succeeds.
    pub fn load(
        &self,
        request: &SamplingInputRequest,
    ) -> Result<ValidatedSamplingInput, SamplingInputError> {
        let validated = self
            .baseline
            .load(&PatchPreviewInputRequest {
                localization_run_id: request.baseline_localization_run_id,
                change_id: request.change_id.clone(),
                task_id: request.task_id.clone(),
                route_override: super::RouteOverride::Automatic,
            })
            .map_err(SamplingInputError::from)?;
        Ok(ValidatedSamplingInput {
            report: validated.report,
            contract: validated.contract,
        })
    }
}

impl From<PatchPreviewInputError> for SamplingInputError {
    fn from(error: PatchPreviewInputError) -> Self {
        match error {
            PatchPreviewInputError::MissingReport { run_id } => Self::MissingReport { run_id },
            PatchPreviewInputError::ReportLoad { run_id, reason } => {
                Self::ReportLoad { run_id, reason }
            }
            PatchPreviewInputError::ReviewDisposition {
                run_id,
                disposition,
            } => Self::ReviewDisposition {
                run_id,
                disposition,
            },
            PatchPreviewInputError::ChangeMismatch {
                run_id,
                requested,
                actual,
            } => Self::ChangeMismatch {
                run_id,
                requested,
                actual,
            },
            PatchPreviewInputError::TaskMismatch {
                run_id,
                requested,
                actual,
            } => Self::TaskMismatch {
                run_id,
                requested,
                actual,
            },
            PatchPreviewInputError::CurrentOpenSpec { run_id, reason } => {
                Self::CurrentOpenSpec { run_id, reason }
            }
            PatchPreviewInputError::Stale { run_id, paths } => Self::Stale { run_id, paths },
        }
    }
}
