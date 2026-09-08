use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::agent::events::RoutedEvent;
use crate::backend::claude_cli::process::ClaudeCliDriver;
use crate::backend::codex_cli::CodexCliDriver;
use crate::backend::{Backend, ToolPolicy};
use crate::controlled_development::work_card_json_schema;

use super::build_api::build_backend_with_policy;
use super::build_controlled_api::build_controlled_api_backend;
use super::factory::BackendFactory;
use super::factory::ControlledApiProfile;
use super::resolved::ResolvedBackend;

impl BackendFactory {
    /// Build a fresh API backend with the narrow Controlled Development tools.
    pub fn build_controlled_api(
        self: &Arc<Self>,
        name: &str,
        model_override: Option<&str>,
        tx_events: mpsc::UnboundedSender<RoutedEvent>,
        profile: ControlledApiProfile,
        root: PathBuf,
    ) -> Result<Backend, String> {
        let root = canonical_controlled_root(root)?;
        let scoped = self.with_working_dir(Arc::new(Mutex::new(root.clone())));
        let resolved = scoped.resolve(name, model_override)?;
        build_controlled_api_backend(resolved, &scoped, tx_events, profile, root)
    }

    /// Build a fresh read-only planning backend for any configured kind.
    pub fn build_controlled_planning(
        self: &Arc<Self>,
        name: &str,
        model_override: Option<&str>,
        tx_events: mpsc::UnboundedSender<RoutedEvent>,
        root: PathBuf,
    ) -> Result<Backend, String> {
        let root = canonical_controlled_root(root)?;
        let working_dir = Arc::new(Mutex::new(root.clone()));
        let scoped = self.with_working_dir(Arc::clone(&working_dir));
        let resolved = scoped.resolve(name, model_override)?;
        let schema = work_card_json_schema();
        match resolved {
            resolved @ ResolvedBackend::Api { .. } => build_controlled_api_backend(
                resolved,
                &scoped,
                tx_events,
                ControlledApiProfile::Planning,
                root,
            ),
            ResolvedBackend::ClaudeCli { model, env, .. } => Ok(Backend::ClaudeCli(Box::new(
                ClaudeCliDriver::new_planning(model, env, working_dir, tx_events, &schema),
            ))),
            ResolvedBackend::CodexCli { model, env, .. } => Ok(Backend::CodexCli(Box::new(
                CodexCliDriver::new_planning(model, env, working_dir, tx_events, &schema)?,
            ))),
            #[cfg(feature = "test-support")]
            ResolvedBackend::Stub { .. } => build_backend_with_policy(
                &scoped,
                name,
                model_override,
                tx_events,
                0,
                ToolPolicy::All,
            ),
        }
    }

    /// Build a fresh execution backend rooted at one disposable workspace.
    pub fn build_controlled_execution(
        self: &Arc<Self>,
        name: &str,
        model_override: Option<&str>,
        tx_events: mpsc::UnboundedSender<RoutedEvent>,
        root: PathBuf,
    ) -> Result<Backend, String> {
        let root = canonical_controlled_root(root)?;
        let working_dir = Arc::new(Mutex::new(root.clone()));
        let scoped = self.with_working_dir(Arc::clone(&working_dir));
        let resolved = scoped.resolve(name, model_override)?;
        match resolved {
            resolved @ ResolvedBackend::Api { .. } => build_controlled_api_backend(
                resolved,
                &scoped,
                tx_events,
                ControlledApiProfile::Execution,
                root,
            ),
            ResolvedBackend::ClaudeCli { model, env, .. } => Ok(Backend::ClaudeCli(Box::new(
                ClaudeCliDriver::new_execution(model, env, working_dir, tx_events),
            ))),
            ResolvedBackend::CodexCli { model, env, .. } => Ok(Backend::CodexCli(Box::new(
                CodexCliDriver::new_execution(model, env, working_dir, tx_events),
            ))),
            #[cfg(feature = "test-support")]
            ResolvedBackend::Stub { .. } => build_backend_with_policy(
                &scoped,
                name,
                model_override,
                tx_events,
                0,
                ToolPolicy::All,
            ),
        }
    }
}

fn canonical_controlled_root(root: PathBuf) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(&root).map_err(|error| {
        format!(
            "controlled backend root {} is unavailable: {error}",
            root.display()
        )
    })?;
    if !canonical.is_dir() {
        return Err(format!(
            "controlled backend root {} is not a directory",
            canonical.display()
        ));
    }
    Ok(canonical)
}
