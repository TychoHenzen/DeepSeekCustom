use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{error, info};

use super::agent_loop::{AgentLoop, StreamEvent};
use crate::error::Result;

/// A request to run one task repeatedly, sent from the GUI's Autopilot tab
/// to the agent task over an unbounded channel.
pub struct RepeatCommand {
    pub task: String,
    pub iterations: u32,
}

/// The mechanics one backend needs to expose so `run_repeat` can drive it.
///
/// Both implementors guarantee the same thing: no iteration sees anything
/// an earlier iteration said or did in conversation. `AgentLoop` keeps that
/// guarantee by rebuilding `MessageHistory` from the base system prompt.
/// `ClaudeCliDriver` keeps it a different way. It ends the current child
/// process and lets the next turn spawn a fresh one. A new `claude -p`
/// process starts a new session with no prior conversation.
pub trait RepeatTarget {
    /// Reset whatever this backend uses to hold conversation state, before
    /// the next iteration's turn runs.
    fn reset_for_iteration(&mut self) -> impl std::future::Future<Output = ()> + Send;

    /// Run one turn of `task` to completion.
    fn run_turn(&mut self, task: &str) -> impl std::future::Future<Output = Result<()>> + Send;

    /// The flag that stops the whole repeat run, not just the iteration in
    /// flight. Checked before every iteration.
    fn repeat_interrupt_flag(&self) -> Arc<AtomicBool>;

    /// Publish one `StreamEvent` on this backend's event channel.
    fn send_event(&self, event: StreamEvent);

    /// Publish the final `RepeatFinished` event. The default just wraps
    /// `send_event`. `AgentLoop` overrides this to reuse its existing
    /// `send_repeat_finished` helper instead of leaving it dead code.
    fn finish(&self, completed: u32, total: u32) {
        self.send_event(StreamEvent::RepeatFinished { completed, total });
    }
}

impl RepeatTarget for AgentLoop {
    async fn reset_for_iteration(&mut self) {
        self.clear_history();
    }

    async fn run_turn(&mut self, task: &str) -> Result<()> {
        self.run(task).await.map(|_| ())
    }

    fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        self.repeat_interrupt_flag()
    }

    fn send_event(&self, event: StreamEvent) {
        self.send_event(event)
    }

    fn finish(&self, completed: u32, total: u32) {
        self.send_repeat_finished(completed, total);
    }
}

/// Run `task` against `target` `iterations` times, resetting the target's
/// conversation state before each run so no iteration sees any earlier one.
///
/// Checks `target.repeat_interrupt_flag()` before each iteration and stops
/// early if it is set. A per-turn interrupt flag cannot carry a stop
/// signal across iterations on either backend. `AgentLoop::run` resets its
/// own flag whenever it breaks out of a stream. `ClaudeCliDriver::interrupt`
/// kills the child mid-turn without touching anything that survives to the
/// next iteration. A single Escape press that sets the repeat flag instead
/// stops the whole run.
///
/// An `Err` from `target.run_turn` logs at `error` and stops the loop.
/// Iterations already finished still count toward `completed`.
pub async fn run_repeat<T: RepeatTarget>(target: &mut T, task: &str, iterations: u32) {
    if iterations == 0 {
        target.finish(0, 0);
        return;
    }

    let mut completed = 0u32;

    for index in 1..=iterations {
        if target.repeat_interrupt_flag().load(Ordering::SeqCst) {
            info!(index, iterations, "repeat run: stopped by interrupt flag");
            break;
        }

        target.reset_for_iteration().await;
        info!(index, total = iterations, "repeat run: starting iteration");
        target.send_event(StreamEvent::RepeatIterationStart {
            index,
            total: iterations,
        });

        match target.run_turn(task).await {
            Ok(_) => {
                completed += 1;
                info!(index, total = iterations, "repeat run: iteration finished");
            }
            Err(e) => {
                error!(index, total = iterations, "repeat run: iteration failed: {e}");
                break;
            }
        }
    }

    target.finish(completed, iterations);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::HarnessError;
    use std::cell::RefCell;

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

        async fn run_turn(&mut self, _task: &str) -> Result<()> {
            self.turns_run += 1;
            if self.fail_on_turn == Some(self.turns_run) {
                return Err(HarnessError::Tool("mock turn failure".into()));
            }
            if self.set_interrupt_after_turn == Some(self.turns_run) {
                self.interrupt_flag.store(true, Ordering::SeqCst);
            }
            Ok(())
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
                StreamEvent::RepeatIterationStart { index, total } => Some((*index, *total)),
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

    #[tokio::test]
    async fn mock_target_emits_one_iteration_start_per_iteration_and_one_finished() {
        let mut target = MockTarget::new();

        run_repeat(&mut target, "do the thing", 3).await;

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
        let mut target = MockTarget::new();
        target.set_interrupt_after_turn = Some(1);

        run_repeat(&mut target, "do the thing", 5).await;

        let events = target.events();
        // Iteration 1 ran and flipped the flag. Iteration 2's pre-check
        // sees it set and the run stops before a second turn happens.
        assert_eq!(iteration_starts(&events), vec![(1, 5)]);
        assert_eq!(repeat_finished(&events), Some((1, 5)));
    }

    #[tokio::test]
    async fn mock_target_stops_immediately_when_interrupt_flag_already_set() {
        let mut target = MockTarget::new();
        target.interrupt_flag.store(true, Ordering::SeqCst);

        run_repeat(&mut target, "do the thing", 3).await;

        let events = target.events();
        assert!(iteration_starts(&events).is_empty());
        assert_eq!(repeat_finished(&events), Some((0, 3)));
    }

    #[tokio::test]
    async fn mock_target_zero_iterations_emits_only_repeat_finished() {
        let mut target = MockTarget::new();

        run_repeat(&mut target, "do the thing", 0).await;

        let events = target.events();
        assert_eq!(events.len(), 1);
        assert_eq!(repeat_finished(&events), Some((0, 0)));
    }

    #[tokio::test]
    async fn mock_target_stops_on_turn_failure_and_reports_completed_before_it() {
        let mut target = MockTarget::new();
        target.fail_on_turn = Some(2);

        run_repeat(&mut target, "do the thing", 4).await;

        let events = target.events();
        assert_eq!(iteration_starts(&events), vec![(1, 4), (2, 4)]);
        assert_eq!(repeat_finished(&events), Some((1, 4)));
    }
}
