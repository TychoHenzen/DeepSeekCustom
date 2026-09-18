//! Durable, provider-neutral workflow runs and their bounded coordinator.
//!
//! The module keeps one run's state machine separate from the application
//! actor and from backend-specific process handles. Adapters can claim work,
//! publish Git feedback, and observe terminal repository state without
//! gaining authority to approve or merge anything.

use std::cmp::Ordering;

use thiserror::Error;

use crate::error::HarnessError;

pub mod github;
mod route;
mod state;
mod store;

pub use route::WorkflowRoute;
pub use state::{
    MAX_WORKFLOW_ATTEMPTS, MAX_WORKFLOW_EVIDENCE, MAX_WORKFLOW_TEXT_CHARS, WorkflowClaim,
    WorkflowDecision, WorkflowEvidence, WorkflowFeedback, WorkflowIdentity, WorkflowRun,
    WorkflowRunId, WorkflowRunRecord, WorkflowRunState, WorkflowStateError, WorkflowStep,
    WorkflowStepOutcome,
};
pub use store::WorkflowStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EligibleWorkItem {
    pub identity: WorkflowIdentity,
    pub title: String,
    pub priority: i32,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalObservation {
    Merged,
    Closed,
    Open,
    Unknown,
}

pub trait WorkflowQueue {
    fn eligible(&self) -> Result<Vec<EligibleWorkItem>, QueueError>;

    fn claim(&mut self, item: &EligibleWorkItem, run_id: WorkflowRunId) -> Result<(), QueueError>;

    fn accepts(&self, _identity: &WorkflowIdentity) -> bool {
        true
    }

    fn terminal_observation(
        &self,
        identity: &WorkflowIdentity,
    ) -> Result<TerminalObservation, QueueError>;
}

pub trait WorkflowFeedbackPort {
    fn publish_question(
        &mut self,
        identity: &WorkflowIdentity,
        feedback: &WorkflowFeedback,
    ) -> Result<String, FeedbackError>;

    fn poll(
        &mut self,
        run_id: WorkflowRunId,
        identity: &WorkflowIdentity,
    ) -> Result<Vec<WorkflowFeedbackEvent>, FeedbackError>;
}

pub trait WorkflowStepExecutor {
    fn execute(&mut self, run: &WorkflowRunRecord) -> Result<WorkflowStepOutcome, String>;
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum QueueError {
    #[error("queue adapter failed: {0}")]
    Adapter(String),
    #[error("work item is already claimed")]
    AlreadyClaimed,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FeedbackError {
    #[error("feedback adapter failed: {0}")]
    Adapter(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowFeedbackEvent {
    pub run_id: WorkflowRunId,
    pub feedback_id: String,
    pub answer: String,
    pub evidence: Vec<WorkflowEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowTick {
    Claimed {
        run_id: WorkflowRunId,
        title: String,
    },
    Progressed {
        run_id: WorkflowRunId,
    },
    Waiting {
        run_id: WorkflowRunId,
        state: WorkflowRunState,
    },
    QueueEmpty,
    CapacityReached,
}

#[derive(Debug, Error)]
pub enum WorkflowRegistryError {
    #[error(transparent)]
    Store(#[from] HarnessError),
    #[error(transparent)]
    State(#[from] WorkflowStateError),
    #[error("workflow registry capacity {capacity} reached")]
    Capacity { capacity: usize },
    #[error("workflow identity is already registered")]
    DuplicateIdentity,
    #[error("workflow run {0} was not found")]
    MissingRun(WorkflowRunId),
}

pub struct WorkflowRegistry {
    store: WorkflowStore,
    capacity: usize,
    selected_run: Option<WorkflowRunId>,
}

impl WorkflowRegistry {
    pub fn open(store: WorkflowStore, capacity: usize) -> Result<Self, WorkflowRegistryError> {
        let registry = store.load_registry()?;
        let selected_run = registry
            .selected_run
            .filter(|run_id| store.load(run_id).is_ok());
        Ok(Self {
            store,
            capacity: capacity.max(1),
            selected_run,
        })
    }

    pub fn new(store: WorkflowStore, capacity: usize) -> Result<Self, WorkflowRegistryError> {
        let _lock = store.lock_registry()?;
        let mut registry = Self::open(store, capacity)?;
        for record in registry.store.list() {
            if matches!(
                record.state,
                WorkflowRunState::Claimed | WorkflowRunState::Running
            ) {
                let mut run = WorkflowRun::from_record(record)?;
                run.normalize_after_restart();
                registry.store.save(&run)?;
            }
        }
        registry.selected_run = registry
            .store
            .load_registry()?
            .selected_run
            .filter(|run_id| registry.store.load(run_id).is_ok());
        Ok(registry)
    }

    pub fn store(&self) -> &WorkflowStore {
        &self.store
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn selected_run_id(&self) -> Option<WorkflowRunId> {
        self.selected_run
    }

    pub fn refresh_selection(&mut self) -> Result<(), WorkflowRegistryError> {
        self.selected_run = self
            .store
            .load_registry()?
            .selected_run
            .filter(|run_id| self.store.load(run_id).is_ok());
        Ok(())
    }

    pub fn selected_run(&self) -> Result<Option<WorkflowRun>, WorkflowRegistryError> {
        match self.selected_run {
            Some(run_id) => self.load(run_id).map(Some),
            None => Ok(None),
        }
    }

    pub fn runs(&self) -> Vec<WorkflowRunRecord> {
        self.store.list()
    }

    pub fn load(&self, run_id: WorkflowRunId) -> Result<WorkflowRun, WorkflowRegistryError> {
        self.store.load(&run_id).map_err(|error| match error {
            HarnessError::Io(io_error) if io_error.kind() == std::io::ErrorKind::NotFound => {
                WorkflowRegistryError::MissingRun(run_id)
            }
            other => WorkflowRegistryError::Store(other),
        })
    }

    pub fn register(&mut self, run: WorkflowRun) -> Result<(), WorkflowRegistryError> {
        let _lock = self.store.lock_registry()?;
        if self
            .store
            .list()
            .iter()
            .any(|record| record.identity.key() == run.record().identity.key())
        {
            return Err(WorkflowRegistryError::DuplicateIdentity);
        }
        let active = self
            .store
            .list()
            .iter()
            .filter(|record| !record.state.is_terminal())
            .count();
        if active >= self.capacity {
            return Err(WorkflowRegistryError::Capacity {
                capacity: self.capacity,
            });
        }
        self.store.save(&run)?;
        self.store.prune_terminal(128)?;
        if self.selected_run.is_none() {
            self.selected_run = Some(run.id());
            self.persist_selection()?;
        }
        Ok(())
    }

    pub fn save(&self, run: &WorkflowRun) -> Result<(), WorkflowRegistryError> {
        self.store.save(run).map_err(Into::into)
    }

    pub fn select(&mut self, run_id: WorkflowRunId) -> Result<(), WorkflowRegistryError> {
        let _lock = self.store.lock_registry()?;
        self.load(run_id)?;
        self.selected_run = Some(run_id);
        self.persist_selection()
    }

    pub fn clear_selection(&mut self) -> Result<(), WorkflowRegistryError> {
        let _lock = self.store.lock_registry()?;
        self.selected_run = None;
        self.persist_selection()
    }

    pub fn delete(&mut self, run_id: WorkflowRunId) -> Result<(), WorkflowRegistryError> {
        let _lock = self.store.lock_registry()?;
        self.store.delete(&run_id)?;
        if self.selected_run == Some(run_id) {
            self.selected_run = None;
            self.persist_selection()?;
        }
        Ok(())
    }

    pub fn contains_identity(&self, identity: &WorkflowIdentity) -> bool {
        self.store
            .list()
            .iter()
            .any(|record| record.identity.key() == identity.key())
    }

    fn persist_selection(&self) -> Result<(), WorkflowRegistryError> {
        self.store
            .save_registry(&store::WorkflowRegistryRecord {
                selected_run: self.selected_run,
            })
            .map_err(Into::into)
    }
}

#[derive(Debug, Error)]
pub enum WorkflowSchedulerError {
    #[error(transparent)]
    Registry(#[from] WorkflowRegistryError),
    #[error(transparent)]
    State(#[from] WorkflowStateError),
    #[error(transparent)]
    Queue(#[from] QueueError),
    #[error(transparent)]
    Feedback(#[from] FeedbackError),
    #[error("workflow step outcome does not match the canonical route")]
    InvalidRoute,
    #[error("workflow run {0} is not the selected run")]
    UnselectedRun(WorkflowRunId),
}

pub struct WorkflowScheduler<Q, F> {
    queue: Q,
    feedback: F,
    registry: WorkflowRegistry,
    worker_name: String,
    route: WorkflowRoute,
    active_claim: Option<WorkflowClaim>,
}

pub struct WorkflowWorker<Q, F, E> {
    scheduler: WorkflowScheduler<Q, F>,
    executor: E,
}

impl<Q, F, E> WorkflowWorker<Q, F, E>
where
    Q: WorkflowQueue,
    F: WorkflowFeedbackPort,
    E: WorkflowStepExecutor,
{
    pub fn new(scheduler: WorkflowScheduler<Q, F>, executor: E) -> Self {
        Self {
            scheduler,
            executor,
        }
    }

    pub fn scheduler(&self) -> &WorkflowScheduler<Q, F> {
        &self.scheduler
    }

    pub fn scheduler_mut(&mut self) -> &mut WorkflowScheduler<Q, F> {
        &mut self.scheduler
    }

    pub fn run_once(&mut self) -> Result<WorkflowTick, WorkflowSchedulerError> {
        let tick = self.scheduler.tick()?;
        let WorkflowTick::Claimed { run_id, .. } = tick.clone() else {
            return Ok(tick);
        };
        let Some(claim) = self.scheduler.active_claim(run_id) else {
            return Ok(tick);
        };
        let mut run = self.scheduler.registry().load(run_id)?;
        if let Err(error) = run.prepare_execution_workspace() {
            self.scheduler.apply_step(
                run_id,
                &claim,
                WorkflowStepOutcome::Blocked {
                    question: "The isolated workflow workspace could not be prepared.".to_string(),
                    evidence: vec![WorkflowEvidence::new("workspace", error.to_string())],
                    requested_action:
                        "Inspect the workspace error and choose an authorized recovery action."
                            .to_string(),
                },
            )?;
            return Ok(tick);
        }
        self.scheduler.registry_mut().save(&run)?;
        let outcome = match self.executor.execute(run.record()) {
            Ok(outcome) => outcome,
            Err(error) => WorkflowStepOutcome::Blocked {
                question: "The configured workflow step executor failed.".to_string(),
                evidence: vec![WorkflowEvidence::new("executor", error)],
                requested_action:
                    "Inspect the retained evidence and configure or repair the executor."
                        .to_string(),
            },
        };
        self.scheduler.apply_step(run_id, &claim, outcome)?;
        Ok(tick)
    }
}

impl<Q, F> WorkflowScheduler<Q, F>
where
    Q: WorkflowQueue,
    F: WorkflowFeedbackPort,
{
    pub fn new(
        queue: Q,
        feedback: F,
        registry: WorkflowRegistry,
        worker_name: impl Into<String>,
    ) -> Self {
        Self {
            queue,
            feedback,
            registry,
            worker_name: worker_name.into(),
            route: WorkflowRoute,
            active_claim: None,
        }
    }

    pub fn registry(&self) -> &WorkflowRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut WorkflowRegistry {
        &mut self.registry
    }

    pub fn queue(&self) -> &Q {
        &self.queue
    }

    pub fn feedback(&self) -> &F {
        &self.feedback
    }

    pub fn feedback_mut(&mut self) -> &mut F {
        &mut self.feedback
    }

    pub fn active_claim(&self, run_id: WorkflowRunId) -> Option<WorkflowClaim> {
        self.active_claim
            .as_ref()
            .filter(|claim| claim.run_id == run_id)
            .cloned()
    }

    pub fn tick(&mut self) -> Result<WorkflowTick, WorkflowSchedulerError> {
        self.registry.refresh_selection()?;
        if let Some(run_id) = self.registry.selected_run_id() {
            let run = self.registry.load(run_id)?;
            if !self.queue.accepts(&run.record().identity) {
                self.registry.clear_selection()?;
                return self.claim_next();
            }
            match run.state() {
                WorkflowRunState::Blocked | WorkflowRunState::Questions => {
                    if let Some(feedback) = run
                        .record()
                        .pending_feedback_id
                        .as_deref()
                        .and_then(|feedback_id| {
                            run.record()
                                .feedback
                                .iter()
                                .find(|feedback| feedback.id == feedback_id)
                        })
                        .filter(|feedback| feedback.published_reference.is_none())
                        .cloned()
                    {
                        let _lock = self
                            .registry
                            .store()
                            .lock_run(&run_id)
                            .map_err(WorkflowRegistryError::from)?;
                        let mut authoritative = self.registry.load(run_id)?;
                        let reference = self
                            .feedback
                            .publish_question(&authoritative.record().identity, &feedback)?;
                        authoritative.mark_feedback_published(&feedback.id, reference)?;
                        self.registry.save(&authoritative)?;
                        return Ok(WorkflowTick::Waiting {
                            run_id,
                            state: authoritative.state(),
                        });
                    }
                    let events = self.feedback.poll(run_id, &run.record().identity)?;
                    for event in events {
                        if event.run_id != run_id {
                            continue;
                        }
                        let _lock = self
                            .registry
                            .store()
                            .lock_run(&run_id)
                            .map_err(WorkflowRegistryError::from)?;
                        let mut authoritative = self.registry.load(run_id)?;
                        if authoritative
                            .resume_feedback(&event.feedback_id, feedback_evidence(&event))
                            .is_ok()
                        {
                            self.registry.save(&authoritative)?;
                            return Ok(WorkflowTick::Progressed { run_id });
                        }
                    }
                    return Ok(WorkflowTick::Waiting {
                        run_id,
                        state: run.state(),
                    });
                }
                WorkflowRunState::AwaitingApproval
                | WorkflowRunState::Failed
                | WorkflowRunState::Claimed
                | WorkflowRunState::Running => {
                    return Ok(WorkflowTick::Waiting {
                        run_id,
                        state: run.state(),
                    });
                }
                WorkflowRunState::Interrupted => {
                    return Ok(WorkflowTick::Waiting {
                        run_id,
                        state: run.state(),
                    });
                }
                WorkflowRunState::Completed | WorkflowRunState::Closed => {
                    match self.queue.terminal_observation(&run.record().identity)? {
                        TerminalObservation::Merged | TerminalObservation::Closed => {
                            self.registry.clear_selection()?;
                        }
                        TerminalObservation::Open | TerminalObservation::Unknown => {
                            return Ok(WorkflowTick::Waiting {
                                run_id,
                                state: run.state(),
                            });
                        }
                    }
                }
                WorkflowRunState::Queued | WorkflowRunState::Retryable => {}
            }

            if self.registry.selected_run_id() == Some(run_id) {
                if !self.queue.accepts(&run.record().identity) {
                    self.registry.clear_selection()?;
                    return self.claim_next();
                }
                return self.claim_selected(run);
            }
        }

        self.claim_next()
    }

    pub fn resume(
        &mut self,
        run_id: WorkflowRunId,
    ) -> Result<WorkflowRunState, WorkflowSchedulerError> {
        self.registry.select(run_id)?;
        let _lock = self
            .registry
            .store()
            .lock_run(&run_id)
            .map_err(WorkflowRegistryError::from)?;
        let mut run = self.registry.load(run_id)?;
        run.resume_after_restart()?;
        self.registry.save(&run)?;
        Ok(run.state())
    }

    pub fn apply_step(
        &mut self,
        run_id: WorkflowRunId,
        claim: &WorkflowClaim,
        outcome: WorkflowStepOutcome,
    ) -> Result<WorkflowRunState, WorkflowSchedulerError> {
        self.registry.refresh_selection()?;
        if self.registry.selected_run_id() != Some(run_id) {
            return Err(WorkflowSchedulerError::UnselectedRun(run_id));
        }
        let _lock = self
            .registry
            .store()
            .lock_run(&run_id)
            .map_err(WorkflowRegistryError::from)?;
        let mut run = self.registry.load(run_id)?;
        if !self.route.validate(run.current_step(), &outcome) {
            return Err(WorkflowSchedulerError::InvalidRoute);
        }
        run.complete_step(claim, outcome)?;
        if matches!(
            run.state(),
            WorkflowRunState::Blocked | WorkflowRunState::Questions
        ) && let Some(feedback_id) = run.record().pending_feedback_id.clone()
            && let Some(feedback) = run
                .record()
                .feedback
                .iter()
                .find(|feedback| feedback.id == feedback_id)
                .cloned()
        {
            self.registry.save(&run)?;
            let reference = self
                .feedback
                .publish_question(&run.record().identity, &feedback)?;
            run.mark_feedback_published(&feedback_id, reference)?;
        }
        self.registry.save(&run)?;
        if self
            .active_claim
            .as_ref()
            .is_some_and(|claim| claim.run_id == run_id)
            && run.state() != WorkflowRunState::Running
        {
            self.active_claim = None;
        }
        Ok(run.state())
    }

    pub fn approve(
        &mut self,
        run_id: WorkflowRunId,
        decision_id: &str,
    ) -> Result<WorkflowRunState, WorkflowSchedulerError> {
        self.registry.refresh_selection()?;
        if self.registry.selected_run_id() != Some(run_id) {
            return Err(WorkflowSchedulerError::UnselectedRun(run_id));
        }
        let _lock = self
            .registry
            .store()
            .lock_run(&run_id)
            .map_err(WorkflowRegistryError::from)?;
        let mut run = self.registry.load(run_id)?;
        run.approve(decision_id)?;
        self.registry.save(&run)?;
        Ok(run.state())
    }

    pub fn reject(
        &mut self,
        run_id: WorkflowRunId,
        decision_id: &str,
    ) -> Result<WorkflowRunState, WorkflowSchedulerError> {
        self.registry.refresh_selection()?;
        if self.registry.selected_run_id() != Some(run_id) {
            return Err(WorkflowSchedulerError::UnselectedRun(run_id));
        }
        let _lock = self
            .registry
            .store()
            .lock_run(&run_id)
            .map_err(WorkflowRegistryError::from)?;
        let mut run = self.registry.load(run_id)?;
        run.reject(decision_id)?;
        self.registry.save(&run)?;
        Ok(run.state())
    }

    pub fn apply_feedback(
        &mut self,
        event: WorkflowFeedbackEvent,
    ) -> Result<WorkflowRunState, WorkflowSchedulerError> {
        self.registry.refresh_selection()?;
        if self.registry.selected_run_id() != Some(event.run_id) {
            return Err(WorkflowStateError::StaleFeedback(event.run_id).into());
        }
        let _lock = self
            .registry
            .store()
            .lock_run(&event.run_id)
            .map_err(WorkflowRegistryError::from)?;
        let mut run = self.registry.load(event.run_id)?;
        run.resume_feedback(&event.feedback_id, feedback_evidence(&event))?;
        self.registry.save(&run)?;
        Ok(run.state())
    }

    fn claim_selected(&mut self, run: WorkflowRun) -> Result<WorkflowTick, WorkflowSchedulerError> {
        let run_id = run.id();
        let _lock = self
            .registry
            .store()
            .lock_run(&run_id)
            .map_err(WorkflowRegistryError::from)?;
        let mut run = self.registry.load(run_id)?;
        let claim = run.claim(self.worker_name.clone())?;
        run.start(&claim)?;
        self.active_claim = Some(claim);
        self.registry.save(&run)?;
        Ok(WorkflowTick::Claimed {
            run_id,
            title: run.record().identity.key(),
        })
    }

    fn claim_next(&mut self) -> Result<WorkflowTick, WorkflowSchedulerError> {
        if self
            .registry
            .runs()
            .iter()
            .filter(|run| !run.state.is_terminal())
            .count()
            >= self.registry.capacity()
        {
            return Ok(WorkflowTick::CapacityReached);
        }
        let mut candidates = self.queue.eligible()?;
        candidates.sort_by(compare_candidates);
        for candidate in candidates {
            if !self.queue.accepts(&candidate.identity) {
                continue;
            }
            if self.registry.contains_identity(&candidate.identity) {
                continue;
            }
            let run = WorkflowRun::new(candidate.identity.clone());
            let run_id = run.id();
            self.registry.register(run)?;
            match self.queue.claim(&candidate, run_id) {
                Ok(()) => {}
                Err(QueueError::AlreadyClaimed) => {
                    self.registry.delete(run_id)?;
                    continue;
                }
                Err(error) => {
                    self.registry.delete(run_id)?;
                    return Err(error.into());
                }
            }
            let run = self.registry.load(run_id)?;
            return self.claim_selected(run).map(|tick| match tick {
                WorkflowTick::Claimed { run_id, .. } => WorkflowTick::Claimed {
                    run_id,
                    title: candidate.title,
                },
                other => other,
            });
        }
        Ok(WorkflowTick::QueueEmpty)
    }
}

fn compare_candidates(left: &EligibleWorkItem, right: &EligibleWorkItem) -> Ordering {
    left.priority
        .cmp(&right.priority)
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.identity.item.number.cmp(&right.identity.item.number))
}

fn feedback_evidence(event: &WorkflowFeedbackEvent) -> Vec<WorkflowEvidence> {
    let mut evidence = event.evidence.clone();
    if !event.answer.trim().is_empty() {
        evidence.push(WorkflowEvidence::new("git-feedback", &event.answer));
    }
    evidence
}
