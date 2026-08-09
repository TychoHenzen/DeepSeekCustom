//! The `Cascade` tool: dispatch `n` subagent attempts at the same prompt,
//! pick a winner by voting and an optional check command, and escalate to a
//! stronger backend when no candidate reaches the vote threshold.
//!
//! Diversity.md's best-evidenced idea: cheap-model fanout with a strong model
//! or a check command picking the winner, rather than trusting one answer from
//! one run. Each attempt is an ordinary `keep_open: false` dispatch through
//! `run_subagent`, the same mechanism `Task` already uses. The tool result
//! names the winning attempt and the vote count behind it.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::agent::agent_loop::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::subagent::{SubagentRequest, run_subagent};
use crate::effort::Effort;
use crate::error::Result;
use crate::tools::bash::{self, Shell};
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

/// The outcome of voting across candidates.
enum VoteOutcome {
    /// A single answer reached the `vote_k` lead margin.
    Winner {
        /// The winning text (trimmed).
        text: String,
        /// How many candidates voted for this answer.
        count: usize,
        /// 1-based attempt indices of the candidates that produced this answer.
        winning_indices: Vec<usize>,
    },
    /// No answer reached the required lead margin, or there were no candidates
    /// at all to vote on.
    NoConsensus {
        /// Vote tallies, sorted by count descending.
        tallies: Vec<VoteTally>,
    },
}

/// One vote group: candidates that produced the same trimmed text, with
/// their 1-based attempt indices.
struct VoteTally {
    text: String,
    count: usize,
    indices: Vec<usize>,
}

/// A candidate answer from one subagent attempt.
struct Candidate {
    /// 1-based attempt index.
    index: usize,
    text: String,
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
    /// The working directory `check_cmd` runs against, shared with every
    /// other tool this harness registers. Read fresh on every call.
    work_dir: Arc<Mutex<PathBuf>>,
}

impl CascadeTool {
    pub fn new(
        factory: Arc<BackendFactory>,
        dispatch_depth: u32,
        parent_tx: mpsc::UnboundedSender<RoutedEvent>,
        registry: Arc<SubagentRegistry>,
        parent_effort_flag: Arc<AtomicU8>,
        work_dir: Arc<Mutex<PathBuf>>,
    ) -> Self {
        Self {
            factory,
            dispatch_depth,
            parent_tx,
            registry,
            parent_effort_flag,
            work_dir,
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

        // Collect candidates, separating dispatch successes from failures.
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut failures: Vec<String> = Vec::new();

        for (i, result) in results.into_iter().enumerate() {
            match result {
                Ok(Ok(outcome)) => {
                    candidates.push(Candidate {
                        index: i + 1,
                        text: outcome.text,
                    });
                }
                Ok(Err(e)) => {
                    failures.push(format!("Attempt {}: FAILED - {e}", i + 1));
                }
                Err(join_err) => {
                    failures.push(format!("Attempt {}: PANICKED - {join_err}", i + 1));
                }
            }
        }

        // When `check_cmd` is set, run it once per candidate against the
        // current working directory. A candidate whose command exits non-zero
        // is dropped before voting (B4). This is Diversity.md's red-flag step.
        if let Some(ref check_cmd) = parsed.check_cmd {
            let work_dir = self
                .work_dir
                .lock()
                .expect("work_dir mutex poisoned")
                .clone();
            let mut passed: Vec<Candidate> = Vec::new();

            for candidate in candidates {
                match run_check_cmd(check_cmd, &work_dir).await {
                    Ok(true) => passed.push(candidate),
                    Ok(false) => {
                        failures.push(format!(
                            "Attempt {}: rejected by check_cmd (exited non-zero)",
                            candidate.index
                        ));
                    }
                    Err(e) => {
                        failures.push(format!(
                            "Attempt {}: check_cmd error - {e}",
                            candidate.index
                        ));
                    }
                }
            }
            candidates = passed;
        }

        // B4: Vote among candidates that passed check_cmd (or all candidates
        // when no check_cmd was given). Group by exact match on trimmed text.
        // The top group must beat the runner-up by at least `vote_k` to win.
        let vote_outcome = vote(&candidates, parsed.vote_k);

        // Build output: passing candidates grouped by vote tally, then failures.
        let mut parts: Vec<String> = Vec::new();

        match &vote_outcome {
            VoteOutcome::Winner { text, count, winning_indices } => {
                let id_label: String = if winning_indices.len() == 1 {
                    format!("Attempt {}", winning_indices[0])
                } else {
                    let ids: Vec<String> = winning_indices.iter().map(|i| i.to_string()).collect();
                    format!("Attempts {}", ids.join(", "))
                };
                parts.push(format!(
                    "{} won ({} vote(s), lead by at least {}-vote margin): {}",
                    id_label, count, parsed.vote_k, text
                ));
            }
            VoteOutcome::NoConsensus { tallies } => {
                // C1: when `escalate_backend` is set, make one more call instead
                // of returning an error. The escalation prompt carries the
                // original task, every candidate and why it was cut, and an
                // instruction to pick the best or write a fresh answer.
                if let Some(escalate_backend) = &parsed.escalate_backend {
                    return escalate(
                        escalate_backend,
                        &parsed.prompt,
                        tallies,
                        &failures,
                        effort,
                        &self.factory,
                        self.dispatch_depth,
                        &self.parent_tx,
                        &self.registry,
                    )
                    .await;
                }

                // B6: distinguish "every attempt failed" from "no candidate passed
                // check_cmd" from "candidates existed but no consensus". All three
                // are tool errors (#1 and #2 never panic either).
                if candidates.is_empty() && !failures.is_empty() {
                    if parsed.check_cmd.is_some() {
                        parts.push(format!(
                            "No candidate passed check_cmd on backend \"{}\" (0 of {} passed).",
                            parsed.backend,
                            parsed.n
                        ));
                    } else {
                        parts.push(format!(
                            "All {} attempts failed on backend \"{}\".",
                            parsed.n,
                            parsed.backend
                        ));
                    }
                } else if !tallies.is_empty() {
                    parts.push(format!(
                        "No consensus: no answer reached the required {}-vote lead margin.",
                        parsed.vote_k
                    ));
                    for t in tallies {
                        parts.push(format!("{} vote(s): {}", t.count, t.text));
                    }
                }
            }
        }

        parts.extend(failures);

        let is_error = candidates.is_empty() || matches!(&vote_outcome, VoteOutcome::NoConsensus { .. });
        let pass_note = parsed.check_cmd.as_ref().map(|_| {
            format!(
                " ({} of {} passed check_cmd)",
                candidates.len(),
                parsed.n
            )
        }).unwrap_or_default();

        Ok(ToolOutput {
            content: format!(
                "Cascade results ({} attempt(s) on backend \"{}\"{}):\n\n{}",
                n,
                parsed.backend,
                pass_note,
                parts.join("\n\n")
            ),
            is_error,
            image: None,
        })
    }
}

/// Run a single check command against the working directory and report
/// whether it passed (exited zero). A spawn failure or timeout becomes an
/// `Err`, so the caller can distinguish "the command failed" from "we could
/// not run it at all".
async fn run_check_cmd(
    cmd_str: &str,
    work_dir: &Path,
) -> std::result::Result<bool, String> {
    let result = timeout(
        Duration::from_millis(120_000),
        bash::run_command(cmd_str, work_dir, Shell::Auto),
    )
    .await;

    match result {
        Ok(Ok(output)) => Ok(output.exit_code == 0),
        Ok(Err(e)) => Err(format!("failed to run check_cmd: {e}")),
        Err(_elapsed) => Err("check_cmd timed out after 120s".to_string()),
    }
}

/// C1: when no candidate reaches `vote_k` but `escalate_backend` was given,
/// dispatch one more subagent with the original prompt plus every rejected
/// candidate and why it was cut. Its answer becomes the tool output, marked
/// as escalated.
#[allow(clippy::too_many_arguments)]
async fn escalate(
    escalate_backend: &str,
    original_prompt: &str,
    tallies: &[VoteTally],
    failures: &[String],
    effort: Effort,
    factory: &Arc<BackendFactory>,
    dispatch_depth: u32,
    parent_tx: &mpsc::UnboundedSender<RoutedEvent>,
    registry: &Arc<SubagentRegistry>,
) -> Result<ToolOutput> {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("Original task:\n{original_prompt}"));

    if !tallies.is_empty() {
        parts.push(
            "\nCandidates that survived but failed to reach the required vote margin:".to_string(),
        );
        for t in tallies {
            parts.push(format!(
                "- {} vote(s): {}",
                t.count, t.text
            ));
        }
    }

    if !failures.is_empty() {
        parts.push("\nCandidates that were rejected before voting:".to_string());
        for f in failures {
            parts.push(format!("- {f}"));
        }
    }

    parts.push(
        "\nPick the best answer above, or write your own if none of them is right. \
         Return only the final answer, with no commentary.".to_string(),
    );

    let escalation_prompt = parts.join("\n");

    let request = SubagentRequest {
        backend: escalate_backend.to_string(),
        model: None,
        prompt: escalation_prompt,
        depth: dispatch_depth,
        keep_open: false,
        working_dir_override: None,
        effort,
    };

    match run_subagent(factory, request, parent_tx.clone(), Arc::clone(registry)).await {
        Ok(outcome) => {
            Ok(ToolOutput {
                content: format!(
                    "Cascade escalated to backend \"{}\":\n\n[escalated] {}",
                    escalate_backend, outcome.text
                ),
                is_error: false,
                image: None,
            })
        }
        Err(e) => {
            Ok(ToolOutput {
                content: format!(
                    "Cascade escalation to backend \"{}\" failed: {e}",
                    escalate_backend
                ),
                is_error: true,
                image: None,
            })
        }
    }
}

/// Group candidates by exact match on trimmed text and check whether the top
/// group's vote count beats the runner-up's by at least `vote_k`.
///
/// When `candidates` is empty, returns `NoConsensus` with an empty tally.
fn vote(candidates: &[Candidate], vote_k: u32) -> VoteOutcome {
    if candidates.is_empty() {
        return VoteOutcome::NoConsensus {
            tallies: Vec::new(),
        };
    }

    // Group by trimmed text.
    let mut tallies: Vec<VoteTally> = Vec::new();
    for c in candidates {
        let trimmed = c.text.trim();
        if let Some(tally) = tallies.iter_mut().find(|t| t.text == trimmed) {
            tally.count += 1;
            tally.indices.push(c.index);
        } else {
            tallies.push(VoteTally {
                text: trimmed.to_string(),
                count: 1,
                indices: vec![c.index],
            });
        }
    }

    // Sort descending by count, then by text for determinism.
    tallies.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.text.cmp(&b.text)));

    let top = &tallies[0];
    let runner_up = tallies.get(1).map(|t| t.count).unwrap_or(0);

    if top.count.saturating_sub(runner_up) >= vote_k as usize {
        VoteOutcome::Winner {
            text: top.text.clone(),
            count: top.count,
            winning_indices: top.indices.clone(),
        }
    } else {
        VoteOutcome::NoConsensus { tallies }
    }
}
