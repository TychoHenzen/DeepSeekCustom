use std::fmt;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::recovery::{
    ProjectIdentity, RepositoryIdentity, RepositoryProvider, WorkItemIdentity, WorkItemKind,
    sanitize_text,
};
use crate::session::now_timestamp;

pub const MAX_WORKFLOW_TEXT_CHARS: usize = 2_000;
pub const MAX_WORKFLOW_EVIDENCE: usize = 64;
pub const MAX_WORKFLOW_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkflowRunId(Uuid);

impl WorkflowRunId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_str(&self) -> String {
        self.0.to_string()
    }

    pub fn parse(value: &str) -> Result<Self, uuid::Error> {
        if value.contains("..") || value.contains('/') || value.contains('\\') {
            return Err(Uuid::parse_str("").expect_err("an empty UUID must be invalid"));
        }
        Uuid::parse_str(value).map(Self)
    }
}

impl Default for WorkflowRunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for WorkflowRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunState {
    Queued,
    Claimed,
    Running,
    Retryable,
    AwaitingApproval,
    Blocked,
    Questions,
    Completed,
    Failed,
    Interrupted,
    Closed,
}

impl WorkflowRunState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Closed)
    }

    pub const fn is_waiting(self) -> bool {
        matches!(
            self,
            Self::AwaitingApproval | Self::Blocked | Self::Questions
        )
    }

    pub const fn needs_explicit_resume(self) -> bool {
        matches!(self, Self::Interrupted)
    }
}

impl fmt::Display for WorkflowRunState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Running => "running",
            Self::Retryable => "retryable",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Blocked => "blocked",
            Self::Questions => "questions",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Closed => "closed",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStep {
    Capture,
    Refine,
    Implement,
    DraftPullRequest,
    Review,
    FixFindings,
    CompletionGate,
}

impl WorkflowStep {
    pub const fn first() -> Self {
        Self::Capture
    }

    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Capture => Some(Self::Refine),
            Self::Refine => Some(Self::Implement),
            Self::Implement => Some(Self::DraftPullRequest),
            Self::DraftPullRequest => Some(Self::Review),
            Self::Review => Some(Self::FixFindings),
            Self::FixFindings => Some(Self::CompletionGate),
            Self::CompletionGate => None,
        }
    }

    pub const fn is_human_gate(self) -> bool {
        matches!(self, Self::CompletionGate)
    }
}

impl fmt::Display for WorkflowStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Capture => "capture",
            Self::Refine => "refine",
            Self::Implement => "implement",
            Self::DraftPullRequest => "draft_pull_request",
            Self::Review => "review",
            Self::FixFindings => "fix_findings",
            Self::CompletionGate => "completion_gate",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowIdentity {
    pub repository: RepositoryIdentity,
    pub project: ProjectIdentity,
    pub item: WorkItemIdentity,
    pub project_root: String,
    pub working_dir: String,
    pub branch: String,
    pub revision: Option<String>,
}

impl WorkflowIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn github(
        owner: impl Into<String>,
        repository: impl Into<String>,
        project: impl Into<String>,
        issue_number: u64,
        project_root: impl Into<String>,
        working_dir: impl Into<String>,
        branch: impl Into<String>,
        revision: Option<String>,
    ) -> Self {
        Self {
            repository: RepositoryIdentity {
                provider: RepositoryProvider::GitHub,
                namespace: owner.into().trim().to_ascii_lowercase(),
                name: repository.into().trim().to_ascii_lowercase(),
                project: None,
            },
            project: ProjectIdentity {
                provider: RepositoryProvider::GitHub,
                key: project.into().trim().to_ascii_lowercase(),
            },
            item: WorkItemIdentity {
                kind: WorkItemKind::Issue,
                number: issue_number,
            },
            project_root: project_root.into(),
            working_dir: working_dir.into(),
            branch: branch.into(),
            revision,
        }
    }

    pub fn key(&self) -> String {
        format!(
            "{}:{}/{}:{}:{}#{}",
            self.repository.provider,
            self.repository.namespace,
            self.repository.name,
            self.project.key,
            match self.item.kind {
                WorkItemKind::Issue => "issue",
                WorkItemKind::PullRequest => "pull_request",
            },
            self.item.number,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEvidence {
    pub source: String,
    pub detail: String,
}

impl WorkflowEvidence {
    pub fn new(source: impl AsRef<str>, detail: impl AsRef<str>) -> Self {
        Self {
            source: truncate(&sanitize_text(source.as_ref()), 200),
            detail: truncate(&sanitize_text(detail.as_ref()), MAX_WORKFLOW_TEXT_CHARS),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowFeedback {
    pub id: String,
    pub question: String,
    pub evidence: Vec<WorkflowEvidence>,
    pub requested_action: String,
    pub published_reference: Option<String>,
}

impl WorkflowFeedback {
    fn new(
        id: String,
        question: impl AsRef<str>,
        evidence: &[WorkflowEvidence],
        requested_action: impl AsRef<str>,
    ) -> Self {
        Self {
            id,
            question: truncate(&sanitize_text(question.as_ref()), MAX_WORKFLOW_TEXT_CHARS),
            evidence: evidence
                .iter()
                .take(MAX_WORKFLOW_EVIDENCE)
                .cloned()
                .collect(),
            requested_action: truncate(
                &sanitize_text(requested_action.as_ref()),
                MAX_WORKFLOW_TEXT_CHARS,
            ),
            published_reference: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDecision {
    pub id: String,
    pub question: String,
    pub requested_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRunRecord {
    pub id: WorkflowRunId,
    pub identity: WorkflowIdentity,
    pub session_id: String,
    pub context_id: String,
    pub state: WorkflowRunState,
    pub current_step: WorkflowStep,
    pub transition: u64,
    pub attempt: u32,
    pub claimed_by: Option<String>,
    pub claimed_token: Option<String>,
    pub evidence: Vec<WorkflowEvidence>,
    pub feedback: Vec<WorkflowFeedback>,
    pub pending_feedback_id: Option<String>,
    pub pending_decision: Option<WorkflowDecision>,
    pub last_error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRun {
    record: WorkflowRunRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowClaim {
    pub run_id: WorkflowRunId,
    pub transition: u64,
    pub token: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkflowStateError {
    #[error("workflow run {0} is already owned by another worker")]
    AlreadyClaimed(WorkflowRunId),
    #[error("workflow run {0} is not claimable in state {1}")]
    NotClaimable(WorkflowRunId, WorkflowRunState),
    #[error("workflow run {0} has a stale or foreign claim")]
    StaleClaim(WorkflowRunId),
    #[error("workflow run {0} has a stale transition")]
    StaleTransition(WorkflowRunId),
    #[error("workflow run {0} is not waiting for feedback")]
    NotWaitingForFeedback(WorkflowRunId),
    #[error("workflow run {0} received stale or duplicate feedback")]
    StaleFeedback(WorkflowRunId),
    #[error("workflow run {0} is not awaiting a decision")]
    NotAwaitingDecision(WorkflowRunId),
    #[error("workflow run {0} received an unknown decision")]
    UnknownDecision(WorkflowRunId),
    #[error("workflow run {0} cannot advance without evidence")]
    MissingEvidence(WorkflowRunId),
    #[error("workflow run {0} received an invalid route outcome")]
    InvalidRoute(WorkflowRunId),
    #[error("workflow run {0} cannot resume from state {1}")]
    CannotResume(WorkflowRunId, WorkflowRunState),
    #[error("invalid workflow record: {0}")]
    InvalidRecord(String),
}

impl WorkflowRun {
    pub fn new(identity: WorkflowIdentity) -> Self {
        let now = now_timestamp();
        Self {
            record: WorkflowRunRecord {
                id: WorkflowRunId::new(),
                identity,
                session_id: Uuid::new_v4().to_string(),
                context_id: Uuid::new_v4().to_string(),
                state: WorkflowRunState::Queued,
                current_step: WorkflowStep::first(),
                transition: 0,
                attempt: 0,
                claimed_by: None,
                claimed_token: None,
                evidence: Vec::new(),
                feedback: Vec::new(),
                pending_feedback_id: None,
                pending_decision: None,
                last_error: None,
                created_at: now,
                updated_at: now,
            },
        }
    }

    pub fn from_record(mut record: WorkflowRunRecord) -> Result<Self, WorkflowStateError> {
        if record.identity.repository.namespace.is_empty()
            || record.identity.repository.name.is_empty()
            || record.identity.project.key.is_empty()
            || record.identity.project_root.trim().is_empty()
            || record.identity.working_dir.trim().is_empty()
            || record.identity.branch.trim().is_empty()
        {
            return Err(WorkflowStateError::InvalidRecord(
                "workflow identity is incomplete".to_string(),
            ));
        }
        if record.session_id.trim().is_empty() || record.context_id.trim().is_empty() {
            return Err(WorkflowStateError::InvalidRecord(
                "workflow session and context ids are required".to_string(),
            ));
        }
        record.evidence.truncate(MAX_WORKFLOW_EVIDENCE);
        record.feedback.truncate(MAX_WORKFLOW_EVIDENCE);
        if record.state == WorkflowRunState::Claimed
            && (record.claimed_by.is_none() || record.claimed_token.is_none())
        {
            return Err(WorkflowStateError::InvalidRecord(
                "claimed workflow has no owner".to_string(),
            ));
        }
        Ok(Self { record })
    }

    pub fn record(&self) -> &WorkflowRunRecord {
        &self.record
    }

    pub fn id(&self) -> WorkflowRunId {
        self.record.id
    }

    pub fn state(&self) -> WorkflowRunState {
        self.record.state
    }

    pub fn current_step(&self) -> WorkflowStep {
        self.record.current_step
    }

    pub fn transition(&self) -> u64 {
        self.record.transition
    }

    pub fn session_id(&self) -> &str {
        &self.record.session_id
    }

    pub fn context_id(&self) -> &str {
        &self.record.context_id
    }

    pub fn prepare_execution_workspace(&mut self) -> Result<(), WorkflowStateError> {
        let project_root = Path::new(&self.record.identity.project_root);
        if !project_root.join(".git").exists()
            || self.record.identity.project_root != self.record.identity.working_dir
        {
            return Ok(());
        }
        let workspace = project_root
            .parent()
            .unwrap_or(project_root)
            .join(".deepseek-workflows")
            .join(self.id().as_str());
        if !workspace.exists() {
            let output = Command::new("git")
                .args([
                    "-C",
                    &self.record.identity.project_root,
                    "worktree",
                    "add",
                    "--detach",
                    &workspace.to_string_lossy(),
                    self.record.identity.revision.as_deref().unwrap_or("HEAD"),
                ])
                .output()
                .map_err(|error| {
                    WorkflowStateError::InvalidRecord(format!(
                        "could not create workflow workspace: {error}"
                    ))
                })?;
            if !output.status.success() {
                return Err(WorkflowStateError::InvalidRecord(format!(
                    "could not create workflow workspace: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        self.record.identity.working_dir = workspace.to_string_lossy().to_string();
        self.record.updated_at = now_timestamp();
        Ok(())
    }

    pub fn resume_after_restart(&mut self) -> Result<(), WorkflowStateError> {
        if !self.record.state.needs_explicit_resume() {
            return Err(WorkflowStateError::CannotResume(
                self.id(),
                self.record.state,
            ));
        }
        self.record.state = WorkflowRunState::Queued;
        self.record.claimed_by = None;
        self.record.claimed_token = None;
        self.record.context_id = Uuid::new_v4().to_string();
        self.bump_transition();
        Ok(())
    }

    pub fn normalize_after_restart(&mut self) {
        if matches!(
            self.record.state,
            WorkflowRunState::Claimed | WorkflowRunState::Running
        ) {
            self.record.state = WorkflowRunState::Interrupted;
            self.record.claimed_by = None;
            self.record.claimed_token = None;
            self.bump_transition();
        }
    }

    pub fn interrupt(&mut self) {
        if !self.record.state.is_terminal() {
            self.record.state = WorkflowRunState::Interrupted;
            self.record.claimed_by = None;
            self.record.claimed_token = None;
            self.record.pending_decision = None;
            self.bump_transition();
        }
    }

    pub fn claim(&mut self, owner: impl Into<String>) -> Result<WorkflowClaim, WorkflowStateError> {
        if self.record.claimed_by.is_some() {
            return Err(WorkflowStateError::AlreadyClaimed(self.id()));
        }
        if !matches!(
            self.record.state,
            WorkflowRunState::Queued | WorkflowRunState::Retryable
        ) {
            return Err(WorkflowStateError::NotClaimable(
                self.id(),
                self.record.state,
            ));
        }
        let token = Uuid::new_v4().to_string();
        let owner = owner.into();
        if owner.trim().is_empty() {
            return Err(WorkflowStateError::InvalidRecord(
                "workflow owner cannot be empty".to_string(),
            ));
        }
        self.record.claimed_by = Some(owner);
        self.record.claimed_token = Some(token.clone());
        self.record.state = WorkflowRunState::Claimed;
        self.bump_transition();
        Ok(WorkflowClaim {
            run_id: self.id(),
            transition: self.record.transition,
            token,
        })
    }

    pub fn start(&mut self, claim: &WorkflowClaim) -> Result<(), WorkflowStateError> {
        self.check_claim(claim)?;
        if self.record.state != WorkflowRunState::Claimed {
            return Err(WorkflowStateError::StaleClaim(self.id()));
        }
        self.record.state = WorkflowRunState::Running;
        self.bump_transition();
        Ok(())
    }

    pub fn complete_step(
        &mut self,
        claim: &WorkflowClaim,
        outcome: WorkflowStepOutcome,
    ) -> Result<(), WorkflowStateError> {
        self.check_claim(claim)?;
        if self.record.state != WorkflowRunState::Running {
            return Err(WorkflowStateError::StaleClaim(self.id()));
        }
        if outcome.evidence().is_empty() {
            return Err(WorkflowStateError::MissingEvidence(self.id()));
        }
        if matches!(outcome, WorkflowStepOutcome::AwaitingApproval { .. })
            && !self.record.current_step.is_human_gate()
        {
            return Err(WorkflowStateError::InvalidRoute(self.id()));
        }
        match outcome {
            WorkflowStepOutcome::Completed { evidence } => {
                self.append_evidence(evidence);
                if self.record.current_step == WorkflowStep::CompletionGate {
                    self.record.pending_decision = Some(WorkflowDecision {
                        id: Uuid::new_v4().to_string(),
                        question: "Authorize the verified workflow completion.".to_string(),
                        requested_action:
                            "Approve completion without bypassing repository controls.".to_string(),
                    });
                    self.record.state = WorkflowRunState::AwaitingApproval;
                    self.record.claimed_by = None;
                    self.record.claimed_token = None;
                } else {
                    self.record.current_step = self
                        .record
                        .current_step
                        .next()
                        .expect("non-terminal workflow step has a successor");
                    self.record.state = WorkflowRunState::Queued;
                    self.record.claimed_by = None;
                    self.record.claimed_token = None;
                }
                self.record.last_error = None;
            }
            WorkflowStepOutcome::Retryable { reason, evidence } => {
                self.append_evidence(evidence.clone());
                self.record.attempt = self.record.attempt.saturating_add(1);
                self.record.last_error =
                    Some(truncate(&sanitize_text(&reason), MAX_WORKFLOW_TEXT_CHARS));
                if self.record.attempt >= MAX_WORKFLOW_ATTEMPTS {
                    self.enter_feedback(
                        WorkflowRunState::Blocked,
                        "The bounded workflow retry budget was exhausted.".to_string(),
                        evidence,
                        "Review the retained failures and choose an explicit recovery action."
                            .to_string(),
                    );
                } else {
                    self.record.state = WorkflowRunState::Retryable;
                    self.record.claimed_by = None;
                    self.record.claimed_token = None;
                }
            }
            WorkflowStepOutcome::Blocked {
                question,
                evidence,
                requested_action,
            } => self.enter_feedback(
                WorkflowRunState::Blocked,
                question,
                evidence,
                requested_action,
            ),
            WorkflowStepOutcome::NeedsDecision {
                question,
                evidence,
                requested_action,
            } => self.enter_feedback(
                WorkflowRunState::Questions,
                question,
                evidence,
                requested_action,
            ),
            WorkflowStepOutcome::AwaitingApproval {
                question,
                evidence,
                requested_action,
            } => {
                self.append_evidence(evidence);
                self.record.pending_decision = Some(WorkflowDecision {
                    id: Uuid::new_v4().to_string(),
                    question: truncate(&sanitize_text(&question), MAX_WORKFLOW_TEXT_CHARS),
                    requested_action: truncate(
                        &sanitize_text(&requested_action),
                        MAX_WORKFLOW_TEXT_CHARS,
                    ),
                });
                self.record.state = WorkflowRunState::AwaitingApproval;
                self.record.claimed_by = None;
                self.record.claimed_token = None;
            }
            WorkflowStepOutcome::Failed { reason, evidence } => {
                self.append_evidence(evidence);
                self.record.last_error =
                    Some(truncate(&sanitize_text(&reason), MAX_WORKFLOW_TEXT_CHARS));
                self.record.state = WorkflowRunState::Failed;
                self.record.claimed_by = None;
                self.record.claimed_token = None;
            }
        }
        self.bump_transition();
        Ok(())
    }

    pub fn approve(&mut self, decision_id: &str) -> Result<(), WorkflowStateError> {
        self.finish_decision(decision_id, true)
    }

    pub fn reject(&mut self, decision_id: &str) -> Result<(), WorkflowStateError> {
        self.finish_decision(decision_id, false)
    }

    pub fn resume_feedback(
        &mut self,
        feedback_id: &str,
        evidence: Vec<WorkflowEvidence>,
    ) -> Result<(), WorkflowStateError> {
        if !self.record.state.is_waiting()
            || self.record.pending_feedback_id.as_deref() != Some(feedback_id)
        {
            return Err(WorkflowStateError::StaleFeedback(self.id()));
        }
        self.append_evidence(evidence);
        self.record.pending_feedback_id = None;
        self.record.state = WorkflowRunState::Queued;
        self.record.claimed_by = None;
        self.record.claimed_token = None;
        self.record.last_error = None;
        self.bump_transition();
        Ok(())
    }

    pub fn mark_feedback_published(
        &mut self,
        feedback_id: &str,
        reference: String,
    ) -> Result<(), WorkflowStateError> {
        if !matches!(
            self.record.state,
            WorkflowRunState::Blocked | WorkflowRunState::Questions
        ) || self.record.pending_feedback_id.as_deref() != Some(feedback_id)
        {
            return Err(WorkflowStateError::StaleFeedback(self.id()));
        }
        let Some(feedback) = self
            .record
            .feedback
            .iter_mut()
            .find(|feedback| feedback.id == feedback_id)
        else {
            return Err(WorkflowStateError::StaleFeedback(self.id()));
        };
        feedback.published_reference = Some(truncate(
            &sanitize_text(&reference),
            MAX_WORKFLOW_TEXT_CHARS,
        ));
        self.bump_transition();
        Ok(())
    }

    fn finish_decision(
        &mut self,
        decision_id: &str,
        approved: bool,
    ) -> Result<(), WorkflowStateError> {
        if self.record.state != WorkflowRunState::AwaitingApproval {
            return Err(WorkflowStateError::NotAwaitingDecision(self.id()));
        }
        let Some(decision) = self.record.pending_decision.as_ref() else {
            return Err(WorkflowStateError::NotAwaitingDecision(self.id()));
        };
        if decision.id != decision_id {
            return Err(WorkflowStateError::UnknownDecision(self.id()));
        }
        self.record.pending_decision = None;
        self.record.claimed_by = None;
        self.record.claimed_token = None;
        if approved {
            if self.record.current_step == WorkflowStep::CompletionGate {
                self.record.state = WorkflowRunState::Completed;
            } else {
                self.record.current_step = self
                    .record
                    .current_step
                    .next()
                    .expect("approved non-terminal workflow step has a successor");
                self.record.state = WorkflowRunState::Queued;
            }
        } else {
            self.enter_feedback(
                WorkflowRunState::Blocked,
                "The completion decision was rejected; provide the recovery action or approve the verified result.".to_string(),
                Vec::new(),
                "Answer the recovery question or authorize completion after the requested correction."
                    .to_string(),
            );
            self.record.last_error = Some("human approval was rejected".to_string());
        }
        self.bump_transition();
        Ok(())
    }

    fn enter_feedback(
        &mut self,
        state: WorkflowRunState,
        question: String,
        evidence: Vec<WorkflowEvidence>,
        requested_action: String,
    ) {
        let feedback_id = Uuid::new_v4().to_string();
        let feedback =
            WorkflowFeedback::new(feedback_id.clone(), question, &evidence, requested_action);
        self.record.feedback.push(feedback);
        self.record.feedback.truncate(MAX_WORKFLOW_EVIDENCE);
        self.record.pending_feedback_id = Some(feedback_id);
        self.record.state = state;
        self.record.claimed_by = None;
        self.record.claimed_token = None;
    }

    fn append_evidence(&mut self, evidence: Vec<WorkflowEvidence>) {
        self.record.evidence.extend(evidence);
        self.record.evidence.truncate(MAX_WORKFLOW_EVIDENCE);
    }

    fn check_claim(&self, claim: &WorkflowClaim) -> Result<(), WorkflowStateError> {
        if claim.run_id != self.id()
            || self.record.claimed_by.is_none()
            || self.record.claimed_token.as_deref() != Some(claim.token.as_str())
        {
            return Err(WorkflowStateError::StaleClaim(self.id()));
        }
        if claim.transition > self.record.transition
            || self.record.transition > claim.transition.saturating_add(1)
        {
            return Err(WorkflowStateError::StaleTransition(self.id()));
        }
        Ok(())
    }

    fn bump_transition(&mut self) {
        self.record.transition = self.record.transition.saturating_add(1);
        self.record.updated_at = now_timestamp();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowStepOutcome {
    Completed {
        evidence: Vec<WorkflowEvidence>,
    },
    Retryable {
        reason: String,
        evidence: Vec<WorkflowEvidence>,
    },
    Blocked {
        question: String,
        evidence: Vec<WorkflowEvidence>,
        requested_action: String,
    },
    NeedsDecision {
        question: String,
        evidence: Vec<WorkflowEvidence>,
        requested_action: String,
    },
    AwaitingApproval {
        question: String,
        evidence: Vec<WorkflowEvidence>,
        requested_action: String,
    },
    Failed {
        reason: String,
        evidence: Vec<WorkflowEvidence>,
    },
}

impl WorkflowStepOutcome {
    fn evidence(&self) -> &[WorkflowEvidence] {
        match self {
            Self::Completed { evidence }
            | Self::Retryable { evidence, .. }
            | Self::Blocked { evidence, .. }
            | Self::NeedsDecision { evidence, .. }
            | Self::AwaitingApproval { evidence, .. }
            | Self::Failed { evidence, .. } => evidence,
        }
    }
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut result: String = value.chars().take(limit.saturating_sub(1)).collect();
    result.push('\u{2026}');
    result
}
