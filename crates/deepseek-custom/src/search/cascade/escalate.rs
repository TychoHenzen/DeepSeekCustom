//! Escalation: one more dispatch onto a stronger backend when no candidate
//! reached the required vote margin.

use std::sync::Arc;

use crate::backend::subagent::{SubagentRequest, run_subagent};
use crate::search::cascade::context::RunContext;
use crate::search::cascade::params::CascadeParams;
use crate::search::cascade::report::CascadeReport;
use crate::search::cascade::vote_tally::VoteTally;

/// Build the escalation prompt that carries the original task, every candidate
/// that survived, and why each one was cut.
fn build_escalation_prompt(task: &str, tallies: &[VoteTally], failures: &[String]) -> String {
    let mut parts: Vec<String> = vec![format!("Original task:\n{task}")];

    if !tallies.is_empty() {
        parts.push(
            "\nCandidates that survived but failed to \
             reach the required vote margin:"
                .to_string(),
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
        "\nPick the best answer above, or write your own if none of them \
         is right. Return only the final answer, with no commentary."
            .to_string(),
    );

    parts.join("\n")
}

/// One more dispatch onto a stronger backend, carrying the original task,
/// every candidate, and why each one was cut.
pub(super) async fn escalate(
    context: &RunContext<'_>,
    escalate_backend: &str,
    params: &CascadeParams,
    tallies: &[VoteTally],
    failures: &[String],
) -> CascadeReport {
    let prompt = build_escalation_prompt(&params.prompt, tallies, failures);

    let request = SubagentRequest {
        backend: escalate_backend.to_string(),
        model: None,
        prompt,
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
                "Cascade escalated to backend \"{escalate_backend}\":\
                 \n\n[escalated] {}",
                outcome.text
            ),
            is_error: false,
            winner: Some(outcome.text),
        },
        Err(e) => CascadeReport {
            summary: format!(
                "Cascade escalation to backend \"{escalate_backend}\" \
                 failed: {e}"
            ),
            is_error: true,
            winner: None,
        },
    }
}
