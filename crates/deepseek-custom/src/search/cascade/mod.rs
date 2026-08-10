//! Cascade: run `n` attempts at one prompt, each with a different diversity
//! hint, drop the ones a check command rejects, and pick a winner by vote.
//! When no answer reaches the required lead, escalate to a stronger backend.
//!
//! Diversity.md's best-evidenced idea: cheap-model fanout with a check
//! command or a strong model picking the winner, rather than trusting one
//! answer from one run. Every attempt is an ordinary `keep_open: false`
//! dispatch through `run_subagent`, so each one gets its own `Subagent`
//! block in the transcript.

pub mod candidate;
mod context;
pub mod counters;
mod escalate;
pub mod params;
mod report;
pub mod run;
mod vote_outcome;
mod vote_tally;
mod voting;

pub use counters::CascadeCounters;
pub use params::CascadeParams;
pub use report::CascadeReport;
pub use run::run_cascade;

use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::search::{SearchKind, SearchSnapshot};
use tokio::sync::mpsc;

/// The most attempts one cascade may run. Each attempt is a whole model
/// turn. More than this against one backend floods it for little
/// diversity gain.
pub const MAX_ATTEMPTS: u32 = 16;

/// The default diversity hints, used when the run configures none.
pub fn default_diversity_hints() -> Vec<String> {
    vec![
        "Use a different approach or library than the obvious first choice.".to_string(),
        "Favor simplicity over speed.".to_string(),
        "Handle edge cases and error paths first.".to_string(),
        "Write the plain, direct version.".to_string(),
    ]
}

/// Send one progress snapshot on the main session's route.
pub(crate) fn send_progress(tx: &mpsc::UnboundedSender<RoutedEvent>, snapshot: &SearchSnapshot) {
    let _ = tx.send(RoutedEvent::own(StreamEvent::SearchProgress(Box::new(
        snapshot.clone(),
    ))));
}

/// Send the run's terminal event.
pub(crate) fn send_finished(
    tx: &mpsc::UnboundedSender<RoutedEvent>,
    kind: SearchKind,
    summary: &str,
    is_error: bool,
) {
    let _ = tx.send(RoutedEvent::own(StreamEvent::SearchFinished {
        kind,
        summary: summary.to_string(),
        is_error,
    }));
}
