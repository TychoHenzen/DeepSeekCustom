//! Private types for the evolve runner: dispatch context, run state,
//! and candidate outcome enum.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::events::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::search::evolve::params::EvolveParams;

/// Outcome of dispatching and scoring a single candidate.
pub(super) enum CandidateOutcome {
    /// Candidate scored and inserted normally.
    Scored,
    /// The dispatch itself failed (subagent error), skip this slot.
    DispatchFailed,
    /// Reached the max-dispatch cap; stop the run.
    CapReached,
    /// A fitness or feature command failed fatally.
    CmdFailed(String),
}

/// Bundles the read-only context a candidate dispatch needs, so
/// `dispatch_and_score_one` can take fewer arguments.
pub(super) struct DispatchCtx<'a> {
    pub factory: &'a Arc<BackendFactory>,
    pub params: &'a EvolveParams,
    pub hints: &'a [String],
    pub tx_events: &'a mpsc::UnboundedSender<RoutedEvent>,
    pub registry: &'a Arc<SubagentRegistry>,
    pub work_dir: &'a Path,
}

/// Holds mutable state that crosses candidate iterations inside one run.
pub(super) struct RunState {
    pub dispatches: u32,
    pub hint_index: usize,
    pub failure: Option<String>,
}
