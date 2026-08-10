//! The live view both search tabs draw: a progress line, the standings
//! table, and a fitness sparkline.
//!
//! Shared because a cascade and an evolutionary run report the same shape,
//! a ranked list of candidates with a number beside each, even though one
//! number is a vote count and the other a fitness value. The tabs differ in
//! their forms, not in how a run looks while it is going.

use eframe::egui::{self, Color32, RichText};

use crate::search::{SearchKind, SearchSnapshot};

/// A search tab's run state, driven by the two search events.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum SearchProgress {
    #[default]
    Idle,
    Running(Box<SearchSnapshot>),
    Finished {
        summary: String,
        is_error: bool,
    },
}

impl SearchProgress {
    /// Whether a run is in flight, so the tab can disable Run and enable
    /// Stop.
    pub fn is_running(&self) -> bool {
        matches!(self, SearchProgress::Running(_))
    }
}

/// The one-line status beside the Run button, or `None` before anything has
/// run.
pub fn progress_label(progress: &SearchProgress, kind: SearchKind) -> Option<String> {
    match progress {
        SearchProgress::Idle => None,
        SearchProgress::Running(snapshot) => {
            let unit = match kind {
                SearchKind::Cascade => "attempt",
                SearchKind::Evolve => "generation",
            };
            let mut line = format!(
                "{} {} of {} - {}",
                unit, snapshot.done, snapshot.total, snapshot.note
            );
            if let Some((used, cap)) = snapshot.dispatches {
                line.push_str(&format!("   {used}/{cap} dispatches"));
            }
            Some(line)
        }
        SearchProgress::Finished { is_error, .. } => Some(
            if *is_error {
                "Finished: no result"
            } else {
                "Finished"
            }
            .to_string(),
        ),
    }
}

/// Draw the standings table and the sparkline for a running search, or the
/// summary for a finished one. Draws nothing at all while idle.
pub fn render_progress(ui: &mut egui::Ui, progress: &SearchProgress) {
    match progress {
        SearchProgress::Idle => {}
        SearchProgress::Running(snapshot) => render_snapshot(ui, snapshot),
        SearchProgress::Finished { summary, is_error } => {
            let color = if *is_error {
                Color32::from_rgb(255, 180, 120)
            } else {
                Color32::from_rgb(180, 255, 180)
            };
            ui.label(RichText::new(summary).color(color).monospace().small());
        }
    }
}

/// The standings table plus the fitness sparkline.
fn render_snapshot(ui: &mut egui::Ui, snapshot: &SearchSnapshot) {
    if snapshot.top.is_empty() {
        ui.label(
            RichText::new("  no candidates yet")
                .color(Color32::GRAY)
                .small(),
        );
    } else {
        let score_header = match snapshot.kind {
            SearchKind::Cascade => "votes",
            SearchKind::Evolve => "fitness",
        };
        egui::Grid::new("search_standings")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                ui.label(RichText::new("rank").color(Color32::GRAY).small());
                ui.label(RichText::new(score_header).color(Color32::GRAY).small());
                ui.label(RichText::new("candidate").color(Color32::GRAY).small());
                ui.end_row();
                for (rank, entry) in snapshot.top.iter().enumerate() {
                    ui.label(format!("{}", rank + 1));
                    ui.label(match entry.score {
                        Some(score) => format!("{score:.4}"),
                        None => "-".to_string(),
                    });
                    ui.label(
                        RichText::new(format!("{}  {}", entry.label, entry.preview)).monospace(),
                    );
                    ui.end_row();
                }
            });
    }

    if snapshot.history.len() > 1 {
        ui.label(
            RichText::new(format!(
                "  {}  best per generation",
                sparkline(&snapshot.history)
            ))
            .color(Color32::GRAY)
            .monospace(),
        );
    }
}

/// The eight block characters a sparkline is drawn from, lowest first.
const SPARK_BARS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

/// Render `values` as a one-line bar chart, scaled between its own minimum
/// and maximum.
///
/// A flat series (every value equal, including a single point) draws as the
/// lowest bar throughout rather than dividing by a zero range.
pub fn sparkline(values: &[f64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    values
        .iter()
        .map(|v| {
            let scaled = if range <= 0.0 {
                0.0
            } else {
                (v - min) / range * (SPARK_BARS.len() - 1) as f64
            };
            SPARK_BARS[scaled.round().clamp(0.0, 7.0) as usize]
        })
        .collect()
}
