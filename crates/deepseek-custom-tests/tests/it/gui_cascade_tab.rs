//! Tests for `deepseek_custom::gui::cascade_tab` (`src/gui/cascade_tab.rs`):
//! the form's seeding from settings, the params it produces, and the
//! helpers its two text boxes share with the Evolve tab.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deepseek_custom::config::settings::{CascadeSettings, Settings};
use deepseek_custom::effort::Effort;
use deepseek_custom::gui::cascade_tab::{CascadeTab, non_empty, parse_hints};
use deepseek_custom::gui::search_view::SearchProgress;
use deepseek_custom::search::{SearchKind, SearchSnapshot};

use tokio::sync::mpsc;

fn settings_with(cascade: CascadeSettings) -> Settings {
    Settings {
        cascade: Some(cascade),
        ..Default::default()
    }
}

fn running(done: u32) -> SearchProgress {
    SearchProgress::Running(Box::new(SearchSnapshot {
        kind: SearchKind::Cascade,
        done,
        total: 5,
        note: "no winner yet".into(),
        dispatches: None,
        top: Vec::new(),
        history: Vec::new(),
    }))
}

#[test]
fn a_fresh_tab_starts_on_the_documented_defaults() {
    let tab = CascadeTab::new(&Settings::default());
    assert_eq!(tab.n(), 5);
    assert_eq!(tab.prompt(), "");
    assert!(!tab.has_channel(), "no channel until attach");
    assert!(!tab.is_running());
}

#[test]
fn a_saved_form_seeds_the_tab() {
    let settings = settings_with(CascadeSettings {
        prompt: Some("find the bug".into()),
        backend: Some("ollama".into()),
        n: Some(9),
        vote_k: Some(3),
        check_cmd: Some("cargo test".into()),
        diversity_hints: Some(vec!["one".into(), "two".into()]),
        escalate_backend: Some("claude".into()),
    });
    let tab = CascadeTab::new(&settings);

    assert_eq!(tab.prompt(), "find the bug");
    assert_eq!(tab.n(), 9);

    let params = tab.params(Effort::High);
    assert_eq!(params.backend, "ollama");
    assert_eq!(params.vote_k, 3);
    assert_eq!(params.check_cmd.as_deref(), Some("cargo test"));
    assert_eq!(params.diversity_hints, vec!["one", "two"]);
    assert_eq!(params.escalate_backend.as_deref(), Some("claude"));
    // Effort comes from the session, not from the saved form: the sidebar
    // control is the one place that level is set.
    assert_eq!(params.effort, Effort::High);
}

#[test]
fn an_empty_check_command_means_no_check_command() {
    let settings = settings_with(CascadeSettings {
        prompt: Some("q".into()),
        backend: Some("ollama".into()),
        check_cmd: Some("   ".into()),
        ..CascadeSettings::default()
    });
    let params = CascadeTab::new(&settings).params(Effort::None);
    assert_eq!(params.check_cmd, None);
    assert_eq!(params.escalate_backend, None);
}

#[test]
fn attaching_wires_the_channel_and_the_stop_flag() {
    let mut tab = CascadeTab::new(&Settings::default());
    let (tx, _rx) = mpsc::unbounded_channel();
    let flag = Arc::new(AtomicBool::new(false));
    tab.attach(tx, Arc::clone(&flag));

    assert!(tab.has_channel());
    tab.request_stop();
    assert!(flag.load(Ordering::SeqCst), "stop should set the flag");
}

#[test]
fn stopping_an_unattached_tab_does_nothing_rather_than_panicking() {
    // Escape reaches every tab, including one a GUI built without the
    // search channel, so this must be a no-op rather than an unwrap.
    CascadeTab::new(&Settings::default()).request_stop();
}

#[test]
fn progress_moves_the_tab_in_and_out_of_its_running_state() {
    let mut tab = CascadeTab::new(&Settings::default());
    assert!(!tab.is_running());

    tab.set_progress(running(2));
    assert!(tab.is_running());
    assert_eq!(tab.progress(), &running(2));

    tab.set_progress(SearchProgress::Finished {
        summary: "won".into(),
        is_error: false,
    });
    assert!(!tab.is_running());
}

#[test]
fn hints_are_one_per_line_and_blank_lines_are_dropped() {
    assert_eq!(parse_hints("one\n\n  two  \n"), vec!["one", "two"]);
    assert!(parse_hints("   \n\n").is_empty());
}

#[test]
fn a_blank_box_reads_as_unset() {
    assert_eq!(non_empty("  "), None);
    assert_eq!(non_empty(" claude "), Some("claude".to_string()));
}
