//! Typed structural patch failures and the single bounded local parser retry.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    AttemptDisposition, AttemptFailureEvidence, AttemptState, AttemptTransitionError,
    BoundaryValidatedPatch, LocalPatchDraftDispatch, LocalPatchDraftError, PatchApplyCheckError,
    PatchBoundaryError, PatchCandidate, PatchEnvelopeError, RepairCandidateId,
    validate_patch_boundary,
};

/// Stable structural categories accepted by the bounded parser-retry path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuralFailureCategory {
    Schema,
    Envelope,
    Allowlist,
    PatchParse,
}

impl StructuralFailureCategory {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Envelope => "envelope",
            Self::Allowlist => "allowlist",
            Self::PatchParse => "patch_parse",
        }
    }
}

impl fmt::Display for StructuralFailureCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One deterministic candidate diagnostic safe to append to a retry request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralFailure {
    category: StructuralFailureCategory,
    diagnostic: String,
}

impl StructuralFailure {
    pub fn category(&self) -> StructuralFailureCategory {
        self.category
    }

    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }

    pub(crate) fn evidence(&self) -> AttemptFailureEvidence {
        AttemptFailureEvidence::classified_structural(self.category, self.diagnostic.clone())
    }
}

/// A typed failure source considered by structural classification.
pub enum RepairFailureRef<'a> {
    LocalDraft(&'a LocalPatchDraftError),
    Envelope(&'a PatchEnvelopeError),
    LocalizationAllowlist(&'a PatchBoundaryError),
    PatchApply(&'a PatchApplyCheckError),
    Cancellation,
    VerifierCommandFailure,
}

/// Classify only candidate-shape failures that one corrected local response can repair.
pub fn classify_structural_failure(source: RepairFailureRef<'_>) -> Option<StructuralFailure> {
    match source {
        RepairFailureRef::LocalDraft(error) => classify_local_draft(error),
        RepairFailureRef::Envelope(error) => Some(classify_envelope(error)),
        RepairFailureRef::LocalizationAllowlist(error) => Some(StructuralFailure {
            category: StructuralFailureCategory::Allowlist,
            diagnostic: error.to_string(),
        }),
        RepairFailureRef::PatchApply(error) => {
            error
                .is_deterministic_rejection()
                .then(|| StructuralFailure {
                    category: StructuralFailureCategory::PatchParse,
                    diagnostic: error.to_string(),
                })
        }
        RepairFailureRef::Cancellation | RepairFailureRef::VerifierCommandFailure => None,
    }
}

fn classify_local_draft(error: &LocalPatchDraftError) -> Option<StructuralFailure> {
    match error {
        LocalPatchDraftError::InvalidEnvelope { source } => Some(classify_envelope(source)),
        LocalPatchDraftError::MissingFinalContent => Some(StructuralFailure {
            category: StructuralFailureCategory::Envelope,
            diagnostic: error.to_string(),
        }),
        LocalPatchDraftError::UnsupportedBackend { .. } | LocalPatchDraftError::Request { .. } => {
            None
        }
    }
}

fn classify_envelope(error: &PatchEnvelopeError) -> StructuralFailure {
    let category = match error {
        PatchEnvelopeError::Json { .. } => StructuralFailureCategory::Envelope,
        PatchEnvelopeError::Structure { .. } => StructuralFailureCategory::Schema,
        PatchEnvelopeError::Diff { .. } => StructuralFailureCategory::PatchParse,
    };
    StructuralFailure {
        category,
        diagnostic: error.to_string(),
    }
}

/// Final result of local drafting before deterministic verification starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalStructuralRepairOutcome {
    ReadyForVerification(BoundaryValidatedPatch),
    EscalationReady,
    Blocked,
}

/// Non-structural failure while running the bounded local parser retry.
#[derive(Debug, Error)]
pub enum LocalStructuralRepairError {
    #[error(transparent)]
    Transition(#[from] AttemptTransitionError),
    #[error(transparent)]
    Draft(#[from] LocalPatchDraftError),
}

/// The fixed final instruction on a structural retry request.
pub const STRUCTURAL_RETRY_INSTRUCTION: &str = "Return one corrected patch envelope for the same task. Do not include commentary or prior conversation.";

/// Build a fresh retry request from only the exact task context and first diagnostic.
pub fn build_structural_retry_prompt(task_context: &str, failure: &StructuralFailure) -> String {
    format!(
        "{task_context}\n\nStructural failure diagnostic:\n{}\n\n{STRUCTURAL_RETRY_INSTRUCTION}",
        failure.diagnostic()
    )
}

/// Draft one local candidate and use at most one structural retry.
///
/// A successful result has passed envelope parsing and localization-boundary validation.
/// Verifier execution remains a later gate and the verifier-attempt index stays unchanged.
pub async fn draft_local_with_structural_retry(
    dispatcher: &dyn LocalPatchDraftDispatch,
    state: &mut AttemptState,
    task_context: &str,
    localization_allowlist: &[String],
    initial_candidate: RepairCandidateId,
    retry_candidate: RepairCandidateId,
) -> Result<LocalStructuralRepairOutcome, LocalStructuralRepairError> {
    state.start_local_candidate(initial_candidate)?;
    let first =
        draft_boundary_candidate(dispatcher, task_context.to_string(), localization_allowlist)
            .await;
    let first_failure = match first {
        Ok(candidate) => {
            return Ok(LocalStructuralRepairOutcome::ReadyForVerification(
                candidate,
            ));
        }
        Err(DraftBoundaryError::Structural(failure)) => failure,
        Err(DraftBoundaryError::Draft(error)) => return Err(error.into()),
    };

    if state.structural_retry_count() >= state.policy().structural_retries() {
        state.structural_retry_exhausted(first_failure.evidence())?;
        return Ok(exhausted_outcome(state));
    }

    state.retry_structural(first_failure.evidence(), retry_candidate)?;
    let retry_prompt = build_structural_retry_prompt(task_context, &first_failure);
    match draft_boundary_candidate(dispatcher, retry_prompt, localization_allowlist).await {
        Ok(candidate) => Ok(LocalStructuralRepairOutcome::ReadyForVerification(
            candidate,
        )),
        Err(DraftBoundaryError::Draft(error)) => Err(error.into()),
        Err(DraftBoundaryError::Structural(failure)) => {
            state.structural_retry_exhausted(failure.evidence())?;
            Ok(exhausted_outcome(state))
        }
    }
}

fn exhausted_outcome(state: &AttemptState) -> LocalStructuralRepairOutcome {
    match state.disposition() {
        AttemptDisposition::Ready => LocalStructuralRepairOutcome::EscalationReady,
        AttemptDisposition::Blocked { .. } => LocalStructuralRepairOutcome::Blocked,
        disposition => {
            unreachable!("structural exhaustion produced unexpected disposition `{disposition}`")
        }
    }
}

enum DraftBoundaryError {
    Structural(StructuralFailure),
    Draft(LocalPatchDraftError),
}

async fn draft_boundary_candidate(
    dispatcher: &dyn LocalPatchDraftDispatch,
    prompt: String,
    localization_allowlist: &[String],
) -> Result<BoundaryValidatedPatch, DraftBoundaryError> {
    let candidate = dispatcher.draft(prompt).await.map_err(|error| {
        match classify_structural_failure(RepairFailureRef::LocalDraft(&error)) {
            Some(failure) => DraftBoundaryError::Structural(failure),
            None => DraftBoundaryError::Draft(error),
        }
    })?;
    validate_boundary(candidate, localization_allowlist)
}

fn validate_boundary(
    candidate: PatchCandidate,
    localization_allowlist: &[String],
) -> Result<BoundaryValidatedPatch, DraftBoundaryError> {
    validate_patch_boundary(candidate, localization_allowlist).map_err(|error| {
        let failure = classify_structural_failure(RepairFailureRef::LocalizationAllowlist(&error))
            .expect("localization-boundary failures are structural");
        DraftBoundaryError::Structural(failure)
    })
}
