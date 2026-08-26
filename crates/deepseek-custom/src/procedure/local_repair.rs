//! Verifier-driven local repair attempts through the production patch gates.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use thiserror::Error;

use super::{
    AppliedPatchWorkspace, AttemptDisposition, AttemptFailureEvidence, AttemptState,
    BoundaryValidatedPatch, DEFAULT_FAILURE_SECTION_CHARACTER_CAP, FailureDigest,
    LocalPatchDraftDispatch, LocalPatchDraftError, PatchApplyCheckError, PromotionError,
    PromotionResult, PromotionTargetError, RepairCandidateId, RepairFailureRef, RepairInputError,
    RepairInputGate, RepairLadderDisposition, RepairLadderErrorCategory, RepairLadderEvent,
    RepairLadderGateResult, RepairLadderTransition, RepairLadderTrigger, RepairPromptError,
    RepairPromptInput, RepairRequest, RepairTier, StructuralFailure, ValidatedRepairInput,
    VerifierCommandRunner, apply_patch_in_workspace, build_repair_prompt,
    build_structural_retry_prompt, classify_structural_failure, evaluate_applied_patch_eligibility,
    model_promotion_targets, promote_verified_workspace, validate_patch_boundary,
};
use crate::config::settings::ValidatedProcedureRepairPolicy;
use tokio::sync::mpsc;

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
    pub repair_input: ValidatedRepairInput,
    pub state: AttemptState,
    pub failure_digests: Vec<FailureDigest>,
    pub repair_events: Vec<RepairLadderEvent>,
    pub outcome: LocalRepairOutcome,
}

impl LocalRepairRun {
    /// Persist the complete transition sequence beside the named localization report.
    pub fn save_repair_events(
        &self,
        reports: &super::ProcedureReportStore,
    ) -> crate::error::Result<()> {
        reports.save_repair_events(&self.repair_input.report.run.id, &self.repair_events)
    }
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
    progress: Option<mpsc::UnboundedSender<super::ProcedureProgress>>,
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
            progress: None,
        }
    }

    pub fn with_failure_character_cap(mut self, cap: usize) -> Self {
        self.failure_character_cap = cap;
        self
    }

    pub fn with_progress(
        mut self,
        progress: mpsc::UnboundedSender<super::ProcedureProgress>,
    ) -> Self {
        self.progress = Some(progress);
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
        let mut repair_events = Vec::new();
        let backend = dispatcher.backend_name().to_string();
        let model = dispatcher.model().to_string();

        while state.tier() == RepairTier::Local
            && matches!(state.disposition(), AttemptDisposition::Ready)
        {
            if self.interrupted() {
                state.interrupt()?;
                self.record_event(
                    input.report.run.id,
                    &mut repair_events,
                    interrupted_event(state.attempt_index(), state.tier(), &backend, &model),
                );
                return Ok(local_run(
                    input,
                    state,
                    failure_digests,
                    repair_events,
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
            let trigger = failure_digests
                .last()
                .map_or(RepairLadderTrigger::InitialRequest, |_| {
                    RepairLadderTrigger::VerifierFailure
                });
            self.record_event(
                input.report.run.id,
                &mut repair_events,
                RepairLadderEvent {
                    transition: RepairLadderTransition::AttemptStarted,
                    attempt_number,
                    tier: RepairTier::Local,
                    backend: backend.clone(),
                    model: model.clone(),
                    trigger,
                    error_category: failure_digests
                        .last()
                        .map(|failure| failure.error_category.into()),
                    gate_result: RepairLadderGateResult::NotRun,
                    disposition: RepairLadderDisposition::CandidateActive,
                },
            );
            let prepared = self
                .prepare_candidate(
                    dispatcher,
                    &input,
                    &mut state,
                    &mut repair_events,
                    prompt,
                    attempt_number,
                )
                .await?;
            let applied = match prepared {
                PreparedCandidate::Applied(applied) => *applied,
                PreparedCandidate::StructuralExhausted => {
                    let outcome = match state.disposition() {
                        AttemptDisposition::Blocked { .. } => LocalRepairOutcome::Blocked,
                        _ => LocalRepairOutcome::StructuralExhausted,
                    };
                    return Ok(local_run(
                        input,
                        state,
                        failure_digests,
                        repair_events,
                        outcome,
                    ));
                }
                PreparedCandidate::Interrupted => {
                    state.interrupt()?;
                    self.record_event(
                        input.report.run.id,
                        &mut repair_events,
                        interrupted_event(attempt_number, RepairTier::Local, &backend, &model),
                    );
                    return Ok(local_run(
                        input,
                        state,
                        failure_digests,
                        repair_events,
                        LocalRepairOutcome::Interrupted,
                    ));
                }
            };

            if self.interrupted() {
                applied.close().map_err(PatchApplyCheckError::from)?;
                state.interrupt()?;
                self.record_event(
                    input.report.run.id,
                    &mut repair_events,
                    interrupted_event(attempt_number, RepairTier::Local, &backend, &model),
                );
                return Ok(local_run(
                    input,
                    state,
                    failure_digests,
                    repair_events,
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
                applied.close().map_err(PatchApplyCheckError::from)?;
                state.interrupt()?;
                self.record_event(
                    input.report.run.id,
                    &mut repair_events,
                    interrupted_event(attempt_number, RepairTier::Local, &backend, &model),
                );
                return Ok(local_run(
                    input,
                    state,
                    failure_digests,
                    repair_events,
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
                self.record_event(
                    input.report.run.id,
                    &mut repair_events,
                    RepairLadderEvent {
                        transition: RepairLadderTransition::Promoted,
                        attempt_number,
                        tier: RepairTier::Local,
                        backend: backend.clone(),
                        model: model.clone(),
                        trigger: RepairLadderTrigger::CandidatePassed,
                        error_category: None,
                        gate_result: RepairLadderGateResult::VerifierPassed,
                        disposition: RepairLadderDisposition::Promoted,
                    },
                );
                return Ok(local_run(
                    input,
                    state,
                    failure_digests,
                    repair_events,
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
            applied.close().map_err(PatchApplyCheckError::from)?;
            state.local_verifier_failure(AttemptFailureEvidence::verifier(
                digest.command.clone(),
                digest.exit_code,
                digest.diagnostic.clone(),
            ))?;
            let disposition = if state.disposition() == &AttemptDisposition::LocalExhausted {
                RepairLadderDisposition::LocalExhausted
            } else {
                RepairLadderDisposition::Ready
            };
            self.record_event(
                input.report.run.id,
                &mut repair_events,
                RepairLadderEvent {
                    transition: RepairLadderTransition::VerifierFailure,
                    attempt_number,
                    tier: RepairTier::Local,
                    backend: backend.clone(),
                    model: model.clone(),
                    trigger: RepairLadderTrigger::VerifierFailure,
                    error_category: Some(digest.error_category.into()),
                    gate_result: RepairLadderGateResult::VerifierFailed,
                    disposition,
                },
            );
            failure_digests.push(digest);
        }

        if state.disposition() == &AttemptDisposition::LocalExhausted
            && state.policy().frontier_attempts() == 0
        {
            state.block("local repair exhausted and frontier escalation is disabled")?;
            self.record_event(
                input.report.run.id,
                &mut repair_events,
                RepairLadderEvent {
                    transition: RepairLadderTransition::Blocked,
                    attempt_number: state.attempt_index(),
                    tier: RepairTier::Local,
                    backend,
                    model,
                    trigger: RepairLadderTrigger::LocalBudgetExhausted,
                    error_category: failure_digests
                        .last()
                        .map(|failure| failure.error_category.into()),
                    gate_result: RepairLadderGateResult::VerifierFailed,
                    disposition: RepairLadderDisposition::Blocked,
                },
            );
        }
        let outcome = match state.disposition() {
            AttemptDisposition::LocalExhausted => LocalRepairOutcome::LocalExhausted,
            AttemptDisposition::Blocked { .. } => LocalRepairOutcome::Blocked,
            AttemptDisposition::Interrupted => LocalRepairOutcome::Interrupted,
            _ => LocalRepairOutcome::StructuralExhausted,
        };
        Ok(local_run(
            input,
            state,
            failure_digests,
            repair_events,
            outcome,
        ))
    }

    async fn prepare_candidate(
        &self,
        dispatcher: &dyn LocalPatchDraftDispatch,
        input: &ValidatedRepairInput,
        state: &mut AttemptState,
        repair_events: &mut Vec<RepairLadderEvent>,
        initial_prompt: String,
        attempt_number: u8,
    ) -> Result<PreparedCandidate, LocalRepairError> {
        let mut prompt = initial_prompt;
        loop {
            let prepared = draft_apply_candidate(
                dispatcher,
                &self.project_root,
                prompt.clone(),
                &input.preview.targets,
                &self.interrupt,
            )
            .await;
            match prepared {
                Ok(applied) => return Ok(PreparedCandidate::Applied(Box::new(applied))),
                Err(CandidatePreparationError::Structural(failure)) => {
                    if state.structural_retry_count() < state.policy().structural_retries() {
                        let retry = RepairCandidateId::new(format!(
                            "local-{attempt_number}-structural-retry"
                        ))?;
                        state.retry_structural(failure.evidence(), retry)?;
                        self.record_event(
                            input.report.run.id,
                            repair_events,
                            structural_event(
                                RepairLadderTransition::StructuralRetry,
                                attempt_number,
                                dispatcher,
                                &failure,
                                RepairLadderDisposition::CandidateActive,
                            ),
                        );
                        prompt = build_structural_retry_prompt(&prompt, &failure);
                        continue;
                    }
                    state.structural_retry_exhausted(failure.evidence())?;
                    let disposition = match state.disposition() {
                        AttemptDisposition::Ready => RepairLadderDisposition::FrontierReady,
                        AttemptDisposition::Blocked { .. } => RepairLadderDisposition::Blocked,
                        _ => RepairLadderDisposition::Ready,
                    };
                    self.record_event(
                        input.report.run.id,
                        repair_events,
                        structural_event(
                            RepairLadderTransition::StructuralRetryExhausted,
                            attempt_number,
                            dispatcher,
                            &failure,
                            disposition,
                        ),
                    );
                    return Ok(PreparedCandidate::StructuralExhausted);
                }
                Err(CandidatePreparationError::Interrupted) => {
                    return Ok(PreparedCandidate::Interrupted);
                }
                Err(CandidatePreparationError::Draft(error)) => return Err(error.into()),
                Err(CandidatePreparationError::Patch(error)) => return Err(error.into()),
            }
        }
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }

    fn record_event(
        &self,
        run_id: super::ProcedureRunId,
        events: &mut Vec<RepairLadderEvent>,
        event: RepairLadderEvent,
    ) {
        events.push(event.clone());
        if let Some(progress) = &self.progress {
            let _ = progress.send(super::ProcedureProgress::RepairTransition { run_id, event });
        }
    }
}

fn local_run(
    repair_input: ValidatedRepairInput,
    state: AttemptState,
    failure_digests: Vec<FailureDigest>,
    repair_events: Vec<RepairLadderEvent>,
    outcome: LocalRepairOutcome,
) -> LocalRepairRun {
    LocalRepairRun {
        repair_input,
        state,
        failure_digests,
        repair_events,
        outcome,
    }
}

enum CandidatePreparationError {
    Structural(StructuralFailure),
    Interrupted,
    Draft(LocalPatchDraftError),
    Patch(PatchApplyCheckError),
}

pub(super) enum CandidateGateError {
    Boundary(super::PatchBoundaryError),
    Patch(PatchApplyCheckError),
}

pub(super) fn apply_candidate_in_fresh_workspace(
    project_root: &Path,
    candidate: super::PatchCandidate,
    targets: &[String],
) -> Result<AppliedPatchWorkspace, CandidateGateError> {
    let boundary: BoundaryValidatedPatch =
        validate_patch_boundary(candidate, targets).map_err(CandidateGateError::Boundary)?;
    apply_patch_in_workspace(project_root, boundary).map_err(CandidateGateError::Patch)
}

async fn draft_apply_candidate(
    dispatcher: &dyn LocalPatchDraftDispatch,
    project_root: &Path,
    prompt: String,
    targets: &[String],
    interrupt: &Arc<AtomicBool>,
) -> Result<AppliedPatchWorkspace, CandidatePreparationError> {
    let candidate = dispatch_local_interruptibly(dispatcher.draft(prompt), interrupt)
        .await
        .ok_or(CandidatePreparationError::Interrupted)?
        .map_err(|error| {
            classify_structural_failure(RepairFailureRef::LocalDraft(&error)).map_or_else(
                || CandidatePreparationError::Draft(error),
                CandidatePreparationError::Structural,
            )
        })?;
    apply_candidate_in_fresh_workspace(project_root, candidate, targets).map_err(
        |error| match error {
            CandidateGateError::Boundary(error) => CandidatePreparationError::Structural(
                classify_structural_failure(RepairFailureRef::LocalizationAllowlist(&error))
                    .expect("localization-boundary failures are structural"),
            ),
            CandidateGateError::Patch(error) => {
                classify_structural_failure(RepairFailureRef::PatchApply(&error)).map_or_else(
                    || CandidatePreparationError::Patch(error),
                    CandidatePreparationError::Structural,
                )
            }
        },
    )
}

enum PreparedCandidate {
    Applied(Box<AppliedPatchWorkspace>),
    StructuralExhausted,
    Interrupted,
}

fn interrupted_event(
    attempt_number: u8,
    tier: RepairTier,
    backend: &str,
    model: &str,
) -> RepairLadderEvent {
    RepairLadderEvent {
        transition: RepairLadderTransition::Interrupted,
        attempt_number,
        tier,
        backend: backend.to_string(),
        model: model.to_string(),
        trigger: RepairLadderTrigger::UserInterrupt,
        error_category: Some(RepairLadderErrorCategory::Interrupted),
        gate_result: RepairLadderGateResult::Interrupted,
        disposition: RepairLadderDisposition::Interrupted,
    }
}

fn structural_event(
    transition: RepairLadderTransition,
    attempt_number: u8,
    dispatcher: &dyn LocalPatchDraftDispatch,
    failure: &StructuralFailure,
    disposition: RepairLadderDisposition,
) -> RepairLadderEvent {
    RepairLadderEvent {
        transition,
        attempt_number,
        tier: RepairTier::Local,
        backend: dispatcher.backend_name().to_string(),
        model: dispatcher.model().to_string(),
        trigger: RepairLadderTrigger::StructuralFailure,
        error_category: Some(failure.category().into()),
        gate_result: RepairLadderGateResult::StructuralRejected,
        disposition,
    }
}

async fn dispatch_local_interruptibly<T>(
    future: impl std::future::Future<Output = T>,
    interrupt: &Arc<AtomicBool>,
) -> Option<T> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Some(output),
            _ = tokio::time::sleep(Duration::from_millis(25)) => {
                if interrupt.load(Ordering::SeqCst) {
                    return None;
                }
            }
        }
    }
}
