//! The `CloseSession` tool: ends an open subagent session a `Task` call
//! left open with `keep_open: true`, releasing its backend. See "Phase 3:
//! multi-turn subagent sessions" in
//! `docs/plans/2026-08-04-long-term-roadmap.md`. The lifetime rule already
//! closes every open session at the end of its parent's turn, so this tool
//! is optional there, but it lets a model release a session early, before
//! its own turn ends, once it knows it has no more use for it.
//!
//! Closing runs through `SubagentRegistry::close`, the same path a turn end
//! and a `Reset` already use to shut a session's backend down: for
//! `claude_cli` that kills the child process, for `Api` it drops the agent,
//! for `Stub` it rewinds the script. There is no second shutdown path here.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::agent::agent_loop::SubagentId;
use crate::backend::registry::SubagentRegistry;
use crate::error::Result;
use crate::tools::{Tool, ToolOutput};

/// Raw, deserialized `CloseSession` input. `session_id` is a string, not a
/// number, matching `SendMessage`'s own input shape: the id a `keep_open`
/// `Task` call names in its result is plain text.
#[derive(Debug, Deserialize, PartialEq)]
struct CloseSessionInput {
    session_id: String,
}

/// Ends an open subagent session and releases its backend. Registered on
/// every backend whose dispatch depth is still under `subagent_max_depth`,
/// the same gate `Task` and `SendMessage` are registered behind. See
/// `may_dispatch` in `src/backend/factory.rs`.
pub struct CloseSessionTool {
    /// The calling agent's own subagent registry, the same `Arc` its
    /// `TaskTool` and `SendMessageTool` were built with. A session `Task`
    /// opened with `keep_open: true` is only ever reachable through this
    /// registry, so this tool can only close sessions its own agent opened.
    registry: Arc<SubagentRegistry>,
}

impl CloseSessionTool {
    pub fn new(registry: Arc<SubagentRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for CloseSessionTool {
    fn name(&self) -> &str {
        "CloseSession"
    }

    fn description(&self) -> &str {
        "End a subagent session a `Task` call left open with `keep_open: true`, and release \
        its backend. A session left open is closed automatically once your own turn ends, so \
        this tool only matters when you want to release one early, before that. Closing an \
        unknown or already-closed session comes back as a tool error rather than blocking."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The session id a `keep_open` Task call named in its result."
                }
            },
            "required": ["session_id"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: CloseSessionInput = match serde_json::from_value(input) {
            Ok(parsed) => parsed,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Invalid CloseSession input: {e}"),
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
                        "Invalid CloseSession input: \"{}\" is not a valid session id",
                        parsed.session_id
                    ),
                    is_error: true,
                    image: None,
                });
            }
        };

        if self.registry.close(id).await {
            Ok(ToolOutput {
                content: format!("session {id} closed"),
                is_error: false,
                image: None,
            })
        } else {
            Ok(ToolOutput {
                content: format!("no open session with id {id}"),
                is_error: true,
                image: None,
            })
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
}
