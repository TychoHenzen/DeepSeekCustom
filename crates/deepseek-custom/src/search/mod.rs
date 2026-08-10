//! Harness-driven search runs: cascade fanout and evolutionary search.
//!
//! The point of both procedures is repeatability. A run's shape has to be
//! fixed before it starts, by a person, not chosen turn by turn by the
//! model the run is driving. So neither of these is a tool. Each one is a
//! command the GUI sends, exactly the way the Autopilot tab already sends
//! `RepeatCommand`, and the runner below drives the model rather than the
//! other way round.
//!
//! `Api`-only in the sense that matters: the run itself is this harness's
//! code. Each attempt inside it is an ordinary subagent dispatch, so it can
//! land on any backend the `backends` map names, `claude_cli` included.

pub mod cascade;
pub mod evolve;

use serde::{Deserialize, Serialize};

pub use cascade::{CascadeCounters, CascadeParams, run_cascade};
pub use evolve::{EvolveParams, run_evolve};

/// Which of the two procedures a snapshot or a command belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchKind {
    Cascade,
    Evolve,
}

impl SearchKind {
    /// The name shown in the progress line and the finished notice.
    pub fn label(self) -> &'static str {
        match self {
            SearchKind::Cascade => "Cascade",
            SearchKind::Evolve => "Evolve",
        }
    }
}

/// One row of the live standings a running search reports.
///
/// Deliberately not tied to either procedure's own candidate type: a
/// cascade row carries a vote count and an evolve row carries a fitness
/// value, and the view draws both the same way.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchEntry {
    /// What identifies this row: "Attempt 3" or "cell [2,1]".
    pub label: String,
    /// The number this row is ranked by, when it has one.
    pub score: Option<f64>,
    /// A short, single-line preview of the candidate's text.
    pub preview: String,
}

/// Everything the running search wants on screen right now.
///
/// Sent whole rather than as a stream of deltas. A search reports at most
/// once per dispatch, so rebuilding the standings each time costs nothing
/// and the view never has to reconstruct state from a partial history.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchSnapshot {
    pub kind: SearchKind,
    /// Units done and units total. Attempts for a cascade, generations for
    /// an evolve run.
    pub done: u32,
    pub total: u32,
    /// A short free-text status: "best 0.84", "no winner yet".
    pub note: String,
    /// Dispatches used against the run's own cap, when the run has one.
    pub dispatches: Option<(u32, u32)>,
    /// The current standings, best first.
    pub top: Vec<SearchEntry>,
    /// Best fitness per finished generation, oldest first. Empty for a
    /// cascade, which has only one round.
    pub history: Vec<f64>,
}

impl SearchSnapshot {
    /// A snapshot with no standings yet, for the moment a run starts.
    pub fn starting(kind: SearchKind, total: u32) -> Self {
        Self {
            kind,
            done: 0,
            total,
            note: "starting".to_string(),
            dispatches: None,
            top: Vec::new(),
            history: Vec::new(),
        }
    }
}

/// A search the GUI asked for. Carries the whole configuration, so the run
/// is fully specified before the first dispatch goes out.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchCommand {
    Cascade(Box<CascadeParams>),
    Evolve(Box<EvolveParams>),
}

/// Trim `text` to at most `max_len` characters on one line, appending "..."
/// when cut.
///
/// Counts characters, not bytes, and flattens newlines. Slicing by byte
/// index panics the moment a candidate holds a multi-byte character and the
/// cut lands inside it, and a candidate is arbitrary model output.
pub fn preview_text(text: &str, max_len: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_len {
        return flat;
    }
    let head: String = flat.chars().take(max_len).collect();
    format!("{head}...")
}
