/// One vote group: candidates whose trimmed text matched exactly.
pub(super) struct VoteTally {
    pub(super) text: String,
    pub(super) count: usize,
    pub(super) indices: Vec<usize>,
}
