//! Evolve: evolutionary search over a population carried forward across
//! rounds. Each candidate is a mutation of a parent the archive chose,
//! scored by a fitness command and placed by an optional feature command.
//!
//! Diversity.md #7: island models and MAP-Elites, with the population, the
//! archive, and the selection rule as fixed code in `src/evolution/mod.rs`.
//! The one thing a model decides is the text of each new candidate.

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tracing::info;

use crate::agent::events::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::subagent::{SubagentRequest, run_subagent};
use crate::evolution::{Candidate, Island};

pub mod params;
pub mod report;
mod scoring;
mod types;

use super::cascade::{send_finished, send_progress};
use super::{SearchEntry, SearchKind, SearchSnapshot, preview_text};

pub use params::{EvolveParams, default_mutation_hints};
pub use report::{EvolveReport, archive_table, best_note, best_of, build_report};

use types::{CandidateOutcome, DispatchCtx, RunState};

/// The most subagent dispatches one run may make, across every generation,
/// island, and population slot. Each dispatch is a whole model turn, so
/// `generations * population * islands` multiplied out runs for hours and
/// spends real money. Reaching the cap ends the run and reports the best
/// candidate so far, rather than failing.
pub const MAX_TOTAL_DISPATCHES: u32 = 200;

/// The live standings: every archived candidate across every island, best
/// fitness first, capped so a wide archive cannot flood the view.
pub fn standings(islands: &[Island]) -> Vec<SearchEntry> {
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

/// A prompt for a seed candidate, before any parent exists.
fn build_seed_prompt(seed: &str, hint: &str) -> String {
    format!(
        "{seed}\n\nMutation hint: {hint}\n\nGenerate a solution following \
         this hint. Return only the solution, with no commentary."
    )
}

/// A prompt for a mutation, carrying the parent's current text.
fn build_mutation_prompt(seed: &str, parent_text: &str, hint: &str) -> String {
    format!(
        "{seed}\n\nCurrent best solution:\n{parent_text}\n\n\
         Mutation hint: {hint}\n\nImprove the solution above by applying \
         this mutation. Return only the improved solution, with no commentary."
    )
}

/// Run one evolutionary search to completion, reporting after every scored
/// candidate.
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

    let mut state = RunState {
        dispatches: 0,
        hint_index: 0,
        failure: None,
    };

    for generation in 0..params.generations {
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

        let ctx = DispatchCtx {
            factory,
            params: &params,
            hints: &hints,
            tx_events: &tx_events,
            registry: &registry,
            work_dir: &work_dir,
        };

        let flow =
            process_generation(&ctx, &mut islands, generation, &mut state, &mut snapshot).await;
        if flow.is_break() {
            break;
        }

        if let Some(best) = best_of(&islands) {
            snapshot.history.push(best.fitness);
        }
    }

    registry.close_all().await;
    let report = build_report(&params, &islands, state.failure);
    send_finished(
        &tx_events,
        SearchKind::Evolve,
        &report.summary,
        report.is_error,
    );
    report
}

/// Process all candidates for one generation.
/// Returns `Break` when the run should stop (cap reached or fatal error).
async fn process_generation(
    ctx: &DispatchCtx<'_>,
    islands: &mut [Island],
    generation: u32,
    state: &mut RunState,
    snapshot: &mut SearchSnapshot,
) -> ControlFlow<()> {
    for island_index in 0..islands.len() {
        for _slot in 0..ctx.params.population {
            let outcome =
                dispatch_and_score_one(ctx, &mut islands[island_index], generation, state).await;
            match outcome {
                CandidateOutcome::Scored => {}
                CandidateOutcome::DispatchFailed => continue,
                CandidateOutcome::CapReached => return ControlFlow::Break(()),
                CandidateOutcome::CmdFailed(msg) => {
                    state.failure = Some(msg);
                    return ControlFlow::Break(());
                }
            }
            snapshot.done = generation + 1;
            snapshot.dispatches = Some((state.dispatches, MAX_TOTAL_DISPATCHES));
            snapshot.top = standings(islands);
            snapshot.note = best_note(islands);
            send_progress(ctx.tx_events, snapshot);
        }
    }
    ControlFlow::Continue(())
}

/// Dispatch one subagent for a slot, score its output, and insert the
/// candidate into the archive.
async fn dispatch_and_score_one(
    ctx: &DispatchCtx<'_>,
    island: &mut Island,
    generation: u32,
    state: &mut RunState,
) -> CandidateOutcome {
    if state.dispatches >= MAX_TOTAL_DISPATCHES {
        info!(
            state.dispatches,
            "evolve: reached the dispatch cap, stopping"
        );
        return CandidateOutcome::CapReached;
    }
    state.dispatches += 1;

    let hint = &ctx.hints[state.hint_index % ctx.hints.len()];
    state.hint_index = state.hint_index.wrapping_add(1);
    let prompt = match island.select_parent(generation as usize) {
        Some(parent) => build_mutation_prompt(&ctx.params.prompt, &parent.text, hint),
        None => build_seed_prompt(&ctx.params.prompt, hint),
    };

    let request = SubagentRequest {
        backend: ctx.params.backend.clone(),
        model: None,
        prompt,
        depth: 1,
        keep_open: false,
        working_dir_override: None,
        effort: ctx.params.effort,
    };
    let outcome = match run_subagent(
        ctx.factory,
        request,
        ctx.tx_events.clone(),
        Arc::clone(ctx.registry),
    )
    .await
    {
        Ok(o) => o,
        Err(_) => return CandidateOutcome::DispatchFailed,
    };

    let (fitness, features) =
        match score_candidate_output(ctx.params, &outcome.text, ctx.work_dir).await {
            Ok(v) => v,
            Err(e) => return CandidateOutcome::CmdFailed(e),
        };
    island.insert(Candidate {
        text: outcome.text,
        fitness,
        features,
    });
    CandidateOutcome::Scored
}

/// Run the fitness and feature commands against one candidate's output.
async fn score_candidate_output(
    params: &EvolveParams,
    text: &str,
    work_dir: &Path,
) -> Result<(f64, Vec<f64>), String> {
    let fitness = scoring::run_score_cmd(&params.fitness_cmd, text, work_dir)
        .await
        .map_err(|e| format!("fitness_cmd error: {e}"))?;
    let features = match &params.feature_cmd {
        Some(cmd) => scoring::run_feature_cmd(cmd, text, work_dir)
            .await
            .map_err(|e| format!("feature_cmd error: {e}"))?,
        None => Vec::new(),
    };
    Ok((fitness, features))
}
