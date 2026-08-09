//! The runtime backend an agent turn runs against: either the in-process
//! DeepSeek/Ollama HTTP client (`AgentLoop`), or a `claude -p` subprocess
//! driven over stream-json (`ClaudeCliDriver`). `main.rs` builds one
//! `Backend` at startup from the resolved config entry. The GUI never sees
//! the difference: both variants expose the same six shared flags.

pub mod claude_cli;
pub mod factory;
pub mod registry;
pub mod stub;
pub mod subagent;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::agent::agent_loop::{AgentLoop, DEFAULT_CONTEXT_BUDGET, RoutedEvent};
use crate::agent::repeat::run_repeat;
use crate::api::types::{ImageAttachment, Message};
use crate::error::Result;

use claude_cli::process::ClaudeCliDriver;
#[cfg(feature = "test-support")]
use stub::StubBackend;

/// The six handles the GUI holds onto for the whole life of the process,
/// independent of which backend is currently running behind them.
///
/// Each backend kind owns a set of these of its own. Before backends could
/// be switched at runtime, `main.rs` simply read the startup backend's own
/// handles off it and gave those to the GUI. That stopped working the
/// moment a switch could replace the backend: the GUI would keep writing
/// into the handles of a backend nobody drives anymore, and Escape, the
/// effort control, the model picker, and the voice-mode toggle would all
/// go silently dead.
///
/// So the handles are created once, here, and every backend built for the
/// GUI's own session adopts them instead of keeping the ones it made for
/// itself. See `BackendFactory::with_session_flags`, which is what carries
/// them into a build. A subagent never adopts them: it gets its own, so a
/// subagent's effort level and model never move the session's.
#[derive(Clone)]
pub struct SharedFlags {
    pub interrupt: Arc<AtomicBool>,
    pub effort: Arc<AtomicU8>,
    pub voice_mode: Arc<AtomicBool>,
    pub context_budget: Arc<AtomicUsize>,
    pub model: Arc<Mutex<String>>,
    pub repeat_interrupt: Arc<AtomicBool>,
}

impl SharedFlags {
    /// A fresh set, with `model` seeded to the given name. `main.rs` builds
    /// one of these before it builds any backend at all, then seeds the
    /// effort and context-budget values from settings.
    pub fn new(model: String) -> Self {
        Self {
            interrupt: Arc::new(AtomicBool::new(false)),
            effort: Arc::new(AtomicU8::new(crate::effort::Effort::None.to_u8())),
            voice_mode: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(DEFAULT_CONTEXT_BUDGET)),
            model: Arc::new(Mutex::new(model)),
            repeat_interrupt: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Overwrite the shared model name. Called on a backend switch, since
    /// the incoming entry declares a model of its own and the outgoing
    /// entry's name would otherwise be sent to it on the next turn.
    pub fn set_model(&self, model: String) {
        if let Ok(mut held) = self.model.lock() {
            *held = model;
        }
    }
}

/// The active backend for one running session. Built at startup from
/// `default_backend`, rebuilt in place whenever the settings sidebar picks
/// a different entry, and driven per turn by the agent task in `main.rs`.
/// `Stub` only ever comes from `BackendFactory::with_stub`. That builder,
/// and this variant, are gated on `#[cfg(feature = "test-support")]`: no
/// `settings.json` entry can produce a `Stub` in a normal build, see
/// `src/backend/stub.rs` for why the gate exists.
pub enum Backend {
    Api(Box<AgentLoop>),
    ClaudeCli(Box<ClaudeCliDriver>),
    #[cfg(feature = "test-support")]
    Stub(Box<StubBackend>),
}

impl Backend {
    /// Build the `ClaudeCli` variant from a resolved config entry. Thin
    /// wrapper so `main.rs` does not need to reach into
    /// `backend::claude_cli::process` directly.
    pub fn new_claude_cli(
        model: String,
        permission_mode: Option<String>,
        env: Option<HashMap<String, String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        tx_events: mpsc::UnboundedSender<RoutedEvent>,
    ) -> Self {
        Backend::ClaudeCli(Box::new(ClaudeCliDriver::new(
            model,
            permission_mode,
            env,
            working_dir,
            tx_events,
        )))
    }

    /// Run one user turn against whichever backend is active. The `Api`
    /// variant returns the response text segments `AgentLoop::run` collects.
    /// The `ClaudeCli` variant streams its reply as `StreamEvent`s instead,
    /// so it always returns an empty vector on success.
    pub async fn run(&mut self, input: &str) -> Result<Vec<String>> {
        self.run_with_image(input, None).await
    }

    /// Same as `run`, with an optional image attachment. The `Api` variant
    /// maps it per provider in `AgentLoop::run_with_image`. The `ClaudeCli`
    /// variant maps it onto the Anthropic content-block shape in
    /// `ClaudeCliDriver::send_with_image`. The `Stub` variant has no image
    /// handling of its own; it is test-only and never carries an
    /// attachment, so the image is simply unused there. `run` is this
    /// method called with no image, so a turn with no attachment is
    /// unaffected on every backend.
    pub async fn run_with_image(
        &mut self,
        input: &str,
        image: Option<&ImageAttachment>,
    ) -> Result<Vec<String>> {
        match self {
            Backend::Api(agent) => agent.run_with_image(input, image).await,
            Backend::ClaudeCli(driver) => {
                driver.send_with_image(input, image).await?;
                Ok(Vec::new())
            }
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.run(input).await,
        }
    }

    /// Run an autopilot repeat loop against whichever backend is active.
    /// Both variants drive the same `run_repeat` loop in
    /// `src/agent/repeat.rs`, through the `RepeatTarget` trait each
    /// implements its own way.
    pub async fn run_repeat(&mut self, task: &str, iterations: u32) {
        match self {
            Backend::Api(agent) => run_repeat(agent.as_mut(), task, iterations).await,
            Backend::ClaudeCli(driver) => run_repeat(driver.as_mut(), task, iterations).await,
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => run_repeat(stub.as_mut(), task, iterations).await,
        }
    }

    /// Replace this backend's own six handles with the ones the GUI
    /// already holds, so a backend built to replace another answers to the
    /// same controls. `BackendFactory::build` calls this on every backend
    /// it builds at depth 0, and never on a subagent.
    pub fn adopt_flags(&mut self, flags: &SharedFlags) {
        match self {
            Backend::Api(agent) => agent.adopt_flags(flags),
            Backend::ClaudeCli(driver) => driver.adopt_flags(flags),
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.adopt_flags(flags),
        }
    }

    /// Release whatever this backend holds outside the process, before it
    /// is dropped for another one. Only the `ClaudeCli` variant has any:
    /// its child process, which `shutdown` kills. The other two hold
    /// nothing beyond memory, so this is where a switch ends for them.
    pub async fn shutdown(&mut self) {
        match self {
            Backend::Api(_) => {}
            Backend::ClaudeCli(driver) => driver.shutdown().await,
            #[cfg(feature = "test-support")]
            Backend::Stub(_) => {}
        }
    }

    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.interrupt_flag(),
            Backend::ClaudeCli(driver) => driver.interrupt_flag(),
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.interrupt_flag(),
        }
    }

    pub fn effort_flag(&self) -> Arc<AtomicU8> {
        match self {
            Backend::Api(agent) => agent.effort_flag(),
            Backend::ClaudeCli(driver) => driver.effort_flag(),
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.effort_flag(),
        }
    }

    pub fn voice_mode_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.voice_mode_flag(),
            Backend::ClaudeCli(driver) => driver.voice_mode_flag(),
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.voice_mode_flag(),
        }
    }

    pub fn context_budget_flag(&self) -> Arc<AtomicUsize> {
        match self {
            Backend::Api(agent) => agent.context_budget_flag(),
            Backend::ClaudeCli(driver) => driver.context_budget_flag(),
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.context_budget_flag(),
        }
    }

    pub fn model_flag(&self) -> Arc<Mutex<String>> {
        match self {
            Backend::Api(agent) => agent.model_flag(),
            Backend::ClaudeCli(driver) => driver.model_flag(),
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.model_flag(),
        }
    }

    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.repeat_interrupt_flag(),
            Backend::ClaudeCli(driver) => driver.repeat_interrupt_flag(),
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.repeat_interrupt_flag(),
        }
    }

    /// Start a fresh, empty conversation. The `Api` variant clears its
    /// message history in place. The `ClaudeCli` variant shuts its child
    /// down, the same shutdown `run_repeat`'s reset path already uses
    /// between autopilot iterations, so the next turn spawns a fresh
    /// child with no prior conversation.
    pub async fn start_new_session(&mut self) {
        match self {
            Backend::Api(agent) => agent.clear_history(),
            Backend::ClaudeCli(driver) => driver.shutdown().await,
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.reset(),
        }
    }

    /// Load a saved conversation. `messages` restores the `Api` variant's
    /// history in place of whatever it held. `claude_session_id` is stored
    /// on the `ClaudeCli` variant for a later `--resume` (added in S07);
    /// this step only holds the value and respawns the child so the next
    /// turn starts clean, the same shutdown `start_new_session` uses.
    pub async fn load_session(
        &mut self,
        messages: Vec<Message>,
        claude_session_id: Option<String>,
    ) {
        match self {
            Backend::Api(agent) => agent.restore_history(messages),
            Backend::ClaudeCli(driver) => {
                driver.set_claude_session_id(claude_session_id);
                driver.shutdown().await;
            }
            // A stub carries no message history and no claude session id
            // of its own. Loading a session onto it can only mean
            // restarting its script from the top, the same as a reset.
            #[cfg(feature = "test-support")]
            Backend::Stub(stub) => stub.reset(),
        }
    }
}
