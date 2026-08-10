//! Tests for `deepseek_custom::gui::search_view` (`src/gui/search_view.rs`):
//! the progress line and the sparkline both search tabs draw.

use deepseek_custom::gui::search_view::{SearchProgress, progress_label, sparkline};
use deepseek_custom::search::{SearchEntry, SearchKind, SearchSnapshot};

fn snapshot(kind: SearchKind, done: u32, total: u32, note: &str) -> Box<SearchSnapshot> {
    Box::new(SearchSnapshot {
        kind,
        done,
        total,
        note: note.to_string(),
        dispatches: None,
        top: Vec::new(),
        history: Vec::new(),
    })
}

#[test]
fn an_idle_tab_shows_no_progress_line() {
    assert_eq!(
        progress_label(&SearchProgress::Idle, SearchKind::Cascade),
        None
    );
}

#[test]
fn a_running_cascade_counts_attempts() {
    let progress = SearchProgress::Running(snapshot(SearchKind::Cascade, 3, 5, "no winner yet"));
    assert_eq!(
        progress_label(&progress, SearchKind::Cascade).unwrap(),
        "attempt 3 of 5 - no winner yet"
    );
}

#[test]
fn a_running_evolve_counts_generations_and_names_its_dispatch_budget() {
    let mut inner = snapshot(SearchKind::Evolve, 2, 10, "best 0.8400");
    inner.dispatches = Some((54, 200));
    let progress = SearchProgress::Running(inner);
    assert_eq!(
        progress_label(&progress, SearchKind::Evolve).unwrap(),
        "generation 2 of 10 - best 0.8400   54/200 dispatches"
    );
}

#[test]
fn a_finished_run_says_whether_it_produced_anything() {
    let ok = SearchProgress::Finished {
        summary: "won".into(),
        is_error: false,
    };
    let failed = SearchProgress::Finished {
        summary: "no consensus".into(),
        is_error: true,
    };
    assert_eq!(
        progress_label(&ok, SearchKind::Cascade).unwrap(),
        "Finished"
    );
    assert_eq!(
        progress_label(&failed, SearchKind::Cascade).unwrap(),
        "Finished: no result"
    );
}

#[test]
fn only_a_running_search_counts_as_running() {
    assert!(!SearchProgress::Idle.is_running());
    assert!(SearchProgress::Running(snapshot(SearchKind::Cascade, 0, 1, "")).is_running());
    assert!(
        !SearchProgress::Finished {
            summary: String::new(),
            is_error: false,
        }
        .is_running()
    );
}

#[test]
fn a_rising_series_draws_a_rising_sparkline() {
    assert_eq!(sparkline(&[0.0, 1.0]), "\u{2581}\u{2588}");
    assert_eq!(
        sparkline(&[0.0, 0.5, 1.0]),
        "\u{2581}\u{2585}\u{2588}",
        "the midpoint should land near the middle bar"
    );
}

#[test]
fn a_flat_series_draws_the_lowest_bar_rather_than_dividing_by_zero() {
    assert_eq!(sparkline(&[3.0, 3.0, 3.0]), "\u{2581}\u{2581}\u{2581}");
    assert_eq!(sparkline(&[7.0]), "\u{2581}");
}

#[test]
fn an_empty_series_draws_nothing() {
    assert_eq!(sparkline(&[]), "");
}

#[test]
fn a_search_entry_keeps_its_score_and_preview() {
    let entry = SearchEntry {
        label: "Attempt 1".into(),
        score: Some(2.0),
        preview: "answer".into(),
    };
    assert_eq!(entry.score, Some(2.0));
    assert_eq!(entry.preview, "answer");
}
