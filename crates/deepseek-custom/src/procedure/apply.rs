//! End-to-end isolated Apply orchestration.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use tokio::sync::mpsc;

use super::{
    ApplyRequest, CandidateEligibility, CandidateIneligibility, GitApplyPhase,
    PatchApplyCheckError, PatchApplyProgress, PatchEnvelope, PatchGateEvidence, PatchRouteMetadata,
    ProcedureApplyProgress, ProcedureProgress, ProcedureReportRepository,
    ProcedureTerminalDisposition, PromotionBaseline, PromotionError, PromotionTargetError,
    VerificationInputError, VerificationInputGate, VerifierCommandRunner, VerifierGateDisposition,
    VerifierGateEvidence, VerifierReport, VerifierRunProgress,
    apply_patch_in_workspace_with_progress, decode_patch_envelope,
    evaluate_applied_patch_eligibility, model_promotion_targets, promote_verified_workspace,
    validate_patch_boundary,
};
use crate::error::HarnessError;

/// Failure before an Apply run can publish its terminal progress.
#[derive(Debug, Error)]
pub enum ProcedureApplyError {
    #[error(transparent)]
    Input(#[from] VerificationInputError),
    #[error("preview patch could not be decoded: {0}")]
    PatchDecode(String),
    #[error(transparent)]
    PatchBoundary(#[from] super::PatchBoundaryError),
    #[error(transparent)]
    Targets(#[from] PromotionTargetError),
    #[error(transparent)]
    Baseline(#[from] super::ProcedureFingerprintError),
    #[error(transparent)]
    PatchApply(Box<PatchApplyCheckError>),
    #[error(transparent)]
    Promotion(#[from] PromotionError),
    #[error("could not save Apply evidence for procedure run {run_id}: {source}")]
    ReportSave {
        run_id: String,
        #[source]
        source: HarnessError,
    },
}

/// Executes the complete isolated verification and promotion transaction.
pub struct ProcedureApplyRunner {
    input_gate: VerificationInputGate,
    project_root: PathBuf,
    reports: ProcedureReportRepository,
    interrupt: Arc<AtomicBool>,
    progress: Option<mpsc::UnboundedSender<ProcedureProgress>>,
}

impl ProcedureApplyRunner {
    pub fn new(
        input_gate: VerificationInputGate,
        project_root: PathBuf,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        let reports = ProcedureReportRepository::for_project(&project_root);
        Self {
            input_gate,
            project_root,
            reports,
            interrupt,
            progress: None,
        }
    }

    pub fn with_progress(mut self, progress: mpsc::UnboundedSender<ProcedureProgress>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Run Apply with the production promotion transaction.
    pub async fn run(
        &self,
        run_id: super::ProcedureRunId,
        request: ApplyRequest,
        verifier_commands: &[String],
    ) -> Result<ProcedureTerminalDisposition, ProcedureApplyError> {
        self.run_started(run_id, request, verifier_commands, None)
            .await
    }

    #[cfg(feature = "test-support")]
    pub async fn run_with_failure_injection(
        &self,
        run_id: super::ProcedureRunId,
        request: ApplyRequest,
        verifier_commands: &[String],
        injection: super::PromotionFailureInjection,
    ) -> Result<ProcedureTerminalDisposition, ProcedureApplyError> {
        self.run_started(run_id, request, verifier_commands, Some(injection))
            .await
    }

    async fn run_started(
        &self,
        run_id: super::ProcedureRunId,
        request: ApplyRequest,
        verifier_commands: &[String],
        #[cfg(feature = "test-support")] injection: Option<super::PromotionFailureInjection>,
        #[cfg(not(feature = "test-support"))] injection: Option<()>,
    ) -> Result<ProcedureTerminalDisposition, ProcedureApplyError> {
        self.emit(run_id, ProcedureApplyProgress::Started);
        let outcome = self
            .run_inner(
                run_id,
                request,
                verifier_commands,
                #[cfg(feature = "test-support")]
                injection,
                #[cfg(not(feature = "test-support"))]
                injection,
            )
            .await;
        if let Err(error) = &outcome {
            self.finish(
                run_id,
                ProcedureTerminalDisposition::Failed {
                    reason: error.to_string(),
                },
            );
        }
        outcome
    }

    async fn run_inner(
        &self,
        run_id: super::ProcedureRunId,
        request: ApplyRequest,
        verifier_commands: &[String],
        #[cfg(feature = "test-support")] injection: Option<super::PromotionFailureInjection>,
        #[cfg(not(feature = "test-support"))] _injection: Option<()>,
    ) -> Result<ProcedureTerminalDisposition, ProcedureApplyError> {
        if self.interrupted() {
            return Ok(self.finish(run_id, ProcedureTerminalDisposition::Interrupted));
        }
        if verifier_commands.is_empty() {
            return Ok(self.finish(
                run_id,
                ProcedureTerminalDisposition::Failed {
                    reason: "no verifier commands are configured".to_string(),
                },
            ));
        }

        let input = self.input_gate.load(&request)?;
        let patch = decode_preview_patch(&input.preview)?;
        let boundary = validate_patch_boundary(patch, &input.preview.targets)?;
        let targets = model_promotion_targets(&boundary)?;
        let baseline = input.promotion_baseline;

        self.emit(run_id, ProcedureApplyProgress::SnapshotStarted);
        let mut patch_gates = Vec::with_capacity(2);
        let applied =
            match apply_patch_in_workspace_with_progress(&self.project_root, boundary, |progress| {
                match progress {
                    PatchApplyProgress::Snapshot(progress) => self.emit(
                        run_id,
                        ProcedureApplyProgress::SnapshotProgress { progress },
                    ),
                    PatchApplyProgress::GateStarted(phase) => {
                        self.emit(run_id, ProcedureApplyProgress::PatchGateStarted { phase })
                    }
                    PatchApplyProgress::GateCompleted(result) => {
                        patch_gates.push(result.evidence());
                        self.emit(
                            run_id,
                            ProcedureApplyProgress::PatchGateCompleted { result },
                        );
                    }
                }
            }) {
                Ok(applied) => applied,
                Err(error) => {
                    if let Some(result) = error.result() {
                        patch_gates = error.gate_evidence();
                        let eligibility = if error.is_deterministic_rejection() {
                            CandidateEligibility::ineligible(match result.phase {
                                GitApplyPhase::Check => CandidateIneligibility::PatchCheckFailed,
                                GitApplyPhase::Apply => CandidateIneligibility::PatchApplyFailed,
                            })
                        } else {
                            CandidateEligibility::ineligible(
                                CandidateIneligibility::PatchGateInfrastructureFailed {
                                    phase: result.phase,
                                    disposition: result.disposition,
                                    error: error.to_string(),
                                },
                            )
                        };
                        let terminal = ProcedureTerminalDisposition::Failed {
                            reason: error.to_string(),
                        };
                        let report = patch_failure_report(
                            patch_gates,
                            verifier_commands,
                            eligibility,
                            terminal.clone(),
                        );
                        self.save_verification(&request.localization_run_id, &report)?;
                        self.emit(
                            run_id,
                            ProcedureApplyProgress::VerificationFinished { report },
                        );
                        return Ok(self.finish(run_id, terminal));
                    }
                    return Err(error.into());
                }
            };

        if self.interrupted() {
            drop(applied);
            return Ok(self.finish(run_id, ProcedureTerminalDisposition::Interrupted));
        }

        let verifier = VerifierCommandRunner::with_interrupt(Arc::clone(&self.interrupt));
        let verifier_run = verifier
            .run_with_progress(
                applied.path(),
                verifier_commands,
                |progress| match progress {
                    VerifierRunProgress::GateStarted { index, command } => self.emit(
                        run_id,
                        ProcedureApplyProgress::VerifierGateStarted { index, command },
                    ),
                    VerifierRunProgress::GateCompleted { index, evidence } => self.emit(
                        run_id,
                        ProcedureApplyProgress::VerifierGateCompleted { index, evidence },
                    ),
                },
            )
            .await;
        let eligibility = evaluate_applied_patch_eligibility(&applied, &verifier_run);
        let mut report = verifier_run
            .report(eligibility.clone())
            .with_patch_gates(patch_gates);
        let report_id = &request.localization_run_id;
        self.emit(
            run_id,
            ProcedureApplyProgress::VerificationFinished {
                report: report.clone(),
            },
        );

        if self.interrupted()
            || report.gates.iter().any(|gate| {
                matches!(
                    gate.disposition,
                    super::VerifierGateDisposition::Interrupted
                )
            })
        {
            let terminal = ProcedureTerminalDisposition::Interrupted;
            self.save_terminal_verification(report_id, &mut report, &terminal)?;
            drop(applied);
            return Ok(self.finish(run_id, terminal));
        }
        if !eligibility.eligible {
            let reason = format!("verification failed: {:?}", eligibility.reason);
            let terminal = ProcedureTerminalDisposition::Failed { reason };
            self.save_terminal_verification(report_id, &mut report, &terminal)?;
            drop(applied);
            return Ok(self.finish(run_id, terminal));
        }

        match self.interruptible_promotion(
            run_id,
            &applied,
            &baseline,
            &targets,
            #[cfg(feature = "test-support")]
            injection,
        ) {
            Ok(result) => {
                let terminal = ProcedureTerminalDisposition::Succeeded;
                self.save_terminal_verification(report_id, &mut report, &terminal)?;
                drop(applied);
                self.emit(
                    run_id,
                    ProcedureApplyProgress::PromotionSucceeded { result },
                );
                Ok(self.finish(run_id, terminal))
            }
            Err(PromotionError::Baseline(super::PromotionBaselineCheckError::Stale {
                stale_paths,
            })) => {
                let terminal = ProcedureTerminalDisposition::Failed {
                    reason: "promotion baseline is stale".to_string(),
                };
                self.save_terminal_verification(report_id, &mut report, &terminal)?;
                drop(applied);
                self.emit(
                    run_id,
                    ProcedureApplyProgress::ConflictDetected { paths: stale_paths },
                );
                Ok(self.finish(run_id, terminal))
            }
            Err(PromotionError::ConcurrentEdit {
                stale_paths,
                recovery,
            }) if recovery.rollback_succeeded() => {
                let terminal = ProcedureTerminalDisposition::Failed {
                    reason: "promotion endpoint became stale".to_string(),
                };
                self.save_terminal_verification(report_id, &mut report, &terminal)?;
                drop(applied);
                self.emit(
                    run_id,
                    ProcedureApplyProgress::ConflictDetected { paths: stale_paths },
                );
                Ok(self.finish(run_id, terminal))
            }
            Err(error) => {
                let terminal = ProcedureTerminalDisposition::Failed {
                    reason: error.to_string(),
                };
                self.save_terminal_verification(report_id, &mut report, &terminal)?;
                let recovery = error.recovery();
                self.emit(
                    run_id,
                    ProcedureApplyProgress::PromotionFailed {
                        message: error.to_string(),
                        recovery,
                    },
                );
                drop(applied);
                Ok(self.finish(run_id, terminal))
            }
        }
    }

    #[cfg(feature = "test-support")]
    fn interruptible_promotion(
        &self,
        run_id: super::ProcedureRunId,
        applied: &super::AppliedPatchWorkspace,
        baseline: &PromotionBaseline,
        targets: &[super::PromotionTarget],
        injection: Option<super::PromotionFailureInjection>,
    ) -> Result<super::PromotionResult, PromotionError> {
        self.emit(run_id, ProcedureApplyProgress::PromotionStarted);
        match injection {
            Some(injection) => super::promote_verified_workspace_with_failure_injection(
                &self.project_root,
                applied.path(),
                baseline,
                targets,
                injection,
            ),
            None => {
                promote_verified_workspace(&self.project_root, applied.path(), baseline, targets)
            }
        }
    }

    #[cfg(not(feature = "test-support"))]
    fn interruptible_promotion(
        &self,
        run_id: super::ProcedureRunId,
        applied: &super::AppliedPatchWorkspace,
        baseline: &PromotionBaseline,
        targets: &[super::PromotionTarget],
    ) -> Result<super::PromotionResult, PromotionError> {
        self.emit(run_id, ProcedureApplyProgress::PromotionStarted);
        promote_verified_workspace(&self.project_root, applied.path(), baseline, targets)
    }

    fn emit(&self, run_id: super::ProcedureRunId, progress: ProcedureApplyProgress) {
        if let Some(sender) = &self.progress {
            let _ = sender.send(ProcedureProgress::Apply {
                run_id,
                progress: Box::new(progress),
            });
        }
    }

    fn save_verification(
        &self,
        localization_run_id: &super::ProcedureRunId,
        report: &VerifierReport,
    ) -> Result<(), ProcedureApplyError> {
        self.reports
            .save_verification(localization_run_id, report)
            .map_err(|source| ProcedureApplyError::ReportSave {
                run_id: localization_run_id.as_str(),
                source,
            })
    }

    fn save_terminal_verification(
        &self,
        localization_run_id: &super::ProcedureRunId,
        report: &mut VerifierReport,
        terminal: &ProcedureTerminalDisposition,
    ) -> Result<(), ProcedureApplyError> {
        report.terminal_disposition = Some(terminal.clone());
        self.save_verification(localization_run_id, report)
    }

    fn finish(
        &self,
        run_id: super::ProcedureRunId,
        disposition: ProcedureTerminalDisposition,
    ) -> ProcedureTerminalDisposition {
        self.emit(
            run_id,
            ProcedureApplyProgress::Finished {
                disposition: disposition.clone(),
            },
        );
        disposition
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }
}

impl From<PatchApplyCheckError> for ProcedureApplyError {
    fn from(error: PatchApplyCheckError) -> Self {
        Self::PatchApply(Box::new(error))
    }
}

fn patch_failure_report(
    patch_gates: Vec<PatchGateEvidence>,
    verifier_commands: &[String],
    eligibility: CandidateEligibility,
    terminal_disposition: ProcedureTerminalDisposition,
) -> VerifierReport {
    VerifierReport {
        patch_gates,
        gates: verifier_commands
            .iter()
            .map(|command| VerifierGateEvidence {
                command: command.clone(),
                disposition: VerifierGateDisposition::NotRunAfterPatch {
                    blocked_by: eligibility_patch_phase(&eligibility),
                },
                result: None,
            })
            .collect(),
        stopped_after_failure: true,
        first_failed_gate: None,
        eligibility,
        terminal_disposition: Some(terminal_disposition),
    }
}

fn eligibility_patch_phase(eligibility: &CandidateEligibility) -> GitApplyPhase {
    match eligibility.reason.as_ref() {
        Some(CandidateIneligibility::PatchApplyFailed)
        | Some(CandidateIneligibility::PatchGateInfrastructureFailed {
            phase: GitApplyPhase::Apply,
            ..
        }) => GitApplyPhase::Apply,
        _ => GitApplyPhase::Check,
    }
}

fn decode_preview_patch(
    preview: &super::PatchPreview,
) -> Result<super::PatchCandidate, ProcedureApplyError> {
    let envelope = PatchEnvelope {
        targets: preview.targets.clone(),
        rationale: preview.rationale.clone(),
        route: PatchRouteMetadata::from(preview.route.clone()),
        unified_diff: preview.unified_diff.clone(),
    };
    let encoded = serde_json::to_string(&envelope)
        .map_err(|error| ProcedureApplyError::PatchDecode(error.to_string()))?;
    decode_patch_envelope(&encoded)
        .map_err(|error| ProcedureApplyError::PatchDecode(error.to_string()))
}
