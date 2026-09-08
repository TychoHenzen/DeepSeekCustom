use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use crate::agent::events::RoutedEvent;
use crate::effort::Effort;

use super::CodexCliDriver;
use super::controlled_profile::ControlledProfile;
use super::spawn::build_execution_args;

/// Fixed invocation policy for one fresh controlled execution driver.
pub(super) struct ExecutionProfile;

impl ExecutionProfile {
    pub(super) fn args(&self, prompt: &str, model: &str, effort: Effort) -> Vec<String> {
        build_execution_args(prompt, Some(model), effort)
    }
}

impl CodexCliDriver {
    pub(crate) fn new_execution(
        model: String,
        extra_env: Option<HashMap<String, String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        tx_events: UnboundedSender<RoutedEvent>,
    ) -> Self {
        let mut driver = Self::new(model, None, extra_env, working_dir, tx_events);
        driver.controlled_profile = Some(ControlledProfile::Execution(ExecutionProfile));
        driver
    }

    #[cfg(feature = "test-support")]
    pub fn execution_args_for_test(&self, prompt: &str, effort: Effort) -> Option<Vec<String>> {
        let model = self.model_flag.lock().ok()?.clone();
        match &self.controlled_profile {
            Some(ControlledProfile::Execution(profile)) => {
                Some(profile.args(prompt, &model, effort))
            }
            _ => None,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn execution_working_dir_for_test(&self) -> Option<PathBuf> {
        matches!(
            self.controlled_profile,
            Some(ControlledProfile::Execution(_))
        )
        .then(|| self.working_dir.lock().ok().map(|root| root.clone()))
        .flatten()
    }
}
