//! The `Task` tool: dispatches a subagent onto a named backend. A model
//! plans, then hands a self-contained piece of work to a subagent running
//! on whatever backend fits, a cheaper or local model for mechanical work,
//! a strong one for judgment calls. See `src/backend/subagent.rs` for the
//! machinery this sits on.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::backend::factory::BackendFactory;
use crate::backend::subagent::{SubagentRequest, run_subagent};
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
}

/// Parse and validate raw JSON into a `SubagentRequest`, pinning `depth` to
/// `dispatch_depth`. A pure function, so the depth wiring and malformed
/// input are both testable without running a subagent.
fn build_request(
    input: serde_json::Value,
    dispatch_depth: u32,
) -> std::result::Result<SubagentRequest, String> {
    let parsed: TaskInput =
        serde_json::from_value(input).map_err(|e| format!("Invalid Task input: {e}"))?;
    Ok(SubagentRequest {
        backend: parsed.backend,
        model: parsed.model,
        prompt: parsed.prompt,
        depth: dispatch_depth,
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
}

impl TaskTool {
    pub fn new(factory: Arc<BackendFactory>, dispatch_depth: u32) -> Self {
        Self {
            factory,
            dispatch_depth,
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
        goal, name the relevant files or facts, and say what a finished answer looks like."
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
                }
            },
            "required": ["description", "prompt", "backend"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let request = match build_request(input, self.dispatch_depth) {
            Ok(request) => request,
            Err(e) => {
                return Ok(ToolOutput {
                    content: e,
                    is_error: true,
                });
            }
        };

        match run_subagent(&self.factory, request).await {
            Ok(outcome) => Ok(ToolOutput {
                content: outcome.text,
                is_error: false,
            }),
            Err(e) => Ok(ToolOutput {
                content: format!("Task dispatch failed: {e}"),
                is_error: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::Settings;
    use std::path::PathBuf;

    fn empty_factory() -> Arc<BackendFactory> {
        Arc::new(BackendFactory::new(Settings::default(), PathBuf::from(".")))
    }

    fn well_formed_input() -> serde_json::Value {
        serde_json::json!({
            "description": "check the weather",
            "prompt": "What is 2 + 2?",
            "backend": "nope",
        })
    }

    #[test]
    fn input_schema_declares_all_four_properties_with_correct_requiredness() {
        let tool = TaskTool::new(empty_factory(), 1);
        let schema = tool.input_schema();

        let properties = schema["properties"]
            .as_object()
            .expect("schema should have properties");
        assert!(properties.contains_key("description"));
        assert!(properties.contains_key("prompt"));
        assert!(properties.contains_key("backend"));
        assert!(properties.contains_key("model"));

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
    }

    #[test]
    fn build_request_at_dispatch_depth_one_carries_depth_through() {
        let request = build_request(well_formed_input(), 1).expect("should parse");
        assert_eq!(request.depth, 1);
        assert_eq!(request.backend, "nope");
        assert_eq!(request.prompt, "What is 2 + 2?");
        assert_eq!(request.model, None);
    }

    #[test]
    fn build_request_missing_prompt_is_an_error_not_a_panic() {
        let input = serde_json::json!({
            "description": "check the weather",
            "backend": "nope",
        });
        let err = match build_request(input, 1) {
            Ok(_) => panic!("missing prompt should fail to parse"),
            Err(e) => e,
        };
        assert!(err.contains("Invalid Task input"));
    }

    #[tokio::test]
    async fn execute_with_unknown_backend_returns_tool_error_naming_it() {
        let tool = TaskTool::new(empty_factory(), 1);

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
        let tool = TaskTool::new(empty_factory(), 1);
        let input = serde_json::json!({"description": "missing everything else"});

        let output = tool
            .execute(input)
            .await
            .expect("execute should not return a hard error");

        assert!(output.is_error);
        assert!(output.content.contains("Invalid Task input"));
    }
}
