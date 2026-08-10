//! Cascade execution: launch attempts, collect results, decide a winner.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::mpsc;
use tracing::info;

use crate::agent::events::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::subagent::{SubagentRequest, run_subagent};
use crate::search::cascade::candidate::Candidate;
use crate::search::cascade::context::RunContext;
use crate::search::cascade::counters::CascadeCounters;
use crate::search::cascade::default_diversity_hints;
use crate::search::cascade::escalate::escalate;
use crate::search::cascade::params::CascadeParams;
use crate::search::cascade::report::CascadeReport;
use crate::search::cascade::vote_outcome::VoteOutcome;
use crate::search::cascade::voting::{progress_note, standings, vote};
use crate::search::{SearchKind, SearchSnapshot};
use crate::tools::shell_stdin::run_command_with_stdin;

use super::MAX_ATTEMPTS;
use super::{send_finished, send_progress};

/// Run one cascade to completion, reporting progress as attempts land.
///
/// Sends `StreamEvent::SearchProgress` on every state change, and one
/// `StreamEvent::SearchFinished` at the end. Both go out on the main
/// session's route, so the tab's readout and the transcript both see the
/// run. The report comes back as well. That lets a test assert on the
/// outcome without draining the channel.
pub async fn run_cascade(
    factory: &Arc<BackendFactory>,
    params: CascadeParams,
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
    interrupt: Arc<AtomicBool>,
    counters: CascadeCounters,
) -> CascadeReport {
    counters.total.fetch_add(1, Ordering::SeqCst);
    let hints = if params.diversity_hints.is_empty() {
        default_diversity_hints()
    } else {
        params.diversity_hints.clone()
    };
    let n = params.n.clamp(1, MAX_ATTEMPTS);
    let registry = Arc::new(SubagentRegistry::new());
    let work_dir = factory.working_dir().lock().unwrap().clone();

    let snapshot = SearchSnapshot::starting(SearchKind::Cascade, n);
    send_progress(&tx_events, &snapshot);
    info!(n, backend = %params.backend, "cascade run started");

    let candidates = launch_attempts(factory, &tx_events, &registry, &params, &hints, n).await;
    let mut failures = Vec::new();

    // Interrupted mid-flight means zero candidates. Only report what landed.
    if interrupt.load(Ordering::SeqCst) {
        info!(
            done = candidates.len() as u32,
            "cascade: interrupted between attempts"
        );
    }
    registry.close_all().await;

    let candidates = apply_check_cmd(&params, candidates, &mut failures, &work_dir).await;

    // Report check outcomes before the final verdict, only when the check
    // command ran and may have changed the standings.
    if params.check_cmd.is_some() {
        let mut snapshot = SearchSnapshot::starting(SearchKind::Cascade, n);
        snapshot.done = n;
        snapshot.top = standings(&candidates);
        snapshot.note = progress_note(&candidates, params.vote_k);
        send_progress(&tx_events, &snapshot);
    }

    let context = RunContext {
        factory,
        tx_events: &tx_events,
        registry: &registry,
        counters: &counters,
    };
    let report = finish(&context, &params, n, candidates, failures).await;
    send_finished(
        &tx_events,
        SearchKind::Cascade,
        &report.summary,
        report.is_error,
    );
    report
}

/// Launch all attempts at once, collecting candidates as they land.
async fn launch_attempts(
    factory: &Arc<BackendFactory>,
    tx_events: &mpsc::UnboundedSender<RoutedEvent>,
    registry: &Arc<SubagentRegistry>,
    params: &CascadeParams,
    hints: &[String],
    n: u32,
) -> Vec<Candidate> {
    let mut pending = FuturesUnordered::new();
    for i in 0..n {
        let hint = &hints[i as usize % hints.len()];
        let request = SubagentRequest {
            backend: params.backend.clone(),
            model: None,
            prompt: format!("{}\n\nDiversity hint: {hint}", params.prompt),
            depth: 1,
            keep_open: false,
            working_dir_override: None,
            effort: params.effort,
        };
        let factory = Arc::clone(factory);
        let tx = tx_events.clone();
        let registry = Arc::clone(registry);
        pending.push(async move {
            let outcome = run_subagent(&factory, request, tx, registry).await;
            (i as usize + 1, outcome)
        });
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut done = 0u32;

    while let Some((index, result)) = pending.next().await {
        done += 1;
        if let Ok(outcome) = result {
            candidates.push(Candidate {
                index,
                text: outcome.text,
            });
        }
        // Report progress after each attempt lands.
        let mut snapshot = SearchSnapshot::starting(SearchKind::Cascade, n);
        snapshot.done = done;
        snapshot.top = standings(&candidates);
        snapshot.note = progress_note(&candidates, params.vote_k);
        send_progress(tx_events, &snapshot);
    }
    candidates
}

/// Run the check command against each candidate, keeping those that pass.
async fn apply_check_cmd(
    params: &CascadeParams,
    candidates: Vec<Candidate>,
    failures: &mut Vec<String>,
    work_dir: &Path,
) -> Vec<Candidate> {
    let Some(ref check_cmd) = params.check_cmd else {
        return candidates;
    };
    let mut passed: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        match run_check_cmd(check_cmd, &candidate.text, work_dir).await {
            Ok(true) => passed.push(candidate),
            Ok(false) => failures.push(format!(
                "Attempt {}: rejected by check_cmd (exited non-zero)",
                candidate.index
            )),
            Err(e) => failures.push(format!(
                "Attempt {}: check_cmd error - {e}",
                candidate.index
            )),
        }
    }
    passed
}

/// Decide the winner and build the run's summary, escalating first when no
/// candidate reached the required lead and an escalation backend was set.
async fn finish(
    context: &RunContext<'_>,
    params: &CascadeParams,
    n: u32,
    candidates: Vec<Candidate>,
    mut failures: Vec<String>,
) -> CascadeReport {
    let outcome = vote(&candidates, params.vote_k);
    let mut parts: Vec<String> = Vec::new();

    match &outcome {
        VoteOutcome::Winner {
            text,
            count,
            winning_indices,
        } => {
            let ids: Vec<String> = winning_indices.iter().map(|i| i.to_string()).collect();
            let label = if ids.len() == 1 {
                format!("Attempt {}", ids[0])
            } else {
                format!("Attempts {}", ids.join(", "))
            };
            parts.push(format!(
                "{label} won ({count} vote(s), lead by at least a {}-vote margin): {text}",
                params.vote_k
            ));
        }
        VoteOutcome::NoConsensus { tallies } => {
            if let Some(escalate_backend) = &params.escalate_backend {
                context.counters.escalated.fetch_add(1, Ordering::SeqCst);
                return escalate(context, escalate_backend, params, tallies, &failures).await;
            }
            if candidates.is_empty() && !failures.is_empty() {
                if params.check_cmd.is_some() {
                    parts.push(format!(
                        "No candidate passed check_cmd on backend \"{}\" (0 of {n} passed).",
                        params.backend
                    ));
                } else {
                    parts.push(format!(
                        "All {n} attempts failed on backend \"{}\".",
                        params.backend
                    ));
                }
            } else if !tallies.is_empty() {
                parts.push(format!(
                    "No consensus: no answer reached the required {}-vote lead margin.",
                    params.vote_k
                ));
                for t in tallies {
                    parts.push(format!("{} vote(s): {}", t.count, t.text));
                }
            }
        }
    }

    let winner = match &outcome {
        VoteOutcome::Winner { text, .. } => Some(text.clone()),
        VoteOutcome::NoConsensus { .. } => None,
    };
    let is_error = winner.is_none();
    let pass_note = params
        .check_cmd
        .as_ref()
        .map(|_| format!(" ({} of {n} passed check_cmd)", candidates.len()))
        .unwrap_or_default();

    parts.append(&mut failures);
    CascadeReport {
        summary: format!(
            "Cascade results ({n} attempt(s) on backend \"{}\"{pass_note}):\n\n{}",
            params.backend,
            parts.join("\n\n")
        ),
        is_error,
        winner,
    }
}

/// Run the check command against one candidate, with the candidate's own
/// text on stdin, and report whether it passed.
async fn run_check_cmd(
    cmd_str: &str,
    candidate_text: &str,
    work_dir: &Path,
) -> std::result::Result<bool, String> {
    run_command_with_stdin(cmd_str, candidate_text, work_dir)
        .await
        .map(|output| output.exit_code == 0)
        .map_err(|e| format!("failed to run check_cmd: {e}"))
}
