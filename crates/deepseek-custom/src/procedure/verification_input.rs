//! Preflight validation for the future isolated Apply path.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use super::{
    OpenSpecInput, PatchPreview, PatchPreviewId, PatchPreviewInputError, PatchPreviewInputGate,
    PatchPreviewInputRequest, PatchPreviewStore, ProcedureReportRepository as ReportRepository,
    ProcedureRunId, RouteOverride, StoredProcedureReport,
};
use crate::error::HarnessError;

/// Explicit inputs required before verification setup may begin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyRequest {
    pub localization_run_id: ProcedureRunId,
    pub preview_id: PatchPreviewId,
    pub change_id: String,
    pub task_id: String,
}

/// Approved, current localization input paired with its matching preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedApplyInput {
    pub report: StoredProcedureReport,
    pub contract: super::SelectedContractSlice,
    pub preview: PatchPreview,
    pub promotion_baseline: super::PromotionBaseline,
}

/// Refusal before snapshot creation, patch application, model work, or verifier work.
#[derive(Debug, PartialEq, Eq)]
pub enum VerificationInputError {
    Localization(PatchPreviewInputError),
    PreviewMissing {
        preview_id: String,
    },
    PreviewLoad {
        preview_id: String,
        reason: String,
    },
    PreviewRunMismatch {
        preview_id: String,
        requested: String,
        actual: String,
    },
    PreviewChangeMismatch {
        preview_id: String,
        requested: String,
        actual: String,
    },
    PreviewTaskMismatch {
        preview_id: String,
        requested: String,
        actual: String,
    },
    PreviewTargetsMismatch {
        preview_id: String,
        expected: Vec<String>,
        actual: Vec<String>,
    },
    PreviewBaselineMissing {
        preview_id: String,
    },
    PreviewBaselineTargetsMismatch {
        preview_id: String,
        expected: Vec<String>,
        actual: Vec<String>,
    },
    PreviewBaselineStale {
        preview_id: String,
        paths: Vec<String>,
    },
    PreviewBaselineRead {
        preview_id: String,
        reason: String,
    },
}

impl fmt::Display for VerificationInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Localization(error) => error.fmt(formatter),
            Self::PreviewMissing { preview_id } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: preview is missing"
            ),
            Self::PreviewLoad { preview_id, reason } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: could not load preview: {reason}"
            ),
            Self::PreviewRunMismatch {
                preview_id,
                requested,
                actual,
            } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: localization run mismatch; requested `{requested}`, preview belongs to `{actual}`"
            ),
            Self::PreviewChangeMismatch {
                preview_id,
                requested,
                actual,
            } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: change mismatch; requested `{requested}`, preview belongs to `{actual}`"
            ),
            Self::PreviewTaskMismatch {
                preview_id,
                requested,
                actual,
            } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: task mismatch; requested `{requested}`, preview belongs to `{actual}`"
            ),
            Self::PreviewTargetsMismatch {
                preview_id,
                expected,
                actual,
            } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: preview targets do not match the approved localization report; expected {expected:?}, actual {actual:?}"
            ),
            Self::PreviewBaselineMissing { preview_id } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: preview has no promotion baseline; generate a new preview before applying"
            ),
            Self::PreviewBaselineTargetsMismatch {
                preview_id,
                expected,
                actual,
            } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: promotion baseline endpoints do not match preview targets; expected {expected:?}, actual {actual:?}"
            ),
            Self::PreviewBaselineStale { preview_id, paths } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: promotion baseline is stale for paths: {paths:?}"
            ),
            Self::PreviewBaselineRead { preview_id, reason } => write!(
                formatter,
                "verification setup rejected for patch preview {preview_id}: could not validate promotion baseline: {reason}"
            ),
        }
    }
}

impl std::error::Error for VerificationInputError {}

/// Loads and validates all named Apply inputs without owning downstream side effects.
pub struct VerificationInputGate {
    localization: PatchPreviewInputGate,
    reports: ReportRepository,
    previews: PatchPreviewStore,
    project_root: PathBuf,
}

impl VerificationInputGate {
    pub fn new(input: OpenSpecInput, project_root: PathBuf, reports: ReportRepository) -> Self {
        Self {
            localization: PatchPreviewInputGate::new(input, project_root.clone(), reports.clone()),
            reports,
            previews: PatchPreviewStore::for_project(&project_root),
            project_root,
        }
    }

    /// Return only a current approved report paired with its matching preview.
    pub fn load(
        &self,
        request: &ApplyRequest,
    ) -> Result<ValidatedApplyInput, VerificationInputError> {
        let localization = self
            .localization
            .load(&PatchPreviewInputRequest {
                localization_run_id: request.localization_run_id,
                change_id: request.change_id.clone(),
                task_id: request.task_id.clone(),
                route_override: RouteOverride::Automatic,
            })
            .map_err(VerificationInputError::Localization)?;

        let (preview, promotion_baseline) = self.load_preview(request.preview_id)?;
        let preview_id = request.preview_id.as_str();
        let actual_run_id = preview.localization_run_id.as_str();
        if actual_run_id != request.localization_run_id.as_str() {
            return Err(VerificationInputError::PreviewRunMismatch {
                preview_id,
                requested: request.localization_run_id.as_str(),
                actual: actual_run_id,
            });
        }
        if preview.change_id != request.change_id {
            return Err(VerificationInputError::PreviewChangeMismatch {
                preview_id,
                requested: request.change_id.clone(),
                actual: preview.change_id,
            });
        }
        if preview.task_id != request.task_id {
            return Err(VerificationInputError::PreviewTaskMismatch {
                preview_id,
                requested: request.task_id.clone(),
                actual: preview.task_id,
            });
        }

        let expected_targets = accepted_target_paths(&localization.report);
        if preview.targets != expected_targets {
            return Err(VerificationInputError::PreviewTargetsMismatch {
                preview_id,
                expected: expected_targets,
                actual: preview.targets,
            });
        }

        let promotion_baseline =
            promotion_baseline.ok_or_else(|| VerificationInputError::PreviewBaselineMissing {
                preview_id: preview_id.clone(),
            })?;
        let expected_baseline_paths = preview.targets.iter().cloned().collect::<BTreeSet<_>>();
        let actual_baseline_paths = promotion_baseline
            .fingerprints()
            .iter()
            .map(|fingerprint| fingerprint.path.clone())
            .collect::<BTreeSet<_>>();
        if actual_baseline_paths != expected_baseline_paths {
            return Err(VerificationInputError::PreviewBaselineTargetsMismatch {
                preview_id,
                expected: expected_baseline_paths.into_iter().collect(),
                actual: actual_baseline_paths.into_iter().collect(),
            });
        }
        match promotion_baseline.ensure_current(&self.project_root) {
            Ok(_) => {}
            Err(super::PromotionBaselineCheckError::Stale { stale_paths }) => {
                return Err(VerificationInputError::PreviewBaselineStale {
                    preview_id,
                    paths: stale_paths.into_iter().map(|stale| stale.path).collect(),
                });
            }
            Err(super::PromotionBaselineCheckError::Fingerprint(error)) => {
                return Err(VerificationInputError::PreviewBaselineRead {
                    preview_id,
                    reason: error.to_string(),
                });
            }
        }

        let report = self
            .reports
            .load_with_fingerprints(&request.localization_run_id)
            .map_err(|error| {
                VerificationInputError::Localization(load_error(request.localization_run_id, error))
            })?;
        Ok(ValidatedApplyInput {
            report,
            contract: localization.contract.contract,
            preview,
            promotion_baseline,
        })
    }

    fn load_preview(
        &self,
        id: PatchPreviewId,
    ) -> Result<(PatchPreview, Option<super::PromotionBaseline>), VerificationInputError> {
        let preview_id = id.as_str();
        self.previews
            .load_with_baseline(id)
            .map_err(|error| match error {
                HarnessError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    VerificationInputError::PreviewMissing { preview_id }
                }
                error => VerificationInputError::PreviewLoad {
                    preview_id,
                    reason: error.to_string(),
                },
            })
    }
}

fn load_error(run_id: ProcedureRunId, error: HarnessError) -> PatchPreviewInputError {
    let run_id_string = run_id.as_str();
    match error {
        HarnessError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            PatchPreviewInputError::MissingReport {
                run_id: run_id_string,
            }
        }
        error => PatchPreviewInputError::ReportLoad {
            run_id: run_id_string,
            reason: error.to_string(),
        },
    }
}

fn accepted_target_paths(report: &super::ProcedureRun) -> Vec<String> {
    let mut paths = report
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.disposition == super::ProcedureAttemptDisposition::Accepted)
        .map(|attempt| {
            attempt
                .targets
                .iter()
                .map(|target| target.path.replace('\\', "/"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    paths.sort();
    paths.dedup();
    paths
}
