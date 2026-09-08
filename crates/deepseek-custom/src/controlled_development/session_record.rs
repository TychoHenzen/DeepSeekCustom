use serde::{Deserialize, Serialize};

use super::{
    ControlledDevelopmentCompactEvidence, ControlledDevelopmentRetainedWorkspaceReference,
    ControlledDevelopmentState,
};

/// Backward-compatible saved projection for one session-owned coordinator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControlledDevelopmentSessionRecord {
    pub state: ControlledDevelopmentState,
    pub compact_evidence: ControlledDevelopmentCompactEvidence,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub raw_details: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_workspace: Option<ControlledDevelopmentRetainedWorkspaceReference>,
}
