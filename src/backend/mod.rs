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

use crate::agent::agent_loop::{AgentLoop, RoutedEvent};
use crate::agent::repeat::run_repeat;
use crate::api::types::{ImageAttachment, Message};
use crate::error::Result;

use claude_cli::process::ClaudeCliDriver;
use stub::StubBackend;

/// The active backend for one running session. Built once at startup and
/// driven per turn by the agent task in `main.rs`. `Stub` only ever comes
/// from `BackendFactory::with_stub`, a `#[cfg(test)]` builder: no
/// `settings.json` entry can produce it, see `src/backend/stub.rs`.
pub enum Backend {
    Api(Box<AgentLoop>),
    ClaudeCli(ClaudeCliDriver),
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
        Backend::ClaudeCli(ClaudeCliDriver::new(
            model,
            permission_mode,
            env,
            working_dir,
            tx_events,
        ))
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
            Backend::ClaudeCli(driver) => run_repeat(driver, task, iterations).await,
            Backend::Stub(stub) => run_repeat(stub.as_mut(), task, iterations).await,
        }
    }

    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.interrupt_flag(),
            Backend::ClaudeCli(driver) => driver.interrupt_flag(),
            Backend::Stub(stub) => stub.interrupt_flag(),
        }
    }

    pub fn effort_flag(&self) -> Arc<AtomicU8> {
        match self {
            Backend::Api(agent) => agent.effort_flag(),
            Backend::ClaudeCli(driver) => driver.effort_flag(),
            Backend::Stub(stub) => stub.effort_flag(),
        }
    }

    pub fn voice_mode_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.voice_mode_flag(),
            Backend::ClaudeCli(driver) => driver.voice_mode_flag(),
            Backend::Stub(stub) => stub.voice_mode_flag(),
        }
    }

    pub fn context_budget_flag(&self) -> Arc<AtomicUsize> {
        match self {
            Backend::Api(agent) => agent.context_budget_flag(),
            Backend::ClaudeCli(driver) => driver.context_budget_flag(),
            Backend::Stub(stub) => stub.context_budget_flag(),
        }
    }

    pub fn model_flag(&self) -> Arc<Mutex<String>> {
        match self {
            Backend::Api(agent) => agent.model_flag(),
            Backend::ClaudeCli(driver) => driver.model_flag(),
            Backend::Stub(stub) => stub.model_flag(),
        }
    }

    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.repeat_interrupt_flag(),
            Backend::ClaudeCli(driver) => driver.repeat_interrupt_flag(),
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
            Backend::Stub(stub) => stub.reset(),
        }
    }

    /// Load a saved conversation. `messages` restores the `Api` variant's
    /// history in place of whatever it held. `claude_session_id` is stored
    /// on the `ClaudeCli` variant for a later `--resume` (added in S07);
    /// this step only holds the value and respawns the child so the next
    /// turn starts clean, the same shutdown `start_new_session` uses.
    pub async fn load_session(&mut self, messages: Vec<Message>, claude_session_id: Option<String>) {
        match self {
            Backend::Api(agent) => agent.restore_history(messages),
            Backend::ClaudeCli(driver) => {
                driver.set_claude_session_id(claude_session_id);
                driver.shutdown().await;
            }
            // A stub carries no message history and no claude session id
            // of its own. Loading a session onto it can only mean
            // restarting its script from the top, the same as a reset.
            Backend::Stub(stub) => stub.reset(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agent_loop::StreamEvent;
    use std::sync::atomic::Ordering;

    fn test_working_dir() -> Arc<Mutex<PathBuf>> {
        Arc::new(Mutex::new(PathBuf::from(".")))
    }

    fn new_test_claude_cli_backend() -> Backend {
        let (tx, _rx) = mpsc::unbounded_channel();
        Backend::new_claude_cli(
            "opus".to_string(),
            None,
            None,
            test_working_dir(),
            tx,
        )
    }

    #[test]
    fn claude_cli_backend_exposes_all_six_flags_and_round_trips_writes() {
        let backend = new_test_claude_cli_backend();

        backend.interrupt_flag().store(true, Ordering::SeqCst);
        assert!(backend.interrupt_flag().load(Ordering::SeqCst));

        backend.effort_flag().store(3, Ordering::SeqCst);
        assert_eq!(backend.effort_flag().load(Ordering::SeqCst), 3);

        backend.voice_mode_flag().store(true, Ordering::SeqCst);
        assert!(backend.voice_mode_flag().load(Ordering::SeqCst));

        backend.context_budget_flag().store(64_000, Ordering::SeqCst);
        assert_eq!(backend.context_budget_flag().load(Ordering::SeqCst), 64_000);

        *backend.model_flag().lock().unwrap() = "sonnet".to_string();
        assert_eq!(*backend.model_flag().lock().unwrap(), "sonnet");

        backend.repeat_interrupt_flag().store(true, Ordering::SeqCst);
        assert!(backend.repeat_interrupt_flag().load(Ordering::SeqCst));
    }

    fn new_test_api_backend() -> Backend {
        let client = crate::api::client::ApiClient::new(
            crate::api::client::Provider::DeepSeek,
            "sk-test".into(),
            None,
            None,
        );
        let tools = crate::tools::ToolRegistry::new();
        let agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            crate::agent::agent_loop::AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        Backend::Api(Box::new(agent))
    }

    fn api_history_len(backend: &Backend) -> usize {
        match backend {
            Backend::Api(agent) => agent.history().len(),
            Backend::ClaudeCli(_) => panic!("expected Api backend"),
            Backend::Stub(_) => panic!("expected Api backend"),
        }
    }

    #[tokio::test]
    async fn api_backend_new_session_leaves_empty_history_with_system_prompt() {
        let mut backend = new_test_api_backend();
        backend
            .load_session(vec![Message::user("hello".into())], None)
            .await;
        assert_eq!(api_history_len(&backend), 1);

        backend.start_new_session().await;

        assert_eq!(api_history_len(&backend), 0);
    }

    #[tokio::test]
    async fn api_backend_load_session_leaves_exactly_restored_messages() {
        let mut backend = new_test_api_backend();
        let messages = vec![
            Message::user("first".into()),
            Message::assistant("second".into()),
        ];

        backend.load_session(messages, None).await;

        assert_eq!(api_history_len(&backend), 2);
    }

    #[tokio::test]
    async fn api_backend_new_session_after_load_does_not_stick() {
        let mut backend = new_test_api_backend();
        backend
            .load_session(vec![Message::user("hello".into())], None)
            .await;

        backend.start_new_session().await;
        backend.start_new_session().await;

        assert_eq!(api_history_len(&backend), 0);
    }

    #[tokio::test]
    async fn claude_cli_driver_accepts_and_returns_stored_session_id() {
        let mut backend = new_test_claude_cli_backend();
        match &mut backend {
            Backend::ClaudeCli(driver) => {
                assert_eq!(driver.claude_session_id(), None);
                driver.set_claude_session_id(Some("abc-123".into()));
                assert_eq!(driver.claude_session_id(), Some("abc-123"));
            }
            Backend::Api(_) => panic!("expected ClaudeCli backend"),
            Backend::Stub(_) => panic!("expected ClaudeCli backend"),
        }
    }

    #[tokio::test]
    async fn claude_cli_backend_load_session_stores_id_without_spawning() {
        let mut backend = new_test_claude_cli_backend();

        backend
            .load_session(Vec::new(), Some("resume-me".into()))
            .await;

        match &backend {
            Backend::ClaudeCli(driver) => {
                assert_eq!(driver.claude_session_id(), Some("resume-me"));
            }
            Backend::Api(_) => panic!("expected ClaudeCli backend"),
            Backend::Stub(_) => panic!("expected ClaudeCli backend"),
        }
    }

    // These two tests exercise `Backend::run_repeat` on the `ClaudeCli`
    // variant without ever spawning the `claude` binary. Zero iterations
    // and an interrupt flag already set both short-circuit before the
    // loop's first call to `RepeatTarget::run_turn`. That call is the only
    // place this backend would touch a real child process. A test that
    // let the loop reach a real turn would spawn `claude` for real. This
    // suite must not do that.

    #[tokio::test]
    async fn claude_cli_backend_run_repeat_zero_iterations_emits_only_finished() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut backend = Backend::new_claude_cli(
            "opus".to_string(),
            None,
            None,
            test_working_dir(),
            tx,
        );

        backend.run_repeat("do the thing", 0).await;

        let event = rx.try_recv().expect("expected a RepeatFinished event");
        match event.event {
            StreamEvent::RepeatFinished { completed, total } => {
                assert_eq!(completed, 0);
                assert_eq!(total, 0);
            }
            other => panic!("expected RepeatFinished, got {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn claude_cli_backend_run_repeat_stops_immediately_when_interrupt_flag_already_set() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut backend = Backend::new_claude_cli(
            "opus".to_string(),
            None,
            None,
            test_working_dir(),
            tx,
        );
        backend.repeat_interrupt_flag().store(true, Ordering::SeqCst);

        backend.run_repeat("do the thing", 3).await;

        let mut events: Vec<StreamEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev.event);
        }
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, StreamEvent::RepeatIterationStart { .. }))
        );
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::RepeatFinished { completed, total } => {
                assert_eq!(*completed, 0);
                assert_eq!(*total, 3);
            }
            other => panic!("expected RepeatFinished, got {other:?}"),
        }
    }
}
