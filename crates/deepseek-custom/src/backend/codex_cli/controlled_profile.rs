use crate::effort::Effort;

use super::CodexCliDriver;
use super::events::CodexEvent;
use super::execution::ExecutionProfile;
use super::planning::PlanningProfile;
use super::spawn::build_args;

/// Invocation policy for a fresh Controlled Development backend instance.
pub(super) enum ControlledProfile {
    Planning(PlanningProfile),
    Execution(ExecutionProfile),
}

impl ControlledProfile {
    fn args(&self, prompt: &str, model: &str, effort: Effort) -> Vec<String> {
        match self {
            Self::Planning(profile) => profile.args(prompt, model, effort),
            Self::Execution(profile) => profile.args(prompt, model, effort),
        }
    }
}

impl CodexCliDriver {
    pub(super) fn turn_args(&self, prompt: &str, model: &str, effort: Effort) -> Vec<String> {
        match &self.controlled_profile {
            Some(profile) => profile.args(prompt, model, effort),
            None => {
                let sandbox = if self.tools_enabled {
                    self.sandbox.as_deref()
                } else {
                    Some("read-only")
                };
                build_args(
                    prompt,
                    self.thread_id.as_deref(),
                    sandbox,
                    Some(model),
                    effort,
                )
            }
        }
    }

    pub(super) fn capture_thread(&mut self, event: &CodexEvent) {
        if self.controlled_profile.is_none()
            && let CodexEvent::ThreadStarted(started) = event
        {
            self.thread_id = Some(started.thread_id.clone());
        }
    }
}
