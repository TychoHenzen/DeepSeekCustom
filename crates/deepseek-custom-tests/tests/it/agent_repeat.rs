//! Unit tests for `deepseek_custom::agent::repeat`, moved out of the
//! production module as part of the two-crate workspace split.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deepseek_custom::agent::agent_loop::StreamEvent;
use deepseek_custom::agent::repeat::{RepeatTarget, run_repeat};
use deepseek_custom::error::{HarnessError, Result};

/// Test-only `RepeatTarget`. It never touches a real process or API
/// client. These tests exercise the shared loop control flow that
/// both the API and Claude CLI backends drive through.
struct MockTarget {
    events: RefCell<Vec<StreamEvent>>,
    interrupt_flag: Arc<AtomicBool>,
    turns_run: u32,
    /// When set, `run_turn` flips `interrupt_flag` right after the
    /// turn with this 1-based index. This simulates an Escape press
    /// mid-run.
    set_interrupt_after_turn: Option<u32>,
    /// When set, `run_turn` fails on the turn with this 1-based index.
    fail_on_turn: Option<u32>,
}

impl MockTarget {
    fn new() -> Self {
        Self {
            events: RefCell::new(Vec::new()),
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            turns_run: 0,
            set_interrupt_after_turn: None,
            fail_on_turn: None,
        }
    }

    fn events(&self) -> Vec<StreamEvent> {
        self.events.borrow().clone()
    }
}

impl RepeatTarget for MockTarget {
    async fn reset_for_iteration(&mut self) {}

    async fn run_turn(&mut self, _task: &str) -> Result<String> {
        self.turns_run += 1;
        if self.fail_on_turn == Some(self.turns_run) {
            return Err(HarnessError::Tool("mock turn failure".into()));
        }
        if self.set_interrupt_after_turn == Some(self.turns_run) {
            self.interrupt_flag.store(true, Ordering::SeqCst);
        }
        Ok("mock reply".into())
    }

    fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    fn send_event(&self, event: StreamEvent) {
        self.events.borrow_mut().push(event);
    }
}

fn iteration_starts(events: &[StreamEvent]) -> Vec<(u32, u32)> {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::RepeatIterationStart { index, total, .. } => Some((*index, *total)),
            _ => None,
        })
        .collect()
}

fn iteration_tasks(events: &[StreamEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::RepeatIterationStart { task, .. } => Some(task.clone()),
            _ => None,
        })
        .collect()
}

fn repeat_finished(events: &[StreamEvent]) -> Option<(u32, u32)> {
    events.iter().find_map(|e| match e {
        StreamEvent::RepeatFinished { completed, total } => Some((*completed, *total)),
        _ => None,
    })
}

fn temp_project_root() -> PathBuf {
    std::env::temp_dir().join("deepseek_repeat_test")
}

#[tokio::test]
async fn mock_target_emits_one_iteration_start_per_iteration_and_one_finished() {
    let root = temp_project_root();
    let mut target = MockTarget::new();

    run_repeat(&mut target, "do the thing", 3, &root).await;

    let events = target.events();
    assert_eq!(iteration_starts(&events), vec![(1, 3), (2, 3), (3, 3)]);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, StreamEvent::RepeatFinished { .. }))
            .count(),
        1
    );
    assert_eq!(repeat_finished(&events), Some((3, 3)));
}

#[tokio::test]
async fn mock_target_stops_when_interrupt_flag_flips_mid_run() {
    let root = temp_project_root();
    let mut target = MockTarget::new();
    target.set_interrupt_after_turn = Some(1);

    run_repeat(&mut target, "do the thing", 5, &root).await;

    let events = target.events();
    // Iteration 1 ran and flipped the flag. Iteration 2's pre-check
    // sees it set and the run stops before a second turn happens.
    assert_eq!(iteration_starts(&events), vec![(1, 5)]);
    assert_eq!(repeat_finished(&events), Some((1, 5)));
}

#[tokio::test]
async fn mock_target_stops_immediately_when_interrupt_flag_already_set() {
    let root = temp_project_root();
    let mut target = MockTarget::new();
    target.interrupt_flag.store(true, Ordering::SeqCst);

    run_repeat(&mut target, "do the thing", 3, &root).await;

    let events = target.events();
    assert!(iteration_starts(&events).is_empty());
    assert_eq!(repeat_finished(&events), Some((0, 3)));
}

#[tokio::test]
async fn mock_target_zero_iterations_emits_only_repeat_finished() {
    let root = temp_project_root();
    let mut target = MockTarget::new();

    run_repeat(&mut target, "do the thing", 0, &root).await;

    let events = target.events();
    assert_eq!(events.len(), 1);
    assert_eq!(repeat_finished(&events), Some((0, 0)));
}

#[tokio::test]
async fn mock_target_stops_on_turn_failure_and_reports_completed_before_it() {
    let root = temp_project_root();
    let mut target = MockTarget::new();
    target.fail_on_turn = Some(2);

    run_repeat(&mut target, "do the thing", 4, &root).await;

    let events = target.events();
    assert_eq!(iteration_starts(&events), vec![(1, 4), (2, 4)]);
    assert_eq!(repeat_finished(&events), Some((1, 4)));
}

#[tokio::test]
async fn every_iteration_start_carries_the_task_text() {
    let root = temp_project_root();
    let mut target = MockTarget::new();

    run_repeat(&mut target, "do the thing", 3, &root).await;

    let events = target.events();
    assert_eq!(
        iteration_tasks(&events),
        vec![
            "do the thing".to_string(),
            "do the thing".to_string(),
            "do the thing".to_string()
        ]
    );
}
