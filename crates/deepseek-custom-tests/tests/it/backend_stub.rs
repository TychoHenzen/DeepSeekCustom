//! Unit tests for `deepseek_custom::backend::stub` (`src/backend/stub.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use deepseek_custom::agent::agent_loop::StreamEvent;
use deepseek_custom::agent::repeat::RepeatTarget;
use deepseek_custom::backend::stub::{StubBackend, StubTurn};

use tokio::sync::mpsc;

fn new_stub(script: Vec<StubTurn>) -> StubBackend {
    StubBackend::new(
        script,
        "stub-model".to_string(),
        Arc::new(AtomicBool::new(false)),
    )
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

    RepeatTarget::run_turn(&mut stub, "one")
        .await
        .expect("first turn");
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
