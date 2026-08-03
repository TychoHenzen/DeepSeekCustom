//! Builds a `Backend` from a named entry in the `backends` map of
//! `settings.json`. `main.rs` calls `BackendFactory::build` once at
//! startup, for the entry `default_backend` names. A future `Task` tool
//! calls it again at runtime, by name, to build a backend for a subagent
//! that may run on a different provider or model than the parent session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::info;

use crate::agent::agent_loop::{AgentConfig, AgentLoop, StreamEvent};
use crate::agent::prompt::SystemPromptBuilder;
use crate::api::client::{ApiClient, Provider, resolve_api_key};
use crate::autopilot::answerer::{PolicyAnswerer, QuestionAnswerer};
use crate::autopilot::policy::PolicyStore;
use crate::backend::Backend;
use crate::config::settings::{ApiProvider, BackendConfig, Settings};
use crate::memory::MemoryStore;
use crate::skills::{SkillLoader, format_skills_for_prompt};
use crate::tools::ToolRegistry;
use crate::tools::{
    ask::AskUserQuestionTool, bash::BashTool, read::ReadTool, reset::ResetTool, task::TaskTool,
    write::WriteTool,
};

/// The pieces needed to build either kind of backend, resolved from a
/// named entry in `settings.json`. `pub(crate)`: a subagent dispatch
/// (`src/backend/subagent.rs`) resolves a `ClaudeCli` entry through
/// `BackendFactory::resolve` to reach `run_once` directly.
#[derive(Debug)]
pub(crate) enum ResolvedBackend {
    /// Enough to build an `ApiClient` and drive an in-process `AgentLoop`.
    Api {
        name: String,
        provider: Provider,
        api_key: String,
        base_url: Option<String>,
        model: String,
    },
    /// Enough to spawn a `claude -p` child through `ClaudeCliDriver`.
    ClaudeCli {
        name: String,
        model: String,
        permission_mode: Option<String>,
        env: Option<HashMap<String, String>>,
    },
}

/// Map the config-side `ApiProvider` to the runtime `Provider` used by
/// `ApiClient`. This lives here, not in `src/config/`. That module must
/// not depend on `src/api/`.
fn map_provider(provider: &ApiProvider) -> Provider {
    match provider {
        ApiProvider::DeepSeek => Provider::DeepSeek,
        ApiProvider::Ollama => Provider::Ollama,
    }
}

/// Resolve one named backend entry, applying `model_override` when given.
/// Returns an error message naming the requested entry and listing the
/// entries that exist when `name` does not match a configured backend.
/// This is the one resolution path both `BackendFactory::build` and the
/// `resolve_active_backend` test helper below go through.
fn resolve_named_backend(
    settings: &Settings,
    project_root: &Path,
    name: &str,
    model_override: Option<&str>,
) -> Result<ResolvedBackend, String> {
    let backend = settings.resolve_backend(name).ok_or_else(|| {
        let known = settings
            .backends()
            .map(|b| b.keys().cloned().collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "none configured".to_string());
        format!("backend \"{name}\" is not a known backend (known: {known})")
    })?;

    match backend {
        BackendConfig::Api {
            provider,
            model,
            base_url,
            api_key,
            models: _,
        } => {
            let runtime_provider = map_provider(provider);
            let key = match api_key {
                Some(k) => k.clone(),
                None => resolve_api_key(runtime_provider, project_root).map_err(|e| e.to_string())?,
            };
            Ok(ResolvedBackend::Api {
                name: name.to_string(),
                provider: runtime_provider,
                api_key: key,
                base_url: base_url.clone(),
                model: model_override
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| model.clone()),
            })
        }
        BackendConfig::ClaudeCli {
            model,
            permission_mode,
            env,
            models: _,
        } => Ok(ResolvedBackend::ClaudeCli {
            name: name.to_string(),
            model: model_override
                .map(|m| m.to_string())
                .unwrap_or_else(|| model.clone()),
            permission_mode: permission_mode.clone(),
            env: env.clone(),
        }),
    }
}

/// Resolve the backend named by `settings.default_backend()`, or
/// `"deepseek"` when that field is absent. Kept as a free function, with
/// the same signature it always had, so the test suite below can call it
/// directly without going through `BackendFactory`. Production code goes
/// through `BackendFactory::build` instead, which is why this is test-only.
#[cfg(test)]
fn resolve_active_backend(settings: &Settings, project_root: &Path) -> Result<ResolvedBackend, String> {
    let name = settings.default_backend().unwrap_or("deepseek").to_string();
    resolve_named_backend(settings, project_root, &name, None)
}

/// True when a backend built at `depth` may dispatch a subagent of its
/// own, below the configured depth limit. Depth 0 is the main session.
/// Each dispatch adds one. At `max_depth` this is false, so the chain
/// stops. A pure function, checkable without building a backend.
fn may_dispatch(depth: u32, max_depth: u32) -> bool {
    depth < max_depth
}

/// Build the `Api` backend: an `AgentLoop` wired up with the tool
/// registry, memory, skills, and system prompt. The claude_cli path
/// skips it, see the comment at that branch in `BackendFactory::build`.
///
/// Takes the factory itself, not its individual fields, so this stays at
/// seven parameters: `settings`, `project_root`, and the shared
/// `interrupt_flag` all come off it. `depth` gates the `Task` tool this
/// backend gets, and `factory.clone()` is what that tool dispatches
/// subagents through.
fn build_api_backend(
    provider: Provider,
    api_key: String,
    base_url: Option<String>,
    model: String,
    factory: &Arc<BackendFactory>,
    tx_events: mpsc::UnboundedSender<StreamEvent>,
    depth: u32,
) -> Backend {
    let settings = &factory.settings;
    let project_root = &factory.project_root;
    let client = ApiClient::new(provider, api_key.clone(), base_url.clone(), Some(model.clone()));

    let memory = MemoryStore::load(project_root);
    let project_skills = SkillLoader::load_all(project_root).unwrap_or_default();
    let global_skills = SkillLoader::load_global().unwrap_or_default();
    let global_count = global_skills.len();
    let skills = SkillLoader::merge(project_skills, global_skills);
    let project_count = skills.len().saturating_sub(global_count);
    info!(
        "loaded {} skills ({} project, {} global)",
        skills.len(),
        project_count,
        global_count,
    );

    // The answerer needs its own client, since `client` above is moved
    // into the agent. Both come from the same resolved backend. An Ollama
    // selection then sends the answerer's questions to Ollama too, so it
    // never demands a DeepSeek key the user does not have.
    let answerer_client = ApiClient::new(provider, api_key, base_url, None);
    let policy_store = PolicyStore::new(project_root.to_path_buf(), settings.autopilot_policy_path());
    let answerer: Arc<dyn QuestionAnswerer> = Arc::new(PolicyAnswerer::new(
        answerer_client,
        policy_store,
        settings.autopilot_answerer_model(),
    ));

    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(BashTool::new(project_root.to_path_buf())));
    tools.register(Arc::new(ReadTool::new(project_root.to_path_buf())));
    tools.register(Arc::new(WriteTool::new(project_root.to_path_buf())));
    tools.register(Arc::new(ResetTool));
    tools.register(Arc::new(AskUserQuestionTool::new(answerer)));
    // Below the depth limit, this backend may dispatch a subagent of its
    // own. The dispatched subagent sits one depth deeper, hence `depth +
    // 1`. At the limit, no `Task` tool goes in and the chain stops.
    if may_dispatch(depth, settings.subagent_max_depth()) {
        tools.register(Arc::new(TaskTool::new(factory.clone(), depth + 1)));
    }
    info!("registered {} tools", tools.list().len());

    let memory_fragment = memory.to_system_prompt_fragment();
    let skills_fragment = format_skills_for_prompt(&skills);
    let tool_defs = tools.to_api_definitions();
    let system_prompt = SystemPromptBuilder::new().build(
        if memory_fragment.is_empty() {
            None
        } else {
            Some(memory_fragment.as_str())
        },
        if skills_fragment.is_empty() {
            None
        } else {
            Some(skills_fragment.as_str())
        },
        &tool_defs,
    );
    info!(
        "system prompt built: {} chars, {} tools defined",
        system_prompt.len(),
        tool_defs.len(),
    );

    let config = AgentConfig {
        model,
        ..Default::default()
    };
    let mut agent = AgentLoop::new(client, tools, system_prompt, config, factory.interrupt_flag.clone());
    agent.set_event_sender(tx_events);
    Backend::Api(Box::new(agent))
}

/// Builds a `Backend` from the `backends` map of a `Settings` value,
/// resolved against a fixed project root. Startup builds one from this.
/// A `Task` tool dispatching a subagent onto a different backend builds
/// another, at runtime, the same way.
pub struct BackendFactory {
    settings: Settings,
    project_root: PathBuf,
    /// Shared with every `AgentLoop` this factory builds, main session or
    /// subagent, so Escape reaches all of them. Defaults to a fresh flag
    /// nobody holds. `with_interrupt_flag` replaces it with the one the
    /// GUI actually has.
    interrupt_flag: Arc<AtomicBool>,
}

impl BackendFactory {
    pub fn new(settings: Settings, project_root: PathBuf) -> Self {
        Self {
            settings,
            project_root,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Replace the factory's interrupt flag with one the caller already
    /// holds. `main.rs` uses this to thread the GUI's own flag through
    /// every backend and subagent the factory builds.
    pub fn with_interrupt_flag(mut self, interrupt_flag: Arc<AtomicBool>) -> Self {
        self.interrupt_flag = interrupt_flag;
        self
    }

    /// The name of the entry `default_backend` selects, falling back to
    /// `"deepseek"` when it is absent.
    pub fn default_backend_name(&self) -> String {
        self.settings
            .default_backend()
            .unwrap_or("deepseek")
            .to_string()
    }

    /// Resolve a backend by name without building it. A subagent dispatch
    /// (`src/backend/subagent.rs`) uses this to reach the raw `ClaudeCli`
    /// fields for `ClaudeCliDriver::run_once`, instead of the long-lived
    /// driver `build` hands back. Same unknown-name error as `build`.
    pub(crate) fn resolve(
        &self,
        name: &str,
        model_override: Option<&str>,
    ) -> Result<ResolvedBackend, String> {
        resolve_named_backend(&self.settings, &self.project_root, name, model_override)
    }

    /// The project root this factory resolves backends against.
    pub(crate) fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Build a backend by name from the `backends` map, optionally
    /// overriding the model the entry declares. An unknown name returns
    /// an error naming the entry requested and listing the entries that
    /// exist. `depth` is 0 for the main session, up by one per subagent
    /// dispatch. It gates the `Task` tool, see `may_dispatch`.
    ///
    /// Takes `self: &Arc<Self>`, not `&self`. That lets it hand a clone
    /// of the factory to any tool it constructs. The `Task` tool T04
    /// adds needs a `BackendFactory` of its own to build subagents.
    pub fn build(
        self: &Arc<Self>,
        name: &str,
        model_override: Option<&str>,
        tx_events: mpsc::UnboundedSender<StreamEvent>,
        depth: u32,
    ) -> Result<Backend, String> {
        let resolved = resolve_named_backend(&self.settings, &self.project_root, name, model_override)?;

        Ok(match resolved {
            ResolvedBackend::Api {
                name,
                provider,
                api_key,
                base_url,
                model,
            } => {
                info!(
                    "resolved backend: name={} kind=api provider={:?} model={} base_url={}",
                    name,
                    provider,
                    model,
                    base_url.as_deref().unwrap_or("(provider default)"),
                );
                build_api_backend(provider, api_key, base_url, model, self, tx_events, depth)
            }
            ResolvedBackend::ClaudeCli {
                name,
                model,
                permission_mode,
                env,
            } => {
                info!(
                    "resolved backend: name={} kind=claude_cli model={} permission_mode={}",
                    name,
                    model,
                    permission_mode
                        .as_deref()
                        .unwrap_or("(default: bypassPermissions)"),
                );
                // The claude_cli path skips the tool registry, the hook
                // runner, the memory store, context pruning, and relevance
                // scoring. Claude Code loads its own CLAUDE.md, its own
                // skills, and its own hooks, and it runs its own tools.
                // There is no flag to hand this harness's tool definitions
                // to the child, and no way for the child to execute them.
                Backend::new_claude_cli(model, permission_mode, env, self.project_root.clone(), tx_events)
            }
        })
    }
}

// Split into its own file to keep this one under the file-length ratchet.
// `#[path]` makes it a child module of `factory`, so `super::*` there
// reaches every private item defined above.
#[cfg(test)]
#[path = "factory_tests.rs"]
mod backend_resolution_tests;
