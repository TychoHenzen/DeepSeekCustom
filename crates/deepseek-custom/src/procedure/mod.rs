//! Typed state and services for the staged coding procedure.
//!
//! Procedure runs are separate from conversational history and the search
//! subsystem. Each run carries the complete typed state needed to rebuild a
//! short prompt for its current stage.

mod apply;
mod dispatch;
mod disposable_workspace;
mod fingerprint;
mod frontier_patch_draft;
mod frontier_patch_output;
mod index;
mod input;
mod local_patch_draft;
mod patch_apply_check;
mod patch_boundary;
mod patch_envelope;
mod patch_preview;
mod preview_input;
mod promotion;
mod prompt;
pub mod report;
mod route;
mod run;
mod runner;
mod schema;
mod validation;
mod verification_input;
mod verifier;

pub use apply::{ProcedureApplyError, ProcedureApplyRunner};
pub use dispatch::{LocalizationDispatch, LocalizationDispatchError, LocalizationDispatcher};
pub use disposable_workspace::{
    DEFAULT_DISPOSABLE_WORKSPACE_MAX_BYTES, DisposableDraftWorkspace, DisposableWorkspaceError,
    DisposableWorkspaceOptions, RetainedRecoveryWorkspace, SnapshotProgress,
};
pub use fingerprint::{
    ProcedureFingerprintError, ProcedureInputFingerprints, ProcedurePathFingerprint,
    ProcedurePathState, capture_path_fingerprint, capture_path_fingerprints, sha256_json,
};
pub use frontier_patch_draft::{
    FrontierPatchDraftError, FrontierPatchDraftRequest, draft_frontier_patch,
};
pub use frontier_patch_output::decode_frontier_patch_output;
pub use index::{RepositoryIndexError, build_repository_index};
pub use input::{
    CapabilityDeltaSlice, ContractSelection, OpenSpecChange, OpenSpecCommandFailure, OpenSpecInput,
    OpenSpecInputError, OpenSpecValidation, ProposalScope, RequirementSlice, ScenarioSlice,
    SelectedContractSlice, ValidatedContractInput,
};
pub use local_patch_draft::{
    LocalPatchDraftDispatch, LocalPatchDraftDispatcher, LocalPatchDraftError,
};
pub use patch_apply_check::{
    AppliedPatchWorkspace, ApplyCheckedPatch, GitApplyDisposition, GitApplyPhase, GitApplyResult,
    PatchApplyCheckError, PatchApplyProgress, PatchGateDisposition, PatchGateEvidence,
    apply_patch_in_workspace, apply_patch_in_workspace_with_progress, check_patch_applicability,
};
pub use patch_boundary::{BoundaryValidatedPatch, PatchBoundaryError, validate_patch_boundary};
pub use patch_envelope::{
    PatchCandidate, PatchEnvelope, PatchEnvelopeError, PatchRouteMetadata, decode_patch_envelope,
    patch_envelope_response_format,
};
pub use patch_preview::{
    PatchPreview, PatchPreviewError, PatchPreviewId, PatchPreviewRequest, PatchPreviewRunner,
    PatchPreviewStore,
};
pub use preview_input::{
    PatchPreviewInputError, PatchPreviewInputGate, PatchPreviewInputRequest,
    ValidatedPatchPreviewInput,
};
pub use promotion::{
    PromotionBaseline, PromotionBaselineCheckError, PromotionBaselineComparison,
    PromotionCleanupEvidence, PromotionError, PromotionRecoveryEvidence, PromotionResult,
    PromotionTarget, PromotionTargetError, PromotionTargetKind, StalePromotionPath,
    model_promotion_targets, promote_verified_workspace,
};
#[cfg(feature = "test-support")]
pub use promotion::{PromotionFailureInjection, promote_verified_workspace_with_failure_injection};
pub use prompt::{
    LocalizationPromptInput, RepositoryIndexEntry, TARGET_SELECTION_INSTRUCTION,
    build_localization_prompt,
};
pub use report::{
    ProcedureApprovedReportError, ProcedureReportStore, ProcedureReviewError,
    StoredProcedureReport, require_approved_report,
};
pub use route::{
    DifficultyAssessment, MechanicalVerb, RouteDecision, RouteOverride, RouteSignal, RouteTier,
    apply_route_override, assess_route,
};

pub use run::{
    LocalizationAttempt, LocalizationEnvelope, LocalizationTarget, ProcedureAttemptDisposition,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition,
};
pub use runner::{
    ProcedureApplyProgress, ProcedureCommand, ProcedureProgress, ProcedureReviewDecision,
    ProcedureRunRequest, ProcedureRunner, ProcedureRunnerError, apply_review_decision,
};
pub use schema::localization_response_format;
pub use validation::{
    LocalizationTargetRejection, LocalizationTargetValidationError, validate_localization_targets,
};
pub use verification_input::{
    ApplyRequest, ValidatedApplyInput, VerificationInputError, VerificationInputGate,
};
pub use verifier::{
    BoundedVerifierOutput, CandidateEligibility, CandidateIneligibility,
    VERIFIER_OUTPUT_EDGE_BYTES, VerifierCommandDisposition, VerifierCommandEvidence,
    VerifierCommandResult, VerifierCommandRunner, VerifierGateDisposition, VerifierGateEvidence,
    VerifierGateResult, VerifierReport, VerifierRun, VerifierRunProgress,
    evaluate_applied_patch_eligibility, evaluate_candidate_eligibility, run_verifier_commands,
    run_verifier_commands_with_interrupt,
};
