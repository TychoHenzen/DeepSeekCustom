//! The shared handles one cascade run stage needs.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::events::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::search::cascade::counters::CascadeCounters;

/// The handles every stage of one run shares. Where to build backends,
/// where events go, which registry holds the dispatches, and the counters
/// the status bar reads.
///
/// Bundled rather than passed loose, because they always travel together.
pub(super) struct RunContext<'a> {
    pub(super) factory: &'a Arc<BackendFactory>,
    pub(super) tx_events: &'a mpsc::UnboundedSender<RoutedEvent>,
    pub(super) registry: &'a Arc<SubagentRegistry>,
    pub(super) counters: &'a CascadeCounters,
}
