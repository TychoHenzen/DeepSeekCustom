//! The configuration of one cascade run, fully specified before the first
//! dispatch goes out.

use crate::effort::Effort;

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
