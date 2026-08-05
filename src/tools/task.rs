//! The `Task` tool: dispatches a subagent onto a named backend. A model
//! plans, then hands a self-contained piece of work to a subagent running
//! on whatever backend fits, a cheaper or local model for mechanical work,
//! a strong one for judgment calls. See `src/backend/subagent.rs` for the
//! machinery this sits on.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::agent::agent_loop::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::subagent::{SubagentRequest, run_subagent};
use crate::effort::Effort;
use crate::error::Result;
use crate::tools::{Tool, ToolOutput};

/// Raw, deserialized `Task` input. `description` is carried through only
/// as a label for the model's own bookkeeping. The dispatch itself never
/// reads it back.
#[derive(Debug, Deserialize, PartialEq)]
struct TaskInput {
    #[allow(dead_code)]
    description: String,
    prompt: String,
    backend: String,
    model: Option<String>,
    keep_open: Option<bool>,
    working_dir: Option<String>,
    effort: Option<Effort>,
}

/// Parse and validate raw JSON into a `SubagentRequest`, pinning `depth` to
/// `dispatch_depth` and resolving `effort`: an explicit value on the input
/// wins, otherwise `default_effort` (the dispatching session's own current
/// level, read by the caller) is used. A pure function, so the depth
/// wiring, the effort resolution, and malformed input are all testable
/// without running a subagent. `Effort`'s own `Deserialize` rejects an
/// unrecognised string, naming the accepted values in its error, so a bad
/// `effort` surfaces the same way a missing `prompt` already does: as an
/// `Err` here, never a panic.
fn build_request(
    input: serde_json::Value,
    dispatch_depth: u32,
    default_effort: Effort,
) -> std::result::Result<SubagentRequest, String> {
    let parsed: TaskInput =
        serde_json::from_value(input).map_err(|e| format!("Invalid Task input: {e}"))?;
    Ok(SubagentRequest {
        backend: parsed.backend,
        model: parsed.model,
        prompt: parsed.prompt,
        depth: dispatch_depth,
        keep_open: parsed.keep_open.unwrap_or(false),
        working_dir_override: parsed.working_dir.map(PathBuf::from),
        effort: parsed.effort.unwrap_or(default_effort),
    })
}

/// Dispatches a subagent onto a named backend. Registered on every backend
/// whose dispatch depth is still under `subagent_max_depth`. See
/// `may_dispatch` in `src/backend/factory.rs`.
pub struct TaskTool {
    factory: Arc<BackendFactory>,
    /// Depth of the subagent this tool will dispatch. The main session
    /// registers the tool with 1.
    dispatch_depth: u32,
    /// Where a dispatched subagent's events land once routed. This is the
    /// same sender the backend that owns this tool was itself built with,
    /// so a subagent's forwarder (`src/backend/subagent.rs`) relays events
    /// onto the exact channel this tool's own turn already streams onto.
    /// Nesting composes for free: a dispatch at depth N+1 prepends its own
    /// id to whatever route an event already carries before it reaches
    /// this sender.
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    /// The calling agent's own subagent registry. A `keep_open` dispatch
    /// registers its backend here, under the id this tool's own
    /// `run_subagent` call allocates. This is the registry `AgentLoop::run`
    /// and its `SessionReset` branch close out, so a session this tool
    /// opens is bound to that agent's own turn and reset lifetime, never
    /// to some other agent's.
    registry: Arc<SubagentRegistry>,
    /// The dispatching session's own effort flag: the same `Arc` its own
    /// agent reports from `effort_flag()`. Read at dispatch time as the
    /// default level for a `Task` call that carries no explicit `effort`
    /// of its own. Never written here: a subagent's own level must never
    /// move its parent's, in either direction. See `Effort::store` calls
    /// in `src/backend/subagent.rs`, which write only the freshly built
    /// subagent's own flag, never this one.
    parent_effort_flag: Arc<AtomicU8>,
}

impl TaskTool {
    pub fn new(
        factory: Arc<BackendFactory>,
        dispatch_depth: u32,
        parent_tx: mpsc::UnboundedSender<RoutedEvent>,
        registry: Arc<SubagentRegistry>,
        parent_effort_flag: Arc<AtomicU8>,
    ) -> Self {
        Self {
            factory,
            dispatch_depth,
            parent_tx,
            registry,
            parent_effort_flag,
        }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "Task"
    }

    fn description(&self) -> &str {
        "Dispatch a subagent to carry out one self-contained piece of work on a named backend. \
        The subagent starts with no conversation history of its own: it sees only the `prompt` \
        text you give it here, nothing else from this conversation. It does not stream its \
        output back to you; only its final answer comes back as this tool's result. Pick a \
        `backend` suited to the work: a strong model to plan, judge, or handle ambiguity, a \
        cheaper or local model to iterate on small, mechanical, well-specified pieces. Because \
        the subagent has nothing but `prompt` to go on, write it fully self-contained: state the \
        goal, name the relevant files or facts, and say what a finished answer looks like. Set \
        `keep_open` to true to keep the subagent's session alive after this call returns, so a \
        later follow-up can continue it instead of restating the whole prompt from scratch. Set \
        `working_dir` to point the subagent at a directory of its own, such as a sibling \
        checkout, instead of wherever this session currently stands. Leave it unset to have the \
        subagent inherit this session's current working directory. Either way, the subagent's \
        own directory is independent of this session's: nothing it does to its directory moves \
        yours, and nothing you do afterward moves its. Set `effort` to one of `none`, `low`, \
        `medium`, `high`, `max` to run the subagent at that reasoning-effort level, regardless \
        of what this session is currently set to: dispatch a cheap subagent at `low` and a hard \
        one at `max` in the same turn if the work calls for it. Leave it unset to have the \
        subagent inherit this session's current effort level. Either way, this session's own \
        level is never moved by a dispatch, in either direction."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "A short label for the dispatched work, a few words."
                },
                "prompt": {
                    "type": "string",
                    "description": "The full, self-contained instruction for the subagent. It sees only this text, nothing else from the current conversation."
                },
                "backend": {
                    "type": "string",
                    "description": "Name of an entry in the `backends` map in settings.json to run the subagent on."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model name, overriding the one the backend entry declares."
                },
                "keep_open": {
                    "type": "boolean",
                    "description": "When true, the subagent's session stays open after this call returns instead of being dropped. The result names the session id. Defaults to false."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional working directory for the subagent, such as a sibling checkout. Must already exist and be a directory. When absent, the subagent inherits this session's current working directory."
                },
                "effort": {
                    "type": "string",
                    "enum": ["none", "low", "medium", "high", "max"],
                    "description": "Optional reasoning-effort level for the subagent: none, low, medium, high, or max. When absent, the subagent inherits this session's current effort level."
                }
            },
            "required": ["description", "prompt", "backend"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let default_effort = Effort::load(&self.parent_effort_flag);
        let request = match build_request(input, self.dispatch_depth, default_effort) {
            Ok(request) => request,
            Err(e) => {
                return Ok(ToolOutput {
                    content: e,
                    is_error: true,
                    image: None,
                });
            }
        };

        match run_subagent(&self.factory, request, self.parent_tx.clone(), self.registry.clone()).await {
            Ok(outcome) => {
                let content = match outcome.session_id {
                    Some(id) => format!("{}\n\n[session kept open: {id}]", outcome.text),
                    None => outcome.text,
                };
                Ok(ToolOutput {
                    content,
                    is_error: false,
                    image: None,
                })
            }
            Err(e) => Ok(ToolOutput {
                content: format!("Task dispatch failed: {e}"),
                is_error: true,
                image: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::stub::StubTurn;
    use crate::config::settings::Settings;
    use std::path::PathBuf;

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
    fn session_id_from_output(content: &str) -> crate::agent::agent_loop::SubagentId {
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
}
