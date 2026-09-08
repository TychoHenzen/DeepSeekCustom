use serde::{Deserialize, Serialize};

use crate::procedure::VerifierGateEvidence;

/// Bounded, structured evidence projected with one saved session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControlledDevelopmentCompactEvidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_evidence: Vec<VerifierGateEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_promoted_diff: Option<String>,
}
