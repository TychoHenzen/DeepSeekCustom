use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use std::collections::HashMap;

use tokio::sync::mpsc::UnboundedSender;

use crate::agent::events::RoutedEvent;
use crate::effort::Effort;

use super::CodexCliDriver;
use super::controlled_profile::ControlledProfile;
use super::spawn::build_planning_args;

static NEXT_SCHEMA_ID: AtomicU64 = AtomicU64::new(1);

/// Owned schema file for one fresh controlled planning driver.
pub(super) struct PlanningProfile {
    schema_path: PathBuf,
}

impl PlanningProfile {
    pub(super) fn new(schema: &serde_json::Value) -> Result<Self, String> {
        let bytes = serde_json::to_vec(schema)
            .map_err(|error| format!("failed to encode Work Card schema: {error}"))?;
        for _ in 0..32 {
            let id = NEXT_SCHEMA_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "deepseek-controlled-work-card-{}-{id}.json",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(&bytes) {
                        drop(file);
                        let _ = std::fs::remove_file(&path);
                        return Err(format!(
                            "failed to write Work Card schema {}: {error}",
                            path.display()
                        ));
                    }
                    return Ok(Self { schema_path: path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "failed to create Work Card schema {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        Err("failed to allocate a unique Work Card schema file".to_string())
    }

    pub(super) fn args(&self, prompt: &str, model: &str, effort: Effort) -> Vec<String> {
        build_planning_args(prompt, &self.schema_path, Some(model), effort)
    }

    #[cfg(feature = "test-support")]
    pub(super) fn schema_path(&self) -> &Path {
        &self.schema_path
    }
}

impl CodexCliDriver {
    pub(crate) fn new_planning(
        model: String,
        extra_env: Option<HashMap<String, String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        tx_events: UnboundedSender<RoutedEvent>,
        schema: &serde_json::Value,
    ) -> Result<Self, String> {
        let mut driver = Self::new(
            model,
            Some("read-only".to_string()),
            extra_env,
            working_dir,
            tx_events,
        );
        driver.controlled_profile =
            Some(ControlledProfile::Planning(PlanningProfile::new(schema)?));
        Ok(driver)
    }

    #[cfg(feature = "test-support")]
    pub fn planning_schema_path_for_test(&self) -> Option<&Path> {
        match &self.controlled_profile {
            Some(ControlledProfile::Planning(profile)) => Some(profile.schema_path()),
            _ => None,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn planning_args_for_test(&self, prompt: &str, effort: Effort) -> Option<Vec<String>> {
        let model = self.model_flag.lock().ok()?.clone();
        match &self.controlled_profile {
            Some(ControlledProfile::Planning(profile)) => {
                Some(profile.args(prompt, &model, effort))
            }
            _ => None,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn planning_working_dir_for_test(&self) -> Option<PathBuf> {
        matches!(
            self.controlled_profile,
            Some(ControlledProfile::Planning(_))
        )
        .then(|| self.working_dir.lock().ok().map(|root| root.clone()))
        .flatten()
    }
}

impl Drop for PlanningProfile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.schema_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                "codex_cli: failed to remove controlled schema {}: {error}",
                self.schema_path.display()
            );
        }
    }
}
