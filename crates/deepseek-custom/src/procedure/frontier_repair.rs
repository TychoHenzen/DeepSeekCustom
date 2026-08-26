//! Typed escalation request and isolated frontier dispatch boundary.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::agent::events::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::effort::Effort;

use super::{
    AttemptDisposition, AttemptFailureEvidence, ContractSelection,
    DEFAULT_FAILURE_SECTION_CHARACTER_CAP, FailureDigest, FailureDigestSectionError,
    FrontierPatchDraftError, FrontierPatchDraftRequest, LocalRepairRun, PatchApplyCheckError,
    PatchBoundaryError, PatchCandidate, ProcedureScratchpad, ProcedureTask, PromotionError,
    PromotionResult, PromotionTargetError, RepairCandidateId, RepairTier, ValidatedRepairInput,
    VerifierCommandRunner, build_failure_digest_section, draft_frontier_patch,
    evaluate_applied_patch_eligibility, model_promotion_targets, promote_verified_workspace,
};

use super::local_repair::{CandidateGateError, apply_candidate_in_fresh_workspace};

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
}

/// Runs frontier candidates through the same deterministic gates as local repair.
pub struct FrontierRepairRunner {
    project_root: PathBuf,
}

impl FrontierRepairRunner {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
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

        let mut dispatched = dispatch_frontier_repair(local_run, dispatcher).await?;
        loop {
            let attempt_number = local_run.state.attempt_index();
            let applied = apply_candidate_in_fresh_workspace(
                &self.project_root,
                dispatched.candidate,
                &local_run.repair_input.preview.targets,
            )
            .map_err(FrontierRepairError::from)?;
            let verifier = VerifierCommandRunner::new();
            let verifier_run = verifier.run(applied.path(), verifier_commands).await;

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
            local_run.failure_digests.push(digest);

            if local_run.state.disposition() == &AttemptDisposition::FrontierExhausted {
                let attempts = local_run.state.attempt_index();
                let reason = format!(
                    "frontier repair exhausted after {attempts} deterministic verifier attempts"
                );
                local_run.state.block(reason.clone())?;
                return Ok(FrontierRepairOutcome::Blocked { attempts, reason });
            }

            dispatched = dispatch_next_frontier_repair(local_run, dispatcher).await?;
        }
    }
}

/// Narrow seam for an isolated one-shot frontier draft.
#[async_trait]
pub trait FrontierRepairDispatch: Send + Sync {
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
    let candidate = dispatcher.draft(&request).await?;
    Ok(FrontierRepairDispatchResult { request, candidate })
}

async fn dispatch_next_frontier_repair(
    local_run: &mut LocalRepairRun,
    dispatcher: &dyn FrontierRepairDispatch,
) -> Result<FrontierRepairDispatchResult, FrontierRepairError> {
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
    let candidate = dispatcher.draft(&request).await?;
    Ok(FrontierRepairDispatchResult { request, candidate })
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
