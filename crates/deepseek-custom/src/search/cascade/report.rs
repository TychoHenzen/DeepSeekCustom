//! The outcome of one cascade run.

/// What one cascade run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct CascadeReport {
    pub summary: String,
    pub is_error: bool,
    /// The winning text, when one answer won or an escalation wrote one.
    pub winner: Option<String>,
}
