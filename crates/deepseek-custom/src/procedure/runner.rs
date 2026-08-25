//! Read-only orchestration from strict OpenSpec input to localization report.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use tokio::sync::mpsc;

use super::{
    GitApplyPhase, GitApplyResult, LocalizationAttempt, LocalizationDispatch,
    LocalizationDispatchError, LocalizationEnvelope, LocalizationPromptInput,
    ProcedureAttemptDisposition, ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun,
    ProcedureRunId, ProcedureScratchpad, ProcedureStage, ProcedureTask,
    ProcedureTerminalDisposition, PromotionRecoveryEvidence, PromotionResult, RepositoryIndexEntry,
    SnapshotProgress, StalePromotionPath, VerifierGateEvidence, VerifierReport,
    build_localization_prompt, build_repository_index, sha256_json, validate_localization_targets,
};
use crate::config::settings::RepositoryIndexLimits;
use crate::error::HarnessError;
use crate::procedure::OpenSpecInput;

const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// User-selected input for one read-only procedure run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcedureRunRequest {
    pub change_id: String,
    pub task_id: String,
    pub scratchpad: ProcedureScratchpad,
}

/// One GUI-selected run sent to the background procedure executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcedureCommand {
    Run {
        run_id: ProcedureRunId,
        backend: String,
        request: ProcedureRunRequest,
    },
    Review {
        run_id: ProcedureRunId,
        decision: ProcedureReviewDecision,
    },
    Preview {
        preview_id: super::PatchPreviewId,
        request: super::PatchPreviewRequest,
    },
}

/// Terminal decision requested for one awaiting-review run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureReviewDecision {
    Approve,
    Reject,
}

impl ProcedureReviewDecision {
    pub const fn disposition(self) -> ProcedureReviewDisposition {
        match self {
            Self::Approve => ProcedureReviewDisposition::Approved,
            Self::Reject => ProcedureReviewDisposition::Rejected,
        }
    }
}

/// Ordered progress emitted by an isolated Apply operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcedureApplyProgress {
    Started,
    SnapshotStarted,
    SnapshotProgress {
        progress: SnapshotProgress,
    },
    PatchGateStarted {
        phase: GitApplyPhase,
    },
    PatchGateCompleted {
        result: GitApplyResult,
    },
    VerifierGateStarted {
        index: usize,
        command: String,
    },
    VerifierGateCompleted {
        index: usize,
        evidence: VerifierGateEvidence,
    },
    VerificationFinished {
        report: VerifierReport,
    },
    ConflictDetected {
        paths: Vec<StalePromotionPath>,
    },
    PromotionStarted,
    PromotionSucceeded {
        result: PromotionResult,
    },
    PromotionFailed {
        message: String,
        recovery: Option<PromotionRecoveryEvidence>,
    },
    Finished {
        disposition: ProcedureTerminalDisposition,
    },
}

/// Small procedure-only events emitted in execution order.
///
/// This type is intentionally separate from chat `StreamEvent`, so a run
/// cannot enter `MessageHistory` through the normal transcript event path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcedureProgress {
    RunStarted {
        run_id: ProcedureRunId,
        change_id: String,
        task_id: String,
    },
    StageStarted {
        run_id: ProcedureRunId,
        stage: ProcedureStage,
    },
    StageCompleted {
        run_id: ProcedureRunId,
        stage: ProcedureStage,
    },
    AttemptStarted {
        run_id: ProcedureRunId,
        number: u8,
        backend: String,
        model: String,
    },
    AttemptRejected {
        run_id: ProcedureRunId,
        number: u8,
        error: String,
    },
    AttemptAccepted {
        run_id: ProcedureRunId,
        number: u8,
        targets: usize,
    },
    RunFinished {
        run_id: ProcedureRunId,
        disposition: ProcedureTerminalDisposition,
    },
    ReviewSucceeded {
        run_id: ProcedureRunId,
        disposition: ProcedureReviewDisposition,
    },
    ReviewFailed {
        run_id: ProcedureRunId,
        disposition: ProcedureReviewDisposition,
        error: String,
    },
    PreviewStarted {
        preview_id: super::PatchPreviewId,
    },
    PreviewFinished {
        preview_id: super::PatchPreviewId,
        preview: Box<super::PatchPreview>,
        report_path: PathBuf,
    },
    PreviewFailed {
        preview_id: super::PatchPreviewId,
        message: String,
    },
    /// Infrastructure or backend preflight failed before a runner could
    /// produce its normal terminal report.
    RunFailed {
        run_id: ProcedureRunId,
        message: String,
    },
    Apply {
        run_id: ProcedureRunId,
        progress: ProcedureApplyProgress,
    },
}

/// Apply one persisted review decision and publish its run-scoped result.
pub fn apply_review_decision(
    reports: &ProcedureReportStore,
    run_id: ProcedureRunId,
    decision: ProcedureReviewDecision,
    progress: &mpsc::UnboundedSender<ProcedureProgress>,
) {
    let disposition = decision.disposition();
    let result = match decision {
        ProcedureReviewDecision::Approve => reports.approve(&run_id),
        ProcedureReviewDecision::Reject => reports.reject(&run_id),
    };
    let event = match result {
        Ok(_) => ProcedureProgress::ReviewSucceeded {
            run_id,
            disposition,
        },
        Err(error) => ProcedureProgress::ReviewFailed {
            run_id,
            disposition,
            error: error.to_string(),
        },
    };
    let _ = progress.send(event);
}

/// Infrastructure failure that prevents a terminal report from being saved.
#[derive(Debug, Error)]
pub enum ProcedureRunnerError {
    #[error("could not save procedure run {run_id}: {source}")]
    ReportSave {
        run_id: String,
        #[source]
        source: HarnessError,
    },
}

/// Drives one strict, bounded, non-mutating localization run.
pub struct ProcedureRunner<D> {
    input: OpenSpecInput,
    working_dir: PathBuf,
    index_limits: RepositoryIndexLimits,
    dispatcher: D,
    reports: ProcedureReportStore,
    interrupt: Arc<AtomicBool>,
    progress: Option<mpsc::UnboundedSender<ProcedureProgress>>,
}

impl<D> ProcedureRunner<D>
where
    D: LocalizationDispatch,
{
    /// Create a runner with explicit input, workspace, report, and interrupt seams.
    pub fn new(
        input: OpenSpecInput,
        working_dir: PathBuf,
        index_limits: RepositoryIndexLimits,
        dispatcher: D,
        reports: ProcedureReportStore,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Self {
            input,
            working_dir,
            index_limits,
            dispatcher,
            reports,
            interrupt,
            progress: None,
        }
    }

    /// Send ordered progress to a procedure-specific receiver.
    pub fn with_progress(mut self, progress: mpsc::UnboundedSender<ProcedureProgress>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Effective limits retained for the next repository-index build.
    #[cfg(feature = "test-support")]
    pub fn index_limits_for_test(&self) -> &RepositoryIndexLimits {
        &self.index_limits
    }

    /// Validate Stage 0, run bounded localization, and save the terminal report.
    pub async fn run(
        &self,
        request: ProcedureRunRequest,
    ) -> Result<ProcedureRun, ProcedureRunnerError> {
        self.run_with_id(ProcedureRunId::new(), request).await
    }

    /// Run localization under the identifier already owned by the GUI channel.
    pub async fn run_with_id(
        &self,
        run_id: ProcedureRunId,
        request: ProcedureRunRequest,
    ) -> Result<ProcedureRun, ProcedureRunnerError> {
        let mut run = ProcedureRun {
            id: run_id,
            change_id: request.change_id.clone(),
            selected_task: ProcedureTask {
                id: request.task_id.clone(),
                text: String::new(),
                covers: None,
            },
            spec_fingerprint: None,
            repository_fingerprint: None,
            validation: None,
            scratchpad: request.scratchpad,
            stage: ProcedureStage::SpecValidation,
            attempts: Vec::new(),
            review_disposition: ProcedureReviewDisposition::Pending,
            terminal_disposition: None,
        };
        self.emit(ProcedureProgress::RunStarted {
            run_id: run.id,
            change_id: request.change_id.clone(),
            task_id: request.task_id.clone(),
        });

        if self.interrupted() {
            return self.finish(run, ProcedureTerminalDisposition::Interrupted);
        }
        self.emit(ProcedureProgress::StageStarted {
            run_id: run.id,
            stage: ProcedureStage::SpecValidation,
        });

        let validated = match self
            .input
            .validate_and_select_task(&request.change_id, &request.task_id)
        {
            Ok(validated) => validated,
            Err(error) => {
                return self.finish(
                    run,
                    ProcedureTerminalDisposition::Failed {
                        reason: error.to_string(),
                    },
                );
            }
        };
        run.selected_task = validated.contract.task.clone();
        run.validation = Some(validated.validation);
        run.spec_fingerprint = match sha256_json(&validated.contract) {
            Ok(fingerprint) => Some(fingerprint),
            Err(error) => {
                return self.finish(
                    run,
                    ProcedureTerminalDisposition::Failed {
                        reason: format!(
                            "could not fingerprint selected OpenSpec contract: {error}"
                        ),
                    },
                );
            }
        };
        self.emit(ProcedureProgress::StageCompleted {
            run_id: run.id,
            stage: ProcedureStage::SpecValidation,
        });

        if self.interrupted() {
            return self.finish(run, ProcedureTerminalDisposition::Interrupted);
        }
        run.stage = ProcedureStage::Localization;
        self.emit(ProcedureProgress::StageStarted {
            run_id: run.id,
            stage: ProcedureStage::Localization,
        });

        let repository_index = match build_repository_index(&self.working_dir, &self.index_limits) {
            Ok(index) => index,
            Err(error) => {
                return self.finish(
                    run,
                    ProcedureTerminalDisposition::Failed {
                        reason: error.to_string(),
                    },
                );
            }
        };
        run.repository_fingerprint = match content_fingerprint(&repository_index) {
            Ok(fingerprint) => Some(fingerprint),
            Err(error) => {
                return self.finish(
                    run,
                    ProcedureTerminalDisposition::Failed {
                        reason: format!("could not fingerprint repository index: {error}"),
                    },
                );
            }
        };
        if self.interrupted() {
            return self.finish(run, ProcedureTerminalDisposition::Interrupted);
        }

        self.run_localization_attempts(run, &validated.contract, &repository_index)
            .await
    }

    async fn run_localization_attempts(
        &self,
        mut run: ProcedureRun,
        contract: &super::SelectedContractSlice,
        repository_index: &[RepositoryIndexEntry],
    ) -> Result<ProcedureRun, ProcedureRunnerError> {
        let backend = self.dispatcher.backend_name().to_string();
        let model = self.dispatcher.model().to_string();
        for attempt_number in 1..=2 {
            self.emit(ProcedureProgress::AttemptStarted {
                run_id: run.id,
                number: attempt_number,
                backend: backend.clone(),
                model: model.clone(),
            });

            let prompt = match build_localization_prompt(LocalizationPromptInput {
                contract,
                repository_index,
                scratchpad: &run.scratchpad,
            }) {
                Ok(prompt) => prompt,
                Err(error) => {
                    return self.finish(
                        run,
                        ProcedureTerminalDisposition::Failed {
                            reason: format!("failed to serialize the localization prompt: {error}"),
                        },
                    );
                }
            };

            let envelope = match self.dispatch_interruptibly(prompt, repository_index).await {
                DispatchOutcome::Completed(Ok(envelope)) => envelope,
                DispatchOutcome::Completed(Err(error)) => {
                    let retryable =
                        matches!(error, LocalizationDispatchError::InvalidEnvelope { .. });
                    let message = error.to_string();
                    run.attempts.push(rejected_attempt(
                        attempt_number,
                        backend.clone(),
                        model.clone(),
                        Vec::new(),
                        message.clone(),
                    ));
                    self.emit(ProcedureProgress::AttemptRejected {
                        run_id: run.id,
                        number: attempt_number,
                        error: message.clone(),
                    });
                    if retryable && attempt_number == 1 {
                        run.scratchpad.last_error = Some(message);
                        continue;
                    }
                    return self.finish(
                        run,
                        ProcedureTerminalDisposition::Failed { reason: message },
                    );
                }
                DispatchOutcome::Interrupted => {
                    run.attempts.push(LocalizationAttempt {
                        number: attempt_number,
                        backend: backend.clone(),
                        model: model.clone(),
                        disposition: ProcedureAttemptDisposition::Interrupted,
                        targets: Vec::new(),
                        validation_error: None,
                    });
                    return self.finish(run, ProcedureTerminalDisposition::Interrupted);
                }
            };

            if self.interrupted() {
                run.attempts.push(LocalizationAttempt {
                    number: attempt_number,
                    backend: backend.clone(),
                    model: model.clone(),
                    disposition: ProcedureAttemptDisposition::Interrupted,
                    targets: envelope.targets,
                    validation_error: None,
                });
                return self.finish(run, ProcedureTerminalDisposition::Interrupted);
            }

            let returned_targets = envelope.targets;
            match validate_localization_targets(returned_targets.clone(), repository_index) {
                Ok(targets) => {
                    run.scratchpad.files = unique_paths(&targets);
                    run.attempts.push(LocalizationAttempt {
                        number: attempt_number,
                        backend: backend.clone(),
                        model: model.clone(),
                        disposition: ProcedureAttemptDisposition::Accepted,
                        targets,
                        validation_error: None,
                    });
                    self.emit(ProcedureProgress::AttemptAccepted {
                        run_id: run.id,
                        number: attempt_number,
                        targets: run
                            .attempts
                            .last()
                            .map_or(0, |attempt| attempt.targets.len()),
                    });
                    self.emit(ProcedureProgress::StageCompleted {
                        run_id: run.id,
                        stage: ProcedureStage::Localization,
                    });
                    return self.finish(run, ProcedureTerminalDisposition::AwaitingReview);
                }
                Err(error) => {
                    let message = error.to_string();
                    run.attempts.push(rejected_attempt(
                        attempt_number,
                        backend.clone(),
                        model.clone(),
                        returned_targets,
                        message.clone(),
                    ));
                    self.emit(ProcedureProgress::AttemptRejected {
                        run_id: run.id,
                        number: attempt_number,
                        error: message.clone(),
                    });
                    if attempt_number == 1 {
                        run.scratchpad.last_error = Some(message);
                        continue;
                    }
                    return self.finish(
                        run,
                        ProcedureTerminalDisposition::Failed { reason: message },
                    );
                }
            }
        }
        unreachable!("the bounded localization loop always returns")
    }

    async fn dispatch_interruptibly(
        &self,
        prompt: String,
        repository_index: &[RepositoryIndexEntry],
    ) -> DispatchOutcome {
        if self.interrupted() {
            return DispatchOutcome::Interrupted;
        }
        let dispatch = self.dispatcher.dispatch_prompt(prompt, repository_index);
        tokio::pin!(dispatch);
        loop {
            tokio::select! {
                result = &mut dispatch => return DispatchOutcome::Completed(result),
                _ = tokio::time::sleep(INTERRUPT_POLL_INTERVAL) => {
                    if self.interrupted() {
                        return DispatchOutcome::Interrupted;
                    }
                }
            }
        }
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }

    fn emit(&self, event: ProcedureProgress) {
        if let Some(progress) = &self.progress {
            let _ = progress.send(event);
        }
    }

    fn finish(
        &self,
        mut run: ProcedureRun,
        disposition: ProcedureTerminalDisposition,
    ) -> Result<ProcedureRun, ProcedureRunnerError> {
        run.stage = ProcedureStage::Finished;
        run.terminal_disposition = Some(disposition.clone());
        self.reports
            .save(&run)
            .map_err(|source| ProcedureRunnerError::ReportSave {
                run_id: run.id.as_str(),
                source,
            })?;
        self.emit(ProcedureProgress::RunFinished {
            run_id: run.id,
            disposition,
        });
        Ok(run)
    }
}

enum DispatchOutcome {
    Completed(Result<LocalizationEnvelope, LocalizationDispatchError>),
    Interrupted,
}

fn rejected_attempt(
    number: u8,
    backend: String,
    model: String,
    targets: Vec<super::LocalizationTarget>,
    error: String,
) -> LocalizationAttempt {
    LocalizationAttempt {
        number,
        backend,
        model,
        disposition: ProcedureAttemptDisposition::Rejected,
        targets,
        validation_error: Some(error),
    }
}

fn unique_paths(targets: &[super::LocalizationTarget]) -> Vec<String> {
    let mut paths = Vec::new();
    for target in targets {
        if !paths.contains(&target.path) {
            paths.push(target.path.clone());
        }
    }
    paths
}

fn content_fingerprint(value: &impl Serialize) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(value)?;
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(format!("fnv1a64:{hash:016x}"))
}
