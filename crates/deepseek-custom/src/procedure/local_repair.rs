//! Verifier-driven local repair attempts through the production patch gates.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

use super::{
    AppliedPatchWorkspace, AttemptDisposition, AttemptFailureEvidence, AttemptState,
    BoundaryValidatedPatch, DEFAULT_FAILURE_SECTION_CHARACTER_CAP, FailureDigest,
    LocalPatchDraftDispatch, LocalPatchDraftError, PatchApplyCheckError, PromotionError,
    PromotionResult, PromotionTargetError, RepairCandidateId, RepairFailureRef, RepairInputError,
    RepairInputGate, RepairPromptError, RepairPromptInput, RepairRequest, RepairTier,
    StructuralFailure, ValidatedRepairInput, VerifierCommandRunner, apply_patch_in_workspace,
    build_repair_prompt, build_structural_retry_prompt, classify_structural_failure,
    evaluate_applied_patch_eligibility, model_promotion_targets, promote_verified_workspace,
    validate_patch_boundary,
};
use crate::config::settings::ValidatedProcedureRepairPolicy;

/// Terminal local-tier result. Frontier dispatch remains a later orchestration step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalRepairOutcome {
    Promoted {
        attempt_number: u8,
        promotion: PromotionResult,
    },
    LocalExhausted,
    StructuralExhausted,
    Blocked,
    Interrupted,
}

/// Complete typed local-tier state returned to later escalation orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRepairRun {
    pub state: AttemptState,
    pub failure_digests: Vec<FailureDigest>,
    pub outcome: LocalRepairOutcome,
}

/// Non-repairable failure while executing the local tier.
#[derive(Debug, Error)]
pub enum LocalRepairError {
    #[error(transparent)]
    Input(#[from] RepairInputError),
    #[error(transparent)]
    Prompt(#[from] RepairPromptError),
    #[error(transparent)]
    Transition(#[from] super::AttemptTransitionError),
    #[error(transparent)]
    Draft(#[from] LocalPatchDraftError),
    #[error(transparent)]
    Patch(#[from] PatchApplyCheckError),
    #[error(transparent)]
    Targets(#[from] PromotionTargetError),
    #[error(transparent)]
    Promotion(#[from] PromotionError),
    #[error("local repair requires at least one verifier command")]
    NoVerifierCommands,
}

/// Runs bounded local candidates without owning a frontier dispatcher.
pub struct LocalRepairRunner {
    input_gate: RepairInputGate,
    project_root: PathBuf,
    interrupt: Arc<AtomicBool>,
    failure_character_cap: usize,
}

impl LocalRepairRunner {
    pub fn new(
        input_gate: RepairInputGate,
        project_root: PathBuf,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Self {
            input_gate,
            project_root,
            interrupt,
            failure_character_cap: DEFAULT_FAILURE_SECTION_CHARACTER_CAP,
        }
    }

    pub fn with_failure_character_cap(mut self, cap: usize) -> Self {
        self.failure_character_cap = cap;
        self
    }

    /// Validate the named input, then run each local candidate through the
    /// shared patch, verifier, baseline, and promotion gates.
    pub async fn run(
        &self,
        request: &RepairRequest,
        policy: ValidatedProcedureRepairPolicy,
        dispatcher: &dyn LocalPatchDraftDispatch,
        verifier_commands: &[String],
    ) -> Result<LocalRepairRun, LocalRepairError> {
        let input = self.input_gate.load(request)?;
        if verifier_commands.is_empty() {
            return Err(LocalRepairError::NoVerifierCommands);
        }
        let mut state = AttemptState::from_validated_input(&input, policy);
        let mut failure_digests = Vec::new();

        while state.tier() == RepairTier::Local
            && matches!(state.disposition(), AttemptDisposition::Ready)
        {
            if self.interrupted() {
                state.interrupt()?;
                return Ok(local_run(
                    state,
                    failure_digests,
                    LocalRepairOutcome::Interrupted,
                ));
            }

            let attempt_number = state.attempt_index();
            let prompt = build_repair_prompt(RepairPromptInput {
                repair: &input,
                failure_digests: &failure_digests,
                failure_character_cap: self.failure_character_cap,
            })?;
            let candidate = RepairCandidateId::new(format!("local-{attempt_number}"))?;
            state.start_local_candidate(candidate)?;
            let prepared = self
                .prepare_candidate(dispatcher, &input, &mut state, prompt, attempt_number)
                .await?;
            let Some(applied) = prepared else {
                let outcome = match state.disposition() {
                    AttemptDisposition::Blocked { .. } => LocalRepairOutcome::Blocked,
                    _ => LocalRepairOutcome::StructuralExhausted,
                };
                return Ok(local_run(state, failure_digests, outcome));
            };

            if self.interrupted() {
                drop(applied);
                state.interrupt()?;
                return Ok(local_run(
                    state,
                    failure_digests,
                    LocalRepairOutcome::Interrupted,
                ));
            }
            let verifier = VerifierCommandRunner::with_interrupt(Arc::clone(&self.interrupt));
            let verifier_run = verifier.run(applied.path(), verifier_commands).await;
            if self.interrupted()
                || verifier_run.commands.iter().any(|command| {
                    command.disposition == super::VerifierCommandDisposition::Interrupted
                })
            {
                drop(applied);
                state.interrupt()?;
                return Ok(local_run(
                    state,
                    failure_digests,
                    LocalRepairOutcome::Interrupted,
                ));
            }

            let eligibility = evaluate_applied_patch_eligibility(&applied, &verifier_run);
            if eligibility.eligible {
                let targets = model_promotion_targets(applied.boundary_patch())?;
                let promotion = promote_verified_workspace(
                    &self.project_root,
                    applied.path(),
                    &input.promotion_baseline,
                    &targets,
                )?;
                drop(applied);
                state.promote()?;
                return Ok(local_run(
                    state,
                    failure_digests,
                    LocalRepairOutcome::Promoted {
                        attempt_number,
                        promotion,
                    },
                ));
            }

            let failed = verifier_run
                .commands
                .last()
                .expect("nonempty verifier configuration produces command evidence");
            let digest =
                FailureDigest::from_verifier_result(attempt_number, RepairTier::Local, failed);
            state.local_verifier_failure(AttemptFailureEvidence::verifier(
                digest.command.clone(),
                digest.exit_code,
                digest.diagnostic.clone(),
            ))?;
            failure_digests.push(digest);
            drop(applied);
        }

        let outcome = match state.disposition() {
            AttemptDisposition::LocalExhausted => LocalRepairOutcome::LocalExhausted,
            AttemptDisposition::Blocked { .. } => LocalRepairOutcome::Blocked,
            AttemptDisposition::Interrupted => LocalRepairOutcome::Interrupted,
            _ => LocalRepairOutcome::StructuralExhausted,
        };
        Ok(local_run(state, failure_digests, outcome))
    }

    async fn prepare_candidate(
        &self,
        dispatcher: &dyn LocalPatchDraftDispatch,
        input: &ValidatedRepairInput,
        state: &mut AttemptState,
        initial_prompt: String,
        attempt_number: u8,
    ) -> Result<Option<AppliedPatchWorkspace>, LocalRepairError> {
        let mut prompt = initial_prompt;
        loop {
            let prepared = draft_apply_candidate(
                dispatcher,
                &self.project_root,
                prompt.clone(),
                &input.preview.targets,
            )
            .await;
            match prepared {
                Ok(applied) => return Ok(Some(applied)),
                Err(CandidatePreparationError::Structural(failure)) => {
                    if state.structural_retry_count() < state.policy().structural_retries() {
                        let retry = RepairCandidateId::new(format!(
                            "local-{attempt_number}-structural-retry"
                        ))?;
                        state.retry_structural(failure.evidence(), retry)?;
                        prompt = build_structural_retry_prompt(&prompt, &failure);
                        continue;
                    }
                    state.structural_retry_exhausted(failure.evidence())?;
                    return Ok(None);
                }
                Err(CandidatePreparationError::Draft(error)) => return Err(error.into()),
                Err(CandidatePreparationError::Patch(error)) => return Err(error.into()),
            }
        }
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }
}

fn local_run(
    state: AttemptState,
    failure_digests: Vec<FailureDigest>,
    outcome: LocalRepairOutcome,
) -> LocalRepairRun {
    LocalRepairRun {
        state,
        failure_digests,
        outcome,
    }
}

enum CandidatePreparationError {
    Structural(StructuralFailure),
    Draft(LocalPatchDraftError),
    Patch(PatchApplyCheckError),
}

async fn draft_apply_candidate(
    dispatcher: &dyn LocalPatchDraftDispatch,
    project_root: &Path,
    prompt: String,
    targets: &[String],
) -> Result<AppliedPatchWorkspace, CandidatePreparationError> {
    let candidate = dispatcher.draft(prompt).await.map_err(|error| {
        classify_structural_failure(RepairFailureRef::LocalDraft(&error)).map_or_else(
            || CandidatePreparationError::Draft(error),
            CandidatePreparationError::Structural,
        )
    })?;
    let boundary: BoundaryValidatedPatch =
        validate_patch_boundary(candidate, targets).map_err(|error| {
            CandidatePreparationError::Structural(
                classify_structural_failure(RepairFailureRef::LocalizationAllowlist(&error))
                    .expect("localization-boundary failures are structural"),
            )
        })?;
    apply_patch_in_workspace(project_root, boundary).map_err(|error| {
        classify_structural_failure(RepairFailureRef::PatchApply(&error)).map_or_else(
            || CandidatePreparationError::Patch(error),
            CandidatePreparationError::Structural,
        )
    })
}
