//! Evolve: evolutionary search over a population carried forward across
//! rounds. Each candidate is a mutation of a parent the archive chose,
//! scored by a fitness command and placed by an optional feature command.
//!
//! Diversity.md #7: island models and MAP-Elites, with the population, the
//! archive, and the selection rule as fixed code in `src/evolution/mod.rs`.
//! The one thing a model decides is the text of each new candidate.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tracing::info;

use crate::agent::agent_loop::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::subagent::{SubagentRequest, run_subagent};
use crate::effort::Effort;
use crate::evolution::{Candidate, Island};
use crate::search::cascade::{send_finished, send_progress};
use crate::search::{SearchEntry, SearchKind, SearchSnapshot, preview_text};
use crate::tools::shell_stdin::run_command_with_stdin;

/// The most subagent dispatches one run may make, across every generation,
/// island, and population slot. Each dispatch is a whole model turn, so
/// `generations * population * islands` multiplied out runs for hours and
/// spends real money. Reaching the cap ends the run and reports the best
/// candidate so far, rather than failing.
pub const MAX_TOTAL_DISPATCHES: u32 = 200;

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

/// What one evolutionary run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct EvolveReport {
    pub summary: String,
    pub is_error: bool,
    /// The best candidate's text, when the run found one.
    pub best: Option<String>,
    /// That candidate's fitness.
    pub best_fitness: Option<f64>,
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

/// Run one evolutionary search to completion, reporting after every scored
/// candidate.
///
/// Sends `StreamEvent::SearchProgress` as the archive moves and one
/// `StreamEvent::SearchFinished` at the end. A failed dispatch drops that
/// candidate for the round rather than stopping the run. A fitness or
/// feature command that cannot be parsed does stop it: a silent zero would
/// corrupt the archive without saying so.
pub async fn run_evolve(
    factory: &Arc<BackendFactory>,
    params: EvolveParams,
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
    interrupt: Arc<AtomicBool>,
) -> EvolveReport {
    let hints = if params.mutation_hints.is_empty() {
        default_mutation_hints()
    } else {
        params.mutation_hints.clone()
    };
    let registry = Arc::new(SubagentRegistry::new());
    let work_dir = factory.working_dir().lock().unwrap().clone();
    let bucket_width = 1.0;
    let elite_k = params.population.max(1) as usize;
    let mut islands: Vec<Island> = (0..params.islands.max(1))
        .map(|_| Island::new(bucket_width, elite_k))
        .collect();

    let mut snapshot = SearchSnapshot::starting(SearchKind::Evolve, params.generations);
    snapshot.dispatches = Some((0, MAX_TOTAL_DISPATCHES));
    send_progress(&tx_events, &snapshot);
    info!(
        generations = params.generations,
        backend = %params.backend,
        "evolve run started"
    );

    let mut hint_index: usize = 0;
    let mut dispatches: u32 = 0;
    let mut failure: Option<String> = None;

    'generations: for generation in 0..params.generations {
        // Escape has to reach a run this long. A dispatch in flight owns
        // the interrupt flag itself, so this catches a press between two
        // generations, which still cuts the rest of the run short.
        if interrupt.load(Ordering::SeqCst) {
            info!(generation, "evolve: interrupted between generations");
            break;
        }

        if params.migration_interval > 0
            && generation > 0
            && generation % params.migration_interval == 0
        {
            crate::evolution::migrate(&mut islands);
        }

        for island_index in 0..islands.len() {
            for _slot in 0..params.population {
                if dispatches >= MAX_TOTAL_DISPATCHES {
                    info!(dispatches, "evolve: reached the dispatch cap, stopping");
                    break 'generations;
                }
                dispatches += 1;

                let hint = &hints[hint_index % hints.len()];
                hint_index = hint_index.wrapping_add(1);
                let prompt = match islands[island_index].select_parent(generation as usize) {
                    Some(parent) => build_mutation_prompt(&params.prompt, &parent.text, hint),
                    None => build_seed_prompt(&params.prompt, hint),
                };

                let request = SubagentRequest {
                    backend: params.backend.clone(),
                    model: None,
                    prompt,
                    depth: 1,
                    keep_open: false,
                    working_dir_override: None,
                    effort: params.effort,
                };
                let outcome =
                    match run_subagent(factory, request, tx_events.clone(), Arc::clone(&registry))
                        .await
                    {
                        Ok(o) => o,
                        // A failed dispatch loses this candidate for the round
                        // and nothing else.
                        Err(_) => continue,
                    };

                let fitness =
                    match run_score_cmd(&params.fitness_cmd, &outcome.text, &work_dir).await {
                        Ok(f) => f,
                        Err(e) => {
                            failure = Some(format!("fitness_cmd error: {e}"));
                            break 'generations;
                        }
                    };
                let features = match &params.feature_cmd {
                    Some(cmd) => match run_feature_cmd(cmd, &outcome.text, &work_dir).await {
                        Ok(f) => f,
                        Err(e) => {
                            failure = Some(format!("feature_cmd error: {e}"));
                            break 'generations;
                        }
                    },
                    None => Vec::new(),
                };

                islands[island_index].insert(Candidate {
                    text: outcome.text,
                    fitness,
                    features,
                });

                snapshot.done = generation + 1;
                snapshot.dispatches = Some((dispatches, MAX_TOTAL_DISPATCHES));
                snapshot.top = standings(&islands);
                snapshot.note = best_note(&islands);
                send_progress(&tx_events, &snapshot);
            }
        }

        // One point per finished generation, so the sparkline shows the
        // shape of the search rather than every individual candidate.
        if let Some(best) = best_of(&islands) {
            snapshot.history.push(best.fitness);
        }
    }

    registry.close_all().await;
    let report = build_report(&params, &islands, failure);
    send_finished(
        &tx_events,
        SearchKind::Evolve,
        &report.summary,
        report.is_error,
    );
    report
}

/// The best candidate across every island.
fn best_of(islands: &[Island]) -> Option<&Candidate> {
    islands
        .iter()
        .filter_map(|isle| isle.best())
        .max_by(|a, b| a.fitness.total_cmp(&b.fitness))
}

/// The one-line status: the best fitness found so far.
fn best_note(islands: &[Island]) -> String {
    match best_of(islands) {
        Some(c) => format!("best {:.4}", c.fitness),
        None => "no candidate yet".to_string(),
    }
}

/// The live standings: every archived candidate across every island, best
/// fitness first, capped so a wide archive cannot flood the view.
fn standings(islands: &[Island]) -> Vec<SearchEntry> {
    let mut rows: Vec<(f64, String, String)> = Vec::new();
    for (i, island) in islands.iter().enumerate() {
        for (key, c) in island.archive.iter() {
            let cell = key
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<_>>()
                .join(",");
            rows.push((
                c.fitness,
                format!("island {i} cell [{cell}]"),
                c.text.clone(),
            ));
        }
        for (rank, c) in island.elites.iter().enumerate() {
            rows.push((c.fitness, format!("island {i} #{rank}"), c.text.clone()));
        }
    }
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    rows.truncate(STANDINGS_LIMIT);
    rows.into_iter()
        .map(|(fitness, label, text)| SearchEntry {
            label,
            score: Some(fitness),
            preview: preview_text(&text, 60),
        })
        .collect()
}

/// How many rows the live standings carry. A MAP-Elites grid can hold
/// hundreds of cells, and the view shows a table, not a database.
const STANDINGS_LIMIT: usize = 12;

/// Build the run's summary, including the full archive table.
fn build_report(
    params: &EvolveParams,
    islands: &[Island],
    failure: Option<String>,
) -> EvolveReport {
    if let Some(message) = failure {
        return EvolveReport {
            summary: message,
            is_error: true,
            best: None,
            best_fitness: None,
        };
    }
    match best_of(islands) {
        Some(c) => {
            let features = if c.features.is_empty() {
                String::new()
            } else {
                let coords: Vec<String> = c.features.iter().map(|f| format!("{f:.3}")).collect();
                format!(", features=[{}]", coords.join(", "))
            };
            EvolveReport {
                summary: format!(
                    "Evolve finished after {} generation(s) on backend \"{}\":\n\
                     Best fitness: {:.6}{}\n\
                     Best solution:\n{}\n\n{}",
                    params.generations,
                    params.backend,
                    c.fitness,
                    features,
                    c.text,
                    archive_table(islands),
                ),
                is_error: false,
                best: Some(c.text.clone()),
                best_fitness: Some(c.fitness),
            }
        }
        None => EvolveReport {
            summary: format!(
                "Evolve: no viable candidate found after {} generation(s) on backend \"{}\".",
                params.generations, params.backend
            ),
            is_error: true,
            best: None,
            best_fitness: None,
        },
    }
}

/// A prompt for a seed candidate, before any parent exists.
fn build_seed_prompt(seed: &str, hint: &str) -> String {
    format!(
        "{seed}\n\nMutation hint: {hint}\n\nGenerate a solution following this hint. \
         Return only the solution, with no commentary."
    )
}

/// A prompt for a mutation, carrying the parent's current text.
fn build_mutation_prompt(seed: &str, parent_text: &str, hint: &str) -> String {
    format!(
        "{seed}\n\nCurrent best solution:\n{parent_text}\n\n\
         Mutation hint: {hint}\n\nImprove the solution above by applying this \
         mutation. Return only the improved solution, with no commentary."
    )
}

/// Run a scoring command with the candidate's text on stdin, and read one
/// number back off stdout.
async fn run_score_cmd(
    cmd_str: &str,
    candidate_text: &str,
    work_dir: &Path,
) -> std::result::Result<f64, String> {
    let output = run_command_with_stdin(cmd_str, candidate_text, work_dir).await?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    stdout
        .parse::<f64>()
        .map_err(|e| format!("fitness_cmd did not print a number: '{stdout}': {e}"))
}

/// Run a feature command the same way, reading comma-separated numbers back.
async fn run_feature_cmd(
    cmd_str: &str,
    candidate_text: &str,
    work_dir: &Path,
) -> std::result::Result<Vec<f64>, String> {
    let output = run_command_with_stdin(cmd_str, candidate_text, work_dir).await?;
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

/// The full archive table for the run's summary: one section per island,
/// listing every occupied cell and every elite, so the runners-up are
/// recorded alongside the winner.
fn archive_table(islands: &[Island]) -> String {
    let mut lines: Vec<String> = vec!["Archive summary:".to_string()];
    for (i, island) in islands.iter().enumerate() {
        let total = island.len();
        if total == 0 {
            lines.push(format!("  Island {i}: (empty)"));
            continue;
        }
        if !island.archive.is_empty() {
            lines.push(format!("  Island {i} ({total} total):"));
            let mut cells: Vec<(&[isize], &Candidate)> = island.archive.iter().collect();
            cells.sort_by(|(key_a, _), (key_b, _)| key_a.cmp(key_b));
            for (key, c) in &cells {
                let key_str = key
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                lines.push(format!(
                    "    cell [{key_str}]  fitness={:.6}  {}",
                    c.fitness,
                    preview_text(&c.text, 60)
                ));
            }
        }
        if !island.elites.is_empty() {
            lines.push(if island.archive.is_empty() {
                format!("  Island {i} ({total} total):")
            } else {
                format!("  Island {i} elites:")
            });
            for (rank, c) in island.elites.iter().enumerate() {
                lines.push(format!(
                    "    #{rank}  fitness={:.6}  {}",
                    c.fitness,
                    preview_text(&c.text, 60)
                ));
            }
        }
    }
    lines.join("\n")
}
