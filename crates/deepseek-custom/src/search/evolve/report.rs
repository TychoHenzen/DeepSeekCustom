//! `EvolveReport` and summary formatting: the archive table and the
//! final report a finished run delivers.

use std::fmt::Write;

use super::super::preview_text;
use super::params::EvolveParams;
use crate::evolution::{Candidate, Island};

/// What one evolutionary run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct EvolveReport {
    pub summary: String,
    pub is_error: bool,
    /// The best candidate's text, when the run found one.
    pub best: Option<String>,
    /// That candidate's fitness.
    pub best_fitness: Option<f64>,
}

/// The best candidate across every island.
pub fn best_of(islands: &[Island]) -> Option<&Candidate> {
    islands
        .iter()
        .filter_map(|isle| isle.best())
        .max_by(|a, b| a.fitness.total_cmp(&b.fitness))
}

/// The one-line status: the best fitness found so far.
pub fn best_note(islands: &[Island]) -> String {
    match best_of(islands) {
        Some(c) => format!("best {:.4}", c.fitness),
        None => "no candidate yet".to_string(),
    }
}

/// Build the run's summary, including the full archive table.
pub fn build_report(
    params: &EvolveParams,
    islands: &[Island],
    failure: Option<String>,
) -> EvolveReport {
    if let Some(message) = failure {
        return EvolveReport {
            summary: message,
            is_error: true,
            best: None,
            best_fitness: None,
        };
    }
    match best_of(islands) {
        Some(c) => {
            let mut summary = String::new();
            let _ = writeln!(
                summary,
                "Evolve finished after {} generation(s) on backend \"{}\":",
                params.generations, params.backend
            );
            let _ = write!(summary, "Best fitness: {:.6}", c.fitness);
            if !c.features.is_empty() {
                let coords: Vec<String> = c.features.iter().map(|f| format!("{f:.3}")).collect();
                let _ = write!(summary, ", features=[{}]", coords.join(", "));
            }
            let _ = writeln!(summary);
            let _ = writeln!(summary, "Best solution:");
            let _ = writeln!(summary, "{}", c.text);
            let _ = writeln!(summary);
            let _ = writeln!(summary, "{}", archive_table(islands));
            EvolveReport {
                summary,
                is_error: false,
                best: Some(c.text.clone()),
                best_fitness: Some(c.fitness),
            }
        }
        None => EvolveReport {
            summary: format!(
                "Evolve: no viable candidate found after {} generation(s) on backend \"{}\".",
                params.generations, params.backend
            ),
            is_error: true,
            best: None,
            best_fitness: None,
        },
    }
}

/// The full archive table for the run's summary: one section per island,
/// listing every occupied cell and every elite, so the runners-up are
/// recorded alongside the winner.
pub fn archive_table(islands: &[Island]) -> String {
    let mut lines: Vec<String> = vec!["Archive summary:".to_string()];
    for (i, island) in islands.iter().enumerate() {
        let total = island.len();
        if total == 0 {
            lines.push(format!("  Island {i}: (empty)"));
            continue;
        }
        if !island.archive.is_empty() {
            lines.push(format!("  Island {i} ({total} total):"));
            let mut cells: Vec<(&[isize], &Candidate)> = island.archive.iter().collect();
            cells.sort_by_key(|(key_a, _)| *key_a);
            for (key, c) in &cells {
                let key_str = key
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                lines.push(format!(
                    "    cell [{key_str}]  fitness={:.6}  {}",
                    c.fitness,
                    preview_text(&c.text, 60)
                ));
            }
        }
        if !island.elites.is_empty() {
            lines.push(if island.archive.is_empty() {
                format!("  Island {i} ({total} total):")
            } else {
                format!("  Island {i} elites:")
            });
            for (rank, c) in island.elites.iter().enumerate() {
                lines.push(format!(
                    "    #{rank}  fitness={:.6}  {}",
                    c.fitness,
                    preview_text(&c.text, 60)
                ));
            }
        }
    }
    lines.join("\n")
}
