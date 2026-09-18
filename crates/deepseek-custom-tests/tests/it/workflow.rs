use std::collections::HashMap;
use std::path::PathBuf;

use deepseek_custom::workflow::{
    EligibleWorkItem, FeedbackError, QueueError, TerminalObservation, WorkflowEvidence,
    WorkflowFeedback, WorkflowFeedbackEvent, WorkflowFeedbackPort, WorkflowIdentity, WorkflowQueue,
    WorkflowRegistry, WorkflowRoute, WorkflowRun, WorkflowRunId, WorkflowRunState,
    WorkflowScheduler, WorkflowStep, WorkflowStepExecutor, WorkflowStepOutcome, WorkflowStore,
    WorkflowTick, WorkflowWorker,
};

fn temp_dir(tag: &str) -> PathBuf {
    super::scratch_dir("dsc-workflow", tag)
}

fn identity(issue: u64, project: &str) -> WorkflowIdentity {
    let root = temp_dir(&format!("root-{issue}"));
    std::fs::create_dir_all(&root).unwrap();
    WorkflowIdentity::github(
        "TychoHenzen",
        "DeepSeekCustom",
        project,
        issue,
        root.to_string_lossy(),
        root.to_string_lossy(),
        format!("codex/{issue}-workflow"),
        Some(format!("revision-{issue}")),
    )
}

#[test]
fn persisted_running_work_becomes_interrupted_and_resumes_without_authority() {
    let directory = temp_dir("restart");
    let store = WorkflowStore::for_project(&directory);
    let mut run = WorkflowRun::new(identity(5, "project-a"));
    let claim = run.claim("worker-a").unwrap();
    run.start(&claim).unwrap();
    store.save(&run).unwrap();

    let mut restarted = store.load_after_restart(&run.id()).unwrap();
    assert_eq!(restarted.state(), WorkflowRunState::Interrupted);
    assert!(restarted.record().claimed_by.is_none());
    assert!(restarted.record().claimed_token.is_none());
    let prior_context = restarted.context_id().to_string();
    restarted.resume_after_restart().unwrap();
    assert_eq!(restarted.state(), WorkflowRunState::Queued);
    assert_ne!(restarted.context_id(), prior_context);

    std::fs::remove_dir_all(directory).ok();
}

#[test]
fn canonical_route_requires_observed_steps_and_human_completion_gate() {
    assert_eq!(
        WorkflowRoute::STEPS,
        [
            WorkflowStep::Capture,
            WorkflowStep::Refine,
            WorkflowStep::Implement,
            WorkflowStep::DraftPullRequest,
            WorkflowStep::Review,
            WorkflowStep::FixFindings,
            WorkflowStep::CompletionGate,
        ]
    );

    let mut run = WorkflowRun::new(identity(6, "project-a"));
    for step in [
        WorkflowStep::Capture,
        WorkflowStep::Refine,
        WorkflowStep::Implement,
        WorkflowStep::DraftPullRequest,
        WorkflowStep::Review,
        WorkflowStep::FixFindings,
    ] {
        assert_eq!(run.current_step(), step);
        let next_claim = run.claim("worker-a").unwrap();
        run.start(&next_claim).unwrap();
        run.complete_step(
            &next_claim,
            WorkflowStepOutcome::Completed {
                evidence: vec![WorkflowEvidence::new("test", "step completed")],
            },
        )
        .unwrap();
    }
    let gate_claim = run.claim("worker-a").unwrap();
    run.start(&gate_claim).unwrap();
    run.complete_step(
        &gate_claim,
        WorkflowStepOutcome::Completed {
            evidence: vec![WorkflowEvidence::new("test", "gate completed")],
        },
    )
    .unwrap();
    assert_eq!(run.state(), WorkflowRunState::AwaitingApproval);
    let decision_id = run.record().pending_decision.as_ref().unwrap().id.clone();
    run.approve(&decision_id).unwrap();
    assert_eq!(run.state(), WorkflowRunState::Completed);
}

#[test]
fn retryable_steps_and_duplicate_claims_are_rejected() {
    let mut run = WorkflowRun::new(identity(10, "project-a"));
    let claim = run.claim("worker-a").unwrap();
    run.start(&claim).unwrap();
    run.complete_step(
        &claim,
        WorkflowStepOutcome::Retryable {
            reason: "provider timed out".into(),
            evidence: vec![WorkflowEvidence::new("test", "timeout observed")],
        },
    )
    .unwrap();
    assert_eq!(run.state(), WorkflowRunState::Retryable);
    assert!(
        run.complete_step(
            &claim,
            WorkflowStepOutcome::Completed {
                evidence: vec![WorkflowEvidence::new("test", "duplicate rejected")],
            },
        )
        .is_err()
    );
    let retry_claim = run.claim("worker-a").unwrap();
    run.start(&retry_claim).unwrap();
    assert!(run.claim("worker-b").is_err());
    let mut current_claim = retry_claim;
    for attempt in 0..2 {
        run.complete_step(
            &current_claim,
            WorkflowStepOutcome::Retryable {
                reason: format!("retry {attempt}"),
                evidence: vec![WorkflowEvidence::new("test", "retry evidence")],
            },
        )
        .unwrap();
        if attempt == 0 {
            current_claim = run.claim("worker-a").unwrap();
            run.start(&current_claim).unwrap();
        }
    }
    assert_eq!(run.state(), WorkflowRunState::Blocked);
    assert!(run.record().pending_feedback_id.is_some());
}

#[derive(Default)]
struct FakeQueue {
    items: Vec<EligibleWorkItem>,
    claims: HashMap<String, WorkflowRunId>,
    terminal: HashMap<String, TerminalObservation>,
}

impl WorkflowQueue for FakeQueue {
    fn eligible(&self) -> Result<Vec<EligibleWorkItem>, QueueError> {
        Ok(self.items.clone())
    }

    fn claim(&mut self, item: &EligibleWorkItem, run_id: WorkflowRunId) -> Result<(), QueueError> {
        let key = item.identity.key();
        if self.claims.insert(key, run_id).is_some() {
            return Err(QueueError::AlreadyClaimed);
        }
        Ok(())
    }

    fn terminal_observation(
        &self,
        identity: &WorkflowIdentity,
    ) -> Result<TerminalObservation, QueueError> {
        Ok(self
            .terminal
            .get(&identity.key())
            .copied()
            .unwrap_or(TerminalObservation::Open))
    }
}

#[derive(Default)]
struct FakeFeedback {
    published: Vec<(String, String)>,
    events: Vec<WorkflowFeedbackEvent>,
}

impl WorkflowFeedbackPort for FakeFeedback {
    fn publish_question(
        &mut self,
        identity: &WorkflowIdentity,
        feedback: &WorkflowFeedback,
    ) -> Result<String, FeedbackError> {
        let reference = format!(
            "https://github.test/{}/feedback/{}",
            identity.key(),
            feedback.id
        );
        self.published
            .push((feedback.question.clone(), reference.clone()));
        Ok(reference)
    }

    fn poll(
        &mut self,
        run_id: WorkflowRunId,
        _identity: &WorkflowIdentity,
    ) -> Result<Vec<WorkflowFeedbackEvent>, FeedbackError> {
        let mut matching = Vec::new();
        self.events.retain(|event| {
            if event.run_id == run_id {
                matching.push(event.clone());
                false
            } else {
                true
            }
        });
        Ok(matching)
    }
}

struct CompletingExecutor;

impl WorkflowStepExecutor for CompletingExecutor {
    fn execute(
        &mut self,
        _run: &deepseek_custom::workflow::WorkflowRunRecord,
    ) -> Result<WorkflowStepOutcome, String> {
        Ok(WorkflowStepOutcome::Completed {
            evidence: vec![WorkflowEvidence::new("test", "executor completed")],
        })
    }
}

#[test]
fn worker_executes_one_claimed_route_step_without_bypassing_the_scheduler() {
    let directory = temp_dir("worker");
    let registry = WorkflowRegistry::new(WorkflowStore::for_project(&directory), 1).unwrap();
    let queue = FakeQueue {
        items: vec![candidate(12, "project-a", 1)],
        ..Default::default()
    };
    let mut worker = WorkflowWorker::new(
        WorkflowScheduler::new(queue, FakeFeedback::default(), registry, "worker-a"),
        CompletingExecutor,
    );
    let tick = worker.run_once().unwrap();
    let run_id = match tick {
        WorkflowTick::Claimed { run_id, .. } => run_id,
        other => panic!("unexpected worker tick: {other:?}"),
    };
    let run = worker.scheduler().registry().load(run_id).unwrap();
    assert_eq!(run.current_step(), WorkflowStep::Refine);
    assert_eq!(run.state(), WorkflowRunState::Queued);
    std::fs::remove_dir_all(directory).ok();
}

#[test]
fn registry_persists_selection_and_normalizes_inflight_runs_after_restart() {
    let directory = temp_dir("registry-restart");
    let store = WorkflowStore::for_project(&directory);
    let mut registry = WorkflowRegistry::new(store.clone(), 2).unwrap();
    let run = WorkflowRun::new(identity(11, "project-a"));
    let run_id = run.id();
    registry.register(run).unwrap();
    registry.select(run_id).unwrap();
    let mut running = registry.load(run_id).unwrap();
    let claim = running.claim("worker-a").unwrap();
    running.start(&claim).unwrap();
    registry.save(&running).unwrap();
    drop(registry);

    let restarted = WorkflowRegistry::new(store, 2).unwrap();
    assert_eq!(restarted.selected_run_id(), Some(run_id));
    assert_eq!(
        restarted.load(run_id).unwrap().state(),
        WorkflowRunState::Interrupted
    );
    std::fs::remove_dir_all(directory).ok();
}

fn candidate(issue: u64, project: &str, priority: i32) -> EligibleWorkItem {
    EligibleWorkItem {
        identity: identity(issue, project),
        title: format!("issue {issue}"),
        priority,
        created_at: issue,
    }
}

#[test]
fn scheduler_claims_once_publishes_feedback_and_advances_after_merge() {
    let directory = temp_dir("scheduler");
    let store = WorkflowStore::for_project(&directory);
    let registry = WorkflowRegistry::new(store, 2).unwrap();
    let first = candidate(8, "project-a", 1);
    let second = candidate(9, "project-b", 2);
    let first_key = first.identity.key();
    let queue = FakeQueue {
        items: vec![first, second],
        terminal: HashMap::from([(first_key.clone(), TerminalObservation::Merged)]),
        ..Default::default()
    };
    let feedback = FakeFeedback::default();
    let mut scheduler = WorkflowScheduler::new(queue, feedback, registry, "worker-a");

    let first_tick = scheduler.tick().unwrap();
    let first_id = match first_tick {
        deepseek_custom::workflow::WorkflowTick::Claimed { run_id, .. } => run_id,
        other => panic!("unexpected first tick: {other:?}"),
    };
    let first_claim = scheduler.active_claim(first_id).unwrap();
    let feedback_state = scheduler
        .apply_step(
            first_id,
            &first_claim,
            WorkflowStepOutcome::Blocked {
                question: "Which review policy applies?".into(),
                evidence: vec![WorkflowEvidence::new("test", "operator blocked")],
                requested_action: "Reply with the policy name.".into(),
            },
        )
        .unwrap();
    assert_eq!(feedback_state, WorkflowRunState::Blocked);
    let first_record = scheduler.registry().load(first_id).unwrap();
    let feedback_id = first_record.record().pending_feedback_id.clone().unwrap();
    assert!(
        first_record.record().feedback[0]
            .published_reference
            .is_some()
    );
    assert_eq!(scheduler.feedback().published.len(), 1);

    let waiting = scheduler.tick().unwrap();
    assert!(matches!(
        waiting,
        deepseek_custom::workflow::WorkflowTick::Waiting { .. }
    ));
    scheduler.feedback_mut().events.push(WorkflowFeedbackEvent {
        run_id: first_id,
        feedback_id,
        answer: "strict policy".into(),
        evidence: vec![WorkflowEvidence::new("test", "operator answer")],
    });
    assert!(matches!(
        scheduler.tick().unwrap(),
        deepseek_custom::workflow::WorkflowTick::Progressed { run_id } if run_id == first_id
    ));

    for _ in 0..6 {
        let claim = scheduler.active_claim(first_id).unwrap_or_else(|| {
            let tick = scheduler.tick().unwrap();
            assert!(matches!(
                tick,
                deepseek_custom::workflow::WorkflowTick::Claimed { .. }
            ));
            scheduler.active_claim(first_id).unwrap()
        });
        scheduler
            .apply_step(
                first_id,
                &claim,
                WorkflowStepOutcome::Completed {
                    evidence: vec![WorkflowEvidence::new("test", "step completed")],
                },
            )
            .unwrap();
    }
    let gate_claim = scheduler.active_claim(first_id).unwrap_or_else(|| {
        let tick = scheduler.tick().unwrap();
        assert!(matches!(
            tick,
            deepseek_custom::workflow::WorkflowTick::Claimed { .. }
        ));
        scheduler.active_claim(first_id).unwrap()
    });
    scheduler
        .apply_step(
            first_id,
            &gate_claim,
            WorkflowStepOutcome::Completed {
                evidence: vec![WorkflowEvidence::new("test", "gate completed")],
            },
        )
        .unwrap();
    let decision_id = scheduler
        .registry()
        .load(first_id)
        .unwrap()
        .record()
        .pending_decision
        .as_ref()
        .unwrap()
        .id
        .clone();
    assert_eq!(
        scheduler.approve(first_id, &decision_id).unwrap(),
        WorkflowRunState::Completed
    );
    let second_tick = scheduler.tick().unwrap();
    let second_id = match second_tick {
        deepseek_custom::workflow::WorkflowTick::Claimed { run_id, .. } => run_id,
        other => panic!("unexpected second tick: {other:?}"),
    };
    assert_ne!(first_id, second_id);
    let first_run = scheduler.registry().load(first_id).unwrap();
    let second_run = scheduler.registry().load(second_id).unwrap();
    assert_ne!(first_run.session_id(), second_run.session_id());
    assert_ne!(first_run.context_id(), second_run.context_id());

    assert!(
        scheduler
            .apply_feedback(WorkflowFeedbackEvent {
                run_id: first_id,
                feedback_id: "stale".into(),
                answer: "late".into(),
                evidence: vec![WorkflowEvidence::new("test", "late answer")],
            })
            .is_err()
    );
    std::fs::remove_dir_all(directory).ok();
}
