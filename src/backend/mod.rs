//! The runtime backend an agent turn runs against: either the in-process
//! DeepSeek/Ollama HTTP client (`AgentLoop`), or a `claude -p` subprocess
//! driven over stream-json (`ClaudeCliDriver`). `main.rs` builds one
//! `Backend` at startup from the resolved config entry. The GUI never sees
//! the difference: both variants expose the same six shared flags.

pub mod claude_cli;
pub mod factory;
pub mod subagent;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::agent::agent_loop::{AgentLoop, StreamEvent};
use crate::agent::repeat::run_repeat;
use crate::error::Result;

use claude_cli::process::ClaudeCliDriver;

/// The active backend for one running session. Built once at startup and
/// driven per turn by the agent task in `main.rs`.
pub enum Backend {
    Api(Box<AgentLoop>),
    ClaudeCli(ClaudeCliDriver),
}

impl Backend {
    /// Build the `ClaudeCli` variant from a resolved config entry. Thin
    /// wrapper so `main.rs` does not need to reach into
    /// `backend::claude_cli::process` directly.
    pub fn new_claude_cli(
        model: String,
        permission_mode: Option<String>,
        env: Option<HashMap<String, String>>,
        project_root: PathBuf,
        tx_events: mpsc::UnboundedSender<StreamEvent>,
    ) -> Self {
        Backend::ClaudeCli(ClaudeCliDriver::new(
            model,
            permission_mode,
            env,
            project_root,
            tx_events,
        ))
    }

    /// Run one user turn against whichever backend is active. The `Api`
    /// variant returns the response text segments `AgentLoop::run` collects.
    /// The `ClaudeCli` variant streams its reply as `StreamEvent`s instead,
    /// so it always returns an empty vector on success.
    pub async fn run(&mut self, input: &str) -> Result<Vec<String>> {
        match self {
            Backend::Api(agent) => agent.run(input).await,
            Backend::ClaudeCli(driver) => {
                driver.send(input).await?;
                Ok(Vec::new())
            }
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
        }
    }

    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.interrupt_flag(),
            Backend::ClaudeCli(driver) => driver.interrupt_flag(),
        }
    }

    pub fn thinking_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.thinking_flag(),
            Backend::ClaudeCli(driver) => driver.thinking_flag(),
        }
    }

    pub fn voice_mode_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.voice_mode_flag(),
            Backend::ClaudeCli(driver) => driver.voice_mode_flag(),
        }
    }

    pub fn context_budget_flag(&self) -> Arc<AtomicUsize> {
        match self {
            Backend::Api(agent) => agent.context_budget_flag(),
            Backend::ClaudeCli(driver) => driver.context_budget_flag(),
        }
    }

    pub fn model_flag(&self) -> Arc<Mutex<String>> {
        match self {
            Backend::Api(agent) => agent.model_flag(),
            Backend::ClaudeCli(driver) => driver.model_flag(),
        }
    }

    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        match self {
            Backend::Api(agent) => agent.repeat_interrupt_flag(),
            Backend::ClaudeCli(driver) => driver.repeat_interrupt_flag(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn new_test_claude_cli_backend() -> Backend {
        let (tx, _rx) = mpsc::unbounded_channel();
        Backend::new_claude_cli(
            "opus".to_string(),
            None,
            None,
            PathBuf::from("."),
            tx,
        )
    }

    #[test]
    fn claude_cli_backend_exposes_all_six_flags_and_round_trips_writes() {
        let backend = new_test_claude_cli_backend();

        backend.interrupt_flag().store(true, Ordering::SeqCst);
        assert!(backend.interrupt_flag().load(Ordering::SeqCst));

        backend.thinking_flag().store(true, Ordering::SeqCst);
        assert!(backend.thinking_flag().load(Ordering::SeqCst));

        backend.voice_mode_flag().store(true, Ordering::SeqCst);
        assert!(backend.voice_mode_flag().load(Ordering::SeqCst));

        backend.context_budget_flag().store(64_000, Ordering::SeqCst);
        assert_eq!(backend.context_budget_flag().load(Ordering::SeqCst), 64_000);

        *backend.model_flag().lock().unwrap() = "sonnet".to_string();
        assert_eq!(*backend.model_flag().lock().unwrap(), "sonnet");

        backend.repeat_interrupt_flag().store(true, Ordering::SeqCst);
        assert!(backend.repeat_interrupt_flag().load(Ordering::SeqCst));
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
            PathBuf::from("."),
            tx,
        );

        backend.run_repeat("do the thing", 0).await;

        let event = rx.try_recv().expect("expected a RepeatFinished event");
        match event {
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
            PathBuf::from("."),
            tx,
        );
        backend.repeat_interrupt_flag().store(true, Ordering::SeqCst);

        backend.run_repeat("do the thing", 3).await;

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
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
