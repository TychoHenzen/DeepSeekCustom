//! Unit tests for `deepseek_custom::tools::task` (`src/tools/task.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use deepseek_custom::agent::agent_loop::RoutedEvent;
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::effort::Effort;
use deepseek_custom::tools::Tool;
use deepseek_custom::tools::task::{TaskTool, build_request};

use tokio::sync::mpsc;

fn empty_factory() -> Arc<BackendFactory> {
    Arc::new(BackendFactory::new(Settings::default(), PathBuf::from(".")))
}

fn factory_with_stub(name: &str, script: Vec<StubTurn>) -> Arc<BackendFactory> {
    Arc::new(BackendFactory::new(Settings::default(), PathBuf::from(".")).with_stub(name, script))
}

fn test_parent_tx() -> mpsc::UnboundedSender<RoutedEvent> {
    let (tx, _rx) = mpsc::unbounded_channel();
    tx
}

fn empty_registry() -> Arc<SubagentRegistry> {
    Arc::new(SubagentRegistry::new())
}

fn effort_flag_at(level: Effort) -> Arc<AtomicU8> {
    let flag = Arc::new(AtomicU8::new(0));
    level.store(&flag);
    flag
}

fn well_formed_input() -> serde_json::Value {
    serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
    })
}

#[test]
fn input_schema_declares_all_seven_properties_with_correct_requiredness() {
    let tool = TaskTool::new(
        empty_factory(),
        1,
        test_parent_tx(),
        empty_registry(),
        effort_flag_at(Effort::None),
    );
    let schema = tool.input_schema();

    let properties = schema["properties"]
        .as_object()
        .expect("schema should have properties");
    assert!(properties.contains_key("description"));
    assert!(properties.contains_key("prompt"));
    assert!(properties.contains_key("backend"));
    assert!(properties.contains_key("model"));
    assert!(properties.contains_key("keep_open"));
    assert!(properties.contains_key("working_dir"));
    assert!(properties.contains_key("effort"));

    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("schema should have required array")
        .iter()
        .map(|v| v.as_str().expect("required entries are strings"))
        .collect();
    assert!(required.contains(&"description"));
    assert!(required.contains(&"prompt"));
    assert!(required.contains(&"backend"));
    assert!(!required.contains(&"model"));
    assert!(!required.contains(&"keep_open"));
    assert!(!required.contains(&"working_dir"));
    assert!(!required.contains(&"effort"));
}

#[test]
fn build_request_at_dispatch_depth_one_carries_depth_through() {
    let request = build_request(well_formed_input(), 1, Effort::None).expect("should parse");
    assert_eq!(request.depth, 1);
    assert_eq!(request.backend, "nope");
    assert_eq!(request.prompt, "What is 2 + 2?");
    assert_eq!(request.model, None);
    assert!(!request.keep_open);
}

#[test]
fn build_request_defaults_keep_open_to_false_when_absent() {
    let request = build_request(well_formed_input(), 1, Effort::None).expect("should parse");
    assert!(!request.keep_open);
}

#[test]
fn build_request_honors_an_explicit_keep_open_true() {
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
        "keep_open": true,
    });
    let request = build_request(input, 1, Effort::None).expect("should parse");
    assert!(request.keep_open);
}

#[test]
fn build_request_defaults_working_dir_override_to_none_when_absent() {
    let request = build_request(well_formed_input(), 1, Effort::None).expect("should parse");
    assert_eq!(request.working_dir_override, None);
}

#[test]
fn build_request_honors_an_explicit_working_dir() {
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
        "working_dir": "C:/sibling-checkout",
    });
    let request = build_request(input, 1, Effort::None).expect("should parse");
    assert_eq!(
        request.working_dir_override,
        Some(PathBuf::from("C:/sibling-checkout"))
    );
}

#[test]
fn build_request_missing_prompt_is_an_error_not_a_panic() {
    let input = serde_json::json!({
        "description": "check the weather",
        "backend": "nope",
    });
    let err = match build_request(input, 1, Effort::None) {
        Ok(_) => panic!("missing prompt should fail to parse"),
        Err(e) => e,
    };
    assert!(err.contains("Invalid Task input"));
}

#[test]
fn build_request_falls_back_to_the_given_default_effort_when_absent() {
    let request =
        build_request(well_formed_input(), 1, Effort::Max).expect("should parse");
    assert_eq!(request.effort, Effort::Max);
}

#[test]
fn build_request_honors_an_explicit_effort_override_over_the_default() {
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
        "effort": "high",
    });
    let request = build_request(input, 1, Effort::Low).expect("should parse");
    assert_eq!(request.effort, Effort::High);
}

#[test]
fn build_request_with_an_unrecognised_effort_value_is_an_error_not_a_panic() {
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
        "effort": "extreme",
    });
    let err = match build_request(input, 1, Effort::None) {
        Ok(_) => panic!("an unrecognised effort value should fail to parse"),
        Err(e) => e,
    };
    assert!(err.contains("Invalid Task input"));
}

#[tokio::test]
async fn execute_with_unknown_backend_returns_tool_error_naming_it() {
    let tool = TaskTool::new(
        empty_factory(),
        1,
        test_parent_tx(),
        empty_registry(),
        effort_flag_at(Effort::None),
    );

    let output = tool
        .execute(well_formed_input())
        .await
        .expect("execute should not return a hard error");

    assert!(output.is_error);
    assert!(output.content.contains("nope"));
    assert!(output.content.contains("none configured"));
}

#[tokio::test]
async fn execute_with_malformed_input_returns_tool_error_not_a_panic() {
    let tool = TaskTool::new(
        empty_factory(),
        1,
        test_parent_tx(),
        empty_registry(),
        effort_flag_at(Effort::None),
    );
    let input = serde_json::json!({"description": "missing everything else"});

    let output = tool
        .execute(input)
        .await
        .expect("execute should not return a hard error");

    assert!(output.is_error);
    assert!(output.content.contains("Invalid Task input"));
}

/// An unrecognised `effort` value comes back as a tool error, never a
/// hard `Result::Err` from `execute`, the same as any other malformed
/// input field.
#[tokio::test]
async fn execute_with_an_unrecognised_effort_value_returns_a_tool_error() {
    let tool = TaskTool::new(
        empty_factory(),
        1,
        test_parent_tx(),
        empty_registry(),
        effort_flag_at(Effort::None),
    );
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "hello",
        "backend": "nope",
        "effort": "extreme",
    });

    let output = tool.execute(input).await.expect("execute should not return a hard error");

    assert!(output.is_error);
    assert!(output.content.contains("Invalid Task input"));
}

/// `keep_open: true` against a stub backend leaves the session
/// registered in this tool's own registry, and the tool's text result
/// names the session id.
#[tokio::test]
async fn execute_with_keep_open_registers_a_session_and_names_it_in_the_result() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("scripted reply".to_string())]);
    let registry = empty_registry();
    let tool = TaskTool::new(
        factory,
        1,
        test_parent_tx(),
        registry.clone(),
        effort_flag_at(Effort::None),
    );
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "hello",
        "backend": "stub-agent",
        "keep_open": true,
    });

    let output = tool.execute(input).await.expect("execute should not return a hard error");

    assert!(!output.is_error);
    assert!(output.content.contains("scripted reply"));
    assert!(output.content.contains("session kept open"));
    assert_eq!(registry.len().await, 1);
}

/// A `working_dir` pointing at a path that does not exist comes back
/// as a tool error naming that path, never a hard `Result::Err` from
/// `execute`. The message names the exact path, not just "not found"
/// generically, so a model reading it can tell what it typed wrong.
#[tokio::test]
async fn execute_with_a_missing_working_dir_returns_a_tool_error_naming_the_path() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("scripted reply".to_string())]);
    let tool = TaskTool::new(
        factory,
        1,
        test_parent_tx(),
        empty_registry(),
        effort_flag_at(Effort::None),
    );
    let missing = std::env::temp_dir().join("dsc-task-tool-missing-working-dir-xyz");
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "hello",
        "backend": "stub-agent",
        "working_dir": missing.to_string_lossy(),
    });

    let output = tool.execute(input).await.expect("execute should not return a hard error");

    assert!(output.is_error);
    assert!(output.content.contains(&missing.display().to_string()));
}

/// Without `keep_open`, a dispatch against a stub backend leaves the
/// registry empty and the result carries only the subagent's text.
#[tokio::test]
async fn execute_without_keep_open_leaves_the_registry_empty() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("scripted reply".to_string())]);
    let registry = empty_registry();
    let tool = TaskTool::new(
        factory,
        1,
        test_parent_tx(),
        registry.clone(),
        effort_flag_at(Effort::None),
    );
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "hello",
        "backend": "stub-agent",
    });

    let output = tool.execute(input).await.expect("execute should not return a hard error");

    assert!(!output.is_error);
    assert_eq!(output.content, "scripted reply");
    assert_eq!(registry.len().await, 0);
}

/// Parse the numeric id `Task`'s `keep_open` text embeds, so a test can
/// go look up that session in the registry directly. Mirrors how
/// `SendMessage`'s own input turns this text back into a `SubagentId`.
fn session_id_from_output(content: &str) -> deepseek_custom::agent::agent_loop::SubagentId {
    content
        .split("session kept open: ")
        .nth(1)
        .expect("expected the session-kept-open marker")
        .trim_end_matches(']')
        .parse()
        .expect("expected a numeric session id")
}

/// An explicit `effort` override reaches the dispatched subagent: its
/// own effort flag ends up at exactly that level, not the dispatching
/// session's own current level.
#[tokio::test]
async fn execute_with_an_explicit_effort_override_reaches_the_subagent() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("scripted reply".to_string())]);
    let registry = empty_registry();
    let tool = TaskTool::new(
        factory,
        1,
        test_parent_tx(),
        registry.clone(),
        effort_flag_at(Effort::Low),
    );
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "hello",
        "backend": "stub-agent",
        "keep_open": true,
        "effort": "high",
    });

    let output = tool.execute(input).await.expect("execute should not return a hard error");
    assert!(!output.is_error);

    let id = session_id_from_output(&output.content);
    let effort_flag = registry
        .effort_flag_for_test(id)
        .await
        .expect("session should be registered");
    assert_eq!(Effort::load(&effort_flag), Effort::High);
}

/// Without an explicit `effort`, the dispatched subagent inherits the
/// dispatching session's own current level, read at dispatch time.
#[tokio::test]
async fn execute_without_an_effort_override_inherits_the_parents_current_level() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("scripted reply".to_string())]);
    let registry = empty_registry();
    let tool = TaskTool::new(
        factory,
        1,
        test_parent_tx(),
        registry.clone(),
        effort_flag_at(Effort::Medium),
    );
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "hello",
        "backend": "stub-agent",
        "keep_open": true,
    });

    let output = tool.execute(input).await.expect("execute should not return a hard error");
    assert!(!output.is_error);

    let id = session_id_from_output(&output.content);
    let effort_flag = registry
        .effort_flag_for_test(id)
        .await
        .expect("session should be registered");
    assert_eq!(Effort::load(&effort_flag), Effort::Medium);
}

/// A dispatch, with or without an explicit `effort`, never moves the
/// dispatching session's own level: only the freshly built subagent's
/// flag is written.
#[tokio::test]
async fn a_dispatch_never_moves_the_parents_own_effort_level() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("scripted reply".to_string())]);
    let parent_flag = effort_flag_at(Effort::Low);
    let tool = TaskTool::new(
        factory,
        1,
        test_parent_tx(),
        empty_registry(),
        parent_flag.clone(),
    );
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "hello",
        "backend": "stub-agent",
        "effort": "max",
    });

    let output = tool.execute(input).await.expect("execute should not return a hard error");

    assert!(!output.is_error);
    assert_eq!(Effort::load(&parent_flag), Effort::Low);
}
