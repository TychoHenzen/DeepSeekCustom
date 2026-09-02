use std::path::Path;

use crate::procedure::{
    DisposableWorkspaceError, DisposableWorkspacePair, RetainedDisposableWorkspacePair,
};

use super::diagnostic_diff::render_diagnostic_diff;
use super::{
    ControlledDevelopmentOwnedWorkspaceRoot, ControlledDevelopmentRetainedWorkspaceReference,
};

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

    pub fn reference(
        &self,
    ) -> Result<ControlledDevelopmentRetainedWorkspaceReference, DisposableWorkspaceError> {
        Ok(ControlledDevelopmentRetainedWorkspaceReference {
            packet_id: self.packet_id.clone(),
            baseline_root: ControlledDevelopmentOwnedWorkspaceRoot::new(
                self.baseline_path().to_path_buf(),
            )?,
            execution_root: ControlledDevelopmentOwnedWorkspaceRoot::new(
                self.execution_path().to_path_buf(),
            )?,
        })
    }

    pub(crate) fn restore(
        reference: ControlledDevelopmentRetainedWorkspaceReference,
    ) -> Result<Self, DisposableWorkspaceError> {
        let packet_id = reference.packet_id;
        let pair = RetainedDisposableWorkspacePair::from_owned_paths(
            reference.baseline_root.into_path(),
            reference.execution_root.into_path(),
        )?;
        let diagnostic_diff = render_diagnostic_diff(pair.baseline_path(), pair.execution_path());
        Ok(Self {
            packet_id,
            pair,
            diagnostic_diff,
        })
    }

    /// Validate and remove both feature-owned temporary roots.
    pub fn cleanup(&self) -> Result<(), DisposableWorkspaceError> {
        self.pair.cleanup()
    }
}
