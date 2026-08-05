//! The `SendMessage` tool: sends another turn into a subagent session a
//! `Task` call left open with `keep_open: true`. See "Phase 3: multi-turn
//! subagent sessions" in `docs/plans/2026-08-04-long-term-roadmap.md`.
//!
//! The session's backend and history stay exactly where `Task` left them:
//! this tool runs one more turn on the same `Backend` `SubagentRegistry`
//! already owns, through `SubagentRegistry::send_message`. That method
//! also owns the two runaway-cost caps this tool otherwise has no
//! enforcement of. Both caps come back as a tool error, never a hard
//! `Result::Err`, matching every other error path a subagent dispatch can
//! take.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::agent::agent_loop::SubagentId;
use crate::backend::registry::SubagentRegistry;
use crate::error::Result;
use crate::tools::{Tool, ToolOutput};

/// Raw, deserialized `SendMessage` input. `session_id` is a string, not a
/// number: the id a `keep_open` `Task` call names in its result is plain
/// text, and taking it back as a string here avoids any JSON
/// number-vs-string mismatch a model's own formatting might introduce.
#[derive(Debug, Deserialize, PartialEq)]
struct SendMessageInput {
    session_id: String,
    prompt: String,
}

/// Sends another turn into an open subagent session. Registered on every
/// backend whose dispatch depth is still under `subagent_max_depth`, the
/// same gate `Task` itself is registered behind. See `may_dispatch` in
/// `src/backend/factory.rs`.
pub struct SendMessageTool {
    /// The calling agent's own subagent registry, the same `Arc` its
    /// `TaskTool` was built with. A session `Task` opened with
    /// `keep_open: true` is only ever reachable through this registry, so
    /// this tool can only reach sessions its own agent opened.
    registry: Arc<SubagentRegistry>,
    /// Turns allowed in total on one session, counting the turn that
    /// opened it. See `SubagentRegistry::send_message`.
    session_turn_cap: u32,
    /// `SendMessage` calls allowed in total during one of this tool's
    /// owner's own turns, across every session it has open.
    send_message_call_cap: u32,
}

impl SendMessageTool {
    pub fn new(
        registry: Arc<SubagentRegistry>,
        session_turn_cap: u32,
        send_message_call_cap: u32,
    ) -> Self {
        Self {
            registry,
            session_turn_cap,
            send_message_call_cap,
        }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "SendMessage"
    }

    fn description(&self) -> &str {
        "Send another turn into a subagent session a `Task` call left open with \
        `keep_open: true`. The session keeps its history from every turn before this \
        one, so `prompt` can be a short follow-up that refers to an earlier answer \
        without restating it. Returns that turn's final text. Both a per-session turn \
        limit and a per-turn limit on total `SendMessage` calls apply; either one being \
        reached comes back as a tool error rather than silently blocking."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The session id a `keep_open` Task call named in its result."
                },
                "prompt": {
                    "type": "string",
                    "description": "The follow-up turn to send into that session."
                }
            },
            "required": ["session_id", "prompt"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: SendMessageInput = match serde_json::from_value(input) {
            Ok(parsed) => parsed,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Invalid SendMessage input: {e}"),
                    is_error: true,
                    image: None,
                });
            }
        };

        let id = match SubagentId::from_str(&parsed.session_id) {
            Ok(id) => id,
            Err(_) => {
                return Ok(ToolOutput {
                    content: format!(
                        "Invalid SendMessage input: \"{}\" is not a valid session id",
                        parsed.session_id
                    ),
                    is_error: true,
                    image: None,
                });
            }
        };

        match self
            .registry
            .send_message(
                id,
                &parsed.prompt,
                self.session_turn_cap,
                self.send_message_call_cap,
            )
            .await
        {
            Ok(text) => Ok(ToolOutput {
                content: text,
                is_error: false,
                image: None,
            }),
            Err(e) => Ok(ToolOutput {
                content: format!("SendMessage failed: {e}"),
                is_error: true,
                image: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::factory::BackendFactory;
    use crate::backend::stub::StubTurn;
    use crate::backend::subagent::{SubagentRequest, run_subagent};
    use crate::config::settings::Settings;
    use std::path::PathBuf;

    fn factory_with_stub(name: &str, script: Vec<StubTurn>) -> Arc<BackendFactory> {
        Arc::new(BackendFactory::new(Settings::default(), PathBuf::from(".")).with_stub(name, script))
    }

    fn empty_registry() -> Arc<SubagentRegistry> {
        Arc::new(SubagentRegistry::new())
    }

    fn test_parent_tx() -> tokio::sync::mpsc::UnboundedSender<crate::agent::agent_loop::RoutedEvent> {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        tx
    }

    /// Opens a `keep_open` session against a scripted stub backend and
    /// returns its session id, ready for a `SendMessageTool` to be pointed
    /// at.
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
                effort: crate::effort::Effort::None,
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
            vec![StubTurn::Text("a".to_string()), StubTurn::Text("b".to_string())],
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
}
