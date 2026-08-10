//! `TaskInput` and `build_request`: the pure-function layer that parses
//! and validates a `Task` call's JSON into a `SubagentRequest`, without
//! touching any IO or state. Kept separate from `TaskTool` so every rule
//! about depth wiring, defaulting, and effort resolution is testable
//! against a plain value, no backend required.

use std::path::PathBuf;

use serde::Deserialize;

use crate::backend::subagent::SubagentRequest;
use crate::effort::Effort;

/// Raw, deserialized `Task` input. `description` is a label the model
/// fills in for its own bookkeeping. The dispatch itself never reads it.
#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct TaskInput {
    #[allow(dead_code)]
    description: String,
    pub(crate) prompt: String,
    pub(crate) backend: String,
    pub(crate) model: Option<String>,
    pub(crate) keep_open: Option<bool>,
    pub(crate) working_dir: Option<String>,
    pub(crate) effort: Option<Effort>,
}

/// Parse and validate raw JSON into a `SubagentRequest`, pinning
/// `depth` to `dispatch_depth`. `default_effort` is the dispatching
/// session's own current level; an explicit `effort` on the input wins.
///
/// `Effort`'s own `Deserialize` rejects an unrecognised string, naming
/// the accepted values in its error, so a bad `effort` surfaces the
/// same way a missing `prompt` already does -- as an `Err` here, never
/// a panic.
pub fn build_request(
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
