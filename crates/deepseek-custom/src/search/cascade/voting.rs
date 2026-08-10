//! Voting logic: tally candidates, pick a winner by vote margin.

use super::candidate::Candidate;
use super::vote_outcome::VoteOutcome;
use super::vote_tally::VoteTally;
use crate::search::{SearchEntry, preview_text};

/// The live standings: vote groups, biggest first.
pub(super) fn standings(candidates: &[Candidate]) -> Vec<SearchEntry> {
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
pub(super) fn progress_note(candidates: &[Candidate], vote_k: u32) -> String {
    match vote(candidates, vote_k) {
        VoteOutcome::Winner { count, .. } => {
            format!("leader has {count} vote(s)")
        }
        VoteOutcome::NoConsensus { tallies } if tallies.is_empty() => "no candidates yet".into(),
        VoteOutcome::NoConsensus { .. } => "no winner yet".into(),
    }
}

/// Group candidates by exact match on trimmed text, biggest group first,
/// ties broken by text so the order is stable.
pub(super) fn tallies_of(candidates: &[Candidate]) -> Vec<VoteTally> {
    let mut tallies: Vec<VoteTally> = Vec::new();
    for c in candidates {
        let trimmed = c.text.trim();
        if let Some(tally) = tallies.iter_mut().find(|t| t.text == trimmed) {
            tally.count += 1;
            tally.indices.push(c.index);
            continue;
        }
        tallies.push(VoteTally {
            text: trimmed.to_string(),
            count: 1,
            indices: vec![c.index],
        });
    }
    tallies.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.text.cmp(&b.text)));
    tallies
}

/// Whether the top group beats the runner-up by at least `vote_k`.
pub(super) fn vote(candidates: &[Candidate], vote_k: u32) -> VoteOutcome {
    let tallies = tallies_of(candidates);
    if tallies.is_empty() {
        return VoteOutcome::NoConsensus { tallies };
    }
    let top = &tallies[0];
    let runner_up = tallies.get(1).map(|t| t.count).unwrap_or(0);
    if top.count.saturating_sub(runner_up) >= vote_k as usize {
        return VoteOutcome::Winner {
            text: top.text.clone(),
            count: top.count,
            winning_indices: top.indices.clone(),
        };
    }
    VoteOutcome::NoConsensus { tallies }
}
