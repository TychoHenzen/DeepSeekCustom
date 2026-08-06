//! Builds a `Backend` from a named entry in the `backends` map of
//! `settings.json`. `main.rs` calls `BackendFactory::build` once at
//! startup, for the entry `default_backend` names. A future `Task` tool
//! calls it again at runtime, by name, to build a backend for a subagent
//! that may run on a different provider or model than the parent session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::info;

use crate::agent::agent_loop::{AgentConfig, AgentLoop, RoutedEvent};
use crate::agent::prompt::SystemPromptBuilder;
use crate::api::client::{ApiClient, Provider, resolve_api_key};
use crate::autopilot::answerer::{PolicyAnswerer, QuestionAnswerer};
use crate::autopilot::policy::PolicyStore;
use crate::backend::Backend;
use crate::effort::Effort;
use crate::backend::registry::SubagentRegistry;
#[cfg(feature = "test-support")]
use crate::backend::stub::{StubBackend, StubTurn};
use crate::config::settings::{ApiProvider, BackendConfig, Settings};
use crate::memory::MemoryStore;
use crate::skills::{SkillLoader, format_skills_for_prompt};
use crate::tools::ToolRegistry;
use crate::tools::{
    ask::AskUserQuestionTool, bash::BashTool, cd::CdTool, close_session::CloseSessionTool,
    read::ReadTool, read_image::ReadImageTool, reset::ResetTool, send_message::SendMessageTool,
    task::TaskTool, write::WriteTool,
};

/// The pieces needed to build either kind of backend, resolved from a
/// named entry in `settings.json`. `pub`, not `pub(crate)`: a subagent
/// dispatch (`src/backend/subagent.rs`) resolves a `ClaudeCli` entry
/// through `BackendFactory::resolve` to reach `run_once` directly, and the
/// factory's own tests, now in `deepseek-custom-tests/tests/backend_factory.rs`,
/// match on this type from outside the crate, which `pub(crate)` cannot
/// reach. The `Stub` variant keeps its own narrower gate below.
#[derive(Debug)]
pub enum ResolvedBackend {
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
    /// Enough to build a `StubBackend`. Never produced from `settings.json`:
    /// only `BackendFactory::with_stub` puts an entry in the map `resolve`
    /// checks first. Gated the same way as the stub itself, see
    /// `src/backend/stub.rs`.
    #[cfg(feature = "test-support")]
    Stub {
        name: String,
        script: Vec<StubTurn>,
        model: String,
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
/// the same signature it always had, so the test suite (now
/// `deepseek-custom-tests/tests/backend_factory.rs`) can call it directly
/// without going through `BackendFactory`. Production code goes through
/// `BackendFactory::build` instead, which is why this is test-only. `pub`,
/// not merely a plain-test gate: that test suite lives in a separate crate
/// now, so both the gate and the visibility must reach across the crate
/// boundary.
#[cfg(feature = "test-support")]
pub fn resolve_active_backend(settings: &Settings, project_root: &Path) -> Result<ResolvedBackend, String> {
    let name = settings.default_backend().unwrap_or("deepseek").to_string();
    resolve_named_backend(settings, project_root, &name, None)
}

/// The model the policy answerer runs on for this backend.
///
/// An explicit `autopilot.answerer_model` always wins. Without one, the
/// default `deepseek-v4-flash` only fits a DeepSeek backend. Any other
/// provider would be asked for a model it does not have, so the answerer
/// falls back to the model the backend itself runs.
fn answerer_model(settings: &Settings, provider: Provider, backend_model: &str) -> String {
    match settings.autopilot_answerer_model_override() {
        Some(explicit) => explicit,
        None if provider == Provider::DeepSeek => settings.autopilot_answerer_model(),
        None => backend_model.to_string(),
    }
}

/// True when a backend built at `depth` may dispatch a subagent of its
/// own, below the configured depth limit. Depth 0 is the main session.
/// Each dispatch adds one. At `max_depth` this is false, so the chain
/// stops. A pure function, checkable without building a backend.
fn may_dispatch(depth: u32, max_depth: u32) -> bool {
    depth < max_depth
}

/// Test-only entry onto `may_dispatch` for the workspace-split test crate,
/// which cannot reach a private free function across a crate boundary.
/// `may_dispatch` itself stays private and unconditional, since
/// `build_api_backend` below calls it on every backend build; this is a thin
/// wrapper, not a widening of the real function.
#[cfg(feature = "test-support")]
pub fn may_dispatch_for_test(depth: u32, max_depth: u32) -> bool {
    may_dispatch(depth, max_depth)
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
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
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
        answerer_model(settings, provider, &model),
    ));

    // Created before the tools that need it, not after the agent: `TaskTool`
    // registers a `keep_open` session into this same registry, so it needs
    // a handle to it at construction time. The agent gets the identical
    // `Arc` below, once it exists, so "the tool registers into it" and "the
    // agent closes it on turn end" are provably the same registry.
    let subagent_registry = Arc::new(SubagentRegistry::new());
    // Created before the tools too, for the same reason: `TaskTool` needs a
    // handle to it as `parent_effort_flag`, to read this session's current
    // effort level as the default for a dispatch that carries no explicit
    // `effort` override. `agent.set_effort_flag(effort_flag)` below hands
    // this exact `Arc` to the agent as well, in place of the fresh one
    // `AgentLoop::new` would otherwise create from `config.effort`. So
    // "what `TaskTool` reads as the current level" and "what this agent's
    // own `effort_flag()` reports" are provably the same object, the same
    // way `subagent_registry` above is shared between the tool and the
    // agent that closes it.
    let effort_flag: Arc<AtomicU8> = Arc::new(AtomicU8::new(Effort::None.to_u8()));

    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(BashTool::new(factory.working_dir())));
    tools.register(Arc::new(ReadTool::new(factory.working_dir())));
    tools.register(Arc::new(ReadImageTool::new(factory.working_dir())));
    tools.register(Arc::new(WriteTool::new(factory.working_dir())));
    // Not depth-gated, unlike `Task`, `SendMessage`, and `CloseSession`
    // below: a subagent may change its own working directory regardless
    // of how deep the dispatch chain has gone.
    tools.register(Arc::new(CdTool::new(factory.working_dir())));
    tools.register(Arc::new(ResetTool));
    tools.register(Arc::new(AskUserQuestionTool::new(answerer)));
    // Below the depth limit, this backend may dispatch a subagent of its
    // own. The dispatched subagent sits one depth deeper, hence `depth +
    // 1`. At the limit, no `Task` tool goes in and the chain stops.
    if may_dispatch(depth, settings.subagent_max_depth()) {
        tools.register(Arc::new(TaskTool::new(
            factory.clone(),
            depth + 1,
            tx_events.clone(),
            subagent_registry.clone(),
            effort_flag.clone(),
        )));
        // Gated the same way as `Task`, not separately: a session this
        // backend cannot open in the first place is never reachable
        // through `SendMessage` either, so gating the two independently
        // would only let a subagent at the depth limit send follow-ups
        // into sessions it could never have opened.
        tools.register(Arc::new(SendMessageTool::new(
            subagent_registry.clone(),
            settings.session_turn_cap(),
            settings.send_message_call_cap(),
        )));
        // Same gate as `Task` and `SendMessage`, for the same reason: a
        // session that could never be opened at this depth can never need
        // closing at this depth either.
        tools.register(Arc::new(CloseSessionTool::new(subagent_registry.clone())));
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
    agent.set_working_dir(factory.working_dir());
    // A fresh registry per agent, not one shared across the whole dispatch
    // tree. The roadmap's lifetime rule is "a session lives until its
    // parent's turn ends": each agent owns the sessions it opened, and
    // only its own turn end or its own Reset may close them. Handing every
    // agent in the tree the same `Arc` would let any agent's turn end
    // close a completely different agent's still-live session. This is the
    // exact `Arc` `TaskTool` above was given, so a `keep_open` session it
    // registers is closed by this agent's own turn end, never anyone else's.
    agent.set_subagent_registry(subagent_registry);
    // Same `Arc` `TaskTool` above was given as `parent_effort_flag`: a
    // seed written into it now (main.rs, at startup) or later (a GUI
    // control) is visible to a `Task` dispatch's "inherit the session's
    // current level" default with no extra sync step.
    agent.set_effort_flag(effort_flag);
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
    /// Where the `Bash`, `Read`, and `Write` tools act, as distinct from
    /// `project_root`, which stays the fixed anchor for config and memory
    /// files. Starts equal to `project_root`. Shared with every tool this
    /// factory builds the same way `interrupt_flag` is: the tools read it
    /// fresh on every call, so a change takes effect on the next tool use.
    /// There is no path sandbox tying it back to `project_root`, on
    /// purpose: see the phase 4 section of
    /// `docs/plans/2026-08-04-long-term-roadmap.md`.
    working_dir: Arc<Mutex<PathBuf>>,
    /// Named scripts for `StubBackend`, checked by `resolve` before it ever
    /// looks at `settings.backends`. Always empty outside a test build or
    /// the `test-support` feature: only `with_stub` inserts into it, and
    /// that method carries the same gate. This, together with the gate on
    /// `Backend::Stub` and `ResolvedBackend::Stub` themselves, is what keeps
    /// a stub unreachable from a normal run.
    #[cfg(feature = "test-support")]
    stubs: HashMap<String, Vec<StubTurn>>,
}

impl BackendFactory {
    pub fn new(settings: Settings, project_root: PathBuf) -> Self {
        let working_dir = Arc::new(Mutex::new(project_root.clone()));
        Self {
            settings,
            project_root,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            working_dir,
            #[cfg(feature = "test-support")]
            stubs: HashMap::new(),
        }
    }

    /// Replace the factory's interrupt flag with one the caller already
    /// holds. `main.rs` uses this to thread the GUI's own flag through
    /// every backend and subagent the factory builds.
    pub fn with_interrupt_flag(mut self, interrupt_flag: Arc<AtomicBool>) -> Self {
        self.interrupt_flag = interrupt_flag;
        self
    }

    /// Register a named script so `build`/`resolve` produce a
    /// `StubBackend` for that name instead of consulting
    /// `settings.backends`. Gated on `#[cfg(feature = "test-support")]`,
    /// the same as the rest of the stub path: this is the one place a
    /// stub can enter a `BackendFactory` at all, and it is reachable only
    /// from the external `deepseek-custom-tests` crate, which turns the
    /// feature on.
    #[cfg(feature = "test-support")]
    pub fn with_stub(mut self, name: impl Into<String>, script: Vec<StubTurn>) -> Self {
        self.stubs.insert(name.into(), script);
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
        #[cfg(feature = "test-support")]
        if let Some(script) = self.stubs.get(name) {
            return Ok(ResolvedBackend::Stub {
                name: name.to_string(),
                script: script.clone(),
                model: model_override
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "stub-model".to_string()),
            });
        }
        resolve_named_backend(&self.settings, &self.project_root, name, model_override)
    }

    /// The shared interrupt flag every backend this factory builds gets.
    /// A `claude_cli` subagent runs outside `AgentLoop`, so it needs this
    /// handle directly. Without it, Escape could not stop that subagent.
    pub(crate) fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    /// The shared working directory every tool this factory builds reads
    /// from, fresh on every call. Starts equal to `project_root`. A clone
    /// of the `Arc`, not a snapshot of the path: writing through it (a
    /// GUI control, the `Cd` tool) is visible to every tool holding a
    /// clone. `pub`, not `pub(crate)`: `main.rs`, in the binary crate,
    /// reads this once at startup to seed the GUI's working-directory
    /// control the same way it already reads the other shared handles off
    /// `Backend`.
    pub fn working_dir(&self) -> Arc<Mutex<PathBuf>> {
        Arc::clone(&self.working_dir)
    }

    /// A snapshot of this factory's current working directory, as a plain
    /// `PathBuf` rather than the shared `Arc`. Used by a subagent dispatch
    /// (`src/backend/subagent.rs`) to seed a fresh, independent working
    /// directory for a subagent that carries no `working_dir` override of
    /// its own: the subagent starts where its parent currently stands, but
    /// from a value it owns, not one it shares.
    pub(crate) fn working_dir_snapshot(&self) -> PathBuf {
        self.working_dir.lock().unwrap().clone()
    }

    /// Test-only entry onto `working_dir_snapshot` for the workspace-split
    /// test crate, which `pub(crate)` cannot reach. `working_dir_snapshot`
    /// itself stays `pub(crate)` and unconditional: `src/backend/subagent.rs`
    /// calls it on every subagent dispatch. This is a thin wrapper, not a
    /// widening of the real method.
    #[cfg(feature = "test-support")]
    pub fn working_dir_snapshot_for_test(&self) -> PathBuf {
        self.working_dir_snapshot()
    }

    /// A clone of this factory with `working_dir` replaced by the given
    /// value, everything else unchanged. A build made through the result
    /// acts in that directory instead of this factory's own shared one.
    ///
    /// This is how a subagent dispatch gets its own working directory
    /// without ever touching the parent's: the parent's `working_dir` stays
    /// exactly the `Arc` it always was, untouched by anything the returned
    /// factory or a backend built through it does. A subagent's own `Cd`
    /// tool call writes only the `Arc` passed in here. If that subagent
    /// dispatches a `Task` of its own, the nested dispatch resolves its
    /// working directory against this same clone, so an inherited
    /// grandchild sees its immediate parent's current directory, not the
    /// top-level session's.
    pub(crate) fn with_working_dir(&self, working_dir: Arc<Mutex<PathBuf>>) -> Arc<Self> {
        Arc::new(Self {
            settings: self.settings.clone(),
            project_root: self.project_root.clone(),
            interrupt_flag: Arc::clone(&self.interrupt_flag),
            working_dir,
            #[cfg(feature = "test-support")]
            stubs: self.stubs.clone(),
        })
    }

    /// Test-only entry onto `with_working_dir` for the workspace-split test
    /// crate, which `pub(crate)` cannot reach. `with_working_dir` itself
    /// stays `pub(crate)` and unconditional: `src/backend/subagent.rs` calls
    /// it on every subagent dispatch. This is a thin wrapper, not a
    /// widening of the real method.
    #[cfg(feature = "test-support")]
    pub fn with_working_dir_for_test(&self, working_dir: Arc<Mutex<PathBuf>>) -> Arc<Self> {
        self.with_working_dir(working_dir)
    }

    /// The per-session turn cap a subagent dispatch reports on its
    /// `RouteHop`s, so the GUI's subagent block header can show it. See
    /// `Settings::session_turn_cap`.
    pub(crate) fn session_turn_cap(&self) -> u32 {
        self.settings.session_turn_cap()
    }

    /// The per-parent-turn `SendMessage` call cap a subagent dispatch
    /// reports on its `RouteHop`s, for the same reason. See
    /// `Settings::send_message_call_cap`.
    pub(crate) fn send_message_call_cap(&self) -> u32 {
        self.settings.send_message_call_cap()
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
        tx_events: mpsc::UnboundedSender<RoutedEvent>,
        depth: u32,
    ) -> Result<Backend, String> {
        let resolved = self.resolve(name, model_override)?;

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
                Backend::new_claude_cli(model, permission_mode, env, self.working_dir(), tx_events)
            }
            #[cfg(feature = "test-support")]
            ResolvedBackend::Stub { name, script, model } => {
                info!(
                    "resolved backend: name={} kind=stub turns={}",
                    name,
                    script.len(),
                );
                let mut stub = StubBackend::new(script, model, self.interrupt_flag.clone());
                stub.set_event_sender(tx_events);
                Backend::Stub(Box::new(stub))
            }
        })
    }
}
