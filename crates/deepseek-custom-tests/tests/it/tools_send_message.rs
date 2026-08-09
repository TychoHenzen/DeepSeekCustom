//! Unit tests for `deepseek_custom::tools::send_message` (`src/tools/send_message.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::path::PathBuf;
use std::sync::Arc;

use deepseek_custom::agent::agent_loop::{RoutedEvent, SubagentId};
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::backend::subagent::{SubagentRequest, run_subagent};
use deepseek_custom::config::settings::Settings;
use deepseek_custom::effort::Effort;
use deepseek_custom::tools::Tool;
use deepseek_custom::tools::send_message::SendMessageTool;

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
/// returns its session id, ready for a `SendMessageTool` to be pointed
/// at.
async fn open_session(
    factory: &Arc<BackendFactory>,
    registry: &Arc<SubagentRegistry>,
    backend: &str,
) -> String {
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
fn input_schema_declares_both_properties_as_required() {
    let tool = SendMessageTool::new(empty_registry(), 20, 10);
    let schema = tool.input_schema();

    let properties = schema["properties"]
        .as_object()
        .expect("schema should have properties");
    assert!(properties.contains_key("session_id"));
    assert!(properties.contains_key("prompt"));

    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("schema should have required array")
        .iter()
        .map(|v| v.as_str().expect("required entries are strings"))
        .collect();
    assert!(required.contains(&"session_id"));
    assert!(required.contains(&"prompt"));
}

/// A follow-up turn against a kept-open session returns the scripted
/// second answer, proving the session's own backend, not a fresh one,
/// handled the call: a fresh stub would restart its script and answer
/// "first" again.
#[tokio::test]
async fn follow_up_turn_returns_the_scripted_second_answer_and_history_survives() {
    let factory = factory_with_stub(
        "stub-agent",
        vec![
            StubTurn::Text("first answer".to_string()),
            StubTurn::Text("second answer".to_string()),
        ],
    );
    let registry = empty_registry();
    let session_id = open_session(&factory, &registry, "stub-agent").await;

    let tool = SendMessageTool::new(registry, 20, 10);
    let output = tool
        .execute(serde_json::json!({
            "session_id": session_id,
            "prompt": "and then?",
        }))
        .await
        .expect("execute should not return a hard error");

    assert!(!output.is_error);
    assert_eq!(output.content, "second answer");
}

#[tokio::test]
async fn unknown_session_id_is_a_tool_error() {
    let registry = empty_registry();
    let tool = SendMessageTool::new(registry, 20, 10);

    let output = tool
        .execute(serde_json::json!({
            "session_id": "999999",
            "prompt": "hello",
        }))
        .await
        .expect("execute should not return a hard error");

    assert!(output.is_error);
    assert!(output.content.contains("999999"));
}

#[tokio::test]
async fn a_malformed_session_id_is_a_tool_error_not_a_panic() {
    let registry = empty_registry();
    let tool = SendMessageTool::new(registry, 20, 10);

    let output = tool
        .execute(serde_json::json!({
            "session_id": "not-a-number",
            "prompt": "hello",
        }))
        .await
        .expect("execute should not return a hard error");

    assert!(output.is_error);
    assert!(output.content.contains("not-a-number"));
}

#[tokio::test]
async fn a_closed_session_is_a_tool_error() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let registry = empty_registry();
    let session_id = open_session(&factory, &registry, "stub-agent").await;
    let id: SubagentId = session_id.parse().unwrap();
    registry.close(id).await;

    let tool = SendMessageTool::new(registry, 20, 10);
    let output = tool
        .execute(serde_json::json!({
            "session_id": session_id,
            "prompt": "hello",
        }))
        .await
        .expect("execute should not return a hard error");

    assert!(output.is_error);
}

#[tokio::test]
async fn the_per_session_turn_cap_comes_back_as_a_tool_error() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let registry = empty_registry();
    let session_id = open_session(&factory, &registry, "stub-agent").await;

    // Cap of 1: the session already used its one turn opening.
    let tool = SendMessageTool::new(registry, 1, 10);
    let output = tool
        .execute(serde_json::json!({
            "session_id": session_id,
            "prompt": "hello",
        }))
        .await
        .expect("execute should not return a hard error");

    assert!(output.is_error);
    assert!(output.content.contains("turn cap"));
}

#[tokio::test]
async fn the_per_parent_turn_call_cap_comes_back_as_a_tool_error() {
    let factory = factory_with_stub(
        "stub-agent",
        vec![
            StubTurn::Text("a".to_string()),
            StubTurn::Text("b".to_string()),
        ],
    );
    let registry = empty_registry();
    let session_id = open_session(&factory, &registry, "stub-agent").await;

    let tool = SendMessageTool::new(registry, 20, 1);
    let first = tool
        .execute(serde_json::json!({
            "session_id": session_id,
            "prompt": "one",
        }))
        .await
        .expect("execute should not return a hard error");
    assert!(!first.is_error);

    let second = tool
        .execute(serde_json::json!({
            "session_id": session_id,
            "prompt": "two",
        }))
        .await
        .expect("execute should not return a hard error");

    assert!(second.is_error);
    assert!(second.content.contains("call limit"));
}
