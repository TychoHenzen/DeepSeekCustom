use std::sync::atomic::Ordering;

use tracing::{error, info};

use super::agent_loop::{AgentLoop, StreamEvent};

/// A request to run one task repeatedly, sent from the GUI's Autopilot tab
/// to the agent task over an unbounded channel.
pub struct RepeatCommand {
    pub task: String,
    pub iterations: u32,
}

/// Run `task` against `agent` `iterations` times, clearing the history
/// before each run so no iteration sees any earlier one.
///
/// Checks the runner's own interrupt flag (`AgentLoop::repeat_interrupt_flag`)
/// before each iteration and stops early if it is set. `AgentLoop::run`
/// consumes its own `interrupt_flag` internally and cannot carry a signal
/// across iterations. A single Escape press that sets this flag instead
/// stops the whole run, not just the current iteration.
///
/// An `Err` from `agent.run` logs at `error` and stops the loop. Iterations
/// already finished still count toward `completed`.
pub async fn run_repeat(agent: &mut AgentLoop, task: &str, iterations: u32) {
    if iterations == 0 {
        agent.send_repeat_finished(0, 0);
        return;
    }

    let mut completed = 0u32;

    for index in 1..=iterations {
        if agent.repeat_interrupt_flag().load(Ordering::SeqCst) {
            info!(index, iterations, "repeat run: stopped by interrupt flag");
            break;
        }

        agent.clear_history();
        info!(index, total = iterations, "repeat run: starting iteration");
        agent.send_event(StreamEvent::RepeatIterationStart {
            index,
            total: iterations,
        });

        match agent.run(task).await {
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

    agent.send_repeat_finished(completed, iterations);
}
