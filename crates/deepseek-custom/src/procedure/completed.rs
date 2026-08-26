//! Production composition for the bounded sampled procedure path.
//!
//! This runner deliberately starts from a named, approved localization report.
//! It keeps the legacy read-only Run, Preview, and Apply actions intact while
//! providing one execution path that records sampling and candidate evidence.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use thiserror::Error;

use super::{
    BoundedRepairCoordinator, FrontierRepairDispatch, FrontierRepairOutcome, FrontierRepairRunner,
    LocalCandidateGenerationEvidence, LocalCandidateVerificationOutcome,
    LocalPatchCandidateGeneration, LocalPatchCandidateGenerator, LocalPatchCandidateResolution,
    LocalPatchCandidateVerifier, LocalPatchDraftDispatch, LocalRepairRunner,
    LocalizationAgreementError, LocalizationAgreementOutcome, LocalizationAgreementResolver,
    LocalizationDispatch, OpenSpecInput, PatchPreview, PatchPreviewId, PatchPreviewStore,
    ProcedureCandidateMetric, ProcedureMetricsDisposition, ProcedureReportStore,
    ProcedureReviewError, ProcedureRouteMetrics, ProcedureRunId, ProcedureRunRequest,
    ProcedureRunner, ProcedureRunnerError, ProcedureScratchpad, ProcedureStageTiming,
    ProcedureTerminalDisposition, PromotionBaseline, PromotionError, RepairInputGate,
    RepairRequest, RouteDecision, RouteOverride, RouteTier, SamplingInputError, SamplingInputGate,
    SamplingInputRequest, apply_patch_in_workspace, apply_route_override, assess_route,
    build_repository_index, model_promotion_targets, promote_verified_workspace,
    select_passing_local_candidate, validate_patch_boundary,
};
use crate::config::settings::{
    RepositoryIndexLimits, ValidatedProcedureRepairPolicy, ValidatedProcedureSamplingSettings,
};
use crate::error::HarnessError;

/// Explicit Procedure-tab input for one sampled execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampledProcedureRequest {
    pub baseline_localization_run_id: ProcedureRunId,
    pub change_id: String,
    pub task_id: String,
    pub route_override: RouteOverride,
}

/// One request to execute all currently unchecked tasks in an OpenSpec change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WholeChangeProcedureRequest {
    pub change_id: String,
    pub route_override: RouteOverride,
}

/// The observable terminal state of a sampled procedure run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampledProcedureOutcome {
    Promoted { candidate_index: u8 },
    Repaired,
    NeedsBoundedRepair,
    Interrupted,
}

/// The terminal state of a whole-change procedure run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WholeChangeProcedureOutcome {
    Completed { task_ids: Vec<String> },
    Failed { task_id: String, reason: String },
    Interrupted { completed_task_ids: Vec<String> },
}

/// Existing bounded-repair policy and dispatcher used only after every
/// sampled candidate fails deterministic verification.
pub struct SampledRepairContext<'a> {
    pub policy: ValidatedProcedureRepairPolicy,
    pub frontier_dispatcher: Option<&'a dyn FrontierRepairDispatch>,
}

struct BoundedRepairInput<'a> {
    input: &'a super::ValidatedSamplingInput,
    request: &'a SampledProcedureRequest,
    route: &'a RouteDecision,
    targets: &'a [String],
    generation: &'a super::LocalPatchCandidateGenerationRun,
    local_patch_dispatcher: &'a dyn LocalPatchDraftDispatch,
    verifier_commands: &'a [String],
    repair: Option<SampledRepairContext<'a>>,
}

/// Failure from the sampled execution path after its pre-dispatch gate.
#[derive(Debug, Error)]
pub enum SampledProcedureError {
    #[error(transparent)]
    Input(#[from] SamplingInputError),
    #[error("could not build the repository index for sampled procedure: {0}")]
    RepositoryIndex(String),
    #[error(transparent)]
    Agreement(#[from] LocalizationAgreementError),
    #[error("could not serialize sampled patch context: {0}")]
    Context(#[from] serde_json::Error),
    #[error("sampled procedure requires at least one verifier command")]
    NoVerifierCommands,
    #[error("sampled procedure reached a frontier route before local candidate generation")]
    FrontierRoute,
    #[error("could not prepare the selected candidate for promotion: {0}")]
    PatchBoundary(String),
    #[error("could not apply the selected candidate in an isolated workspace: {0}")]
    PatchApply(String),
    #[error(transparent)]
    Promotion(#[from] PromotionError),
    #[error("could not save sampled procedure evidence: {0}")]
    Report(#[from] HarnessError),
}

/// Failure that prevents the whole-change coordinator from loading or
/// recording a task. Task-local procedure failures stay in the outcome so
/// callers can report the task that stopped the sequence.
#[derive(Debug, Error)]
pub enum WholeChangeProcedureError {
    #[error(transparent)]
    OpenSpec(#[from] super::OpenSpecInputError),
    #[error(transparent)]
    Localization(#[from] ProcedureRunnerError),
    #[error(transparent)]
    Review(#[from] ProcedureReviewError),
    #[error("whole-change procedure requires at least one verifier command")]
    NoVerifierCommands,
    #[error("OpenSpec change `{change_id}` is no longer active")]
    MissingChange { change_id: String },
}

/// Joins approved-input validation, agreement sampling, best-of-N verification,
/// deterministic selection, promotion, and durable metrics.
pub struct SampledProcedureRunner<L, F> {
    input_gate: SamplingInputGate,
    project_root: PathBuf,
    index_limits: RepositoryIndexLimits,
    resolver: LocalizationAgreementResolver<L, F>,
    settings: ValidatedProcedureSamplingSettings,
    reports: ProcedureReportStore,
    interrupt: Arc<AtomicBool>,
}

/// Executes the current unchecked tasks in one change one at a time.
///
/// The coordinator rereads OpenSpec before every task. It keeps completed
/// task IDs in memory instead of changing `tasks.md`, because task completion
/// remains an explicit OpenSpec workflow decision outside source promotion.
pub struct WholeChangeProcedureRunner<L, F> {
    input: OpenSpecInput,
    project_root: PathBuf,
    index_limits: RepositoryIndexLimits,
    local_dispatcher: Arc<L>,
    frontier_dispatcher: Arc<F>,
    settings: ValidatedProcedureSamplingSettings,
    reports: ProcedureReportStore,
    interrupt: Arc<AtomicBool>,
}

impl<L, F> WholeChangeProcedureRunner<L, F>
where
    L: LocalizationDispatch,
    F: LocalizationDispatch,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input: OpenSpecInput,
        project_root: PathBuf,
        index_limits: RepositoryIndexLimits,
        local_dispatcher: Arc<L>,
        frontier_dispatcher: Arc<F>,
        settings: ValidatedProcedureSamplingSettings,
        reports: ProcedureReportStore,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Self {
            input,
            project_root,
            index_limits,
            local_dispatcher,
            frontier_dispatcher,
            settings,
            reports,
            interrupt,
        }
    }

    /// Localize, approve, sample, and promote every current unchecked task.
    /// A promotion is followed by a fresh OpenSpec read before any later task.
    pub async fn run(
        &self,
        request: WholeChangeProcedureRequest,
        local_patch_dispatcher: &dyn LocalPatchDraftDispatch,
        verifier_commands: &[String],
        repair: Option<SampledRepairContext<'_>>,
    ) -> Result<WholeChangeProcedureOutcome, WholeChangeProcedureError> {
        if verifier_commands.is_empty() {
            return Err(WholeChangeProcedureError::NoVerifierCommands);
        }

        let mut completed = Vec::new();
        let mut processed = BTreeSet::new();
        loop {
            if self.interrupted() {
                return Ok(WholeChangeProcedureOutcome::Interrupted {
                    completed_task_ids: completed,
                });
            }
            let Some(task) = self.next_task(&request.change_id, &processed)? else {
                return Ok(WholeChangeProcedureOutcome::Completed {
                    task_ids: completed,
                });
            };
            let task_id = task.id.clone();
            let localization = ProcedureRunner::new(
                self.input.clone(),
                self.project_root.clone(),
                self.index_limits.clone(),
                Arc::clone(&self.local_dispatcher),
                self.reports.clone(),
                Arc::clone(&self.interrupt),
            )
            .run(ProcedureRunRequest {
                change_id: request.change_id.clone(),
                task_id: task_id.clone(),
                scratchpad: ProcedureScratchpad::default(),
            })
            .await?;
            match localization.terminal_disposition {
                Some(ProcedureTerminalDisposition::AwaitingReview) => {}
                Some(ProcedureTerminalDisposition::Interrupted) => {
                    return Ok(WholeChangeProcedureOutcome::Interrupted {
                        completed_task_ids: completed,
                    });
                }
                Some(ProcedureTerminalDisposition::Failed { reason }) => {
                    return Ok(WholeChangeProcedureOutcome::Failed { task_id, reason });
                }
                disposition => {
                    return Ok(WholeChangeProcedureOutcome::Failed {
                        task_id,
                        reason: format!("localization did not reach review: {disposition:?}"),
                    });
                }
            }
            self.reports.approve(&localization.id)?;

            let sampled = SampledProcedureRunner::new(
                SamplingInputGate::new(
                    self.input.clone(),
                    self.project_root.clone(),
                    self.reports.clone(),
                ),
                self.project_root.clone(),
                self.index_limits.clone(),
                LocalizationAgreementResolver::new(
                    super::LocalizationSampler::new(
                        Arc::clone(&self.local_dispatcher),
                        self.settings.clone(),
                        Arc::clone(&self.interrupt),
                    ),
                    Arc::clone(&self.frontier_dispatcher),
                ),
                self.settings.clone(),
                self.reports.clone(),
                Arc::clone(&self.interrupt),
            );
            let outcome = match sampled
                .run(
                    SampledProcedureRequest {
                        baseline_localization_run_id: localization.id,
                        change_id: request.change_id.clone(),
                        task_id: task_id.clone(),
                        route_override: request.route_override,
                    },
                    local_patch_dispatcher,
                    verifier_commands,
                    repair.as_ref().map(|repair| SampledRepairContext {
                        policy: repair.policy.clone(),
                        frontier_dispatcher: repair.frontier_dispatcher,
                    }),
                )
                .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Ok(WholeChangeProcedureOutcome::Failed {
                        task_id,
                        reason: error.to_string(),
                    });
                }
            };
            match outcome {
                SampledProcedureOutcome::Promoted { .. } | SampledProcedureOutcome::Repaired => {
                    processed.insert(task_id.clone());
                    completed.push(task_id);
                }
                SampledProcedureOutcome::NeedsBoundedRepair => {
                    return Ok(WholeChangeProcedureOutcome::Failed {
                        task_id,
                        reason: "sampled candidates did not produce a promotable repair"
                            .to_string(),
                    });
                }
                SampledProcedureOutcome::Interrupted => {
                    return Ok(WholeChangeProcedureOutcome::Interrupted {
                        completed_task_ids: completed,
                    });
                }
            }
        }
    }

    fn next_task(
        &self,
        change_id: &str,
        processed: &BTreeSet<String>,
    ) -> Result<Option<super::ProcedureTask>, WholeChangeProcedureError> {
        let changes = self.input.active_changes()?;
        let change = changes
            .into_iter()
            .find(|change| change.id == change_id)
            .ok_or_else(|| WholeChangeProcedureError::MissingChange {
                change_id: change_id.to_string(),
            })?;
        Ok(change
            .tasks
            .into_iter()
            .find(|task| !processed.contains(&task.id)))
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl<L, F> SampledProcedureRunner<L, F>
where
    L: LocalizationDispatch,
    F: LocalizationDispatch,
{
    pub fn new(
        input_gate: SamplingInputGate,
        project_root: PathBuf,
        index_limits: RepositoryIndexLimits,
        resolver: LocalizationAgreementResolver<L, F>,
        settings: ValidatedProcedureSamplingSettings,
        reports: ProcedureReportStore,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Self {
            input_gate,
            project_root,
            index_limits,
            resolver,
            settings,
            reports,
            interrupt,
        }
    }

    /// Run the local sampled path. A non-passing candidate set deliberately
    /// reports `NeedsBoundedRepair` so the existing repair ladder can own its
    /// unchanged budgets and evidence.
    pub async fn run(
        &self,
        request: SampledProcedureRequest,
        local_patch_dispatcher: &dyn LocalPatchDraftDispatch,
        verifier_commands: &[String],
        repair: Option<SampledRepairContext<'_>>,
    ) -> Result<SampledProcedureOutcome, SampledProcedureError> {
        if verifier_commands.is_empty() {
            return Err(SampledProcedureError::NoVerifierCommands);
        }
        let started = Instant::now();
        let input = self.input_gate.load(&SamplingInputRequest {
            baseline_localization_run_id: request.baseline_localization_run_id,
            change_id: request.change_id.clone(),
            task_id: request.task_id.clone(),
        })?;
        let index = build_repository_index(&self.project_root, &self.index_limits)
            .map_err(|error| SampledProcedureError::RepositoryIndex(error.to_string()))?;
        let agreement_started = Instant::now();
        let agreement = self.resolver.resolve(&input, &index).await?;
        let mut timings = vec![timing("agreement_sampling", agreement_started.elapsed())];
        if matches!(agreement.outcome, LocalizationAgreementOutcome::Interrupted) {
            self.save_metrics(
                &input.report.id,
                started.elapsed(),
                timings,
                ProcedureRouteMetrics::default(),
                Vec::new(),
                ProcedureMetricsDisposition::Interrupted,
            )?;
            return Ok(SampledProcedureOutcome::Interrupted);
        }

        let (targets, escalated) = localization_targets(&agreement.outcome);
        let contract = serde_json::to_string_pretty(&input.contract.contract)?;
        let route = apply_route_override(
            assess_route(&contract, targets.len()),
            request.route_override,
        );
        let mut route_metrics = ProcedureRouteMetrics {
            signals: route.signals.clone(),
            selected_tier: Some(route.effective_tier),
            local_mechanical_success: None,
            escalation_triggers: if escalated {
                vec!["local_disagreement".to_string()]
            } else {
                Vec::new()
            },
        };
        if route.effective_tier == RouteTier::Frontier {
            self.save_metrics(
                &input.report.id,
                started.elapsed(),
                timings,
                route_metrics,
                Vec::new(),
                ProcedureMetricsDisposition::Failed,
            )?;
            return Err(SampledProcedureError::FrontierRoute);
        }

        let candidate_started = Instant::now();
        let prompt = sampled_patch_prompt(&contract, &targets, &route, &self.project_root)?;
        let generation = LocalPatchCandidateGenerator::new(
            local_patch_dispatcher,
            self.settings.clone(),
            Arc::clone(&self.interrupt),
        )
        .generate(&prompt)
        .await;
        let verification = LocalPatchCandidateVerifier::new(
            self.project_root.clone(),
            targets.clone(),
            verifier_commands.to_vec(),
            Arc::clone(&self.interrupt),
        )
        .verify(&generation)
        .await;
        timings.push(timing(
            "candidate_verification",
            candidate_started.elapsed(),
        ));
        let candidates = candidate_metrics(&generation, &verification);

        let outcome = match select_passing_local_candidate(&verification) {
            LocalPatchCandidateResolution::Selected(selected) => {
                let promotion_started = Instant::now();
                promote_selected(&self.project_root, &targets, &selected.candidate.patch)?;
                timings.push(timing("promotion", promotion_started.elapsed()));
                route_metrics.local_mechanical_success = Some(true);
                SampledProcedureOutcome::Promoted {
                    candidate_index: selected.candidate.evidence.index,
                }
            }
            LocalPatchCandidateResolution::BeginExistingBoundedRepair => {
                route_metrics.local_mechanical_success = Some(false);
                let repair_started = Instant::now();
                let outcome = self
                    .begin_bounded_repair(BoundedRepairInput {
                        input: &input,
                        request: &request,
                        route: &route,
                        targets: &targets,
                        generation: &generation,
                        local_patch_dispatcher,
                        verifier_commands,
                        repair,
                    })
                    .await?;
                timings.push(timing("bounded_repair", repair_started.elapsed()));
                outcome
            }
        };
        let disposition = match outcome {
            SampledProcedureOutcome::Promoted { .. } => ProcedureMetricsDisposition::Succeeded,
            SampledProcedureOutcome::Repaired => ProcedureMetricsDisposition::Succeeded,
            SampledProcedureOutcome::NeedsBoundedRepair => ProcedureMetricsDisposition::Failed,
            SampledProcedureOutcome::Interrupted => ProcedureMetricsDisposition::Interrupted,
        };
        self.save_metrics(
            &input.report.id,
            started.elapsed(),
            timings,
            route_metrics,
            candidates,
            disposition,
        )?;
        Ok(outcome)
    }

    async fn begin_bounded_repair(
        &self,
        input: BoundedRepairInput<'_>,
    ) -> Result<SampledProcedureOutcome, SampledProcedureError> {
        let Some(repair) = input.repair else {
            return Ok(SampledProcedureOutcome::NeedsBoundedRepair);
        };
        let Some(seed) = input
            .generation
            .candidates
            .iter()
            .find_map(|candidate| match candidate {
                LocalPatchCandidateGeneration::Completed(candidate) => Some(candidate),
                LocalPatchCandidateGeneration::Failed { .. }
                | LocalPatchCandidateGeneration::Interrupted { .. } => None,
            })
        else {
            return Ok(SampledProcedureOutcome::NeedsBoundedRepair);
        };
        let preview = PatchPreview {
            id: PatchPreviewId::new(),
            localization_run_id: input.input.report.id,
            change_id: input.request.change_id.clone(),
            task_id: input.request.task_id.clone(),
            route: input.route.clone(),
            backend: seed.evidence.backend.clone(),
            model: seed.evidence.model.clone(),
            targets: input.targets.to_vec(),
            rationale: seed.patch.envelope().rationale.clone(),
            unified_diff: seed.patch.envelope().unified_diff.clone(),
        };
        PatchPreviewStore::for_project(&self.project_root).save(&preview)?;
        let repair_request = RepairRequest {
            localization_run_id: input.input.report.id,
            preview_id: preview.id,
            change_id: input.request.change_id.clone(),
            task_id: input.request.task_id.clone(),
        };
        let local = LocalRepairRunner::new(
            RepairInputGate::new(
                OpenSpecInput::new(&self.project_root),
                self.project_root.clone(),
                self.reports.clone(),
            ),
            self.project_root.clone(),
            Arc::clone(&self.interrupt),
        );
        let frontier = FrontierRepairRunner::with_interrupt(
            self.project_root.clone(),
            Arc::clone(&self.interrupt),
        );
        let repair_run = BoundedRepairCoordinator::new(&local, &frontier, &self.reports)
            .run(
                &repair_request,
                repair.policy,
                input.local_patch_dispatcher,
                repair.frontier_dispatcher,
                input.verifier_commands,
            )
            .await
            .map_err(|error| SampledProcedureError::PatchApply(error.to_string()))?;
        if matches!(
            repair_run.local.outcome,
            super::LocalRepairOutcome::Promoted { .. }
        ) || matches!(
            repair_run.frontier,
            Some(FrontierRepairOutcome::Promoted { .. })
        ) {
            Ok(SampledProcedureOutcome::Repaired)
        } else {
            Ok(SampledProcedureOutcome::NeedsBoundedRepair)
        }
    }

    fn save_metrics(
        &self,
        id: &ProcedureRunId,
        elapsed: std::time::Duration,
        stage_timings: Vec<ProcedureStageTiming>,
        route: ProcedureRouteMetrics,
        candidates: Vec<ProcedureCandidateMetric>,
        terminal_disposition: ProcedureMetricsDisposition,
    ) -> Result<(), SampledProcedureError> {
        let stored = self.reports.load_with_fingerprints(id)?;
        let mut metrics = super::ProcedureRunMetrics::from_terminal_run_with_timing(
            &stored.run,
            elapsed,
            stage_timings,
        )
        .unwrap_or_else(|| super::ProcedureRunMetrics {
            completed_at_unix_ms: 0,
            duration_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            stage_timings: Vec::new(),
            route: ProcedureRouteMetrics::default(),
            backends: Vec::new(),
            localization_attempt_count: 0,
            schema_rejection_count: 0,
            candidates: Vec::new(),
            gate_outcomes: Vec::new(),
            token_usage: Default::default(),
            terminal_disposition,
        });
        metrics.route = route;
        metrics.candidates = candidates;
        metrics.terminal_disposition = terminal_disposition;
        self.reports.replace_metrics(id, &metrics)?;
        Ok(())
    }
}

fn localization_targets(outcome: &LocalizationAgreementOutcome) -> (Vec<String>, bool) {
    let (targets, escalated) = match outcome {
        LocalizationAgreementOutcome::Local { agreement } => (&agreement.targets, false),
        LocalizationAgreementOutcome::Frontier { targets, .. } => (targets, true),
        LocalizationAgreementOutcome::Interrupted => return (Vec::new(), false),
    };
    let mut paths = targets
        .iter()
        .map(|target| target.path.replace('\\', "/"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    (paths, escalated)
}

fn sampled_patch_prompt(
    contract: &str,
    targets: &[String],
    route: &RouteDecision,
    project_root: &std::path::Path,
) -> Result<String, SampledProcedureError> {
    let mut source = String::new();
    for target in targets {
        let contents = std::fs::read_to_string(project_root.join(target))
            .unwrap_or_else(|_| "<path does not exist>\n".to_string());
        source.push_str(&format!(
            "--- BEGIN {target} ---\n{contents}\n--- END {target} ---\n"
        ));
    }
    Ok(format!(
        "Patch context:\n{contract}\n\nTargets: {}\nRoute: {}\n\nSource:\n{source}\nReturn one patch envelope. Change only the targets. Do not apply the patch.",
        targets.join(", "),
        route.effective_tier,
    ))
}

fn candidate_metrics(
    generation: &super::LocalPatchCandidateGenerationRun,
    verification: &super::LocalCandidateVerificationRun,
) -> Vec<ProcedureCandidateMetric> {
    generation
        .candidates
        .iter()
        .map(|generated| {
            let evidence: &LocalCandidateGenerationEvidence = generated.evidence();
            let verified = verification
                .candidates
                .iter()
                .find(|candidate| candidate.candidate.evidence.index == evidence.index);
            ProcedureCandidateMetric {
                index: evidence.index,
                changed_line_count: verified.map(|candidate| candidate.changed_line_count),
                verifier_passed: verified.map(|candidate| matches!(
                    candidate.outcome,
                    LocalCandidateVerificationOutcome::Verified { ref report } if report.eligibility.eligible
                )),
            }
        })
        .collect()
}

fn promote_selected(
    project_root: &std::path::Path,
    targets: &[String],
    patch: &super::PatchCandidate,
) -> Result<(), SampledProcedureError> {
    let boundary = validate_patch_boundary(patch.clone(), targets)
        .map_err(|error| SampledProcedureError::PatchBoundary(error.to_string()))?;
    let promotion_targets = model_promotion_targets(&boundary)
        .map_err(|error| SampledProcedureError::PatchBoundary(error.to_string()))?;
    let baseline = PromotionBaseline::capture(project_root, &promotion_targets)
        .map_err(|error| SampledProcedureError::PatchBoundary(error.to_string()))?;
    let applied = apply_patch_in_workspace(project_root, boundary)
        .map_err(|error| SampledProcedureError::PatchApply(error.to_string()))?;
    let result =
        promote_verified_workspace(project_root, applied.path(), &baseline, &promotion_targets);
    applied
        .close()
        .map_err(|error| SampledProcedureError::PatchApply(error.to_string()))?;
    result?;
    Ok(())
}

fn timing(stage: &str, elapsed: std::time::Duration) -> ProcedureStageTiming {
    ProcedureStageTiming {
        stage: stage.to_string(),
        duration_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
    }
}
