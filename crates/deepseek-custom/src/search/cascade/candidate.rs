//! One candidate answer from one attempt.

/// One candidate answer from one attempt.
pub(super) struct Candidate {
    /// 1-based attempt index.
    pub(super) index: usize,
    pub(super) text: String,
}
