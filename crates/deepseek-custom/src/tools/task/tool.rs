//! `TaskTool`: the `Tool` impl that dispatches a subagent onto a named
//! backend. See `src/backend/subagent.rs` for the dispatch machinery.

use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::agent::events::{RoutedEvent, SubagentId};
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::subagent::run_subagent;
use crate::effort::Effort;
use crate::error::Result;
use crate::tools::{Tool, ToolOutput};

use super::input::build_request;

const TASK_DESCRIPTION: &str = concat!(
    "Dispatch a subagent to carry out one self-contained piece of ",
    "work on a named backend. The subagent starts with no ",
    "conversation history of its own: it sees only the `prompt` ",
    "text you give it here, nothing else from this conversation. ",
    "It does not stream its output back to you; only its final ",
    "answer comes back as this tool's result. Pick a `backend` ",
    "suited to the work: a strong model to plan, judge, or ",
    "handle ambiguity, a cheaper or local model to iterate on ",
    "small, mechanical, well-specified pieces. Because the ",
    "subagent has nothing but `prompt` to go on, write it fully ",
    "self-contained: state the goal, name the relevant files or ",
    "facts, and say what a finished answer looks like. Set ",
    "`keep_open` to true to keep the subagent's session alive ",
    "after this call returns, so a later follow-up can continue ",
    "it instead of restating the whole prompt from scratch. Set ",
    "`working_dir` to point the subagent at a directory of its ",
    "own, such as a sibling checkout, instead of wherever this ",
    "session currently stands. Leave it unset to have the ",
    "subagent inherit this session's current working directory. ",
    "Either way, the subagent's own directory is independent of ",
    "this session's: nothing it does to its directory moves ",
    "yours, and nothing you do afterward moves its. Set `effort` ",
    "to one of `none`, `low`, `medium`, `high`, `max` to run ",
    "the subagent at that reasoning-effort level, regardless of ",
    "what this session is currently set to: dispatch a cheap ",
    "subagent at `low` and a hard one at `max` in the same turn ",
    "if the work calls for it. Leave it unset to have the ",
    "subagent inherit this session's current effort level. ",
    "Either way, this session's own level is never moved by a ",
    "dispatch, in either direction.",
);

/// Dispatches a subagent onto a named backend. Registered on every
/// backend whose dispatch depth is still under `subagent_max_depth`.
/// See `may_dispatch` in `src/backend/factory.rs`.
pub struct TaskTool {
    factory: Arc<BackendFactory>,
    /// Depth of the subagent this tool will dispatch. The main session
    /// registers the tool with 1.
    dispatch_depth: u32,
    /// Where a dispatched subagent's events land, once routed. This is
    /// the same sender the owning backend was built with, so nesting
    /// composes for free: a dispatch at depth N+1 prepends its own id
    /// to whatever route an event already carries.
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    /// The calling agent's own subagent registry. A `keep_open`
    /// dispatch registers its backend here, under the id
    /// `run_subagent` allocates. `AgentLoop::run` and its
    /// `SessionReset` branch close this registry, so a session this
    /// tool opens is bound to that agent's turn and reset lifetime.
    registry: Arc<SubagentRegistry>,
    /// The dispatching session's own effort flag. Read at dispatch
    /// time as the default level for a `Task` call that carries no
    /// explicit `effort`. Never written here: a subagent's own level
    /// must never move its parent's, in either direction.
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

/// Format a successful subagent outcome for the tool result.
/// Appends a `[session kept open: N]` note when the dispatch left
/// its session alive.
fn format_success(text: String, session_id: Option<SubagentId>) -> String {
    match session_id {
        Some(id) => format!("{text}\n\n[session kept open: {id}]"),
        None => text,
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "Task"
    }

    fn description(&self) -> &str {
        TASK_DESCRIPTION
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
                    "description": "The full, self-contained instruction for the \
                    subagent. It sees only this text, nothing else from the \
                    current conversation."
                },
                "backend": {
                    "type": "string",
                    "description": "Name of an entry in the `backends` map in \
                    settings.json to run the subagent on."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model name, overriding the one the \
                    backend entry declares."
                },
                "keep_open": {
                    "type": "boolean",
                    "description": "When true, the subagent's session stays open \
                    after this call returns instead of being dropped. The result \
                    names the session id. Defaults to false."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional working directory for the subagent, \
                    such as a sibling checkout. Must already exist and be a \
                    directory. When absent, the subagent inherits this \
                    session's current working directory."
                },
                "effort": {
                    "type": "string",
                    "enum": ["none", "low", "medium", "high", "max"],
                    "description": "Optional reasoning-effort level for the \
                    subagent: none, low, medium, high, or max. When absent, the \
                    subagent inherits this session's current effort level."
                }
            },
            "required": ["description", "prompt", "backend"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let default_effort = Effort::load(&self.parent_effort_flag);
        let request = match build_request(input, self.dispatch_depth, default_effort) {
            Ok(request) => request,
            Err(e) => return Ok(ToolOutput::error(e)),
        };

        match run_subagent(
            &self.factory,
            request,
            self.parent_tx.clone(),
            self.registry.clone(),
        )
        .await
        {
            Ok(outcome) => Ok(ToolOutput::ok(format_success(
                outcome.text,
                outcome.session_id,
            ))),
            Err(e) => Ok(ToolOutput::error(format!("Task dispatch failed: {e}"))),
        }
    }
}
