//! Running totals for the escalation rate shown in the status bar.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

/// The two running totals the status bar's escalation rate is built from.
///
/// They live for the whole process, not for one run, so the readout says
/// how often cascades have needed a stronger backend across the session.
#[derive(Clone)]
pub struct CascadeCounters {
    /// Bumped once per run, resolved or not.
    pub total: Arc<AtomicUsize>,
    /// Bumped when a run had to escalate.
    pub escalated: Arc<AtomicUsize>,
}

impl CascadeCounters {
    /// A pair starting at zero, for a caller with no counters to share.
    pub fn new() -> Self {
        Self {
            total: Arc::new(AtomicUsize::new(0)),
            escalated: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Default for CascadeCounters {
    fn default() -> Self {
        Self::new()
    }
}
