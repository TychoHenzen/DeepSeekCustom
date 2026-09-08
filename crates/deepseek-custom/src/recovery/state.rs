//! Durable recovery-run state and guarded transitions.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::diagnosis::{
    DiagnosisOutcome, DiagnosticResponse, FailureContext, MAX_PERMITTED_RETRIES, SafeRetrySpec,
    sanitize_text,
};
use super::identity::{
    ProjectIdentity, RepositoryIdentity, RepositoryProvider, ResolvedWorkIdentity, WorkItemIdentity,
};

/// Identifies one recovery run across process restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecoveryRunId(Uuid);

impl RecoveryRunId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_str(&self) -> String {
        self.0.to_string()
    }
}

impl Default for RecoveryRunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RecoveryRunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// Durable status of one recovery run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    Ready,
    Diagnosing,
    Resolved,
    Retryable,
    Blocked,
    NeedsDecision,
}

impl fmt::Display for RecoveryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Ready => "ready",
            Self::Diagnosing => "diagnosing",
            Self::Resolved => "resolved",
            Self::Retryable => "retryable",
            Self::Blocked => "blocked",
            Self::NeedsDecision => "needs_decision",
        };
        f.write_str(label)
    }
}

/// One retained piece of sanitized evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryEvidence {
    pub source: String,
    pub detail: String,
}

/// The result state of a safe action claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Pending,
    Succeeded,
    Failed,
}

/// One action that was claimed before its external call started. A pending
/// action is never silently replayed after restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptedAction {
    pub attempt: u32,
    pub key: String,
    pub status: AttemptStatus,
    pub detail: Option<String>,
}

/// The token that binds a diagnostic result to the run and generation that
/// requested it. A token from another run, project, or completed generation
/// cannot mutate the current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticToken {
    pub run_id: RecoveryRunId,
    pub generation: u64,
    pub provider: RepositoryProvider,
    pub repository: RepositoryIdentity,
    pub project: ProjectIdentity,
    pub item: WorkItemIdentity,
}

impl DiagnosticToken {
    fn from_identity(
        run_id: RecoveryRunId,
        generation: u64,
        identity: &ResolvedWorkIdentity,
    ) -> Self {
        Self {
            run_id,
            generation,
            provider: identity.provider,
            repository: identity.repository.clone(),
            project: identity.project.clone(),
            item: identity.item.clone(),
        }
    }

    fn matches_identity(&self, identity: &ResolvedWorkIdentity) -> bool {
        self.provider == identity.provider
            && self.repository == identity.repository
            && self.project == identity.project
            && self.item == identity.item
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ActiveDiagnosis {
    token: DiagnosticToken,
    context: FailureContext,
    permitted_retries: Vec<SafeRetrySpec>,
}

/// Retained details for the most recent diagnostic response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticRecord {
    pub token: DiagnosticToken,
    pub outcome: DiagnosisOutcome,
    pub summary: String,
    pub session_closed: bool,
}

/// Serializable state for one recovery run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryRunRecord {
    pub id: RecoveryRunId,
    pub identity: ResolvedWorkIdentity,
    pub current_step: String,
    pub status: RecoveryStatus,
    pub generation: u64,
    pub retry_count: u32,
    pub max_retry_attempts: u32,
    pub last_failure: Option<FailureContext>,
    pub last_diagnostic: Option<DiagnosticRecord>,
    pub evidence: Vec<RecoveryEvidence>,
    pub attempted_actions: Vec<AttemptedAction>,
    pub question: Option<String>,
    pub next_required_decision: Option<String>,
    pub pending_retry_key: Option<String>,
    active_diagnosis: Option<ActiveDiagnosis>,
    last_applied_token: Option<DiagnosticToken>,
}

/// An in-memory handle over a serializable recovery record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryRun {
    record: RecoveryRunRecord,
}

/// A claimed retry request passed to the owning workflow adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryClaim {
    pub run_id: RecoveryRunId,
    pub generation: u64,
    pub identity: ResolvedWorkIdentity,
    pub current_step: String,
    pub attempt: u32,
    pub key: String,
}

/// The result persisted after a safe retry call returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryResult {
    Succeeded { detail: String },
    Failed { detail: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RecoveryStateError {
    #[error("recovery run {0} is not ready for diagnosis")]
    InvalidDiagnosisState(RecoveryRunId),
    #[error("diagnostic result is stale or belongs to another recovery run")]
    StaleResult,
    #[error("diagnostic result was already applied")]
    DuplicateResult,
    #[error("diagnostic session did not close")]
    DiagnosticSessionOpen,
    #[error("diagnostic response selected an unauthorized retry key: {0}")]
    UnauthorizedRetry(String),
    #[error("recovery run {0} is not waiting for a safe retry")]
    InvalidRetryState(RecoveryRunId),
    #[error("recovery run {0} reached its retry limit")]
    RetryLimitReached(RecoveryRunId),
    #[error("safe retry for attempt {attempt} and key {key} was already claimed")]
    RetryAlreadyClaimed { attempt: u32, key: String },
    #[error("recovery run {0} has a safe retry pending external confirmation")]
    RetryAlreadyPending(RecoveryRunId),
    #[error("retry claim is stale")]
    StaleRetryClaim,
    #[error("invalid recovery run data: {0}")]
    InvalidRecord(String),
}

impl RecoveryRun {
    pub fn new(
        identity: ResolvedWorkIdentity,
        current_step: impl Into<String>,
        max_retry_attempts: u32,
    ) -> Self {
        let current_step = FailureContext::new(current_step.into(), "", &[]).current_step;
        Self {
            record: RecoveryRunRecord {
                id: RecoveryRunId::new(),
                identity,
                current_step,
                status: RecoveryStatus::Ready,
                generation: 0,
                retry_count: 0,
                max_retry_attempts,
                last_failure: None,
                last_diagnostic: None,
                evidence: Vec::new(),
                attempted_actions: Vec::new(),
                question: None,
                next_required_decision: None,
                pending_retry_key: None,
                active_diagnosis: None,
                last_applied_token: None,
            },
        }
    }

    pub fn from_record(record: RecoveryRunRecord) -> Result<Self, RecoveryStateError> {
        let mut record = record;
        record.current_step = FailureContext::new(&record.current_step, "", &[]).current_step;
        if record.current_step.trim().is_empty() {
            return Err(RecoveryStateError::InvalidRecord(
                "current_step is empty".to_string(),
            ));
        }
        if let Some(active) = &record.active_diagnosis
            && (active.token.run_id != record.id
                || !active.token.matches_identity(&record.identity))
        {
            return Err(RecoveryStateError::InvalidRecord(
                "active diagnosis does not belong to the run".to_string(),
            ));
        }
        let interrupted_diagnosis =
            record.status == RecoveryStatus::Diagnosing || record.active_diagnosis.is_some();
        let interrupted_retry = record
            .attempted_actions
            .iter()
            .any(|action| action.status == AttemptStatus::Pending);

        if interrupted_diagnosis {
            record.active_diagnosis = None;
            record.status = RecoveryStatus::NeedsDecision;
            record.question = Some(sanitize_text(
                "The diagnostic session ended before it produced a durable decision.",
            ));
            record.next_required_decision = Some(sanitize_text(
                "Review the retained failure and choose the next authorized recovery action.",
            ));
            record.evidence.push(RecoveryEvidence {
                source: "recovery".to_string(),
                detail: sanitize_text(
                    "An interrupted diagnostic was recovered as needs_decision after restart.",
                ),
            });
        }

        if interrupted_retry {
            record.status = RecoveryStatus::NeedsDecision;
            record.pending_retry_key = None;
            record.question = Some(sanitize_text(
                "A safe retry was pending when the recovery process stopped.",
            ));
            record.next_required_decision = Some(sanitize_text(
                "Confirm the external action result before authorizing another recovery attempt.",
            ));
            record.evidence.push(RecoveryEvidence {
                source: "recovery".to_string(),
                detail: sanitize_text("A pending safe retry was not replayed after restart."),
            });
        }

        Ok(Self { record })
    }

    pub fn record(&self) -> &RecoveryRunRecord {
        &self.record
    }

    pub fn id(&self) -> RecoveryRunId {
        self.record.id
    }

    pub fn identity(&self) -> &ResolvedWorkIdentity {
        &self.record.identity
    }

    pub fn status(&self) -> RecoveryStatus {
        self.record.status
    }

    pub(crate) fn replace_from(&mut self, other: Self) {
        *self = other;
    }

    pub fn begin_diagnosis(
        &mut self,
        context: FailureContext,
        permitted_retries: Vec<SafeRetrySpec>,
    ) -> Result<DiagnosticToken, RecoveryStateError> {
        if self.record.status != RecoveryStatus::Ready {
            return Err(RecoveryStateError::InvalidDiagnosisState(self.id()));
        }
        let context = FailureContext::new(
            &context.current_step,
            &context.failure,
            &context.prior_evidence,
        );
        if context.current_step != self.record.current_step {
            return Err(RecoveryStateError::InvalidRecord(
                "failure context step does not match current step".to_string(),
            ));
        }
        if permitted_retries.len() > MAX_PERMITTED_RETRIES {
            return Err(RecoveryStateError::InvalidRecord(format!(
                "permitted retry count exceeds the bound of {MAX_PERMITTED_RETRIES}"
            )));
        }
        let permitted_retries = permitted_retries
            .into_iter()
            .map(|retry| {
                SafeRetrySpec::new(retry.key, retry.description)
                    .map_err(RecoveryStateError::InvalidRecord)
            })
            .collect::<Result<Vec<_>, _>>()?;
        for retry in &permitted_retries {
            if permitted_retries
                .iter()
                .filter(|candidate| candidate.key == retry.key)
                .count()
                != 1
            {
                return Err(RecoveryStateError::InvalidRecord(format!(
                    "duplicate permitted retry key: {}",
                    retry.key
                )));
            }
        }

        self.record.generation = self.record.generation.saturating_add(1);
        let token = DiagnosticToken::from_identity(
            self.id(),
            self.record.generation,
            &self.record.identity,
        );
        self.record.last_failure = Some(context.clone());
        self.record.active_diagnosis = Some(ActiveDiagnosis {
            token: token.clone(),
            context,
            permitted_retries,
        });
        self.record.status = RecoveryStatus::Diagnosing;
        self.record.question = None;
        self.record.next_required_decision = None;
        Ok(token)
    }

    pub fn apply_diagnosis(
        &mut self,
        token: &DiagnosticToken,
        response: DiagnosticResponse,
        session_closed: bool,
    ) -> Result<RecoveryStatus, RecoveryStateError> {
        if self.record.last_applied_token.as_ref() == Some(token) {
            return Err(RecoveryStateError::DuplicateResult);
        }
        let Some(active) = self.record.active_diagnosis.clone() else {
            return Err(RecoveryStateError::StaleResult);
        };
        if active.token != *token
            || token.run_id != self.id()
            || !token.matches_identity(&self.record.identity)
        {
            return Err(RecoveryStateError::StaleResult);
        }
        if !session_closed {
            return Err(RecoveryStateError::DiagnosticSessionOpen);
        }

        let outcome = response.outcome;
        self.record.last_diagnostic = Some(DiagnosticRecord {
            token: token.clone(),
            outcome,
            summary: sanitize_text(&response.summary),
            session_closed,
        });
        self.record.last_applied_token = Some(token.clone());
        self.record.active_diagnosis = None;
        self.record.pending_retry_key = None;
        self.record.question = None;
        self.record.next_required_decision = None;
        self.record.evidence.push(RecoveryEvidence {
            source: "diagnostic".to_string(),
            detail: sanitize_text(&response.summary),
        });
        self.record.evidence.extend(
            response
                .evidence
                .into_iter()
                .map(|detail| RecoveryEvidence {
                    source: "diagnostic".to_string(),
                    detail: sanitize_text(&detail),
                }),
        );
        let question = response
            .question
            .map(|question| sanitize_text(&question))
            .filter(|question| !question.is_empty())
            .or_else(|| Some("The diagnostic did not provide a decision question.".to_string()));
        let requested_action = response
            .requested_action
            .map(|action| sanitize_text(&action))
            .filter(|action| !action.is_empty())
            .or_else(|| {
                Some("Review the retained recovery evidence and decide the next step.".to_string())
            });

        match outcome {
            DiagnosisOutcome::Resolved => {
                self.record.status = RecoveryStatus::Resolved;
            }
            DiagnosisOutcome::Retryable => {
                let key = response.retry_key.unwrap_or_default();
                if !active
                    .permitted_retries
                    .iter()
                    .any(|retry| retry.key == key)
                {
                    self.set_needs_decision(
                        "The diagnostic selected an action that this workflow did not permit.",
                        "Authorize one of the recorded safe retry keys or choose another recovery step.",
                    );
                } else {
                    self.record.status = RecoveryStatus::Retryable;
                    self.record.pending_retry_key = Some(key);
                }
            }
            DiagnosisOutcome::Blocked => {
                self.record.status = RecoveryStatus::Blocked;
                self.record.question = question;
                self.record.next_required_decision = requested_action;
            }
            DiagnosisOutcome::NeedsDecision => {
                self.record.status = RecoveryStatus::NeedsDecision;
                self.record.question = question;
                self.record.next_required_decision = requested_action;
            }
        }
        Ok(self.record.status)
    }

    /// Claim exactly one safe retry before the external action is called.
    /// Persisting the pending claim before the call prevents a duplicate after
    /// an interruption or process restart.
    pub fn claim_retry(&mut self) -> Result<RetryClaim, RecoveryStateError> {
        if self.record.status != RecoveryStatus::Retryable {
            return Err(RecoveryStateError::InvalidRetryState(self.id()));
        }
        let Some(key) = self.record.pending_retry_key.clone() else {
            return Err(RecoveryStateError::InvalidRetryState(self.id()));
        };
        if self
            .record
            .attempted_actions
            .iter()
            .any(|action| action.status == AttemptStatus::Pending)
        {
            self.set_needs_decision(
                "A safe retry is already pending and cannot be replayed.",
                "Confirm the external action result before authorizing another recovery attempt.",
            );
            return Err(RecoveryStateError::RetryAlreadyPending(self.id()));
        }
        let attempt = self.record.retry_count.saturating_add(1);
        if attempt > self.record.max_retry_attempts {
            self.set_needs_decision(
                "The permitted recovery retry bound has been reached.",
                "Choose whether to authorize a new bounded recovery attempt.",
            );
            return Err(RecoveryStateError::RetryLimitReached(self.id()));
        }
        if self
            .record
            .attempted_actions
            .iter()
            .any(|action| action.attempt == attempt && action.key == key)
        {
            return Err(RecoveryStateError::RetryAlreadyClaimed { attempt, key });
        }

        self.record.retry_count = attempt;
        self.record.attempted_actions.push(AttemptedAction {
            attempt,
            key: key.clone(),
            status: AttemptStatus::Pending,
            detail: None,
        });
        Ok(RetryClaim {
            run_id: self.id(),
            generation: self.record.generation,
            identity: self.record.identity.clone(),
            current_step: self.record.current_step.clone(),
            attempt,
            key,
        })
    }

    pub fn finish_retry(
        &mut self,
        claim: &RetryClaim,
        result: Result<String, String>,
    ) -> Result<RetryResult, RecoveryStateError> {
        if claim.run_id != self.id()
            || claim.generation != self.record.generation
            || claim.identity != self.record.identity
            || claim.current_step != self.record.current_step
            || self.record.status != RecoveryStatus::Retryable
            || self.record.retry_count != claim.attempt
            || self.record.pending_retry_key.as_deref() != Some(claim.key.as_str())
        {
            return Err(RecoveryStateError::StaleRetryClaim);
        }
        let Some(action) = self.record.attempted_actions.iter_mut().find(|action| {
            action.attempt == claim.attempt
                && action.key == claim.key
                && action.status == AttemptStatus::Pending
        }) else {
            return Err(RecoveryStateError::StaleRetryClaim);
        };

        match result {
            Ok(detail) => {
                action.status = AttemptStatus::Succeeded;
                action.detail = Some(sanitize_text(&detail));
                self.record.status = RecoveryStatus::Ready;
                self.record.pending_retry_key = None;
                self.record.evidence.push(RecoveryEvidence {
                    source: "safe_retry".to_string(),
                    detail: action.detail.clone().unwrap_or_default(),
                });
                Ok(RetryResult::Succeeded {
                    detail: action.detail.clone().unwrap_or_default(),
                })
            }
            Err(detail) => {
                let detail = sanitize_text(&detail);
                action.status = AttemptStatus::Failed;
                action.detail = Some(detail.clone());
                self.record.pending_retry_key = None;
                self.record.evidence.push(RecoveryEvidence {
                    source: "safe_retry".to_string(),
                    detail: detail.clone(),
                });
                self.set_needs_decision(
                    "The permitted safe retry failed.",
                    "Review the retained retry failure and choose the next authorized action.",
                );
                Ok(RetryResult::Failed { detail })
            }
        }
    }

    fn set_needs_decision(&mut self, question: &str, next_required_decision: &str) {
        self.record.status = RecoveryStatus::NeedsDecision;
        self.record.question = Some(sanitize_text(question));
        self.record.next_required_decision = Some(sanitize_text(next_required_decision));
        self.record.pending_retry_key = None;
    }
}
