//! The `Evolve` tool: programmatic evolutionary search over a population of
//! candidates carried forward across many rounds.  Each round selects a parent
//! from an island archive, dispatches a mutation through `run_subagent`, scores
//! the result with `fitness_cmd` and optionally `feature_cmd`, and inserts the
//! candidate into the archive.  The archive and selection rules live in
//! `src/evolution/mod.rs` and never touch a model.
//!
//! Diversity.md #7: island models and MAP-Elites search, reimplemented here in
//! Rust so the population, archive, and selection are fixed code, not LLM
//! judgment.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::agent::agent_loop::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::subagent::{SubagentRequest, run_subagent};
use crate::effort::Effort;
use crate::error::Result;
use crate::evolution::{Candidate, Island};
use crate::tools::{Tool, ToolOutput};

/// Raw, deserialized `Evolve` input.
#[derive(Debug, Deserialize, PartialEq)]
struct EvolveInput {
    prompt: String,
    backend: String,
    #[serde(default = "default_generations")]
    generations: u32,
    #[serde(default = "default_population")]
    population: u32,
    fitness_cmd: String,
    feature_cmd: Option<String>,
    #[serde(default = "default_islands")]
    islands: u32,
    #[serde(default = "default_migration_interval")]
    migration_interval: u32,
    mutation_hints: Option<Vec<String>>,
    effort: Option<Effort>,
}

const fn default_generations() -> u32 {
    10
}

const fn default_population() -> u32 {
    6
}

const fn default_islands() -> u32 {
    1
}

const fn default_migration_interval() -> u32 {
    5
}

/// Programmatic evolutionary search tool.  Registered on every backend below
/// the dispatch depth limit, depth-gated by `may_dispatch` exactly like `Task`
/// and `Cascade`.
#[allow(dead_code)]
pub struct EvolveTool {
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
    /// The working directory `fitness_cmd` and `feature_cmd` run against,
    /// shared with every other tool this harness registers.
    work_dir: Arc<Mutex<PathBuf>>,
}

impl EvolveTool {
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
impl Tool for EvolveTool {
    fn name(&self) -> &str {
        "Evolve"
    }

    fn description(&self) -> &str {
        "Run programmatic evolutionary search: carry a population of candidate answers forward \
        across many rounds, select parents from a fitness archive, mutate them through subagent \
        dispatches, and score each result with a fitness command. An optional feature command \
        drives MAP-Elites grid diversity. Use this for open-ended optimization where more than \
        one round of selection is needed, not just a single fanout."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The seed task text, the starting point for evolution."
                },
                "backend": {
                    "type": "string",
                    "description": "Name of an entry in the `backends` map in settings.json to run every generation on."
                },
                "generations": {
                    "type": "integer",
                    "description": "How many rounds to run. Defaults to 10.",
                    "default": 10
                },
                "population": {
                    "type": "integer",
                    "description": "Candidates per round (per island). Defaults to 6.",
                    "default": 6
                },
                "fitness_cmd": {
                    "type": "string",
                    "description": "A shell command, run against the current working directory, that receives a candidate's text and prints one number to stdout. That number is the fitness. Required."
                },
                "feature_cmd": {
                    "type": "string",
                    "description": "A shell command that prints a comma-separated list of numbers describing where a candidate sits in behaviour space. Drives the MAP-Elites grid when set."
                },
                "islands": {
                    "type": "integer",
                    "description": "How many isolated sub-populations to run side by side. Defaults to 1.",
                    "default": 1
                },
                "migration_interval": {
                    "type": "integer",
                    "description": "How often to run island migration (reset bottom half, reseed from best). Defaults to 5 rounds.",
                    "default": 5
                },
                "mutation_hints": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "One hint round-robined into each new candidate's prompt, framed as an edit against the chosen parent."
                },
                "effort": {
                    "type": "string",
                    "enum": ["none", "low", "medium", "high", "max"],
                    "description": "Optional reasoning-effort level for every generation: none, low, medium, high, or max. When absent, each generation inherits this session's current effort level."
                }
            },
            "required": ["prompt", "backend", "fitness_cmd"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: EvolveInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Invalid Evolve input: {e}"),
                    is_error: true,
                    image: None,
                });
            }
        };

        let hints = parsed
            .mutation_hints
            .unwrap_or_else(default_mutation_hints);
        let default_effort = Effort::load(&self.parent_effort_flag);
        let effort = parsed.effort.unwrap_or(default_effort);
        let work_dir = self
            .work_dir
            .lock()
            .expect("work_dir mutex poisoned")
            .clone();
        let bucket_width = 1.0;
        let elite_k = parsed.population as usize;

        // Create islands.
        let mut islands: Vec<Island> = (0..parsed.islands)
            .map(|_| Island::new(bucket_width, elite_k))
            .collect();
        let use_features = parsed.feature_cmd.is_some();

        let mut hint_index: usize = 0;

        // Generation loop.
        for generation in 0..parsed.generations {
            // Migration: run after round 0, every `migration_interval` rounds.
            if generation > 0 && generation % parsed.migration_interval == 0 {
                crate::evolution::migrate(&mut islands);
            }

            for island in &mut islands {
                for _pop in 0..parsed.population {
                    // Build the prompt for this candidate.
                    let hint = &hints[hint_index % hints.len()];
                    hint_index = hint_index.wrapping_add(1);

                    let prompt = if let Some(parent) = island.select_parent(generation as usize) {
                        build_mutation_prompt(&parsed.prompt, &parent.text, hint)
                    } else {
                        build_seed_prompt(&parsed.prompt, hint)
                    };

                    // Dispatch.
                    let request = SubagentRequest {
                        backend: parsed.backend.clone(),
                        model: None,
                        prompt,
                        depth: self.dispatch_depth,
                        keep_open: false,
                        working_dir_override: None,
                        effort,
                    };

                    let outcome = match run_subagent(
                        &self.factory,
                        request,
                        self.parent_tx.clone(),
                        Arc::clone(&self.registry),
                    )
                    .await
                    {
                        Ok(o) => o,
                        Err(_e) => {
                            // E8: a failed generation drops this candidate
                            // for the round without stopping the run.
                            continue;
                        }
                    };

                    // Score with fitness_cmd.
                    let fitness = match run_score_cmd(
                        &parsed.fitness_cmd,
                        &outcome.text,
                        &work_dir,
                    )
                    .await
                    {
                        Ok(f) => f,
                        Err(e) => {
                            // E8: unparseable fitness is a tool error naming
                            // the command.  A silent zero-fitness value would
                            // corrupt the archive without saying so.
                            return Ok(ToolOutput {
                                content: format!("fitness_cmd error: {e}"),
                                is_error: true,
                                image: None,
                            });
                        }
                    };

                    // Optionally score with feature_cmd.
                    let features = if let Some(ref feature_cmd) = parsed.feature_cmd {
                        match run_feature_cmd(feature_cmd, &outcome.text, &work_dir).await {
                            Ok(f) => f,
                            Err(e) => {
                                // E8: unparseable feature is a tool error
                                // naming the command.
                                return Ok(ToolOutput {
                                    content: format!("feature_cmd error: {e}"),
                                    is_error: true,
                                    image: None,
                                });
                            }
                        }
                    } else {
                        Vec::new()
                    };

                    let candidate = Candidate {
                        text: outcome.text,
                        fitness,
                        features,
                    };
                    island.insert(candidate);
                }
            }
        }

        // Find the best candidate across every island.
        let best = islands
            .iter()
            .filter_map(|isle| isle.best())
            .max_by(|a, b| a.fitness.total_cmp(&b.fitness));

        match best {
            Some(c) => {
                let features_str = if c.features.is_empty() {
                    String::new()
                } else {
                    let coords: Vec<String> =
                        c.features.iter().map(|f| format!("{f:.3}")).collect();
                    format!(", features=[{}]", coords.join(", "))
                };
                let archive_summary = archive_table(&islands, use_features);
                Ok(ToolOutput {
                    content: format!(
                        "Evolve finished after {} generation(s) on backend \"{}\":\n\
                         Best fitness: {:.6}{}\n\
                         Best solution:\n{}\n\n{}",
                        parsed.generations,
                        parsed.backend,
                        c.fitness,
                        features_str,
                        c.text,
                        archive_summary,
                    ),
                    is_error: false,
                    image: None,
                })
            }
            None => Ok(ToolOutput {
                content: format!(
                    "Evolve: no viable candidate found after {} generation(s) on backend \"{}\".",
                    parsed.generations, parsed.backend
                ),
                is_error: true,
                image: None,
            }),
        }
    }
}

/// Default mutation hints, round-robined into each candidate's prompt. These
/// are framed as edits against the chosen parent, not fresh-start instructions.
fn default_mutation_hints() -> Vec<String> {
    vec![
        "Simplify the solution: remove unnecessary steps, variables, or branches."
            .to_string(),
        "Add handling for an edge case or error condition the current solution misses."
            .to_string(),
        "Optimize the most expensive part of the solution."
            .to_string(),
        "Re-express the solution more clearly or concisely."
            .to_string(),
        "Add input validation or defensive checks the current solution lacks."
            .to_string(),
        "Reduce duplication: combine repeated logic into a shared helper."
            .to_string(),
    ]
}

/// Build a prompt for a seed-generation candidate (no parent yet).
fn build_seed_prompt(seed: &str, hint: &str) -> String {
    format!(
        "{seed}\n\nMutation hint: {hint}\n\nGenerate a solution following this hint. \
         Return only the solution, with no commentary."
    )
}

/// Build a prompt for a mutation-generation candidate, carrying the parent's
/// current best text.
fn build_mutation_prompt(seed: &str, parent_text: &str, hint: &str) -> String {
    format!(
        "{seed}\n\nCurrent best solution:\n{parent_text}\n\n\
         Mutation hint: {hint}\n\nImprove the solution above by applying this \
         mutation. Return only the improved solution, with no commentary."
    )
}

/// Run a scoring command, piping `candidate_text` as stdin. Parses stdout as
/// a single `f64`. Returns an error when the command fails, times out, or does
/// not print a parseable number.
async fn run_score_cmd(
    cmd_str: &str,
    candidate_text: &str,
    work_dir: &Path,
) -> std::result::Result<f64, String> {
    let output = run_cmd_with_stdin(cmd_str, candidate_text, work_dir).await?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    stdout
        .parse::<f64>()
        .map_err(|e| format!("fitness_cmd did not print a number: '{stdout}': {e}"))
}

/// Run a feature command, piping `candidate_text` as stdin. Parses stdout as
/// comma-separated `f64` values. Returns an error when the command fails, times
/// out, or does not print parseable numbers.
async fn run_feature_cmd(
    cmd_str: &str,
    candidate_text: &str,
    work_dir: &Path,
) -> std::result::Result<Vec<f64>, String> {
    let output = run_cmd_with_stdin(cmd_str, candidate_text, work_dir).await?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err("feature_cmd printed nothing".to_string());
    }
    stdout
        .split(',')
        .map(|s| {
            s.trim()
                .parse::<f64>()
                .map_err(|e| format!("feature_cmd did not print numbers: '{stdout}': {e}"))
        })
        .collect()
}

/// Run a shell command with `stdin_text` piped to its stdin. Returns the raw
/// stdout/stderr output and exit code, or an error on spawn failure or timeout.
async fn run_cmd_with_stdin(
    cmd_str: &str,
    stdin_text: &str,
    work_dir: &Path,
) -> std::result::Result<CmdOutput, String> {
    let spawn = || async {
        let mut child = Command::new("cmd")
            .args(["/C", cmd_str])
            .current_dir(work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("failed to spawn command: {e}"))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(stdin_text.as_bytes())
                .await
                .map_err(|e| format!("failed to write to command stdin: {e}"))?;
        }
        // stdin is dropped here, closing the pipe.

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| format!("command failed: {e}"))?;
        Ok(CmdOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.status.code().unwrap_or(-1),
        })
    };

    match timeout(Duration::from_millis(120_000), spawn()).await {
        Ok(result) => result,
        Err(_elapsed) => Err("scoring command timed out after 120s".to_string()),
    }
}

/// Raw output from a shell command run through `run_cmd_with_stdin`.
struct CmdOutput {
    stdout: Vec<u8>,
    #[allow(dead_code)]
    stderr: Vec<u8>,
    #[allow(dead_code)]
    exit_code: i32,
}

/// Build a compact archive summary table for the tool result (E9). One row
/// per island, with cell count, best fitness, and best candidate text (trimmed
/// to a short preview).
fn archive_table(islands: &[Island], _use_features: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("Archive summary:".to_string());
    for (i, island) in islands.iter().enumerate() {
        let best = island.best();
        let best_preview = best
            .map(|c| {
                let t = c.text.trim();
                if t.len() > 80 {
                    format!("{}...", &t[..80])
                } else {
                    t.to_string()
                }
            })
            .unwrap_or_else(|| "(empty)".to_string());
        let best_fitness = best
            .map(|c| format!("{:.6}", c.fitness))
            .unwrap_or_else(|| "-".to_string());
        lines.push(format!(
            "  Island {i}: {} cell(s), best fitness={best_fitness}, best: {best_preview}",
            island.len(),
        ));
    }
    lines.join("\n")
}
