//! A scripted stand-in for `AgentLoop` or `ClaudeCliDriver`. Answers each
//! turn from the next entry in a canned script, with no network call and
//! no child process. Exists so `run_subagent`, the depth limit, and (from
//! phase 3 onward) the subagent registry's lifetime rules and turn caps
//! are all testable without hitting a real API or spawning `claude`.
//!
//! Reachable only through `BackendFactory::with_stub`, which is
//! `#[cfg(test)]`. There is no path from `settings.json` to a stub: the
//! `backends` map only ever produces a `BackendConfig::Api` or
//! `BackendConfig::ClaudeCli`, and this module does not touch that type at
//! all. A stub cannot appear in a normal run by accident, because the code
//! that would need to build one does not exist outside test builds.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::agent::agent_loop::{RoutedEvent, StreamEvent};
use crate::agent::repeat::RepeatTarget;
use crate::error::{HarnessError, Result};

/// One scripted answer for a turn. `Text` is a normal reply. `Error` makes
/// the turn fail, so a caller's error handling is exercised the same way a
/// real backend's would be.
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

impl RepeatTarget for StubBackend {
    async fn reset_for_iteration(&mut self) {
        self.reset();
    }

    async fn run_turn(&mut self, task: &str) -> Result<()> {
        self.run(task).await.map(|_| ())
    }

    fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        StubBackend::repeat_interrupt_flag(self)
    }

    fn send_event(&self, event: StreamEvent) {
        StubBackend::emit(self, event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn new_stub(script: Vec<StubTurn>) -> StubBackend {
        StubBackend::new(script, "stub-model".to_string(), Arc::new(AtomicBool::new(false)))
    }

    #[tokio::test]
    async fn answers_each_turn_from_the_script_in_order() {
        let mut stub = new_stub(vec![
            StubTurn::Text("first".to_string()),
            StubTurn::Text("second".to_string()),
        ]);

        let first = stub.run("hello").await.expect("first turn");
        let second = stub.run("again").await.expect("second turn");

        assert_eq!(first, vec!["first".to_string()]);
        assert_eq!(second, vec!["second".to_string()]);
    }

    #[tokio::test]
    async fn sticks_on_the_last_entry_once_the_script_runs_out() {
        let mut stub = new_stub(vec![StubTurn::Text("only".to_string())]);

        stub.run("one").await.expect("first turn");
        let third = stub.run("two").await.expect("third turn");

        assert_eq!(third, vec!["only".to_string()]);
    }

    #[tokio::test]
    async fn empty_script_answers_with_empty_text() {
        let mut stub = new_stub(Vec::new());

        let result = stub.run("hello").await.expect("should not error");

        assert_eq!(result, vec![String::new()]);
    }

    #[tokio::test]
    async fn scripted_error_turn_fails_the_run() {
        let mut stub = new_stub(vec![StubTurn::Error("boom".to_string())]);

        let err = stub.run("hello").await.expect_err("should fail");

        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn interrupt_flag_set_before_run_short_circuits_with_no_script_consumed() {
        let interrupt_flag = Arc::new(AtomicBool::new(true));
        let mut stub = StubBackend::new(
            vec![StubTurn::Text("never seen".to_string())],
            "stub-model".to_string(),
            interrupt_flag,
        );

        let err = stub.run("hello").await.expect_err("should be interrupted");

        assert!(err.to_string().contains("interrupted"));
    }

    #[tokio::test]
    async fn reset_rewinds_the_script_cursor() {
        let mut stub = new_stub(vec![
            StubTurn::Text("first".to_string()),
            StubTurn::Text("second".to_string()),
        ]);

        stub.run("one").await.expect("first turn");
        stub.reset();
        let after_reset = stub.run("two").await.expect("should replay from start");

        assert_eq!(after_reset, vec!["first".to_string()]);
    }

    #[tokio::test]
    async fn run_emits_text_and_turn_end_events() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut stub = new_stub(vec![StubTurn::Text("hi".to_string())]);
        stub.set_event_sender(tx);

        stub.run("hello").await.expect("should succeed");

        let first = rx.try_recv().expect("text event");
        assert!(matches!(first.event, StreamEvent::Text { .. }));
        let second = rx.try_recv().expect("turn end event");
        assert!(matches!(second.event, StreamEvent::TurnEnd { .. }));
    }

    #[tokio::test]
    async fn repeat_target_reset_for_iteration_rewinds_the_script() {
        let mut stub = new_stub(vec![
            StubTurn::Text("first".to_string()),
            StubTurn::Text("second".to_string()),
        ]);

        RepeatTarget::run_turn(&mut stub, "one").await.expect("first turn");
        RepeatTarget::reset_for_iteration(&mut stub).await;
        let after_reset = stub.run("two").await.expect("should replay from start");

        assert_eq!(after_reset, vec!["first".to_string()]);
    }

    #[test]
    fn all_six_flags_are_independently_readable_and_writable() {
        let stub = new_stub(Vec::new());

        stub.interrupt_flag().store(true, Ordering::SeqCst);
        assert!(stub.interrupt_flag().load(Ordering::SeqCst));

        stub.effort_flag().store(3, Ordering::SeqCst);
        assert_eq!(stub.effort_flag().load(Ordering::SeqCst), 3);

        stub.voice_mode_flag().store(true, Ordering::SeqCst);
        assert!(stub.voice_mode_flag().load(Ordering::SeqCst));

        stub.context_budget_flag().store(64_000, Ordering::SeqCst);
        assert_eq!(stub.context_budget_flag().load(Ordering::SeqCst), 64_000);

        *stub.model_flag().lock().unwrap() = "other-model".to_string();
        assert_eq!(*stub.model_flag().lock().unwrap(), "other-model");

        stub.repeat_interrupt_flag().store(true, Ordering::SeqCst);
        assert!(stub.repeat_interrupt_flag().load(Ordering::SeqCst));
    }
}
