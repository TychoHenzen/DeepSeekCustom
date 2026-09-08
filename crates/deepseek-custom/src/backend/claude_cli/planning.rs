use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use crate::agent::events::RoutedEvent;
use crate::effort::Effort;

use super::args::build_args;
use super::controlled_profile::ControlledProfile;
use super::process::ClaudeCliDriver;

/// Fixed invocation policy for one fresh controlled planning driver.
pub(super) struct PlanningProfile {
    json_schema: String,
}

impl PlanningProfile {
    fn new(schema: &serde_json::Value) -> Self {
        Self {
            json_schema: schema.to_string(),
        }
    }

    pub(super) fn args(&self, model: &str, effort: Effort) -> Vec<String> {
        let mut args = build_args(model, Some("plan"), None, None, effort);
        args.extend([
            "--safe-mode".to_string(),
            "--no-session-persistence".to_string(),
            "--allowedTools".to_string(),
            "Read,Glob,Grep".to_string(),
            "--json-schema".to_string(),
            self.json_schema.clone(),
        ]);
        args
    }
}

impl ClaudeCliDriver {
    pub(crate) fn new_planning(
        model: String,
        extra_env: Option<HashMap<String, String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        tx_events: UnboundedSender<RoutedEvent>,
        schema: &serde_json::Value,
    ) -> Self {
        let mut driver = Self::new(
            model,
            Some("plan".to_string()),
            extra_env,
            working_dir,
            tx_events,
        );
        driver.controlled_profile = Some(ControlledProfile::Planning(PlanningProfile::new(schema)));
        driver
    }

    #[cfg(feature = "test-support")]
    pub fn is_controlled_planning_for_test(&self) -> bool {
        matches!(
            self.controlled_profile,
            Some(ControlledProfile::Planning(_))
        )
    }

    #[cfg(feature = "test-support")]
    pub fn planning_args_for_test(&self, effort: Effort) -> Option<Vec<String>> {
        match &self.controlled_profile {
            Some(ControlledProfile::Planning(profile)) => Some(profile.args(&self.model, effort)),
            _ => None,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn planning_working_dir_for_test(&self) -> Option<PathBuf> {
        self.is_controlled_planning_for_test()
            .then(|| self.working_dir.lock().ok().map(|root| root.clone()))
            .flatten()
    }
}
