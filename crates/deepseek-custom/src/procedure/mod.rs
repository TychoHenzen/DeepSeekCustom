//! Typed state and services for the staged coding procedure.
//!
//! Procedure runs are separate from conversational history and the search
//! subsystem. Each run carries the complete typed state needed to rebuild a
//! short prompt for its current stage.

mod apply;
mod bounded_repair;
mod completed;
mod dispatch;
mod disposable_workspace;
mod failure_digest;
mod fingerprint;
mod frontier_patch_draft;
mod frontier_patch_output;
mod frontier_repair;
mod index;
mod input;
mod local_patch_candidates;
mod local_patch_draft;
mod local_repair;
mod patch_apply_check;
mod patch_boundary;
mod patch_envelope;
mod patch_preview;
mod preview_input;
mod promotion;
mod prompt;
mod repair_event;
mod repair_input;
mod repair_prompt;
mod repair_state;
pub mod report;
mod report_encoding;
mod report_repository;
mod report_repository_io;
mod route;
mod routing_metrics;
mod run;
mod run_coordinator;
mod runner;
mod sampling;
mod sampling_input;
mod schema;
mod structural_repair;
mod trace_export;
mod validation;
mod verification_input;
mod verifier;

pub use apply::{ProcedureApplyError, ProcedureApplyRunner};
pub use bounded_repair::{BoundedRepairCoordinator, BoundedRepairError, BoundedRepairRun};
pub use completed::{
    SampledProcedureError, SampledProcedureOutcome, SampledProcedureRequest,
    SampledProcedureRunner, SampledRepairContext, WholeChangeProcedureError,
    WholeChangeProcedureOutcome, WholeChangeProcedureRequest, WholeChangeProcedureRunner,
};
pub use dispatch::{LocalizationDispatch, LocalizationDispatchError, LocalizationDispatcher};
pub use disposable_workspace::{
    DEFAULT_DISPOSABLE_WORKSPACE_MAX_BYTES, DisposableDraftWorkspace, DisposableWorkspaceError,
    DisposableWorkspaceOptions, DisposableWorkspacePair, RetainedDisposableWorkspacePair,
    RetainedRecoveryWorkspace, SnapshotProgress, WorkspaceFileChanges, WorkspaceFileFingerprint,
    WorkspaceFileInventory, WorkspaceFileRename,
};
pub use failure_digest::{
    DEFAULT_FAILURE_SECTION_CHARACTER_CAP, FAILURE_COMMAND_CHARACTER_CAP,
    FAILURE_DIAGNOSTIC_CHARACTER_CAP, FailureDigest, FailureDigestErrorCategory,
    FailureDigestSectionError, build_failure_digest_section,
};
pub use fingerprint::{
    ProcedureFingerprintError, ProcedureInputFingerprints, ProcedurePathFingerprint,
    ProcedurePathState, capture_path_fingerprint, capture_path_fingerprints, sha256_json,
};
pub use frontier_patch_draft::{
    FrontierPatchDraftError, FrontierPatchDraftRequest, draft_frontier_patch,
};
pub use frontier_patch_output::decode_frontier_patch_output;
pub use frontier_repair::{
    FRONTIER_REPAIR_INSTRUCTION, FrontierRepairDispatch, FrontierRepairDispatchResult,
    FrontierRepairDispatcher, FrontierRepairError, FrontierRepairOutcome, FrontierRepairRequest,
    FrontierRepairRunner, dispatch_frontier_repair,
};
pub use index::{RepositoryIndexError, build_repository_index};
pub use input::{
    CapabilityDeltaSlice, ContractSelection, OpenSpecChange, OpenSpecCommandFailure, OpenSpecInput,
    OpenSpecInputError, OpenSpecValidation, ProposalScope, RequirementSlice, ScenarioSlice,
    SelectedContractSlice, ValidatedContractInput,
};
pub use local_patch_candidates::{
    LocalCandidateGenerationEvidence, LocalCandidateVerification,
    LocalCandidateVerificationOutcome, LocalCandidateVerificationRun, LocalPatchCandidate,
    LocalPatchCandidateGeneration, LocalPatchCandidateGenerationRun, LocalPatchCandidateGenerator,
    LocalPatchCandidateResolution, LocalPatchCandidateVerifier, begin_existing_bounded_repair,
    select_passing_local_candidate,
};
pub use local_patch_draft::{
    LocalPatchDraftDispatch, LocalPatchDraftDispatcher, LocalPatchDraftError,
};
pub use local_repair::{LocalRepairError, LocalRepairOutcome, LocalRepairRun, LocalRepairRunner};
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
pub use repair_event::{
    RepairLadderDisposition, RepairLadderErrorCategory, RepairLadderEvent, RepairLadderGateResult,
    RepairLadderTransition, RepairLadderTrigger, repair_ladder_render_lines,
};
pub use repair_input::{RepairInputError, RepairInputGate, RepairRequest, ValidatedRepairInput};
pub use repair_prompt::{
    REPAIR_INSTRUCTION, RepairPromptError, RepairPromptInput, build_repair_prompt,
};
pub use repair_state::{
    AttemptDisposition, AttemptFailure, AttemptFailureEvidence, AttemptFailureKind, AttemptState,
    AttemptTransitionError, RepairCandidateId, RepairTier,
};
pub use report::ProcedureApprovedReportError;
pub use report::ProcedureReviewError;
pub use report::require_approved_report;
pub use report_repository::ProcedureReportRepository;
pub use report_repository::StoredProcedureReport;
pub use route::{
    DifficultyAssessment, MechanicalVerb, RouteDecision, RouteOverride, RouteSignal, RouteTier,
    apply_route_override, assess_route,
};
pub use routing_metrics::{
    ProcedureBackendModel, ProcedureCandidateMetric, ProcedureGateOutcome,
    ProcedureMetricsDisposition, ProcedureMetricsSummary, ProcedureRouteMetrics,
    ProcedureRunMetrics, ProcedureStageTiming, ProcedureTokenUsage,
};

pub use run::{
    LocalizationAttempt, LocalizationEnvelope, LocalizationTarget, ProcedureAttemptDisposition,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition,
};
pub use run_coordinator::ProcedureRunCoordinator;
pub use run_coordinator::ProcedureRunCoordinatorParams;
pub use runner::{
    ProcedureApplyProgress, ProcedureCommand, ProcedureProgress, ProcedureReviewDecision,
    ProcedureRunRequest, ProcedureRunner, ProcedureRunnerError, WholeChangeCommandRequest,
    apply_review_decision,
};
pub use sampling::{
    LocalizationAgreement, LocalizationAgreementError, LocalizationAgreementOutcome,
    LocalizationAgreementResolver, LocalizationAgreementRun, LocalizationEscalationTrigger,
    LocalizationSample, LocalizationSampleOutcome, LocalizationSampler, LocalizationSamplingError,
    LocalizationSamplingRun, NormalizedLocalizationTarget, NormalizedLocalizationTargets,
    select_localization_agreement,
};
pub use sampling_input::{
    SamplingInputError, SamplingInputGate, SamplingInputRequest, ValidatedSamplingInput,
};
pub use schema::localization_response_format;
pub use structural_repair::{
    LocalStructuralRepairError, LocalStructuralRepairOutcome, RepairFailureRef,
    STRUCTURAL_RETRY_INSTRUCTION, StructuralFailure, StructuralFailureCategory,
    build_structural_retry_prompt, classify_structural_failure, draft_local_with_structural_retry,
};
pub use trace_export::{
    LocalizationTraceExport, LocalizationTraceExportRecord, LocalizationTraceMetrics,
    LocalizationTraceOutcomes, LocalizationTraceRoute, LocalizationTraceTarget,
};
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
