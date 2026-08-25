//! End-to-end isolated Apply orchestration.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use tokio::sync::mpsc;

use super::{
    ApplyRequest, PatchApplyCheckError, PatchEnvelope, PatchRouteMetadata, ProcedureApplyProgress,
    ProcedureProgress, ProcedureTerminalDisposition, PromotionBaseline, PromotionError,
    PromotionTargetError, VerificationInputError, VerificationInputGate, VerifierCommandRunner,
    decode_patch_envelope, evaluate_applied_patch_eligibility, model_promotion_targets,
    promote_verified_workspace, validate_patch_boundary,
};

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
    PatchApply(#[from] PatchApplyCheckError),
    #[error(transparent)]
    Promotion(#[from] PromotionError),
}

/// Executes the complete isolated verification and promotion transaction.
pub struct ProcedureApplyRunner {
    input_gate: VerificationInputGate,
    project_root: PathBuf,
    interrupt: Arc<AtomicBool>,
    progress: Option<mpsc::UnboundedSender<ProcedureProgress>>,
}

impl ProcedureApplyRunner {
    pub fn new(
        input_gate: VerificationInputGate,
        project_root: PathBuf,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Self {
            input_gate,
            project_root,
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
        self.run_inner(run_id, request, verifier_commands, None)
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
        self.run_inner(run_id, request, verifier_commands, Some(injection))
            .await
    }

    async fn run_inner(
        &self,
        run_id: super::ProcedureRunId,
        request: ApplyRequest,
        verifier_commands: &[String],
        #[cfg(feature = "test-support")] injection: Option<super::PromotionFailureInjection>,
        #[cfg(not(feature = "test-support"))] _injection: Option<()>,
    ) -> Result<ProcedureTerminalDisposition, ProcedureApplyError> {
        self.emit(run_id, ProcedureApplyProgress::Started);
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
        let baseline = PromotionBaseline::capture(&self.project_root, &targets)?;

        self.emit(run_id, ProcedureApplyProgress::SnapshotStarted);
        let applied = match super::apply_patch_in_workspace(&self.project_root, boundary) {
            Ok(applied) => applied,
            Err(error) => {
                return Err(error.into());
            }
        };
        self.emit(
            run_id,
            ProcedureApplyProgress::PatchGateCompleted {
                result: applied.check_result().clone(),
            },
        );
        self.emit(
            run_id,
            ProcedureApplyProgress::PatchGateCompleted {
                result: applied.apply_result().clone(),
            },
        );

        if self.interrupted() {
            drop(applied);
            return Ok(self.finish(run_id, ProcedureTerminalDisposition::Interrupted));
        }

        let verifier = VerifierCommandRunner::with_interrupt(Arc::clone(&self.interrupt));
        for (index, command) in verifier_commands.iter().enumerate() {
            self.emit(
                run_id,
                ProcedureApplyProgress::VerifierGateStarted {
                    index,
                    command: command.clone(),
                },
            );
        }
        let verifier_run = verifier.run(applied.path(), verifier_commands).await;
        let eligibility = evaluate_applied_patch_eligibility(&applied, &verifier_run);
        let report = verifier_run.report(eligibility.clone());
        for (index, gate) in report.gates.iter().enumerate() {
            self.emit(
                run_id,
                ProcedureApplyProgress::VerifierGateCompleted {
                    index,
                    evidence: gate.clone(),
                },
            );
        }
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
            drop(applied);
            return Ok(self.finish(run_id, ProcedureTerminalDisposition::Interrupted));
        }
        if !eligibility.eligible {
            let reason = format!("verification failed: {:?}", eligibility.reason);
            drop(applied);
            return Ok(self.finish(run_id, ProcedureTerminalDisposition::Failed { reason }));
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
                drop(applied);
                self.emit(
                    run_id,
                    ProcedureApplyProgress::PromotionSucceeded { result },
                );
                Ok(self.finish(run_id, ProcedureTerminalDisposition::Succeeded))
            }
            Err(PromotionError::Baseline(super::PromotionBaselineCheckError::Stale {
                stale_paths,
            })) => {
                drop(applied);
                self.emit(
                    run_id,
                    ProcedureApplyProgress::ConflictDetected { paths: stale_paths },
                );
                Ok(self.finish(
                    run_id,
                    ProcedureTerminalDisposition::Failed {
                        reason: "promotion baseline is stale".to_string(),
                    },
                ))
            }
            Err(error) => {
                let recovery = promotion_recovery(&error);
                self.emit(
                    run_id,
                    ProcedureApplyProgress::PromotionFailed {
                        message: error.to_string(),
                        recovery,
                    },
                );
                drop(applied);
                Ok(self.finish(
                    run_id,
                    ProcedureTerminalDisposition::Failed {
                        reason: error.to_string(),
                    },
                ))
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
        _injection: Option<()>,
    ) -> Result<super::PromotionResult, PromotionError> {
        self.emit(run_id, ProcedureApplyProgress::PromotionStarted);
        promote_verified_workspace(&self.project_root, applied.path(), baseline, targets)
    }

    fn emit(&self, run_id: super::ProcedureRunId, progress: ProcedureApplyProgress) {
        if let Some(sender) = &self.progress {
            let _ = sender.send(ProcedureProgress::Apply { run_id, progress });
        }
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

fn promotion_recovery(error: &PromotionError) -> Option<super::PromotionRecoveryEvidence> {
    match error {
        PromotionError::Transaction { recovery, .. }
        | PromotionError::FinalFingerprint { recovery, .. }
        | PromotionError::FinalMismatch { recovery, .. } => Some(recovery.clone()),
        PromotionError::Baseline(_)
        | PromotionError::InvalidTargets { .. }
        | PromotionError::InvalidVerifiedResult { .. }
        | PromotionError::VerifiedFingerprint { .. }
        | PromotionError::Cleanup { .. } => None,
    }
}
