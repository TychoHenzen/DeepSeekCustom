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
            task: task.to_string(),
        });

        match target.run_turn(task).await {
            Ok(_) => {
                completed += 1;
                info!(index, total = iterations, "repeat run: iteration finished");
            }
            Err(e) => {
                error!(
                    index,
                    total = iterations,
                    "repeat run: iteration failed: {e}"
                );
                break;
            }
        }
    }

    target.finish(completed, iterations);
}
