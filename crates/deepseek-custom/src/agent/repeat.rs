use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{error, info, warn};

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

    /// Run one turn of `task` to completion, returning the final reply text.
    fn run_turn(&mut self, task: &str) -> impl std::future::Future<Output = Result<String>> + Send;

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

    async fn run_turn(&mut self, task: &str) -> Result<String> {
        self.run(task)
            .await
            .map(|replies| replies.into_iter().last().unwrap_or_default())
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

/// Flatten newlines out of a string so it stays on one line in the log.
fn single_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// Write one line to `.autopilot/decisions.log` recording a completed
/// autopilot step, the same file `PolicyStore::append_decision` already
/// writes to. A failure logs at `warn` and is otherwise ignored, the same
/// rule `append_decision` follows: losing a log line must never take a run
/// down.
fn append_step_log(project_root: &Path, task: &str, reply: &str) {
    let dir = project_root.join(".autopilot");
    let path = dir.join("decisions.log");

    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!(
            "autopilot: could not create decision log directory {}: {e}",
            dir.display()
        );
        return;
    }

    let line = format!("step={} reply={}\n", single_line(task), single_line(reply));

    use std::io::Write as _;
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(line.as_bytes()));

    if let Err(e) = result {
        warn!(
            "autopilot: could not append step to {}: {e}",
            path.display()
        );
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
/// Writes one line per completed step to `<project_root>/.autopilot/decisions.log`,
/// with the task text and the final reply, newlines flattened. This gives a
/// long autopilot run a compaction note, matching Diversity.md's account of
/// the progress file Anthropic pairs with compaction.
///
/// An `Err` from `target.run_turn` logs at `error` and stops the loop.
/// Iterations already finished still count toward `completed`.
pub async fn run_repeat<T: RepeatTarget>(
    target: &mut T,
    task: &str,
    iterations: u32,
    project_root: &Path,
) {
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
            Ok(reply) => {
                completed += 1;
                info!(index, total = iterations, "repeat run: iteration finished");
                append_step_log(project_root, task, &reply);
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
