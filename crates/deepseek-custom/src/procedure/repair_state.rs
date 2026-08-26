//! Explicit transition authority for bounded local repair and frontier escalation.

use std::fmt;

use crate::config::settings::ValidatedProcedureRepairPolicy;

use super::{StructuralFailureCategory, ValidatedRepairInput};

/// Tier that owns the current candidate attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairTier {
    Local,
    Frontier,
}

impl fmt::Display for RepairTier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Local => "local",
            Self::Frontier => "frontier",
        })
    }
}

/// Stable caller-supplied identity for one candidate dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairCandidateId(String);

impl RepairCandidateId {
    pub fn new(value: impl Into<String>) -> Result<Self, AttemptTransitionError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(AttemptTransitionError::new(
                "repair candidate identity must not be blank",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Typed class of deterministic failure accumulated by the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptFailureKind {
    Structural,
    Verifier,
}

impl fmt::Display for AttemptFailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Structural => "structural",
            Self::Verifier => "verifier",
        })
    }
}

/// Evidence supplied to one failure transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptFailureEvidence {
    kind: AttemptFailureKind,
    structural_category: Option<StructuralFailureCategory>,
    command: Option<String>,
    exit_code: Option<i32>,
    diagnostic: String,
}

impl AttemptFailureEvidence {
    pub fn structural(diagnostic: impl Into<String>) -> Self {
        Self {
            kind: AttemptFailureKind::Structural,
            structural_category: None,
            command: None,
            exit_code: None,
            diagnostic: diagnostic.into(),
        }
    }

    pub(crate) fn classified_structural(
        category: StructuralFailureCategory,
        diagnostic: impl Into<String>,
    ) -> Self {
        Self {
            kind: AttemptFailureKind::Structural,
            structural_category: Some(category),
            command: None,
            exit_code: None,
            diagnostic: diagnostic.into(),
        }
    }

    pub fn verifier(
        command: impl Into<String>,
        exit_code: Option<i32>,
        diagnostic: impl Into<String>,
    ) -> Self {
        Self {
            kind: AttemptFailureKind::Verifier,
            structural_category: None,
            command: Some(command.into()),
            exit_code,
            diagnostic: diagnostic.into(),
        }
    }

    pub fn kind(&self) -> AttemptFailureKind {
        self.kind
    }

    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    pub fn structural_category(&self) -> Option<StructuralFailureCategory> {
        self.structural_category
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
}

/// One failure bound to the exact tier, attempt, and candidate that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptFailure {
    tier: RepairTier,
    attempt_index: u8,
    candidate: RepairCandidateId,
    evidence: AttemptFailureEvidence,
}

impl AttemptFailure {
    pub fn tier(&self) -> RepairTier {
        self.tier
    }

    pub fn attempt_index(&self) -> u8 {
        self.attempt_index
    }

    pub fn candidate(&self) -> &RepairCandidateId {
        &self.candidate
    }

    pub fn evidence(&self) -> &AttemptFailureEvidence {
        &self.evidence
    }
}

/// Current ladder disposition. Only the last four variants are terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptDisposition {
    Ready,
    CandidateActive,
    LocalExhausted,
    FrontierExhausted,
    Promoted,
    Blocked { reason: String },
    Failed { reason: String },
    Interrupted,
}

impl AttemptDisposition {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Promoted | Self::Blocked { .. } | Self::Failed { .. } | Self::Interrupted
        )
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::CandidateActive => "candidate_active",
            Self::LocalExhausted => "local_exhausted",
            Self::FrontierExhausted => "frontier_exhausted",
            Self::Promoted => "promoted",
            Self::Blocked { .. } => "blocked",
            Self::Failed { .. } => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

impl fmt::Display for AttemptDisposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Deterministic refusal of an unlisted or out-of-order transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptTransitionError {
    message: String,
}

impl AttemptTransitionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AttemptTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AttemptTransitionError {}

/// Complete finite state for one validated bounded-repair ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptState {
    tier: RepairTier,
    attempt_index: u8,
    structural_retry_count: u8,
    last_candidate: Option<RepairCandidateId>,
    failures: Vec<AttemptFailure>,
    policy: ValidatedProcedureRepairPolicy,
    disposition: AttemptDisposition,
}

impl AttemptState {
    /// Construct attempt state only after the named localization and patch input passed its gate.
    pub fn from_validated_input(
        _input: &ValidatedRepairInput,
        policy: ValidatedProcedureRepairPolicy,
    ) -> Self {
        Self {
            tier: RepairTier::Local,
            attempt_index: 1,
            structural_retry_count: 0,
            last_candidate: None,
            failures: Vec::new(),
            policy,
            disposition: AttemptDisposition::Ready,
        }
    }

    pub fn tier(&self) -> RepairTier {
        self.tier
    }

    pub fn attempt_index(&self) -> u8 {
        self.attempt_index
    }

    pub fn structural_retry_count(&self) -> u8 {
        self.structural_retry_count
    }

    pub fn last_candidate(&self) -> Option<&RepairCandidateId> {
        self.last_candidate.as_ref()
    }

    pub fn failures(&self) -> &[AttemptFailure] {
        &self.failures
    }

    pub fn policy(&self) -> &ValidatedProcedureRepairPolicy {
        &self.policy
    }

    pub fn disposition(&self) -> &AttemptDisposition {
        &self.disposition
    }

    /// Dispatch the first or next verifier-counted local candidate.
    pub fn start_local_candidate(
        &mut self,
        candidate: RepairCandidateId,
    ) -> Result<(), AttemptTransitionError> {
        self.reject_terminal_dispatch(RepairTier::Local)?;
        self.require("start_local_candidate", RepairTier::Local, &["ready"])?;
        self.last_candidate = Some(candidate);
        self.disposition = AttemptDisposition::CandidateActive;
        Ok(())
    }

    /// Record one structural failure and dispatch its bounded local retry.
    pub fn retry_structural(
        &mut self,
        failure: AttemptFailureEvidence,
        retry_candidate: RepairCandidateId,
    ) -> Result<(), AttemptTransitionError> {
        self.reject_terminal_dispatch(RepairTier::Local)?;
        self.require("retry_structural", RepairTier::Local, &["candidate_active"])?;
        self.require_failure_kind("retry_structural", &failure, AttemptFailureKind::Structural)?;
        if self.structural_retry_count >= self.policy.structural_retries() {
            return Err(AttemptTransitionError::new(format!(
                "attempt transition `retry_structural` exhausted structural retry budget {}",
                self.policy.structural_retries()
            )));
        }
        self.push_failure(failure)?;
        self.structural_retry_count += 1;
        self.last_candidate = Some(retry_candidate);
        Ok(())
    }

    /// Record the final structural failure and route according to frontier policy.
    pub fn structural_retry_exhausted(
        &mut self,
        failure: AttemptFailureEvidence,
    ) -> Result<(), AttemptTransitionError> {
        self.reject_terminal_dispatch(RepairTier::Local)?;
        self.require(
            "structural_retry_exhausted",
            RepairTier::Local,
            &["candidate_active"],
        )?;
        self.require_failure_kind(
            "structural_retry_exhausted",
            &failure,
            AttemptFailureKind::Structural,
        )?;
        if self.structural_retry_count < self.policy.structural_retries() {
            return Err(AttemptTransitionError::new(format!(
                "attempt transition `structural_retry_exhausted` requires structural retry budget {} to be consumed; consumed {}",
                self.policy.structural_retries(),
                self.structural_retry_count
            )));
        }
        self.push_failure(failure)?;
        if self.policy.frontier_attempts() > 0 {
            self.tier = RepairTier::Frontier;
            self.attempt_index = 1;
            self.disposition = AttemptDisposition::Ready;
        } else {
            self.disposition = AttemptDisposition::Blocked {
                reason: "structural retry exhausted and frontier escalation is disabled"
                    .to_string(),
            };
        }
        Ok(())
    }

    /// Record a local verifier failure and either prepare the next local attempt or exhaust it.
    pub fn local_verifier_failure(
        &mut self,
        failure: AttemptFailureEvidence,
    ) -> Result<(), AttemptTransitionError> {
        self.require(
            "local_verifier_failure",
            RepairTier::Local,
            &["candidate_active"],
        )?;
        self.require_failure_kind(
            "local_verifier_failure",
            &failure,
            AttemptFailureKind::Verifier,
        )?;
        self.push_failure(failure)?;
        if self.attempt_index < self.policy.local_verifier_attempts() {
            self.attempt_index += 1;
            self.disposition = AttemptDisposition::Ready;
        } else {
            self.disposition = AttemptDisposition::LocalExhausted;
        }
        Ok(())
    }

    /// Dispatch the first or next candidate through the frontier escalation tier.
    pub fn escalate_to_frontier(
        &mut self,
        candidate: RepairCandidateId,
    ) -> Result<(), AttemptTransitionError> {
        self.reject_terminal_dispatch(RepairTier::Frontier)?;
        if self.policy.frontier_attempts() == 0 {
            return Err(AttemptTransitionError::new(
                "frontier escalation is disabled by procedure.frontier_attempts = 0",
            ));
        }
        match (self.tier, self.disposition.name()) {
            (RepairTier::Local, "local_exhausted") => {
                self.tier = RepairTier::Frontier;
                self.attempt_index = 1;
            }
            (RepairTier::Frontier, "ready") => {}
            _ => return Err(self.illegal_transition("escalate_to_frontier")),
        }
        self.last_candidate = Some(candidate);
        self.disposition = AttemptDisposition::CandidateActive;
        Ok(())
    }

    /// Record a frontier verifier failure and either prepare its next attempt or exhaust it.
    pub fn frontier_verifier_failure(
        &mut self,
        failure: AttemptFailureEvidence,
    ) -> Result<(), AttemptTransitionError> {
        self.require(
            "frontier_verifier_failure",
            RepairTier::Frontier,
            &["candidate_active"],
        )?;
        self.require_failure_kind(
            "frontier_verifier_failure",
            &failure,
            AttemptFailureKind::Verifier,
        )?;
        self.push_failure(failure)?;
        if self.attempt_index < self.policy.frontier_attempts() {
            self.attempt_index += 1;
            self.disposition = AttemptDisposition::Ready;
        } else {
            self.disposition = AttemptDisposition::FrontierExhausted;
        }
        Ok(())
    }

    /// Record successful promotion of the active candidate.
    pub fn promote(&mut self) -> Result<(), AttemptTransitionError> {
        self.require_any_tier("promote", &["candidate_active"])?;
        self.disposition = AttemptDisposition::Promoted;
        Ok(())
    }

    /// Stop for human action from any nonterminal state.
    pub fn block(&mut self, reason: impl Into<String>) -> Result<(), AttemptTransitionError> {
        self.require_nonterminal("block")?;
        self.disposition = AttemptDisposition::Blocked {
            reason: reason.into(),
        };
        Ok(())
    }

    /// Stop on a non-repairable system failure from any nonterminal state.
    pub fn fail(&mut self, reason: impl Into<String>) -> Result<(), AttemptTransitionError> {
        self.require_nonterminal("fail")?;
        self.disposition = AttemptDisposition::Failed {
            reason: reason.into(),
        };
        Ok(())
    }

    /// Stop current and later work from any nonterminal state.
    pub fn interrupt(&mut self) -> Result<(), AttemptTransitionError> {
        self.require_nonterminal("interrupt")?;
        self.disposition = AttemptDisposition::Interrupted;
        Ok(())
    }

    fn push_failure(
        &mut self,
        evidence: AttemptFailureEvidence,
    ) -> Result<(), AttemptTransitionError> {
        let candidate = self.last_candidate.clone().ok_or_else(|| {
            AttemptTransitionError::new(format!(
                "attempt transition cannot record {} failure without an active candidate identity",
                evidence.kind()
            ))
        })?;
        self.failures.push(AttemptFailure {
            tier: self.tier,
            attempt_index: self.attempt_index,
            candidate,
            evidence,
        });
        Ok(())
    }

    fn require_failure_kind(
        &self,
        transition: &str,
        failure: &AttemptFailureEvidence,
        required: AttemptFailureKind,
    ) -> Result<(), AttemptTransitionError> {
        if failure.kind() != required {
            return Err(AttemptTransitionError::new(format!(
                "attempt transition `{transition}` requires {required} failure evidence, got {}",
                failure.kind()
            )));
        }
        Ok(())
    }

    fn reject_terminal_dispatch(&self, tier: RepairTier) -> Result<(), AttemptTransitionError> {
        if self.disposition.is_terminal() {
            return Err(AttemptTransitionError::new(format!(
                "cannot dispatch {tier} candidate from terminal disposition `{}`",
                self.disposition
            )));
        }
        Ok(())
    }

    fn require(
        &self,
        transition: &str,
        tier: RepairTier,
        dispositions: &[&str],
    ) -> Result<(), AttemptTransitionError> {
        if self.tier == tier && dispositions.contains(&self.disposition.name()) {
            return Ok(());
        }
        Err(self.illegal_transition(transition))
    }

    fn require_any_tier(
        &self,
        transition: &str,
        dispositions: &[&str],
    ) -> Result<(), AttemptTransitionError> {
        if dispositions.contains(&self.disposition.name()) {
            return Ok(());
        }
        Err(self.illegal_transition(transition))
    }

    fn require_nonterminal(&self, transition: &str) -> Result<(), AttemptTransitionError> {
        if !self.disposition.is_terminal() {
            return Ok(());
        }
        Err(self.illegal_transition(transition))
    }

    fn illegal_transition(&self, transition: &str) -> AttemptTransitionError {
        AttemptTransitionError::new(format!(
            "attempt transition `{transition}` is not allowed from disposition `{}` at {} attempt {}",
            self.disposition, self.tier, self.attempt_index
        ))
    }
}
