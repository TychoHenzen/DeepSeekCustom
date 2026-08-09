//! A scripted stand-in for `AgentLoop` or `ClaudeCliDriver`. Answers each
//! turn from the next entry in a canned script, with no network call and
//! no child process. Exists so `run_subagent`, the depth limit, and (from
//! phase 3 onward) the subagent registry's lifetime rules and turn caps
//! are all testable without hitting a real API or spawning `claude`.
//!
//! Reachable only through `BackendFactory::with_stub`. There is no path
//! from `settings.json` to a stub: the `backends` map only ever produces a
//! `BackendConfig::Api` or `BackendConfig::ClaudeCli`, and this module does
//! not touch that type at all. A stub cannot appear in a normal run by
//! accident, because the code that would need to build one does not exist
//! outside a test build or the `test-support` feature.
//!
//! `StubTurn`, `StubBackend`, and `BackendFactory::with_stub` are gated on
//! `#[cfg(feature = "test-support")]`. This crate now holds no inline tests
//! at all, so a bare plain-test gate would serve no one: every test that
//! needs the stub lives in the external `deepseek-custom-tests` crate,
//! which turns the feature on. The feature is off by default, so a plain
//! `cargo build -p deepseek-custom` compiles the stub out entirely, the
//! same guarantee the crate has always kept. Do not widen this to a plain
//! `pub` with no gate at all: that would let a stub reach a normal run.

#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
#[cfg(feature = "test-support")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "test-support")]
use tokio::sync::mpsc;

#[cfg(feature = "test-support")]
use crate::agent::agent_loop::{RoutedEvent, StreamEvent};
#[cfg(feature = "test-support")]
use crate::agent::repeat::RepeatTarget;
#[cfg(feature = "test-support")]
use crate::error::{HarnessError, Result};

/// One scripted answer for a turn. `Text` is a normal reply. `Error` makes
/// the turn fail, so a caller's error handling is exercised the same way a
/// real backend's would be.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone)]
pub enum StubTurn {
    Text(String),
    Error(String),
}

/// A backend that answers from `script` instead of a network call or a
/// child process. Implements the same shared surface `Backend::Api` and
/// `Backend::ClaudeCli` already expose (the six flags, `run`,
/// `start_new_session`), so `run_subagent` and the GUI-facing `Backend`
/// methods treat it exactly like either real variant.
#[cfg(feature = "test-support")]
pub struct StubBackend {
    script: Vec<StubTurn>,
    cursor: usize,
    turn: u32,
    tx_events: Option<mpsc::UnboundedSender<RoutedEvent>>,
    interrupt_flag: Arc<AtomicBool>,
    effort_flag: Arc<AtomicU8>,
    voice_mode_flag: Arc<AtomicBool>,
    context_budget_flag: Arc<AtomicUsize>,
    model_flag: Arc<Mutex<String>>,
    repeat_interrupt_flag: Arc<AtomicBool>,
}

#[cfg(feature = "test-support")]
impl StubBackend {
    /// `interrupt_flag` is shared with the factory that builds this, the
    /// same way a real backend shares it, so Escape reaches a stub-backed
    /// subagent too.
    pub fn new(script: Vec<StubTurn>, model: String, interrupt_flag: Arc<AtomicBool>) -> Self {
        Self {
            script,
            cursor: 0,
            turn: 0,
            tx_events: None,
            interrupt_flag,
            effort_flag: Arc::new(AtomicU8::new(0)),
            voice_mode_flag: Arc::new(AtomicBool::new(false)),
            context_budget_flag: Arc::new(AtomicUsize::new(100_000)),
            model_flag: Arc::new(Mutex::new(model)),
            repeat_interrupt_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_event_sender(&mut self, tx: mpsc::UnboundedSender<RoutedEvent>) {
        self.tx_events = Some(tx);
    }

    fn emit(&self, event: StreamEvent) {
        if let Some(tx) = &self.tx_events {
            let _ = tx.send(RoutedEvent::own(event));
        }
    }

    /// The next scripted turn. Sticks on the last entry once the script
    /// runs out, so a caller does not need to size the script to the exact
    /// number of turns it ends up running. An empty script always answers
    /// with empty text.
    fn next_turn(&mut self) -> StubTurn {
        if self.script.is_empty() {
            return StubTurn::Text(String::new());
        }
        let index = self.cursor.min(self.script.len() - 1);
        let turn = self.script[index].clone();
        if self.cursor < self.script.len() - 1 {
            self.cursor += 1;
        }
        turn
    }

    /// Run one turn. Checks the interrupt flag first, exactly like a real
    /// backend, so the interrupt path is testable without a network call
    /// or a child process.
    pub async fn run(&mut self, _input: &str) -> Result<Vec<String>> {
        self.turn += 1;
        if self.interrupt_flag.load(Ordering::SeqCst) {
            self.emit(StreamEvent::Interrupted {
                message: "interrupted".to_string(),
            });
            return Err(HarnessError::Tool("interrupted".to_string()));
        }

        match self.next_turn() {
            StubTurn::Text(text) => {
                self.emit(StreamEvent::Text {
                    turn: self.turn,
                    text: text.clone(),
                });
                self.emit(StreamEvent::TurnEnd {
                    turn: self.turn,
                    finish_reason: "stop".to_string(),
                    total_tokens: 0,
                    prompt_cache_hit_tokens: 0,
                    prompt_cache_miss_tokens: 0,
                });
                Ok(vec![text])
            }
            StubTurn::Error(message) => {
                self.emit(StreamEvent::Error {
                    message: message.clone(),
                });
                Err(HarnessError::Tool(message))
            }
        }
    }

    /// Replace all six shared handles with the ones a caller already
    /// holds, matching what the two real backends do. See
    /// `Backend::adopt_flags`.
    pub fn adopt_flags(&mut self, flags: &crate::backend::SharedFlags) {
        self.interrupt_flag = Arc::clone(&flags.interrupt);
        self.effort_flag = Arc::clone(&flags.effort);
        self.voice_mode_flag = Arc::clone(&flags.voice_mode);
        self.context_budget_flag = Arc::clone(&flags.context_budget);
        self.model_flag = Arc::clone(&flags.model);
        self.repeat_interrupt_flag = Arc::clone(&flags.repeat_interrupt);
    }

    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    pub fn effort_flag(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.effort_flag)
    }

    pub fn voice_mode_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.voice_mode_flag)
    }

    pub fn context_budget_flag(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.context_budget_flag)
    }

    pub fn model_flag(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.model_flag)
    }

    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.repeat_interrupt_flag)
    }

    /// Rewind the script to its start. `Backend::start_new_session` and
    /// `RepeatTarget::reset_for_iteration` both use this, so no iteration
    /// or fresh session sees where an earlier one left off in the script.
    pub fn reset(&mut self) {
        self.cursor = 0;
        self.turn = 0;
    }
}

#[cfg(feature = "test-support")]
impl RepeatTarget for StubBackend {
    async fn reset_for_iteration(&mut self) {
        self.reset();
    }

    async fn run_turn(&mut self, task: &str) -> Result<String> {
        self.run(task)
            .await
            .map(|replies| replies.into_iter().last().unwrap_or_default())
    }

    fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        StubBackend::repeat_interrupt_flag(self)
    }

    fn send_event(&self, event: StreamEvent) {
        StubBackend::emit(self, event)
    }
}
