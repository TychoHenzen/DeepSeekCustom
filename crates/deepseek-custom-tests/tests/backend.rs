//! Unit tests for `deepseek_custom::backend` (`src/backend/mod.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::agent_loop::{AgentConfig, AgentLoop, StreamEvent};
use deepseek_custom::api::client::{ApiClient, Provider};
use deepseek_custom::api::types::Message;
use deepseek_custom::backend::Backend;
use deepseek_custom::tools::ToolRegistry;

use tokio::sync::mpsc;

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
    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        None,
        None,
    );
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
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
