//! Unit tests for `deepseek_custom::tools::close_session` (`src/tools/close_session.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::path::PathBuf;
use std::sync::Arc;

use deepseek_custom::agent::agent_loop::RoutedEvent;
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::backend::subagent::{SubagentRequest, run_subagent};
use deepseek_custom::config::settings::Settings;
use deepseek_custom::effort::Effort;
use deepseek_custom::tools::Tool;
use deepseek_custom::tools::close_session::CloseSessionTool;

fn factory_with_stub(name: &str, script: Vec<StubTurn>) -> Arc<BackendFactory> {
    Arc::new(BackendFactory::new(Settings::default(), PathBuf::from(".")).with_stub(name, script))
}

fn empty_registry() -> Arc<SubagentRegistry> {
    Arc::new(SubagentRegistry::new())
}

fn test_parent_tx() -> tokio::sync::mpsc::UnboundedSender<RoutedEvent> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    tx
}

/// Opens a `keep_open` session against a scripted stub backend and
/// returns its session id, ready for a `CloseSessionTool` to be pointed
/// at. Mirrors the identical helper in `send_message.rs`'s own tests.
async fn open_session(factory: &Arc<BackendFactory>, registry: &Arc<SubagentRegistry>, backend: &str) -> String {
    let outcome = run_subagent(
        factory,
        SubagentRequest {
            backend: backend.to_string(),
            model: None,
            prompt: "hello".to_string(),
            depth: 1,
            keep_open: true,
            working_dir_override: None,
            effort: Effort::None,
        },
        test_parent_tx(),
        registry.clone(),
    )
    .await
    .expect("keep_open dispatch should succeed");
    outcome
        .session_id
        .expect("keep_open dispatch should report a session id")
        .to_string()
}

#[test]
fn input_schema_declares_session_id_as_required() {
    let tool = CloseSessionTool::new(empty_registry());
    let schema = tool.input_schema();

    let properties = schema["properties"]
        .as_object()
        .expect("schema should have properties");
    assert!(properties.contains_key("session_id"));

    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("schema should have required array")
        .iter()
        .map(|v| v.as_str().expect("required entries are strings"))
        .collect();
    assert!(required.contains(&"session_id"));
}

/// Closing a live session removes it from the registry and reports
/// success.
#[tokio::test]
async fn closing_a_live_session_removes_it() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let registry = empty_registry();
    let session_id = open_session(&factory, &registry, "stub-agent").await;
    assert_eq!(registry.len().await, 1);

    let tool = CloseSessionTool::new(registry.clone());
    let output = tool
        .execute(serde_json::json!({ "session_id": session_id }))
        .await
        .expect("execute should not return a hard error");

    assert!(!output.is_error);
    assert_eq!(registry.len().await, 0);
}

/// A `SendMessage` call into a session `CloseSession` already closed is
/// a tool error, proving the registry, not just this tool's own state,
/// treats the session as gone.
#[tokio::test]
async fn a_later_send_message_into_a_closed_session_is_a_tool_error() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let registry = empty_registry();
    let session_id = open_session(&factory, &registry, "stub-agent").await;

    let close_tool = CloseSessionTool::new(registry.clone());
    let close_output = close_tool
        .execute(serde_json::json!({ "session_id": session_id }))
        .await
        .expect("execute should not return a hard error");
    assert!(!close_output.is_error);

    let err = registry
        .send_message(session_id.parse().unwrap(), "hello", 20, 10)
        .await
        .expect_err("closed session should reject a follow-up turn");
    assert!(err.contains(&session_id));
}

#[tokio::test]
async fn unknown_session_id_is_a_tool_error() {
    let registry = empty_registry();
    let tool = CloseSessionTool::new(registry);

    let output = tool
        .execute(serde_json::json!({ "session_id": "999999" }))
        .await
        .expect("execute should not return a hard error");

    assert!(output.is_error);
    assert!(output.content.contains("999999"));
}

#[tokio::test]
async fn a_malformed_session_id_is_a_tool_error_not_a_panic() {
    let registry = empty_registry();
    let tool = CloseSessionTool::new(registry);

    let output = tool
        .execute(serde_json::json!({ "session_id": "not-a-number" }))
        .await
        .expect("execute should not return a hard error");

    assert!(output.is_error);
    assert!(output.content.contains("not-a-number"));
}

#[tokio::test]
async fn closing_an_already_closed_session_is_a_tool_error() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let registry = empty_registry();
    let session_id = open_session(&factory, &registry, "stub-agent").await;

    let tool = CloseSessionTool::new(registry.clone());
    let first = tool
        .execute(serde_json::json!({ "session_id": session_id }))
        .await
        .expect("execute should not return a hard error");
    assert!(!first.is_error);

    let second = tool
        .execute(serde_json::json!({ "session_id": session_id }))
        .await
        .expect("execute should not return a hard error");
    assert!(second.is_error);
    assert!(second.content.contains(&session_id));
}
