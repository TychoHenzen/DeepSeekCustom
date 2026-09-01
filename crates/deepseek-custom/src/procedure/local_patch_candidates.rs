//! Bounded local patch candidates and their isolated verifier evidence.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    CandidateEligibility, CandidateIneligibility, LocalPatchDraftDispatch, PatchApplyCheckError,
    PatchCandidate, PatchGateEvidence, VerifierCommandRunner, VerifierReport,
    apply_patch_in_workspace, evaluate_applied_patch_eligibility, validate_patch_boundary,
};
use crate::config::settings::ValidatedProcedureSamplingSettings;

/// Durable evidence for one numbered local candidate request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCandidateGenerationEvidence {
    pub index: u8,
    pub diversity_hint: String,
    pub backend: String,
    pub model: String,
}

/// A successfully decoded, numbered local patch candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPatchCandidate {
    pub evidence: LocalCandidateGenerationEvidence,
    pub patch: PatchCandidate,
}

/// One finished or failed bounded candidate-generation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalPatchCandidateGeneration {
    Completed(LocalPatchCandidate),
    Failed {
        evidence: LocalCandidateGenerationEvidence,
        error: String,
    },
    Interrupted {
        evidence: LocalCandidateGenerationEvidence,
    },
}

impl LocalPatchCandidateGeneration {
    pub fn evidence(&self) -> &LocalCandidateGenerationEvidence {
        match self {
            Self::Completed(candidate) => &candidate.evidence,
            Self::Failed { evidence, .. } | Self::Interrupted { evidence } => evidence,
        }
    }
}

/// Evidence from every configured local candidate request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPatchCandidateGenerationRun {
    pub candidates: Vec<LocalPatchCandidateGeneration>,
}

/// Generates the validated three-to-five local candidates in a stable order.
pub struct LocalPatchCandidateGenerator<'a> {
    dispatcher: &'a dyn LocalPatchDraftDispatch,
    settings: ValidatedProcedureSamplingSettings,
    interrupt: Arc<AtomicBool>,
}

impl<'a> LocalPatchCandidateGenerator<'a> {
    pub fn new(
        dispatcher: &'a dyn LocalPatchDraftDispatch,
        settings: ValidatedProcedureSamplingSettings,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Self {
            dispatcher,
            settings,
            interrupt,
        }
    }

    /// Generate every configured candidate unless shared interruption stops dispatch.
    pub async fn generate(&self, prompt: &str) -> LocalPatchCandidateGenerationRun {
        let total = self.settings.local_patch_candidate_count();
        let mut candidates = Vec::with_capacity(usize::from(total));
        for index in 1..=total {
            let evidence = self.evidence(index, total);
            if self.interrupted() {
                candidates.push(LocalPatchCandidateGeneration::Interrupted { evidence });
                break;
            }
            let candidate_prompt = candidate_prompt(prompt, &evidence.diversity_hint, index, total);
            match self.dispatcher.draft(candidate_prompt).await {
                Ok(patch) => candidates.push(LocalPatchCandidateGeneration::Completed(
                    LocalPatchCandidate { evidence, patch },
                )),
                Err(error) => candidates.push(LocalPatchCandidateGeneration::Failed {
                    evidence,
                    error: error.to_string(),
                }),
            }
        }
        LocalPatchCandidateGenerationRun { candidates }
    }

    fn evidence(&self, index: u8, total: u8) -> LocalCandidateGenerationEvidence {
        LocalCandidateGenerationEvidence {
            index,
            diversity_hint: format!("local-patch-candidate-{index}-of-{total}"),
            backend: self.dispatcher.backend_name().to_string(),
            model: self.dispatcher.model().to_string(),
        }
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }
}

fn candidate_prompt(base: &str, hint: &str, index: u8, total: u8) -> String {
    format!(
        "{base}\n\nGenerate local patch candidate {index} of {total}. Use diversity hint `{hint}` to explore a distinct valid mechanical edit."
    )
}

/// One completed candidate with its isolated patch and verifier evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCandidateVerification {
    pub candidate: LocalPatchCandidate,
    pub changed_line_count: usize,
    pub outcome: LocalCandidateVerificationOutcome,
}

/// Every completed candidate gets a verifier report, even when a patch gate prevents commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalCandidateVerificationOutcome {
    Verified {
        report: VerifierReport,
    },
    Rejected {
        report: VerifierReport,
        error: String,
    },
}

impl LocalCandidateVerificationOutcome {
    pub fn report(&self) -> &VerifierReport {
        match self {
            Self::Verified { report } | Self::Rejected { report, .. } => report,
        }
    }
}

/// Serial verification evidence for all completed candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCandidateVerificationRun {
    pub candidates: Vec<LocalCandidateVerification>,
}

/// Deterministic next action after every generated candidate was verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalPatchCandidateResolution {
    /// A passing candidate selected by changed-line count and then generation index.
    Selected(Box<LocalCandidateVerification>),
    /// No sampled candidate passed, so the existing bounded repair ladder must begin.
    BeginExistingBoundedRepair,
}

/// Select the smallest passing patch, using the lowest generation index as a stable tie-breaker.
pub fn select_passing_local_candidate(
    verification: &LocalCandidateVerificationRun,
) -> LocalPatchCandidateResolution {
    verification
        .candidates
        .iter()
        .filter(|candidate| candidate_passed(candidate))
        .min_by_key(|candidate| {
            (
                candidate.changed_line_count,
                candidate.candidate.evidence.index,
            )
        })
        .cloned()
        .map_or(
            LocalPatchCandidateResolution::BeginExistingBoundedRepair,
            |candidate| LocalPatchCandidateResolution::Selected(Box::new(candidate)),
        )
}

/// Begin the pre-existing bounded repair and frontier policy only after every sampled candidate fails.
///
/// The caller owns the existing policy and dispatcher, so this handoff cannot alter either budget.
pub fn begin_existing_bounded_repair<T>(
    resolution: &LocalPatchCandidateResolution,
    begin_repair: impl FnOnce() -> T,
) -> Option<T> {
    matches!(
        resolution,
        LocalPatchCandidateResolution::BeginExistingBoundedRepair
    )
    .then(begin_repair)
}

/// Verifies completed candidates one at a time in new disposable workspaces.
pub struct LocalPatchCandidateVerifier {
    project_root: PathBuf,
    targets: Vec<String>,
    verifier_commands: Vec<String>,
    interrupt: Arc<AtomicBool>,
}

impl LocalPatchCandidateVerifier {
    pub fn new(
        project_root: PathBuf,
        targets: Vec<String>,
        verifier_commands: Vec<String>,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Self {
            project_root,
            targets,
            verifier_commands,
            interrupt,
        }
    }

    /// Verify only completed envelopes in generation order. This deliberately does not select a winner.
    pub async fn verify(
        &self,
        generation: &LocalPatchCandidateGenerationRun,
    ) -> LocalCandidateVerificationRun {
        let mut candidates = Vec::new();
        for generated in &generation.candidates {
            let LocalPatchCandidateGeneration::Completed(candidate) = generated else {
                continue;
            };
            candidates.push(self.verify_one(candidate.clone()).await);
        }
        LocalCandidateVerificationRun { candidates }
    }

    async fn verify_one(&self, candidate: LocalPatchCandidate) -> LocalCandidateVerification {
        let changed_line_count = candidate.patch.changed_line_count();
        let boundary = match validate_patch_boundary(candidate.patch.clone(), &self.targets) {
            Ok(boundary) => boundary,
            Err(error) => {
                return rejected(candidate, changed_line_count, error.to_string(), Vec::new());
            }
        };
        let applied = match apply_patch_in_workspace(&self.project_root, boundary) {
            Ok(applied) => applied,
            Err(error) => {
                let gates = patch_gate_evidence(&error);
                return rejected(candidate, changed_line_count, error.to_string(), gates);
            }
        };
        let verifier_run = VerifierCommandRunner::with_interrupt(Arc::clone(&self.interrupt))
            .run(applied.path(), &self.verifier_commands)
            .await;
        let eligibility = evaluate_applied_patch_eligibility(&applied, &verifier_run);
        let report = verifier_run.report(eligibility).with_patch_gates(vec![
            applied.check_result().evidence(),
            applied.apply_result().evidence(),
        ]);
        let close = applied.close();
        match close {
            Ok(()) => LocalCandidateVerification {
                candidate,
                changed_line_count,
                outcome: LocalCandidateVerificationOutcome::Verified { report },
            },
            Err(error) => rejected(
                candidate,
                changed_line_count,
                error.to_string(),
                report.patch_gates,
            ),
        }
    }
}

fn candidate_passed(candidate: &LocalCandidateVerification) -> bool {
    matches!(
        candidate.outcome,
        LocalCandidateVerificationOutcome::Verified { ref report } if report.eligibility.eligible
    )
}

fn rejected(
    candidate: LocalPatchCandidate,
    changed_line_count: usize,
    error: String,
    patch_gates: Vec<PatchGateEvidence>,
) -> LocalCandidateVerification {
    LocalCandidateVerification {
        candidate,
        changed_line_count,
        outcome: LocalCandidateVerificationOutcome::Rejected {
            report: VerifierReport {
                patch_gates,
                gates: Vec::new(),
                stopped_after_failure: false,
                first_failed_gate: None,
                eligibility: CandidateEligibility {
                    eligible: false,
                    reason: Some(CandidateIneligibility::PatchCheckFailed),
                },
                terminal_disposition: None,
            },
            error,
        },
    }
}

fn patch_gate_evidence(error: &PatchApplyCheckError) -> Vec<PatchGateEvidence> {
    error.gate_evidence()
}
