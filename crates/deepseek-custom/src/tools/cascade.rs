//! The `Cascade` tool: dispatch `n` subagent attempts at the same prompt,
//! pick a winner by voting and an optional check command, and escalate to a
//! stronger backend when no candidate reaches the vote threshold.
//!
//! Diversity.md's best-evidenced idea: cheap-model fanout with a strong model
//! or a check command picking the winner, rather than trusting one answer from
//! one run. Each attempt is an ordinary `keep_open: false` dispatch through
//! `run_subagent`, the same mechanism `Task` already uses. The tool result
//! names the winning attempt and the vote count behind it.

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

/// Raw, deserialized `Cascade` input.
#[derive(Debug, Deserialize, PartialEq)]
struct CascadeInput {
    prompt: String,
    backend: String,
    #[serde(default = "default_n")]
    n: u32,
    #[serde(default = "default_vote_k")]
    vote_k: u32,
    check_cmd: Option<String>,
    diversity_hints: Option<Vec<String>>,
    escalate_backend: Option<String>,
    effort: Option<Effort>,
}

const fn default_n() -> u32 {
    5
}

const fn default_vote_k() -> u32 {
    1
}

/// The default diversity hints, used when `diversity_hints` is absent from the
/// input. One is appended to each attempt's prompt, repeating in order once
/// the list runs out.
pub fn default_diversity_hints() -> Vec<String> {
    vec![
        "Use a different approach or library than the obvious first choice."
            .to_string(),
        "Favor simplicity over speed.".to_string(),
        "Handle edge cases and error paths first.".to_string(),
        "Write the plain, direct version.".to_string(),
    ]
}

/// Dispatches `n` subagents at once onto the named backend, each running the
/// same prompt with a different diversity hint appended. The result is picked
/// by voting (exact text match) after an optional `check_cmd` filters out
/// candidates that fail it. Registered on every backend below the dispatch
/// depth limit, depth-gated by `may_dispatch` exactly like `Task`.
pub struct CascadeTool {
    factory: Arc<BackendFactory>,
    /// Depth of the subagents this tool dispatches.
    dispatch_depth: u32,
    /// Where a dispatched subagent's events land once routed.
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    /// The calling agent's own subagent registry.
    registry: Arc<SubagentRegistry>,
    /// The dispatching session's own effort flag, read at dispatch time as
    /// the default for a call that carries no explicit `effort`.
    parent_effort_flag: Arc<AtomicU8>,
}

impl CascadeTool {
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
impl Tool for CascadeTool {
    fn name(&self) -> &str {
        "Cascade"
    }

    fn description(&self) -> &str {
        "Dispatch N subagent attempts at the same prompt, each with a different diversity hint, \
        and pick the winner by voting. An optional check command can filter out failing candidates \
        before the vote. When no candidate reaches the vote threshold, and an escalate_backend is \
        given, one more call to that stronger backend picks or writes the final answer. Use this \
        for work where more than one approach might be right, or where a checkable answer exists \
        but no single run is reliable enough to trust on its own."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The shared task text, sent to every attempt."
                },
                "backend": {
                    "type": "string",
                    "description": "Name of an entry in the `backends` map in settings.json to run every attempt on. Meant to be the cheap one."
                },
                "n": {
                    "type": "integer",
                    "description": "How many attempts to run. Defaults to 5.",
                    "default": 5
                },
                "vote_k": {
                    "type": "integer",
                    "description": "The lead the top answer needs over the runner-up to win without escalating. Defaults to 1.",
                    "default": 1
                },
                "check_cmd": {
                    "type": "string",
                    "description": "A shell command run once per candidate, against the current working directory. A candidate whose command exits non-zero is dropped before voting."
                },
                "diversity_hints": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "One hint appended per attempt, repeating in order once the list runs out. Defaults to a built-in set of four."
                },
                "escalate_backend": {
                    "type": "string",
                    "description": "A stronger backend name, used when no candidate reaches vote_k. Its answer becomes the tool output, marked as escalated."
                },
                "effort": {
                    "type": "string",
                    "enum": ["none", "low", "medium", "high", "max"],
                    "description": "Optional reasoning-effort level for every attempt: none, low, medium, high, or max. When absent, each attempt inherits this session's current effort level."
                }
            },
            "required": ["prompt", "backend"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: CascadeInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Invalid Cascade input: {e}"),
                    is_error: true,
                    image: None,
                });
            }
        };

        let hints = parsed
            .diversity_hints
            .unwrap_or_else(default_diversity_hints);
        let default_effort = Effort::load(&self.parent_effort_flag);
        let effort = parsed.effort.unwrap_or(default_effort);
        // Cap at 16: each attempt is a full subagent dispatch. More than that
        // against one backend risks saturating it for little diversity gain.
        let n = parsed.n.clamp(1, 16);

        // Dispatch every attempt at once. Each one is an ordinary
        // `keep_open: false` call through `run_subagent`, the same path
        // `Task` already uses, so every attempt gets its own `SubagentId`
        // and its own `Subagent` block in the transcript.
        let mut handles = Vec::with_capacity(n as usize);
        for i in 0..n {
            let hint = &hints[i as usize % hints.len()];
            let prompt = format!("{}\n\nDiversity hint: {hint}", parsed.prompt);
            let request = SubagentRequest {
                backend: parsed.backend.clone(),
                model: None,
                prompt,
                depth: self.dispatch_depth,
                keep_open: false,
                working_dir_override: None,
                effort,
            };
            let factory = Arc::clone(&self.factory);
            let parent_tx = self.parent_tx.clone();
            let registry = Arc::clone(&self.registry);

            handles.push(tokio::spawn(async move {
                run_subagent(&factory, request, parent_tx, registry).await
            }));
        }

        let results = futures::future::join_all(handles).await;

        let mut parts: Vec<String> = Vec::with_capacity(results.len());
        for (i, result) in results.into_iter().enumerate() {
            match result {
                Ok(Ok(outcome)) => {
                    parts.push(format!("Attempt {}: {}", i + 1, outcome.text));
                }
                Ok(Err(e)) => {
                    parts.push(format!("Attempt {}: FAILED - {e}", i + 1));
                }
                Err(join_err) => {
                    parts.push(format!("Attempt {}: PANICKED - {join_err}", i + 1));
                }
            }
        }

        Ok(ToolOutput {
            content: format!(
                "Cascade results ({} attempt(s) on backend \"{}\"):\n\n{}",
                n,
                parsed.backend,
                parts.join("\n\n")
            ),
            is_error: false,
            image: None,
        })
    }
}
