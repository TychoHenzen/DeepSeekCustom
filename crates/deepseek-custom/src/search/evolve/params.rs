//! `EvolveParams`: a fully specified evolutionary run.

use crate::effort::Effort;

/// A fully specified evolutionary run.
#[derive(Debug, Clone, PartialEq)]
pub struct EvolveParams {
    /// The seed task text, the starting point for evolution.
    pub prompt: String,
    /// The `backends` entry every generation runs on.
    pub backend: String,
    /// How many rounds to run.
    pub generations: u32,
    /// Candidates per round, per island.
    pub population: u32,
    /// Receives a candidate's text on stdin and prints one number: its
    /// fitness. Required, since without it nothing ranks a candidate.
    pub fitness_cmd: String,
    /// Prints a comma-separated list of numbers placing a candidate in
    /// behaviour space. Drives the MAP-Elites grid when set.
    pub feature_cmd: Option<String>,
    /// How many isolated sub-populations run side by side.
    pub islands: u32,
    /// How often island migration runs. Zero means never.
    pub migration_interval: u32,
    /// One hint round-robined into each candidate's prompt, framed as an
    /// edit against the chosen parent. Empty falls back to the defaults.
    pub mutation_hints: Vec<String>,
    /// Reasoning effort for every dispatch.
    pub effort: Effort,
}

impl Default for EvolveParams {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            backend: String::new(),
            generations: 10,
            population: 6,
            fitness_cmd: String::new(),
            feature_cmd: None,
            islands: 1,
            migration_interval: 5,
            mutation_hints: Vec::new(),
            effort: Effort::None,
        }
    }
}

/// Default mutation hints, framed as edits against the chosen parent rather
/// than fresh-start instructions.
pub fn default_mutation_hints() -> Vec<String> {
    vec![
        "Simplify the solution: remove unnecessary steps, variables, or branches.".to_string(),
        "Add handling for an edge case or error condition the current solution misses.".to_string(),
        "Optimize the most expensive part of the solution.".to_string(),
        "Re-express the solution more clearly or concisely.".to_string(),
        "Add input validation or defensive checks the current solution lacks.".to_string(),
        "Reduce duplication: combine repeated logic into a shared helper.".to_string(),
    ]
}
