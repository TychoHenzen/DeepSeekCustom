//! Cascade: run `n` attempts at one prompt, each with a different diversity
//! hint, drop the ones a check command rejects, and pick a winner by vote.
//! When no answer reaches the required lead, escalate to a stronger backend.
//!
//! Diversity.md's best-evidenced idea: cheap-model fanout with a check
//! command or a strong model picking the winner, rather than trusting one
//! answer from one run. Every attempt is an ordinary `keep_open: false`
//! dispatch through `run_subagent`, so each one gets its own `Subagent`
//! block in the transcript.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::mpsc;
use tracing::info;

use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::subagent::{SubagentRequest, run_subagent};
use crate::effort::Effort;
use crate::search::{SearchEntry, SearchKind, SearchSnapshot, preview_text};
use crate::tools::shell_stdin::run_command_with_stdin;

/// The most attempts one cascade may run. Each attempt is a whole model
/// turn. More than this against one backend floods it for little
/// diversity gain.
pub const MAX_ATTEMPTS: u32 = 16;

/// A fully specified cascade run. Every field is decided before the first
/// dispatch: nothing here is chosen by a model mid-run.
#[derive(Debug, Clone, PartialEq)]
pub struct CascadeParams {
    /// The shared task text, sent to every attempt.
    pub prompt: String,
    /// The `backends` entry every attempt runs on. Meant to be the cheap one.
    pub backend: String,
    /// How many attempts to run, clamped to `MAX_ATTEMPTS`.
    pub n: u32,
    /// The lead the top answer needs over the runner-up to win outright.
    pub vote_k: u32,
    /// Run once per candidate, with the candidate's text on stdin. A
    /// candidate whose command exits non-zero is dropped before the vote.
    pub check_cmd: Option<String>,
    /// One hint appended per attempt, repeating in order once the list runs
    /// out. Empty falls back to `default_diversity_hints`.
    pub diversity_hints: Vec<String>,
    /// A stronger backend, called when no candidate reaches `vote_k`.
    pub escalate_backend: Option<String>,
    /// Reasoning effort for every attempt.
    pub effort: Effort,
}

impl Default for CascadeParams {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            backend: String::new(),
            n: 5,
            vote_k: 1,
            check_cmd: None,
            diversity_hints: Vec::new(),
            escalate_backend: None,
            effort: Effort::None,
        }
    }
}

/// The two running totals the status bar's escalation rate is built from.
///
/// They live for the whole process, not for one run, so the readout says
/// how often cascades have needed a stronger backend across the session.
#[derive(Clone)]
pub struct CascadeCounters {
    /// Bumped once per run, resolved or not.
    pub total: Arc<AtomicUsize>,
    /// Bumped when a run had to escalate.
    pub escalated: Arc<AtomicUsize>,
}

impl CascadeCounters {
    /// A pair starting at zero, for a caller with no counters to share.
    pub fn new() -> Self {
        Self {
            total: Arc::new(AtomicUsize::new(0)),
            escalated: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Default for CascadeCounters {
    fn default() -> Self {
        Self::new()
    }
}

/// What one cascade run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct CascadeReport {
    pub summary: String,
    pub is_error: bool,
    /// The winning text, when one answer won or an escalation wrote one.
    pub winner: Option<String>,
}

/// The default diversity hints, used when the run configures none.
pub fn default_diversity_hints() -> Vec<String> {
    vec![
        "Use a different approach or library than the obvious first choice.".to_string(),
        "Favor simplicity over speed.".to_string(),
        "Handle edge cases and error paths first.".to_string(),
        "Write the plain, direct version.".to_string(),
    ]
}

/// The handles every stage of one run shares. Where to build backends,
/// where events go, which registry holds the dispatches, and the counters
/// the status bar reads.
///
/// Bundled rather than passed loose, because they always travel together.
/// Two of the four are `Arc`s of the same shape. A caller could swap those
/// two without the compiler noticing.
struct RunContext<'a> {
    factory: &'a Arc<BackendFactory>,
    tx_events: &'a mpsc::UnboundedSender<RoutedEvent>,
    registry: &'a Arc<SubagentRegistry>,
    counters: &'a CascadeCounters,
}

/// One candidate answer from one attempt.
struct Candidate {
    /// 1-based attempt index.
    index: usize,
    text: String,
}

/// One vote group: candidates whose trimmed text matched exactly.
struct VoteTally {
    text: String,
    count: usize,
    indices: Vec<usize>,
}

/// The outcome of voting across candidates.
enum VoteOutcome {
    Winner {
        text: String,
        count: usize,
        winning_indices: Vec<usize>,
    },
    NoConsensus {
        tallies: Vec<VoteTally>,
    },
}

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

    let mut snapshot = SearchSnapshot::starting(SearchKind::Cascade, n);
    send_progress(&tx_events, &snapshot);
    info!(n, backend = %params.backend, "cascade run started");

    // Every attempt goes out at once. Results are folded in as they land
    // rather than at the end, so the standings move while the run is still
    // going.
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
        let registry = Arc::clone(&registry);
        pending.push(async move {
            let outcome = run_subagent(&factory, request, tx, registry).await;
            (i as usize + 1, outcome)
        });
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut done = 0u32;

    while let Some((index, result)) = pending.next().await {
        done += 1;
        match result {
            Ok(outcome) => candidates.push(Candidate {
                index,
                text: outcome.text,
            }),
            Err(e) => failures.push(format!("Attempt {index}: FAILED - {e}")),
        }
        snapshot.done = done;
        snapshot.top = standings(&candidates);
        snapshot.note = progress_note(&candidates, params.vote_k);
        send_progress(&tx_events, &snapshot);

        // A press between two attempts stops the rest. The attempts already
        // in flight own the interrupt flag themselves, so this cannot cut
        // one off mid-turn, which is the same guarantee autopilot gives.
        if interrupt.load(Ordering::SeqCst) {
            info!(done, "cascade: interrupted between attempts");
            break;
        }
    }
    drop(pending);
    registry.close_all().await;

    if let Some(ref check_cmd) = params.check_cmd {
        let mut passed: Vec<Candidate> = Vec::new();
        for candidate in candidates {
            match run_check_cmd(check_cmd, &candidate.text, &work_dir).await {
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
        candidates = passed;
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

/// One more dispatch onto a stronger backend, carrying the original task,
/// every candidate, and why each one was cut.
async fn escalate(
    context: &RunContext<'_>,
    escalate_backend: &str,
    params: &CascadeParams,
    tallies: &[VoteTally],
    failures: &[String],
) -> CascadeReport {
    let mut parts: Vec<String> = vec![format!("Original task:\n{}", params.prompt)];
    if !tallies.is_empty() {
        parts.push(
            "\nCandidates that survived but failed to reach the required vote margin:".to_string(),
        );
        for t in tallies {
            parts.push(format!("- {} vote(s): {}", t.count, t.text));
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
         Return only the final answer, with no commentary."
            .to_string(),
    );

    let request = SubagentRequest {
        backend: escalate_backend.to_string(),
        model: None,
        prompt: parts.join("\n"),
        depth: 1,
        keep_open: false,
        working_dir_override: None,
        effort: params.effort,
    };

    match run_subagent(
        context.factory,
        request,
        context.tx_events.clone(),
        Arc::clone(context.registry),
    )
    .await
    {
        Ok(outcome) => CascadeReport {
            summary: format!(
                "Cascade escalated to backend \"{escalate_backend}\":\n\n[escalated] {}",
                outcome.text
            ),
            is_error: false,
            winner: Some(outcome.text),
        },
        Err(e) => CascadeReport {
            summary: format!("Cascade escalation to backend \"{escalate_backend}\" failed: {e}"),
            is_error: true,
            winner: None,
        },
    }
}

/// Run the check command against one candidate, with the candidate's own
/// text on stdin, and report whether it passed.
///
/// The text has to reach the command somehow. Without it the same command
/// line runs once per attempt against the same directory. It then passes
/// every candidate or fails every candidate, and the filter does nothing.
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

/// The live standings: vote groups, biggest first.
fn standings(candidates: &[Candidate]) -> Vec<SearchEntry> {
    tallies_of(candidates)
        .into_iter()
        .map(|t| {
            let ids: Vec<String> = t.indices.iter().map(|i| i.to_string()).collect();
            SearchEntry {
                label: format!("Attempt {}", ids.join(", ")),
                score: Some(t.count as f64),
                preview: preview_text(&t.text, 60),
            }
        })
        .collect()
}

/// The one-line status: whether anything currently leads by enough.
fn progress_note(candidates: &[Candidate], vote_k: u32) -> String {
    match vote(candidates, vote_k) {
        VoteOutcome::Winner { count, .. } => format!("leader has {count} vote(s)"),
        VoteOutcome::NoConsensus { tallies } if tallies.is_empty() => "no candidates yet".into(),
        VoteOutcome::NoConsensus { .. } => "no winner yet".into(),
    }
}

/// Group candidates by exact match on trimmed text, biggest group first,
/// ties broken by text so the order is stable.
fn tallies_of(candidates: &[Candidate]) -> Vec<VoteTally> {
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
    tallies.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.text.cmp(&b.text)));
    tallies
}

/// Whether the top group beats the runner-up by at least `vote_k`.
fn vote(candidates: &[Candidate], vote_k: u32) -> VoteOutcome {
    let tallies = tallies_of(candidates);
    if tallies.is_empty() {
        return VoteOutcome::NoConsensus { tallies };
    }
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

/// Send one progress snapshot on the main session's route.
pub(crate) fn send_progress(tx: &mpsc::UnboundedSender<RoutedEvent>, snapshot: &SearchSnapshot) {
    let _ = tx.send(RoutedEvent::own(StreamEvent::SearchProgress(Box::new(
        snapshot.clone(),
    ))));
}

/// Send the run's terminal event.
pub(crate) fn send_finished(
    tx: &mpsc::UnboundedSender<RoutedEvent>,
    kind: SearchKind,
    summary: &str,
    is_error: bool,
) {
    let _ = tx.send(RoutedEvent::own(StreamEvent::SearchFinished {
        kind,
        summary: summary.to_string(),
        is_error,
    }));
}
