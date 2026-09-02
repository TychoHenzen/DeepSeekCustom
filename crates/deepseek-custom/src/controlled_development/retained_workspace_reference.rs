use serde::{Deserialize, Serialize};

use super::{ControlledDevelopmentOwnedWorkspaceRoot, ControlledDevelopmentRetainedWorkspace};

/// Persisted ownership needed to reopen one retained diagnostic workspace pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlledDevelopmentRetainedWorkspaceReference {
    pub packet_id: String,
    pub baseline_root: ControlledDevelopmentOwnedWorkspaceRoot,
    pub execution_root: ControlledDevelopmentOwnedWorkspaceRoot,
}

impl ControlledDevelopmentRetainedWorkspaceReference {
    pub fn restore(
        self,
    ) -> Result<ControlledDevelopmentRetainedWorkspace, crate::procedure::DisposableWorkspaceError>
    {
        ControlledDevelopmentRetainedWorkspace::restore(self)
    }
}
