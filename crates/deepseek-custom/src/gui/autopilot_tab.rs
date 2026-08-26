//! The Autopilot tab: the task to repeat, how many times, the resolved
//! policy file path, the Run button, and the progress readout.
//!
//! This owns the six fields `DeepSeekGui` used to hold for repeat runs,
//! including the two channels `with_repeat` attaches. Both are `None`
//! until then, and the Run button stays disabled while they are, so a
//! GUI built without a repeat channel simply cannot start a run.
//!
//! The GUI draws the live transcript below these controls, so `render`
//! folds the task box, the iterations slider, and the two captions into
//! a collapsible "Setup" header, keeping the Run button and the progress
//! label visible on their own row whether that header is open or closed.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, RichText, TextEdit};
use tokio::sync::mpsc;
use tracing::info;

use crate::agent::repeat::RepeatCommand;
use crate::autopilot::policy::PolicyStore;
use crate::config::settings::Settings;

/// Progress readout for the Autopilot tab. Fed from
/// `StreamEvent::RepeatIterationStart` and `StreamEvent::RepeatFinished`.
/// `Idle` before any run has started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutopilotProgress {
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
pub struct AutopilotTab {
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
    /// Set by `set_running` when a run starts from a non-`Running` state.
    /// `render` reads this once, forces the Setup header closed, and clears
    /// it, so the fold closes exactly once per run rather than fighting a
    /// user who reopens it mid-run.
    close_setup_fold: bool,
}

impl AutopilotTab {
    /// Seed the controls from `settings`, with no channels attached yet.
    pub fn new(settings: &Settings, project_root: &std::path::Path) -> Self {
        let policy_path =
            PolicyStore::new(project_root.to_path_buf(), settings.autopilot_policy_path())
                .resolved_policy_path();

        Self {
            repeat_tx: None,
            interrupt_flag: None,
            task: settings.autopilot_task().unwrap_or_default(),
            iterations: settings.autopilot_iterations(),
            progress: AutopilotProgress::default(),
            policy_path,
            close_setup_fold: false,
        }
    }

    /// Attach the repeat channel and the flag that stops a running repeat.
    pub fn attach(
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
    pub fn request_stop(&self) {
        if let Some(flag) = &self.interrupt_flag {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Whether an autopilot run is between its first iteration and its
    /// last. The event pump asks, because one iteration's `TurnEnd` does
    /// not end a run: a session switch held during a run has to wait for
    /// `RepeatFinished` instead. See `DeepSeekGui::event_ends_turn`.
    pub fn is_running(&self) -> bool {
        matches!(self.progress, AutopilotProgress::Running { .. })
    }

    /// The progress readout's current state. `render` only draws
    /// `self.progress` into a label, it never hands the value back, so
    /// there is no other way to read it back out. Test-only.
    #[cfg(feature = "test-support")]
    pub fn progress(&self) -> AutopilotProgress {
        self.progress
    }

    /// The task text box's raw contents. `render` is the only thing that
    /// reads `self.task`, and it only feeds it to a `TextEdit` widget, so
    /// a test confirming `new` seeded it correctly has no other way to
    /// see it. Test-only.
    #[cfg(feature = "test-support")]
    pub fn task(&self) -> &str {
        &self.task
    }

    /// The iteration-count slider's raw value. Same reasoning as `task`:
    /// `render` draws it into a `Slider` but never returns it.
    #[cfg(feature = "test-support")]
    pub fn iterations(&self) -> u32 {
        self.iterations
    }

    /// Whether `attach` has wired a repeat channel yet. The only other
    /// place this is observable is `render_run_button`'s own disabled-
    /// state check, which is private and only visible as a greyed-out
    /// widget in a window a headless test cannot open. Test-only.
    #[cfg(feature = "test-support")]
    pub fn has_repeat_channel(&self) -> bool {
        self.repeat_tx.is_some()
    }

    /// Whether `attach` has wired the interrupt flag yet. Same reasoning
    /// as `has_repeat_channel`.
    #[cfg(feature = "test-support")]
    pub fn has_interrupt_flag(&self) -> bool {
        self.interrupt_flag.is_some()
    }

    /// Move the progress readout to a newly started iteration. When the
    /// previous state was not already `Running`, this also requests a
    /// one-frame forced close of the Setup header, so a run that starts
    /// folds the controls away once. A later iteration of the same run
    /// leaves the header alone, so a user who reopens it mid-run is not
    /// fought every frame.
    pub fn set_running(&mut self, index: u32, total: u32) {
        if !matches!(self.progress, AutopilotProgress::Running { .. }) {
            self.close_setup_fold = true;
        }
        self.progress = AutopilotProgress::Running { index, total };
    }

    /// Move the progress readout to a finished run.
    pub fn set_finished(&mut self, completed: u32, total: u32) {
        self.progress = AutopilotProgress::Finished { completed, total };
    }

    /// The whole tab. Returns true when a control changed something the
    /// settings file holds, so the caller saves it once per frame.
    pub(crate) fn render(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> bool {
        let mut dirty = false;
        ui.heading("Autopilot");
        ui.separator();

        let mut header = egui::CollapsingHeader::new("Setup").default_open(true);
        if self.close_setup_fold {
            header = header.open(Some(false));
            self.close_setup_fold = false;
        }
        header.show(ui, |ui| {
            ui.label("Task");
            // A `TextEdit` with no explicit width falls back to
            // `ui.spacing().text_edit_width`, a fixed 280 points, however
            // wide the window is. An autopilot task runs to many lines, so
            // it takes the full tab width instead.
            let task_response = ui.add(
                TextEdit::multiline(&mut self.task)
                    .desired_rows(6)
                    .desired_width(f32::INFINITY)
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
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            self.render_run_button(ui);
            if let Some(text) = progress_label(self.progress) {
                ui.label(text);
            }
        });

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
pub fn progress_label(progress: AutopilotProgress) -> Option<String> {
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
pub fn apply_autopilot_task(settings: &mut Settings, task: &str) {
    settings.autopilot_mut().task = Some(task.to_string());
}

/// Store the Autopilot tab's iteration count.
pub fn apply_autopilot_iterations(settings: &mut Settings, iterations: u32) {
    settings.autopilot_mut().iterations = Some(iterations);
}
