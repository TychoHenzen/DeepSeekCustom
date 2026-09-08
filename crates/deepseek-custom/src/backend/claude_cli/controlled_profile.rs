use crate::effort::Effort;

use super::execution::ExecutionProfile;
use super::planning::PlanningProfile;

/// Invocation policy for a fresh Controlled Development backend instance.
pub(super) enum ControlledProfile {
    Planning(PlanningProfile),
    Execution(ExecutionProfile),
}

impl ControlledProfile {
    pub(super) fn args(&self, model: &str, effort: Effort) -> Vec<String> {
        match self {
            Self::Planning(profile) => profile.args(model, effort),
            Self::Execution(profile) => profile.args(model, effort),
        }
    }
}
