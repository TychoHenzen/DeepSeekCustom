use std::path::Path;

use crate::procedure::{
    DisposableWorkspaceError, DisposableWorkspacePair, RetainedDisposableWorkspacePair,
};

use super::diagnostic_diff::render_diagnostic_diff;

/// Feature-owned baseline and execution roots retained after a blocked packet.
#[derive(Debug)]
pub struct ControlledDevelopmentRetainedWorkspace {
    packet_id: String,
    pair: RetainedDisposableWorkspacePair,
    diagnostic_diff: String,
}

impl ControlledDevelopmentRetainedWorkspace {
    pub fn retain(packet_id: String, pair: DisposableWorkspacePair) -> Self {
        let diagnostic_diff = render_diagnostic_diff(pair.baseline_path(), pair.execution_path());
        Self {
            packet_id,
            pair: pair.retain_for_diagnostics(),
            diagnostic_diff,
        }
    }

    pub fn packet_id(&self) -> &str {
        &self.packet_id
    }

    pub fn baseline_path(&self) -> &Path {
        self.pair.baseline_path()
    }

    pub fn execution_path(&self) -> &Path {
        self.pair.execution_path()
    }

    pub fn diagnostic_diff(&self) -> &str {
        &self.diagnostic_diff
    }

    /// Validate and remove both feature-owned temporary roots.
    pub fn cleanup(self) -> Result<(), DisposableWorkspaceError> {
        self.pair.cleanup()
    }
}
