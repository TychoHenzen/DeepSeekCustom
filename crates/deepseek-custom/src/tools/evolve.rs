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

use std::path::PathBuf;
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::agent::agent_loop::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::effort::Effort;
use crate::error::Result;
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
        let _parsed: EvolveInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Invalid Evolve input: {e}"),
                    is_error: true,
                    image: None,
                });
            }
        };

        // Placeholder: E7 implements the real generation loop.
        Ok(ToolOutput {
            content: "Evolve: not yet implemented (see E7)".to_string(),
            is_error: false,
            image: None,
        })
    }
}
