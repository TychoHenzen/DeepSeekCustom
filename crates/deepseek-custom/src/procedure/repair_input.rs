//! Preflight validation for one explicitly named bounded repair request.

use std::fmt;
use std::path::PathBuf;

use super::{
    ApplyRequest, OpenSpecInput, PatchPreview, PatchPreviewId,
    ProcedureReportRepository as ReportRepository, ProcedureRunId, PromotionBaseline,
    StoredProcedureReport, VerificationInputError, VerificationInputGate,
};

/// Explicit persisted inputs required before a repair ladder may be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairRequest {
    pub localization_run_id: ProcedureRunId,
    pub preview_id: PatchPreviewId,
    pub change_id: String,
    pub task_id: String,
}

/// The trusted localization report and patch state used to seed repair attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRepairInput {
    pub report: StoredProcedureReport,
    pub contract: super::SelectedContractSlice,
    pub preview: PatchPreview,
    pub promotion_baseline: PromotionBaseline,
}

/// Refusal before repair attempt state or any downstream repair action exists.
#[derive(Debug, PartialEq, Eq)]
pub enum RepairInputError {
    Verification(VerificationInputError),
}

impl fmt::Display for RepairInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Verification(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RepairInputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Verification(error) => Some(error),
        }
    }
}

impl From<VerificationInputError> for RepairInputError {
    fn from(error: VerificationInputError) -> Self {
        Self::Verification(error)
    }
}

/// Loads all trusted repair inputs before attempt construction or dispatch.
pub struct RepairInputGate {
    verification: VerificationInputGate,
}

impl RepairInputGate {
    pub fn new(input: OpenSpecInput, project_root: PathBuf, reports: ReportRepository) -> Self {
        Self {
            verification: VerificationInputGate::new(input, project_root, reports),
        }
    }

    /// Return only the named, approved, current report and its matching patch state.
    pub fn load(&self, request: &RepairRequest) -> Result<ValidatedRepairInput, RepairInputError> {
        let validated = self.verification.load(&ApplyRequest {
            localization_run_id: request.localization_run_id,
            preview_id: request.preview_id,
            change_id: request.change_id.clone(),
            task_id: request.task_id.clone(),
        })?;

        Ok(ValidatedRepairInput {
            report: validated.report,
            contract: validated.contract,
            preview: validated.preview,
            promotion_baseline: validated.promotion_baseline,
        })
    }
}
