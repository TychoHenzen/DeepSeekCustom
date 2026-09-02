use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use crate::agent::events::RoutedEvent;
use crate::effort::Effort;

use super::args::build_args;
use super::controlled_profile::ControlledProfile;
use super::process::ClaudeCliDriver;

const EXECUTION_TOOLS: &str = "Read,Glob,Grep,Write,Edit";

/// Fixed invocation policy for one fresh controlled execution driver.
pub(super) struct ExecutionProfile;

impl ExecutionProfile {
    pub(super) fn args(&self, model: &str, effort: Effort) -> Vec<String> {
        let mut args = build_args(model, Some("acceptEdits"), None, None, effort);
        args.extend([
            "--safe-mode".to_string(),
            "--no-session-persistence".to_string(),
            "--tools".to_string(),
            EXECUTION_TOOLS.to_string(),
            "--allowedTools".to_string(),
            EXECUTION_TOOLS.to_string(),
        ]);
        args
    }
}

impl ClaudeCliDriver {
    pub(crate) fn new_execution(
        model: String,
        extra_env: Option<HashMap<String, String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        tx_events: UnboundedSender<RoutedEvent>,
    ) -> Self {
        let mut driver = Self::new(
            model,
            Some("acceptEdits".to_string()),
            extra_env,
            working_dir,
            tx_events,
        );
        driver.controlled_profile = Some(ControlledProfile::Execution(ExecutionProfile));
        driver
    }

    #[cfg(feature = "test-support")]
    pub fn is_controlled_execution_for_test(&self) -> bool {
        matches!(
            self.controlled_profile,
            Some(ControlledProfile::Execution(_))
        )
    }

    #[cfg(feature = "test-support")]
    pub fn execution_args_for_test(&self, effort: Effort) -> Option<Vec<String>> {
        match &self.controlled_profile {
            Some(ControlledProfile::Execution(profile)) => Some(profile.args(&self.model, effort)),
            _ => None,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn execution_working_dir_for_test(&self) -> Option<PathBuf> {
        self.is_controlled_execution_for_test()
            .then(|| self.working_dir.lock().ok().map(|root| root.clone()))
            .flatten()
    }
}
