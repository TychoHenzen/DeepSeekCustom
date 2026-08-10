//! Unit tests for `deepseek_custom::backend::registry` (`src/backend/registry.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};

use deepseek_custom::agent::events::SubagentId;
use deepseek_custom::backend::Backend;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::backend::stub::{StubBackend, StubTurn};

fn stub_backend() -> Backend {
    Backend::Stub(Box::new(StubBackend::new(
        Vec::new(),
        "stub-model".to_string(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    )))
}

fn stub_backend_with_script(script: Vec<StubTurn>) -> Backend {
    Backend::Stub(Box::new(StubBackend::new(
        script,
        "stub-model".to_string(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    )))
}

/// A `ClaudeCli` backend with no child ever spawned: registering and
/// closing it exercises the registry's generic path for that variant.
/// `Backend::start_new_session` on a `ClaudeCli` driver calls
/// `shutdown`, which is a no-op when no child is running, so this does
/// not spawn a real `claude` process.
fn unspawned_claude_cli_backend() -> Backend {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let working_dir = Arc::new(std::sync::Mutex::new(PathBuf::from(".")));
    Backend::new_claude_cli("opus".to_string(), None, None, working_dir, tx)
}

/// Closing a `ClaudeCli`-backed session removes it from the registry
/// and runs its shutdown path, the same as any other backend kind.
/// This proves the registry treats a `ClaudeCli` entry no differently
/// than a `Stub` one: `close` calls `start_new_session`, which is what
/// actually kills a real child when one is running (see
/// `Backend::start_new_session` and `ClaudeCliDriver::shutdown`).
/// Exercising an actual live child needs a real `claude` process,
/// which this suite must not spawn; see `P7S01` for the fake binary
/// that will let a later test cover that.
#[tokio::test]
async fn closing_a_claude_cli_session_removes_it_and_runs_shutdown() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();
    registry.register(id, unspawned_claude_cli_backend()).await;
    assert!(registry.contains(id).await);

    let closed = registry.close(id).await;

    assert!(closed);
    assert!(!registry.contains(id).await);
    assert_eq!(registry.len().await, 0);
}

#[tokio::test]
async fn a_registered_session_is_reachable_by_id() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();

    registry.register(id, stub_backend()).await;

    assert!(registry.contains(id).await);
    assert_eq!(registry.len().await, 1);
}

#[tokio::test]
async fn an_unregistered_id_is_not_reachable() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();

    assert!(!registry.contains(id).await);
}

#[tokio::test]
async fn closing_one_removes_only_that_session() {
    let registry = SubagentRegistry::new();
    let id_a = SubagentId::next();
    let id_b = SubagentId::next();
    registry.register(id_a, stub_backend()).await;
    registry.register(id_b, stub_backend()).await;

    let closed = registry.close(id_a).await;

    assert!(closed);
    assert!(!registry.contains(id_a).await);
    assert!(registry.contains(id_b).await);
    assert_eq!(registry.len().await, 1);
}

#[tokio::test]
async fn closing_an_unknown_id_returns_false() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();

    let closed = registry.close(id).await;

    assert!(!closed);
}

#[tokio::test]
async fn closing_all_empties_the_registry() {
    let registry = SubagentRegistry::new();
    registry.register(SubagentId::next(), stub_backend()).await;
    registry.register(SubagentId::next(), stub_backend()).await;
    registry.register(SubagentId::next(), stub_backend()).await;

    registry.close_all().await;

    assert_eq!(registry.len().await, 0);
}

#[tokio::test]
async fn closing_all_on_an_empty_registry_is_a_no_op() {
    let registry = SubagentRegistry::new();

    registry.close_all().await;

    assert_eq!(registry.len().await, 0);
}

/// A registered session starts at 1 turn, since `keep_open` already
/// ran its first turn before handing the backend to `register`.
#[tokio::test]
async fn a_freshly_registered_session_starts_at_one_turn() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();

    registry.register(id, stub_backend()).await;

    assert_eq!(registry.session_turns(id).await, Some(1));
}

#[tokio::test]
async fn send_message_against_a_stub_session_returns_the_next_scripted_turn() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();
    registry
        .register(
            id,
            stub_backend_with_script(vec![
                StubTurn::Text("first".to_string()),
                StubTurn::Text("second".to_string()),
            ]),
        )
        .await;

    // The registry itself does not run a session's opening turn: that
    // happens in `run_subagent` before `register` is ever called (see
    // `send_message.rs`'s tests for that end-to-end path). Registering
    // a fresh script directly here means this call is the session's
    // first turn, so it consumes the script's first entry.
    let text = registry
        .send_message(id, "follow up", 20, 10)
        .await
        .expect("should succeed");

    assert_eq!(text, "first");
    assert_eq!(registry.session_turns(id).await, Some(2));
}

#[tokio::test]
async fn send_message_against_an_unknown_id_is_an_error_naming_it() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();

    let err = registry
        .send_message(id, "hello", 20, 10)
        .await
        .expect_err("unknown id should fail");

    assert!(err.contains(&id.to_string()));
}

#[tokio::test]
async fn send_message_against_a_closed_session_is_an_error() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();
    registry.register(id, stub_backend()).await;
    registry.close(id).await;

    let err = registry
        .send_message(id, "hello", 20, 10)
        .await
        .expect_err("closed session should fail");

    assert!(err.contains(&id.to_string()));
}

/// A session capped at 1 turn total has already used it up on
/// registration, so the very next `send_message` trips the cap. The
/// session stays open: only the turn was rejected.
#[tokio::test]
async fn send_message_trips_the_per_session_turn_cap() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();
    registry.register(id, stub_backend()).await;

    let err = registry
        .send_message(id, "hello", 1, 10)
        .await
        .expect_err("session turn cap should reject this call");

    assert!(err.contains("turn cap"));
    assert!(registry.contains(id).await);
}

/// A parent capped at 1 `SendMessage` call total lets the first call
/// through and rejects the second, even against the same session.
#[tokio::test]
async fn send_message_trips_the_per_parent_turn_call_cap() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();
    registry
        .register(
            id,
            stub_backend_with_script(vec![
                StubTurn::Text("first".to_string()),
                StubTurn::Text("second".to_string()),
            ]),
        )
        .await;

    let first = registry.send_message(id, "one", 20, 1).await;
    assert!(first.is_ok());

    let second = registry.send_message(id, "two", 20, 1).await;
    let err = second.expect_err("parent call cap should reject the second call");
    assert!(err.contains("call limit"));
}

/// The per-parent-turn call count resets exactly when `close_all`
/// runs, the same moment a parent's turn ends.
#[tokio::test]
async fn close_all_resets_the_send_message_call_count() {
    let registry = SubagentRegistry::new();
    let id = SubagentId::next();
    registry.register(id, stub_backend()).await;
    let _ = registry.send_message(id, "one", 20, 10).await;
    assert_eq!(registry.send_message_call_count(), 1);

    registry.close_all().await;

    assert_eq!(registry.send_message_call_count(), 0);
}
