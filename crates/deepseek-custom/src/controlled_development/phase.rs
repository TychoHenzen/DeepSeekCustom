use serde::{Deserialize, Serialize};

/// The explicit lifecycle of one session's Controlled Development mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlledDevelopmentPhase {
    #[default]
    Off,
    Planning,
    AwaitingApproval,
    Executing,
    Completed,
    Blocked,
    Interrupted,
}
