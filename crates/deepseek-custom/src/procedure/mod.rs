//! Typed state and services for the staged coding procedure.
//!
//! Procedure runs are separate from conversational history and the search
//! subsystem. Each run carries the complete typed state needed to rebuild a
//! short prompt for its current stage.

mod dispatch;
mod fingerprint;
mod index;
mod input;
mod preview_input;
mod prompt;
pub mod report;
mod run;
mod runner;
mod schema;
mod validation;

pub use dispatch::{LocalizationDispatch, LocalizationDispatchError, LocalizationDispatcher};
pub use fingerprint::{
    ProcedureFingerprintError, ProcedureInputFingerprints, ProcedurePathFingerprint,
    ProcedurePathState, capture_path_fingerprint, capture_path_fingerprints, sha256_json,
};
pub use index::{RepositoryIndexError, build_repository_index};
pub use input::{
    CapabilityDeltaSlice, ContractSelection, OpenSpecChange, OpenSpecCommandFailure, OpenSpecInput,
    OpenSpecInputError, OpenSpecValidation, ProposalScope, RequirementSlice, ScenarioSlice,
    SelectedContractSlice, ValidatedContractInput,
};
pub use preview_input::{
    PatchPreviewInputError, PatchPreviewInputGate, PatchPreviewInputRequest,
    ValidatedPatchPreviewInput,
};
pub use prompt::{
    LocalizationPromptInput, RepositoryIndexEntry, TARGET_SELECTION_INSTRUCTION,
    build_localization_prompt,
};
pub use report::{
    ProcedureApprovedReportError, ProcedureReportStore, ProcedureReviewError,
    StoredProcedureReport, require_approved_report,
};

pub use run::{
    LocalizationAttempt, LocalizationEnvelope, LocalizationTarget, ProcedureAttemptDisposition,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition,
};
pub use runner::{
    ProcedureCommand, ProcedureProgress, ProcedureReviewDecision, ProcedureRunRequest,
    ProcedureRunner, ProcedureRunnerError, apply_review_decision,
};
pub use schema::localization_response_format;
pub use validation::{
    LocalizationTargetRejection, LocalizationTargetValidationError, validate_localization_targets,
};
