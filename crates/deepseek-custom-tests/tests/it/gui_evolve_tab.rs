//! Tests for `deepseek_custom::gui::evolve_tab` (`src/gui/evolve_tab.rs`):
//! the form's seeding, the params it produces, and the planned-dispatch
//! reading that warns before an expensive run starts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deepseek_custom::config::settings::{EvolveSettings, Settings};
use deepseek_custom::effort::Effort;
use deepseek_custom::gui::evolve_tab::EvolveTab;
use deepseek_custom::gui::search_view::SearchProgress;
use deepseek_custom::search::evolve::MAX_TOTAL_DISPATCHES;
use deepseek_custom::search::{SearchKind, SearchSnapshot};

use tokio::sync::mpsc;

fn settings_with(evolve: EvolveSettings) -> Settings {
    Settings {
        evolve: Some(evolve),
        ..Default::default()
    }
}

fn running(done: u32) -> SearchProgress {
    SearchProgress::Running(Box::new(SearchSnapshot {
        kind: SearchKind::Evolve,
        done,
        total: 10,
        note: "best 0.5000".into(),
        dispatches: Some((6, MAX_TOTAL_DISPATCHES)),
        top: Vec::new(),
        history: Vec::new(),
    }))
}

#[test]
fn a_fresh_tab_starts_on_the_documented_defaults() {
    let tab = EvolveTab::new(&Settings::default());
    assert_eq!(tab.generations(), 10);
    assert_eq!(tab.prompt(), "");
    assert!(!tab.has_channel());
    assert!(!tab.is_running());

    let params = tab.params(Effort::None);
    assert_eq!(params.population, 6);
    assert_eq!(params.islands, 1);
    assert_eq!(params.migration_interval, 5);
}

#[test]
fn a_saved_form_seeds_the_tab() {
    let settings = settings_with(EvolveSettings {
        prompt: Some("optimize the loop".into()),
        backend: Some("ollama".into()),
        generations: Some(4),
        population: Some(3),
        fitness_cmd: Some("cargo bench".into()),
        feature_cmd: Some("wc -l".into()),
        islands: Some(2),
        migration_interval: Some(0),
        mutation_hints: Some(vec!["simplify".into()]),
    });
    let tab = EvolveTab::new(&settings);

    assert_eq!(tab.prompt(), "optimize the loop");
    assert_eq!(tab.generations(), 4);

    let params = tab.params(Effort::Max);
    assert_eq!(params.backend, "ollama");
    assert_eq!(params.population, 3);
    assert_eq!(params.fitness_cmd, "cargo bench");
    assert_eq!(params.feature_cmd.as_deref(), Some("wc -l"));
    assert_eq!(params.islands, 2);
    assert_eq!(params.migration_interval, 0, "zero means never migrate");
    assert_eq!(params.mutation_hints, vec!["simplify"]);
    assert_eq!(params.effort, Effort::Max);
}

#[test]
fn the_planned_dispatch_count_multiplies_the_three_sliders() {
    let settings = settings_with(EvolveSettings {
        generations: Some(10),
        population: Some(6),
        islands: Some(2),
        ..EvolveSettings::default()
    });
    let tab = EvolveTab::new(&settings);
    // The reading exists because three small-looking sliders multiply into
    // a run well past the cap.
    assert_eq!(tab.planned_dispatches(), 120);
    assert!(tab.planned_dispatches() < MAX_TOTAL_DISPATCHES);

    let over = EvolveTab::new(&settings_with(EvolveSettings {
        generations: Some(50),
        population: Some(20),
        islands: Some(8),
        ..EvolveSettings::default()
    }));
    assert!(over.planned_dispatches() > MAX_TOTAL_DISPATCHES);
}

#[test]
fn an_empty_feature_command_means_no_feature_command() {
    let settings = settings_with(EvolveSettings {
        fitness_cmd: Some("  echo 1  ".into()),
        feature_cmd: Some("   ".into()),
        ..EvolveSettings::default()
    });
    let params = EvolveTab::new(&settings).params(Effort::None);
    assert_eq!(params.feature_cmd, None);
    assert_eq!(params.fitness_cmd, "echo 1", "trimmed before it is run");
}

#[test]
fn attaching_wires_the_channel_and_the_stop_flag() {
    let mut tab = EvolveTab::new(&Settings::default());
    let (tx, _rx) = mpsc::unbounded_channel();
    let flag = Arc::new(AtomicBool::new(false));
    tab.attach(tx, Arc::clone(&flag));

    assert!(tab.has_channel());
    tab.request_stop();
    assert!(flag.load(Ordering::SeqCst));
}

#[test]
fn progress_moves_the_tab_in_and_out_of_its_running_state() {
    let mut tab = EvolveTab::new(&Settings::default());
    tab.set_progress(running(3));
    assert!(tab.is_running());
    assert_eq!(tab.progress(), &running(3));

    tab.set_progress(SearchProgress::Finished {
        summary: "best 0.9".into(),
        is_error: false,
    });
    assert!(!tab.is_running());
}
