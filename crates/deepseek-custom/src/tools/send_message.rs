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

use crate::agent::events::SubagentId;
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
            Err(e) => return Ok(ToolOutput::error(format!("Invalid SendMessage input: {e}"))),
        };

        let id = match SubagentId::from_str(&parsed.session_id) {
            Ok(id) => id,
            Err(_) => {
                return Ok(ToolOutput::error(format!(
                    "Invalid SendMessage input: \"{}\" is not a valid session id",
                    parsed.session_id
                )));
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
            Ok(text) => Ok(ToolOutput::ok(text)),
            Err(e) => Ok(ToolOutput::error(format!("SendMessage failed: {e}"))),
        }
    }
}
