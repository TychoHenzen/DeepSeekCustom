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

