//! Typed escalation request and isolated frontier dispatch boundary.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::agent::events::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::resolved::ResolvedBackend;
use crate::effort::Effort;

use super::{
    AttemptDisposition, AttemptFailureEvidence, ContractSelection,
    DEFAULT_FAILURE_SECTION_CHARACTER_CAP, FailureDigest, FailureDigestSectionError,
    FrontierPatchDraftError, FrontierPatchDraftRequest, LocalRepairRun, PatchApplyCheckError,
    PatchBoundaryError, PatchCandidate, ProcedureScratchpad, ProcedureTask, PromotionError,
    PromotionResult, PromotionTargetError, RepairCandidateId, RepairLadderDisposition,
    RepairLadderErrorCategory, RepairLadderEvent, RepairLadderGateResult, RepairLadderTransition,
    RepairLadderTrigger, RepairTier, ValidatedRepairInput, VerifierCommandRunner,
    build_failure_digest_section, draft_frontier_patch, evaluate_applied_patch_eligibility,
    model_promotion_targets, promote_verified_workspace,
};

use super::local_repair::{
    CandidateGateError, apply_candidate_in_fresh_workspace, verifier_was_interrupted,
};

/// Fixed final instruction for every frontier escalation request.
pub const FRONTIER_REPAIR_INSTRUCTION: &str = "Return one corrected patch envelope for this task. Change only the normalized targets. Use the deterministic failure evidence. Do not include commentary or prior conversation.";

/// The complete privacy-limited input allowed across the frontier boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontierRepairRequest {
    backend: String,
    change_id: String,
    task: ProcedureTask,
    spec_slice: ContractSelection,
    targets: Vec<String>,
    scratchpad: ProcedureScratchpad,
    failure_digests: Vec<FailureDigest>,
    prompt: String,
}

impl FrontierRepairRequest {
    pub fn backend(&self) -> &str {
        &self.backend
    }

    pub fn change_id(&self) -> &str {
        &self.change_id
    }

    pub fn task(&self) -> &ProcedureTask {
        &self.task
    }

    pub fn spec_slice(&self) -> &ContractSelection {
        &self.spec_slice
    }

    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    pub fn scratchpad(&self) -> &ProcedureScratchpad {
        &self.scratchpad
    }

    pub fn failure_digests(&self) -> &[FailureDigest] {
        &self.failure_digests
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }
}

/// Result of dispatch only. Candidate verification remains a later gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontierRepairDispatchResult {
    pub request: FrontierRepairRequest,
    pub candidate: PatchCandidate,
}

/// Terminal result of the bounded frontier tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontierRepairOutcome {
    Promoted {
        attempt_number: u8,
        promotion: PromotionResult,
    },
    Blocked {
        attempts: u8,
        reason: String,
    },
    Interrupted,
}

/// Runs frontier candidates through the same deterministic gates as local repair.
pub struct FrontierRepairRunner {
    project_root: PathBuf,
    interrupt: Arc<AtomicBool>,
    progress: Option<mpsc::UnboundedSender<super::ProcedureProgress>>,
}

impl FrontierRepairRunner {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            interrupt: Arc::new(AtomicBool::new(false)),
            progress: None,
        }
    }

    pub fn with_interrupt(project_root: PathBuf, interrupt: Arc<AtomicBool>) -> Self {
        Self {
            project_root,
            interrupt,
            progress: None,
        }
    }

    pub fn with_progress(
        mut self,
        progress: mpsc::UnboundedSender<super::ProcedureProgress>,
    ) -> Self {
        self.progress = Some(progress);
        self
    }

    pub async fn run(
        &self,
        local_run: &mut LocalRepairRun,
        dispatcher: &dyn FrontierRepairDispatch,
        verifier_commands: &[String],
    ) -> Result<FrontierRepairOutcome, FrontierRepairError> {
        if verifier_commands.is_empty() {
            return Err(FrontierRepairError::NoVerifierCommands);
        }

        if self.interrupted() {
            local_run.state.interrupt()?;
            let event = frontier_interrupted_event(local_run, dispatcher);
            self.record_event(local_run, event);
            return Ok(FrontierRepairOutcome::Interrupted);
        }
        let initial_request = prepare_initial_frontier_request(local_run)?;
        self.record_event(
            local_run,
            frontier_event(
                FrontierEventSpec {
                    transition: RepairLadderTransition::Escalated,
                    trigger: RepairLadderTrigger::LocalBudgetExhausted,
                    gate_result: RepairLadderGateResult::NotRun,
                    disposition: RepairLadderDisposition::CandidateActive,
                    error_category: None,
                    attempt_number: 1,
                },
                dispatcher,
                &initial_request,
            ),
        );
        self.record_event(
            local_run,
            frontier_event(
                FrontierEventSpec {
                    transition: RepairLadderTransition::AttemptStarted,
                    trigger: RepairLadderTrigger::LocalBudgetExhausted,
                    gate_result: RepairLadderGateResult::NotRun,
                    disposition: RepairLadderDisposition::CandidateActive,
                    error_category: None,
                    attempt_number: 1,
                },
                dispatcher,
                &initial_request,
            ),
        );
        let Some(initial_candidate) =
            dispatch_frontier_interruptibly(dispatcher.draft(&initial_request), &self.interrupt)
                .await
        else {
            local_run.state.interrupt()?;
            let event = frontier_interrupted_event(local_run, dispatcher);
            self.record_event(local_run, event);
            return Ok(FrontierRepairOutcome::Interrupted);
        };
        let mut dispatched = FrontierRepairDispatchResult {
            request: initial_request,
            candidate: initial_candidate?,
        };
        loop {
            let attempt_number = local_run.state.attempt_index();
            let applied = apply_candidate_in_fresh_workspace(
                &self.project_root,
                dispatched.candidate,
                &local_run.repair_input.preview.targets,
            )
            .map_err(FrontierRepairError::from)?;
            if self.interrupted() {
                applied.close().map_err(PatchApplyCheckError::from)?;
                local_run.state.interrupt()?;
                let event = frontier_interrupted_event(local_run, dispatcher);
                self.record_event(local_run, event);
                return Ok(FrontierRepairOutcome::Interrupted);
            }
            let verifier = VerifierCommandRunner::with_interrupt(Arc::clone(&self.interrupt));
            let verifier_run = verifier.run(applied.path(), verifier_commands).await;

            if verifier_was_interrupted(self.interrupted(), &verifier_run) {
                applied.close().map_err(PatchApplyCheckError::from)?;
                local_run.state.interrupt()?;
                let event = frontier_interrupted_event(local_run, dispatcher);
                self.record_event(local_run, event);
                return Ok(FrontierRepairOutcome::Interrupted);
            }

            if evaluate_applied_patch_eligibility(&applied, &verifier_run).eligible {
                let targets = model_promotion_targets(applied.boundary_patch())?;
                let promotion = promote_verified_workspace(
                    &self.project_root,
                    applied.path(),
                    &local_run.repair_input.promotion_baseline,
                    &targets,
                )?;
                drop(applied);
                local_run.state.promote()?;
                self.record_event(
                    local_run,
                    frontier_event(
                        FrontierEventSpec {
                            transition: RepairLadderTransition::Promoted,
                            trigger: RepairLadderTrigger::CandidatePassed,
                            gate_result: RepairLadderGateResult::VerifierPassed,
                            disposition: RepairLadderDisposition::Promoted,
                            error_category: None,
                            attempt_number,
                        },
                        dispatcher,
                        &dispatched.request,
                    ),
                );
                return Ok(FrontierRepairOutcome::Promoted {
                    attempt_number,
                    promotion,
                });
            }

            let failed = verifier_run
                .commands
                .last()
                .expect("nonempty verifier configuration produces command evidence");
            let digest =
                FailureDigest::from_verifier_result(attempt_number, RepairTier::Frontier, failed);
            applied.close().map_err(PatchApplyCheckError::from)?;
            local_run
                .state
                .frontier_verifier_failure(AttemptFailureEvidence::verifier(
                    digest.command.clone(),
                    digest.exit_code,
                    digest.diagnostic.clone(),
                ))?;
            let failure_category = digest.error_category;
            local_run.failure_digests.push(digest);
            let disposition = frontier_failure_disposition(&local_run.state);
            self.record_event(
                local_run,
                frontier_event(
                    FrontierEventSpec {
                        transition: RepairLadderTransition::VerifierFailure,
                        trigger: RepairLadderTrigger::VerifierFailure,
                        gate_result: RepairLadderGateResult::VerifierFailed,
                        disposition,
                        error_category: Some(failure_category.into()),
                        attempt_number,
                    },
                    dispatcher,
                    &dispatched.request,
                ),
            );

            if local_run.state.disposition() == &AttemptDisposition::FrontierExhausted {
                let attempts = local_run.state.attempt_index();
                let reason = format!(
                    "frontier repair exhausted after {attempts} deterministic verifier attempts"
                );
                local_run.state.block(reason.clone())?;
                self.record_event(
                    local_run,
                    frontier_event(
                        FrontierEventSpec {
                            transition: RepairLadderTransition::Blocked,
                            trigger: RepairLadderTrigger::FrontierBudgetExhausted,
                            gate_result: RepairLadderGateResult::VerifierFailed,
                            disposition: RepairLadderDisposition::Blocked,
                            error_category: Some(failure_category.into()),
                            attempt_number: attempts,
                        },
                        dispatcher,
                        &dispatched.request,
                    ),
                );
                return Ok(FrontierRepairOutcome::Blocked { attempts, reason });
            }

            let next_request = prepare_next_frontier_request(local_run)?;
            let next_attempt = local_run.state.attempt_index();
            self.record_event(
                local_run,
                frontier_event(
                    FrontierEventSpec {
                        transition: RepairLadderTransition::AttemptStarted,
                        trigger: RepairLadderTrigger::VerifierFailure,
                        gate_result: RepairLadderGateResult::NotRun,
                        disposition: RepairLadderDisposition::CandidateActive,
                        error_category: Some(failure_category.into()),
                        attempt_number: next_attempt,
                    },
                    dispatcher,
                    &next_request,
                ),
            );
            let Some(next_candidate) =
                dispatch_frontier_interruptibly(dispatcher.draft(&next_request), &self.interrupt)
                    .await
            else {
                local_run.state.interrupt()?;
                let event = frontier_interrupted_event(local_run, dispatcher);
                self.record_event(local_run, event);
                return Ok(FrontierRepairOutcome::Interrupted);
            };
            dispatched = FrontierRepairDispatchResult {
                request: next_request,
                candidate: next_candidate?,
            };
        }
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }

    fn record_event(&self, local_run: &mut LocalRepairRun, event: RepairLadderEvent) {
        local_run.repair_events.push(event.clone());
        if let Some(progress) = &self.progress {
            let _ = progress.send(super::ProcedureProgress::RepairTransition {
                run_id: local_run.repair_input.report.run.id,
                event,
            });
        }
    }
}

/// Narrow seam for an isolated one-shot frontier draft.
#[async_trait]
pub trait FrontierRepairDispatch: Send + Sync {
    fn model(&self, backend: &str) -> String;

    async fn draft(
        &self,
        request: &FrontierRepairRequest,
    ) -> Result<PatchCandidate, FrontierPatchDraftError>;
}

/// Production adapter that delegates to the existing disposable CLI boundary.
pub struct FrontierRepairDispatcher {
    factory: Arc<BackendFactory>,
    source_root: PathBuf,
    model: Option<String>,
    effort: Effort,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    registry: Arc<SubagentRegistry>,
}

impl FrontierRepairDispatcher {
    pub fn new(
        factory: Arc<BackendFactory>,
        source_root: PathBuf,
        model: Option<String>,
        effort: Effort,
        parent_tx: mpsc::UnboundedSender<RoutedEvent>,
        registry: Arc<SubagentRegistry>,
    ) -> Self {
        Self {
            factory,
            source_root,
            model,
            effort,
            parent_tx,
            registry,
        }
    }
}

#[async_trait]
impl FrontierRepairDispatch for FrontierRepairDispatcher {
    fn model(&self, backend: &str) -> String {
        if let Ok(resolved) = self.factory.resolve(backend, self.model.as_deref()) {
            return match resolved {
                ResolvedBackend::Api { model, .. }
                | ResolvedBackend::ClaudeCli { model, .. }
                | ResolvedBackend::CodexCli { model, .. } => model,
                #[cfg(feature = "test-support")]
                ResolvedBackend::Stub { model, .. } => model,
            };
        }
        self.model
            .clone()
            .unwrap_or_else(|| "unresolved backend model".to_string())
    }

    async fn draft(
        &self,
        request: &FrontierRepairRequest,
    ) -> Result<PatchCandidate, FrontierPatchDraftError> {
        draft_frontier_patch(
            &self.factory,
            &self.source_root,
            FrontierPatchDraftRequest {
                backend: request.backend.clone(),
                model: self.model.clone(),
                prompt: request.prompt.clone(),
                effort: self.effort,
            },
            self.parent_tx.clone(),
            Arc::clone(&self.registry),
        )
        .await
    }
}

fn frontier_failure_disposition(state: &super::AttemptState) -> RepairLadderDisposition {
    if state.disposition() == &AttemptDisposition::FrontierExhausted {
        return RepairLadderDisposition::FrontierExhausted;
    }
    RepairLadderDisposition::Ready
}

/// Failure before a frontier candidate can enter deterministic verification.
#[derive(Debug, Error)]
pub enum FrontierRepairError {
    #[error("frontier escalation requires local exhaustion, got `{0}`")]
    LocalTierNotExhausted(String),
    #[error("frontier escalation is disabled by the validated procedure policy")]
    FrontierDisabled,
    #[error("frontier escalation requires {expected} local failure digests, got {actual}")]
    IncompleteLocalEvidence { expected: usize, actual: usize },
    #[error(transparent)]
    FailureSection(#[from] FailureDigestSectionError),
    #[error("could not serialize frontier repair context: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error(transparent)]
    Transition(#[from] super::AttemptTransitionError),
    #[error(transparent)]
    Dispatch(#[from] FrontierPatchDraftError),
    #[error(transparent)]
    Boundary(#[from] PatchBoundaryError),
    #[error(transparent)]
    Patch(#[from] PatchApplyCheckError),
    #[error(transparent)]
    Targets(#[from] PromotionTargetError),
    #[error(transparent)]
    Promotion(#[from] PromotionError),
    #[error("frontier repair requires at least one verifier command")]
    NoVerifierCommands,
}

impl From<CandidateGateError> for FrontierRepairError {
    fn from(error: CandidateGateError) -> Self {
        match error {
            CandidateGateError::Boundary(error) => Self::Boundary(error),
            CandidateGateError::Patch(error) => Self::Patch(error),
        }
    }
}

/// Build the escalation from the original trusted input and dispatch attempt one.
pub async fn dispatch_frontier_repair(
    local_run: &mut LocalRepairRun,
    dispatcher: &dyn FrontierRepairDispatch,
) -> Result<FrontierRepairDispatchResult, FrontierRepairError> {
    let request = prepare_initial_frontier_request(local_run)?;
    let candidate = dispatcher.draft(&request).await?;
    Ok(FrontierRepairDispatchResult { request, candidate })
}

fn prepare_initial_frontier_request(
    local_run: &mut LocalRepairRun,
) -> Result<FrontierRepairRequest, FrontierRepairError> {
    let LocalRepairRun {
        repair_input: repair,
        state,
        failure_digests,
        ..
    } = local_run;
    if state.disposition() != &AttemptDisposition::LocalExhausted {
        return Err(FrontierRepairError::LocalTierNotExhausted(
            state.disposition().name().to_string(),
        ));
    }
    let backend = state
        .policy()
        .frontier_backend()
        .ok_or(FrontierRepairError::FrontierDisabled)?
        .to_string();
    let expected = usize::from(state.policy().local_verifier_attempts());
    let local_failures = failure_digests
        .iter()
        .filter(|failure| failure.tier == RepairTier::Local)
        .cloned()
        .collect::<Vec<_>>();
    let complete = local_failures.len() == expected
        && local_failures
            .iter()
            .enumerate()
            .all(|(index, failure)| usize::from(failure.attempt_number) == index + 1);
    if !complete {
        return Err(FrontierRepairError::IncompleteLocalEvidence {
            expected,
            actual: local_failures.len(),
        });
    }

    let request = build_frontier_request(repair, backend, local_failures)?;
    state.escalate_to_frontier(RepairCandidateId::new("frontier-1")?)?;
    Ok(request)
}

fn prepare_next_frontier_request(
    local_run: &mut LocalRepairRun,
) -> Result<FrontierRepairRequest, FrontierRepairError> {
    let backend = local_run
        .state
        .policy()
        .frontier_backend()
        .ok_or(FrontierRepairError::FrontierDisabled)?
        .to_string();
    let request = build_frontier_request(
        &local_run.repair_input,
        backend,
        local_run.failure_digests.clone(),
    )?;
    let attempt_number = local_run.state.attempt_index();
    local_run
        .state
        .escalate_to_frontier(RepairCandidateId::new(format!(
            "frontier-{attempt_number}"
        ))?)?;
    Ok(request)
}

async fn dispatch_frontier_interruptibly<T>(
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

struct FrontierEventSpec {
    transition: RepairLadderTransition,
    trigger: RepairLadderTrigger,
    gate_result: RepairLadderGateResult,
    disposition: RepairLadderDisposition,
    error_category: Option<RepairLadderErrorCategory>,
    attempt_number: u8,
}

fn frontier_event(
    spec: FrontierEventSpec,
    dispatcher: &dyn FrontierRepairDispatch,
    request: &FrontierRepairRequest,
) -> RepairLadderEvent {
    RepairLadderEvent {
        transition: spec.transition,
        attempt_number: spec.attempt_number,
        tier: RepairTier::Frontier,
        backend: request.backend().to_string(),
        model: dispatcher.model(request.backend()),
        trigger: spec.trigger,
        error_category: spec.error_category,
        gate_result: spec.gate_result,
        disposition: spec.disposition,
    }
}

fn frontier_interrupted_event(
    local_run: &LocalRepairRun,
    dispatcher: &dyn FrontierRepairDispatch,
) -> RepairLadderEvent {
    RepairLadderEvent {
        transition: RepairLadderTransition::Interrupted,
        attempt_number: local_run.state.attempt_index(),
        tier: RepairTier::Frontier,
        backend: local_run
            .state
            .policy()
            .frontier_backend()
            .unwrap_or("frontier")
            .to_string(),
        model: dispatcher.model(
            local_run
                .state
                .policy()
                .frontier_backend()
                .unwrap_or("frontier"),
        ),
        trigger: RepairLadderTrigger::UserInterrupt,
        error_category: Some(RepairLadderErrorCategory::Interrupted),
        gate_result: RepairLadderGateResult::Interrupted,
        disposition: RepairLadderDisposition::Interrupted,
    }
}

fn build_frontier_request(
    repair: &ValidatedRepairInput,
    backend: String,
    failure_digests: Vec<FailureDigest>,
) -> Result<FrontierRepairRequest, FrontierRepairError> {
    let mut targets = repair
        .preview
        .targets
        .iter()
        .map(|path| path.replace('\\', "/"))
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    let change_id = repair.contract.change_id.clone();
    let task = repair.contract.task.clone();
    let spec_slice = repair.contract.selection.clone();
    let scratchpad = repair.report.run.scratchpad.clone();
    let context = serde_json::to_string_pretty(&FrontierContext {
        change_id: &change_id,
        task: &task,
        spec_slice: &spec_slice,
        targets: &targets,
        scratchpad: &scratchpad,
    })?;
    let failures =
        build_failure_digest_section(&failure_digests, DEFAULT_FAILURE_SECTION_CHARACTER_CAP)?;
    let prompt = format!(
        "Frontier repair context:\n{context}\n\nAccumulated deterministic failures:\n{failures}\n\n{FRONTIER_REPAIR_INSTRUCTION}"
    );

    Ok(FrontierRepairRequest {
        backend,
        change_id,
        task,
        spec_slice,
        targets,
        scratchpad,
        failure_digests,
        prompt,
    })
}

#[derive(Serialize)]
struct FrontierContext<'a> {
    change_id: &'a str,
    task: &'a ProcedureTask,
    spec_slice: &'a ContractSelection,
    targets: &'a [String],
    scratchpad: &'a ProcedureScratchpad,
}
