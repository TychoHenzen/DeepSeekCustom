use super::vote_tally::VoteTally;

/// The outcome of voting across candidates.
pub(super) enum VoteOutcome {
    Winner {
        text: String,
        count: usize,
        winning_indices: Vec<usize>,
    },
    NoConsensus {
        tallies: Vec<VoteTally>,
    },
}
