//! Builds a `Backend` from a named entry in the `backends` map of
//! `settings.json`. `main.rs` calls `BackendFactory::build` once at
//! startup, for the entry `default_backend` names. A `Task` tool
//! dispatches subagents through the same factory at runtime.

#[cfg(feature = "test-support")]
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
#[cfg(feature = "test-support")]
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::agent::events::RoutedEvent;
use crate::backend::codex_cli::CodexCliDriver;
#[cfg(feature = "test-support")]
use crate::backend::stub::StubTurn;
use crate::backend::{Backend, SharedFlags};
use crate::config::settings::{BackendConfig, Settings};
use crate::mcp::McpManager;
use crate::tools::ToolRegistry;

use super::build_api::build_backend;
use super::resolved::{self, ResolvedBackend};

/// True when a backend built at `depth` may dispatch a subagent of its
/// own, below the configured depth limit. Depth 0 is the main session.
/// Each dispatch adds one. At `max_depth` this is false, so the chain
/// stops. A pure function, checkable without building a backend.
pub fn may_dispatch(depth: u32, max_depth: u32) -> bool {
    depth < max_depth
}

// ---------------------------------------------------------------------------
// BackendFactory
// ---------------------------------------------------------------------------

/// Builds a `Backend` from the `backends` map of a `Settings` value,
/// resolved against a fixed project root. Startup builds one from this.
/// A `Task` tool dispatching a subagent onto a different backend builds
/// another, at runtime, the same way.
pub struct BackendFactory {
    pub(super) settings: Settings,
    pub(super) project_root: PathBuf,
    /// Shared with every `AgentLoop` this factory builds, main session or
    /// subagent, so Escape reaches all of them.
    interrupt_flag: Arc<AtomicBool>,
    /// Where the `Bash`, `Read`, and `Write` tools act, as distinct from
    /// `project_root`, which stays the fixed anchor for config and memory
    /// files.
    working_dir: Arc<Mutex<PathBuf>>,
    /// The handles the GUI holds, adopted by every backend this factory
    /// builds at depth 0.
    session_flags: Option<SharedFlags>,
    /// The MCP servers this process started, shared by every backend the
    /// factory builds.
    mcp: Option<Arc<McpManager>>,
    /// Named scripts for `StubBackend`, checked by `resolve` before it ever
    /// looks at `settings.backends`. Always empty outside a test build.
    #[cfg(feature = "test-support")]
    stubs: HashMap<String, (Vec<StubTurn>, Arc<AtomicUsize>)>,
}

impl BackendFactory {
    pub fn new(settings: Settings, project_root: PathBuf) -> Self {
        let working_dir = Arc::new(Mutex::new(project_root.clone()));
        Self {
            settings,
            project_root,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            working_dir,
            session_flags: None,
            mcp: None,
            #[cfg(feature = "test-support")]
            stubs: HashMap::new(),
        }
    }

    /// Hand the factory the process's MCP servers.
    pub fn with_mcp(mut self, mcp: Arc<McpManager>) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// Register the MCP tools known so far into this registry, and sign it
    /// up for the ones still to arrive.
    pub(super) fn attach_mcp(&self, tools: &ToolRegistry) {
        let Some(mcp) = self.mcp.clone() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let registry = tools.clone();
        tokio::spawn(async move { mcp.attach(&registry).await });
    }

    /// Hand the factory the handles the GUI holds.
    pub fn with_session_flags(mut self, flags: SharedFlags) -> Self {
        self.session_flags = Some(flags);
        self
    }

    /// The session flags a depth-0 build should adopt, if this factory was
    /// given any.
    pub(super) fn session_flags_for(&self, depth: u32) -> Option<&SharedFlags> {
        if depth == 0 {
            self.session_flags.as_ref()
        } else {
            None
        }
    }

    /// Replace the factory's interrupt flag with one the caller already holds.
    pub fn with_interrupt_flag(mut self, interrupt_flag: Arc<AtomicBool>) -> Self {
        self.interrupt_flag = interrupt_flag;
        self
    }

    /// Register a named script so `build`/`resolve` produce a
    /// `StubBackend` for that name.
    #[cfg(feature = "test-support")]
    pub fn with_stub(mut self, name: impl Into<String>, script: Vec<StubTurn>) -> Self {
        self.stubs
            .insert(name.into(), (script, Arc::new(AtomicUsize::new(0))));
        self
    }

    /// The name of the entry `default_backend` selects.
    pub fn default_backend_name(&self) -> String {
        self.settings
            .default_backend()
            .unwrap_or("deepseek")
            .to_string()
    }

    /// Resolve a backend by name without building it.
    /// Resolve one configured backend and optional model override for an
    /// independent production runner.
    pub fn resolve(
        &self,
        name: &str,
        model_override: Option<&str>,
    ) -> Result<ResolvedBackend, String> {
        #[cfg(feature = "test-support")]
        if let Some((script, cursor)) = self.stubs.get(name) {
            return Ok(ResolvedBackend::Stub {
                name: name.to_string(),
                script: script.clone(),
                model: model_override
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "stub-model".to_string()),
                cursor: Arc::clone(cursor),
            });
        }
        resolved::resolve_named_backend(&self.settings, &self.project_root, name, model_override)
    }

    /// The shared interrupt flag every backend this factory builds gets.
    pub(crate) fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    /// The shared working directory every tool this factory builds reads
    /// from, fresh on every call.
    pub fn working_dir(&self) -> Arc<Mutex<PathBuf>> {
        Arc::clone(&self.working_dir)
    }

    /// A snapshot of this factory's current working directory.
    pub(crate) fn working_dir_snapshot(&self) -> PathBuf {
        self.working_dir.lock().unwrap().clone()
    }

    /// Test-only entry onto `working_dir_snapshot`.
    #[cfg(feature = "test-support")]
    pub fn working_dir_snapshot_for_test(&self) -> PathBuf {
        self.working_dir_snapshot()
    }

    /// A clone of this factory with `working_dir` replaced by the given
    /// value, everything else unchanged.
    pub(crate) fn with_working_dir(&self, working_dir: Arc<Mutex<PathBuf>>) -> Arc<Self> {
        Arc::new(Self {
            settings: self.settings.clone(),
            project_root: self.project_root.clone(),
            interrupt_flag: Arc::clone(&self.interrupt_flag),
            working_dir,
            session_flags: None,
            mcp: self.mcp.clone(),
            #[cfg(feature = "test-support")]
            stubs: self.stubs.clone(),
        })
    }

    /// Test-only entry onto `with_working_dir`.
    #[cfg(feature = "test-support")]
    pub fn with_working_dir_for_test(&self, working_dir: Arc<Mutex<PathBuf>>) -> Arc<Self> {
        self.with_working_dir(working_dir)
    }

    /// The per-session turn cap a subagent dispatch reports on its
    /// `RouteHop`s.
    pub(crate) fn session_turn_cap(&self) -> u32 {
        self.settings.session_turn_cap()
    }

    /// The per-parent-turn `SendMessage` call cap a subagent dispatch
    /// reports on its `RouteHop`s.
    pub(crate) fn send_message_call_cap(&self) -> u32 {
        self.settings.send_message_call_cap()
    }

    /// Build a backend by name from the `backends` map.
    /// Delegates to the free function in `build_api.rs` so this file
    /// stays under the file-length bound.
    pub fn build(
        self: &Arc<Self>,
        name: &str,
        model_override: Option<&str>,
        tx_events: mpsc::UnboundedSender<RoutedEvent>,
        depth: u32,
    ) -> Result<crate::backend::Backend, String> {
        #[cfg(feature = "test-support")]
        if self.stubs.contains_key(name) {
            return build_backend(self, name, model_override, tx_events, depth);
        }

        if let Some(BackendConfig::CodexCli {
            model,
            sandbox,
            env,
            models: _,
        }) = self.settings.resolve_backend(name)
        {
            let model = model_override.unwrap_or(model).to_owned();
            let driver = CodexCliDriver::new(
                model,
                sandbox.clone(),
                env.clone(),
                self.working_dir(),
                tx_events,
            );
            let mut backend = Backend::CodexCli(Box::new(driver));
            if let Some(flags) = self.session_flags_for(depth) {
                backend.adopt_flags(flags);
            }
            return Ok(backend);
        }

        build_backend(self, name, model_override, tx_events, depth)
    }
}
