use crate::procedure::VerifierGateEvidence;

use super::{ControlledDevelopmentSystemMapComponent, WorkCard};

/// Typed harness-owned data used to rewrite `PROJECT_STATE.md`.
pub struct ControlledDevelopmentProjectStateInput {
    pub outcome: String,
    pub system_map: Vec<ControlledDevelopmentSystemMapComponent>,
    pub work_card: WorkCard,
    pub changed_paths: Vec<String>,
    pub proof_evidence: Vec<VerifierGateEvidence>,
    pub blocker: Option<String>,
}
