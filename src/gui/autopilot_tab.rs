//! The Autopilot tab: the task to repeat, how many times, the resolved
//! policy file path, the Run button, and the progress readout.
//!
//! This owns the six fields `DeepSeekGui` used to hold for repeat runs,
//! including the two channels `with_repeat` attaches. Both are `None`
//! until then, and the Run button stays disabled while they are, so a
//! GUI built without a repeat channel simply cannot start a run.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, RichText, TextEdit};
use tokio::sync::mpsc;
use tracing::info;

use crate::agent::repeat::RepeatCommand;
use crate::config::settings::Settings;

/// Progress readout for the Autopilot tab. Fed from
/// `StreamEvent::RepeatIterationStart` and `StreamEvent::RepeatFinished`.
/// `Idle` before any run has started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AutopilotProgress {
    #[default]
    Idle,
    Running {
        index: u32,
        total: u32,
    },
    Finished {
        completed: u32,
        total: u32,
    },
}

/// The Autopilot tab's controls and the channels a run needs.
pub(crate) struct AutopilotTab {
    /// Sends a repeat command to the agent task. `None` until
    /// `DeepSeekGui::with_repeat` is called.
    repeat_tx: Option<mpsc::UnboundedSender<RepeatCommand>>,
    /// Shared with `AgentLoop::repeat_interrupt_flag`. Escape sets this to
    /// stop a running repeat early.
    interrupt_flag: Option<Arc<AtomicBool>>,
    /// Task text box, seeded from `settings.autopilot_task()`.
    task: String,
    /// Iteration count control, seeded from
    /// `settings.autopilot_iterations()`.
    iterations: u32,
    /// Progress readout state.
    progress: AutopilotProgress,
    /// Resolved policy file path, shown read-only next to the Run button.
    /// Computed once at construction.
    policy_path: PathBuf,
}

impl AutopilotTab {
    /// Seed the controls from `settings`, with no channels attached yet.
    pub(crate) fn new(settings: &Settings, project_root: &std::path::Path) -> Self {
        let policy_path = crate::autopilot::policy::PolicyStore::new(
            project_root.to_path_buf(),
            settings.autopilot_policy_path(),
        )
        .resolved_policy_path();

        Self {
            repeat_tx: None,
            interrupt_flag: None,
            task: settings.autopilot_task().unwrap_or_default(),
            iterations: settings.autopilot_iterations(),
            progress: AutopilotProgress::default(),
            policy_path,
        }
    }

    /// Attach the repeat channel and the flag that stops a running repeat.
    pub(crate) fn attach(
        &mut self,
        repeat_tx: mpsc::UnboundedSender<RepeatCommand>,
        interrupt_flag: Arc<AtomicBool>,
    ) {
        self.repeat_tx = Some(repeat_tx);
        self.interrupt_flag = Some(interrupt_flag);
    }

    /// Stop a running repeat. Escape calls this alongside interrupting the
    /// turn in flight, since one Escape stops whatever the session is
    /// doing. Does nothing when no repeat channel is attached.
    pub(crate) fn request_stop(&self) {
        if let Some(flag) = &self.interrupt_flag {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// The progress readout's current state, so a test can check the
    /// stream events reach it.
    #[cfg(test)]
    pub(crate) fn progress(&self) -> AutopilotProgress {
        self.progress
    }

    /// Move the progress readout to a newly started iteration.
    pub(crate) fn set_running(&mut self, index: u32, total: u32) {
        self.progress = AutopilotProgress::Running { index, total };
    }

    /// Move the progress readout to a finished run.
    pub(crate) fn set_finished(&mut self, completed: u32, total: u32) {
        self.progress = AutopilotProgress::Finished { completed, total };
    }

    /// The whole tab. Returns true when a control changed something the
    /// settings file holds, so the caller saves it once per frame.
    pub(crate) fn render(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> bool {
        let mut dirty = false;
        ui.heading("Autopilot");
        ui.separator();

        ui.label("Task");
        let task_response = ui.add(
            TextEdit::multiline(&mut self.task)
                .desired_rows(6)
                .hint_text("Describe the task to repeat"),
        );
        // Save on focus loss, not on every keystroke, matching the wake
        // phrase field in the settings panel.
        if task_response.lost_focus() {
            apply_autopilot_task(settings, &self.task.clone());
            dirty = true;
        }

        ui.add_space(8.0);

        let iter_response =
            ui.add(egui::Slider::new(&mut self.iterations, 1..=100).text("Iterations"));
        // Save when the drag ends, matching the other sliders.
        if iter_response.drag_stopped() {
            apply_autopilot_iterations(settings, self.iterations);
            dirty = true;
        }

        ui.add_space(8.0);
        ui.label(
            RichText::new(format!("Policy file: {}", self.policy_path.display()))
                .color(Color32::GRAY)
                .small(),
        );
        ui.label(
            RichText::new(
                "Questions during a run are answered from that file by a separate model. \
                 A human never answers them.",
            )
            .color(Color32::GRAY)
            .small(),
        );

        ui.add_space(8.0);
        self.render_run_button(ui);

        ui.add_space(8.0);
        if let Some(text) = progress_label(self.progress) {
            ui.label(text);
        }

        dirty
    }

    /// The Run button, disabled while there is nothing to run or no channel
    /// to run it on.
    fn render_run_button(&mut self, ui: &mut egui::Ui) {
        let can_run = !self.task.trim().is_empty() && self.repeat_tx.is_some();
        if !ui.add_enabled(can_run, egui::Button::new("Run")).clicked() {
            return;
        }
        let Some(tx) = &self.repeat_tx else {
            return;
        };
        info!(iterations = self.iterations, "autopilot run requested");
        let _ = tx.send(RepeatCommand {
            task: self.task.clone(),
            iterations: self.iterations,
        });
        self.progress = AutopilotProgress::Idle;
    }
}

/// The progress line, or `None` while nothing has run yet.
fn progress_label(progress: AutopilotProgress) -> Option<String> {
    match progress {
        AutopilotProgress::Idle => None,
        AutopilotProgress::Running { index, total } => {
            Some(format!("Running iteration {index} of {total}"))
        }
        AutopilotProgress::Finished { completed, total } => {
            Some(format!("Finished: {completed} of {total} completed"))
        }
    }
}

/// Store the Autopilot tab's task text box.
pub(super) fn apply_autopilot_task(settings: &mut Settings, task: &str) {
    settings.autopilot_mut().task = Some(task.to_string());
}

/// Store the Autopilot tab's iteration count.
pub(super) fn apply_autopilot_iterations(settings: &mut Settings, iterations: u32) {
    settings.autopilot_mut().iterations = Some(iterations);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tab() -> AutopilotTab {
        AutopilotTab::new(&Settings::default(), std::path::Path::new("."))
    }

    #[test]
    fn a_new_tab_starts_idle_with_no_channels() {
        let tab = make_tab();
        assert_eq!(tab.progress, AutopilotProgress::Idle);
        assert!(tab.repeat_tx.is_none());
        assert!(tab.interrupt_flag.is_none());
    }

    #[test]
    fn new_seeds_the_task_and_iteration_count_from_settings() {
        let mut settings = Settings::default();
        apply_autopilot_task(&mut settings, "run the suite");
        apply_autopilot_iterations(&mut settings, 12);
        let tab = AutopilotTab::new(&settings, std::path::Path::new("."));
        assert_eq!(tab.task, "run the suite");
        assert_eq!(tab.iterations, 12);
    }

    #[test]
    fn new_falls_back_to_an_empty_task_and_the_default_count() {
        let tab = make_tab();
        assert!(tab.task.is_empty());
        assert_eq!(tab.iterations, Settings::default().autopilot_iterations());
    }

    #[test]
    fn attach_wires_both_handles() {
        let mut tab = make_tab();
        let (tx, _rx) = mpsc::unbounded_channel::<RepeatCommand>();
        tab.attach(tx, Arc::new(AtomicBool::new(false)));
        assert!(tab.repeat_tx.is_some());
        assert!(tab.interrupt_flag.is_some());
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
            tab.progress,
            AutopilotProgress::Running { index: 2, total: 5 }
        );
        tab.set_finished(5, 5);
        assert_eq!(
            tab.progress,
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
}
