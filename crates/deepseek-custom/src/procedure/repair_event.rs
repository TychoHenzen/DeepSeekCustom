//! Persistable, privacy-limited evidence for bounded repair transitions.

use serde::{Deserialize, Serialize};

use super::{FailureDigestErrorCategory, RepairTier, StructuralFailureCategory};

/// The state-machine transition represented by one evidence row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairLadderTransition {
    AttemptStarted,
    StructuralRetry,
    StructuralRetryExhausted,
    VerifierFailure,
    Escalated,
    Promoted,
    Blocked,
    Interrupted,
}

/// Why the transition occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairLadderTrigger {
    InitialRequest,
    StructuralFailure,
    VerifierFailure,
    LocalBudgetExhausted,
    FrontierBudgetExhausted,
    CandidatePassed,
    UserInterrupt,
}

/// Privacy-safe error category. Diagnostics and source contents stay elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairLadderErrorCategory {
    Schema,
    Envelope,
    Allowlist,
    PatchParse,
    VerifierFailed,
    VerifierSpawnFailed,
    Interrupted,
}

impl From<StructuralFailureCategory> for RepairLadderErrorCategory {
    fn from(category: StructuralFailureCategory) -> Self {
        match category {
            StructuralFailureCategory::Schema => Self::Schema,
            StructuralFailureCategory::Envelope => Self::Envelope,
            StructuralFailureCategory::Allowlist => Self::Allowlist,
            StructuralFailureCategory::PatchParse => Self::PatchParse,
        }
    }
}

impl From<FailureDigestErrorCategory> for RepairLadderErrorCategory {
    fn from(category: FailureDigestErrorCategory) -> Self {
        match category {
            FailureDigestErrorCategory::VerifierFailed => Self::VerifierFailed,
            FailureDigestErrorCategory::VerifierSpawnFailed => Self::VerifierSpawnFailed,
            FailureDigestErrorCategory::Interrupted => Self::Interrupted,
        }
    }
}

/// The deterministic gate result visible at this transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairLadderGateResult {
    NotRun,
    StructuralRejected,
    VerifierFailed,
    VerifierPassed,
    Interrupted,
}

/// The resulting ladder disposition after this transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairLadderDisposition {
    CandidateActive,
    Ready,
    LocalExhausted,
    FrontierReady,
    FrontierExhausted,
    Promoted,
    Blocked,
    Interrupted,
}

/// One complete transition row retained by progress, reports, and the view model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairLadderEvent {
    pub transition: RepairLadderTransition,
    pub attempt_number: u8,
    pub tier: RepairTier,
    pub backend: String,
    pub model: String,
    pub trigger: RepairLadderTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_category: Option<RepairLadderErrorCategory>,
    pub gate_result: RepairLadderGateResult,
    pub disposition: RepairLadderDisposition,
}

/// Stable text rows consumed by the current Procedure report view.
pub fn repair_ladder_render_lines(events: &[RepairLadderEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            format!(
                "Repair {:?}: attempt {} {:?} via {} / {}; trigger {:?}; error {:?}; gate {:?}; disposition {:?}",
                event.transition,
                event.attempt_number,
                event.tier,
                event.backend,
                event.model,
                event.trigger,
                event.error_category,
                event.gate_result,
                event.disposition,
            )
        })
        .collect()
}
