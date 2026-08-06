//! Unit tests for `deepseek_custom::gui::autopilot_tab` (`src/gui/autopilot_tab.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use deepseek_custom::agent::repeat::RepeatCommand;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::gui::autopilot_tab::{
    AutopilotProgress, AutopilotTab, apply_autopilot_iterations, apply_autopilot_task,
    progress_label,
};

fn make_tab() -> AutopilotTab {
    AutopilotTab::new(&Settings::default(), std::path::Path::new("."))
}

#[test]
fn a_new_tab_starts_idle_with_no_channels() {
    let tab = make_tab();
    assert_eq!(tab.progress(), AutopilotProgress::Idle);
    assert!(!tab.has_repeat_channel());
    assert!(!tab.has_interrupt_flag());
}

#[test]
fn new_seeds_the_task_and_iteration_count_from_settings() {
    let mut settings = Settings::default();
    apply_autopilot_task(&mut settings, "run the suite");
    apply_autopilot_iterations(&mut settings, 12);
    let tab = AutopilotTab::new(&settings, std::path::Path::new("."));
    assert_eq!(tab.task(), "run the suite");
    assert_eq!(tab.iterations(), 12);
}

#[test]
fn new_falls_back_to_an_empty_task_and_the_default_count() {
    let tab = make_tab();
    assert!(tab.task().is_empty());
    assert_eq!(tab.iterations(), Settings::default().autopilot_iterations());
}

#[test]
fn attach_wires_both_handles() {
    let mut tab = make_tab();
    let (tx, _rx) = mpsc::unbounded_channel::<RepeatCommand>();
    tab.attach(tx, Arc::new(AtomicBool::new(false)));
    assert!(tab.has_repeat_channel());
    assert!(tab.has_interrupt_flag());
}

#[test]
fn request_stop_sets_the_shared_flag() {
    let mut tab = make_tab();
    let (tx, _rx) = mpsc::unbounded_channel::<RepeatCommand>();
    let flag = Arc::new(AtomicBool::new(false));
    tab.attach(tx, Arc::clone(&flag));
    tab.request_stop();
    assert!(flag.load(Ordering::SeqCst));
}

#[test]
fn request_stop_without_a_channel_is_harmless() {
    make_tab().request_stop();
}

#[test]
fn progress_moves_through_running_and_finished() {
    let mut tab = make_tab();
    tab.set_running(2, 5);
    assert_eq!(
        tab.progress(),
        AutopilotProgress::Running { index: 2, total: 5 }
    );
    tab.set_finished(5, 5);
    assert_eq!(
        tab.progress(),
        AutopilotProgress::Finished {
            completed: 5,
            total: 5
        }
    );
}

#[test]
fn progress_label_is_absent_while_idle() {
    assert_eq!(progress_label(AutopilotProgress::Idle), None);
}

#[test]
fn progress_label_names_the_current_iteration() {
    assert_eq!(
        progress_label(AutopilotProgress::Running { index: 3, total: 7 }).as_deref(),
        Some("Running iteration 3 of 7")
    );
}

#[test]
fn progress_label_names_the_completed_count() {
    assert_eq!(
        progress_label(AutopilotProgress::Finished {
            completed: 4,
            total: 7
        })
        .as_deref(),
        Some("Finished: 4 of 7 completed")
    );
}

#[test]
fn both_writers_round_trip_through_settings() {
    let mut settings = Settings::default();
    apply_autopilot_task(&mut settings, "keep going");
    apply_autopilot_iterations(&mut settings, 42);
    assert_eq!(settings.autopilot_task().as_deref(), Some("keep going"));
    assert_eq!(settings.autopilot_iterations(), 42);
}
