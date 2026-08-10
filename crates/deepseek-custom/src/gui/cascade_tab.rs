//! The Cascade tab: the prompt, the backend to fan out onto, the attempt
//! count and vote margin, the check command, the diversity hints, the
//! escalation backend, a Run button, and the live standings.
//!
//! A cascade is a procedure with a fixed shape, and a person fixes it here
//! before the run starts. That is the whole reason this is a tab rather
//! than a tool: a model choosing its own attempt count and vote margin per
//! call is the opposite of a repeatable procedure.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, RichText, TextEdit};
use tokio::sync::mpsc;
use tracing::info;

use crate::config::settings::Settings;
use crate::effort::Effort;
use crate::search::cascade::{CascadeParams, MAX_ATTEMPTS, default_diversity_hints};
use crate::search::{SearchCommand, SearchKind};

use super::search_view::{SearchProgress, progress_label, render_progress};

/// The Cascade tab's form, channels, and run state.
pub struct CascadeTab {
    /// Sends a run to the agent task. `None` until `attach` is called, and
    /// the Run button stays disabled while it is, so a GUI built without
    /// the channel simply cannot start a run.
    search_tx: Option<mpsc::UnboundedSender<SearchCommand>>,
    /// Set by Stop, and by Escape, to end a run between attempts.
    interrupt: Option<Arc<AtomicBool>>,
    prompt: String,
    backend: String,
    n: u32,
    vote_k: u32,
    check_cmd: String,
    /// One hint per line, which is how the text box holds them. An empty
    /// box means the run uses the built-in defaults.
    hints: String,
    escalate_backend: String,
    progress: SearchProgress,
    /// Forces the Setup fold closed for one frame when a run starts, the
    /// same one-shot the Autopilot tab uses so a user who reopens it
    /// mid-run is not fought every frame.
    close_setup_fold: bool,
}

impl CascadeTab {
    /// Seed the form from `settings`, with no channels attached yet.
    pub fn new(settings: &Settings) -> Self {
        let saved = settings.cascade();
        Self {
            search_tx: None,
            interrupt: None,
            prompt: saved.and_then(|c| c.prompt.clone()).unwrap_or_default(),
            backend: saved.and_then(|c| c.backend.clone()).unwrap_or_default(),
            n: saved.and_then(|c| c.n).unwrap_or(5),
            vote_k: saved.and_then(|c| c.vote_k).unwrap_or(1),
            check_cmd: saved.and_then(|c| c.check_cmd.clone()).unwrap_or_default(),
            hints: saved
                .and_then(|c| c.diversity_hints.clone())
                .map(|h| h.join("\n"))
                .unwrap_or_default(),
            escalate_backend: saved
                .and_then(|c| c.escalate_backend.clone())
                .unwrap_or_default(),
            progress: SearchProgress::Idle,
            close_setup_fold: false,
        }
    }

    /// Attach the search channel and the flag that stops a running search.
    pub fn attach(
        &mut self,
        search_tx: mpsc::UnboundedSender<SearchCommand>,
        interrupt: Arc<AtomicBool>,
    ) {
        self.search_tx = Some(search_tx);
        self.interrupt = Some(interrupt);
    }

    /// Stop a running cascade. Escape calls this alongside interrupting the
    /// turn, since one Escape stops whatever the session is doing.
    pub fn request_stop(&self) {
        if let Some(flag) = &self.interrupt {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Whether a run is in flight.
    pub fn is_running(&self) -> bool {
        self.progress.is_running()
    }

    /// Move the readout to a newly reported state, folding the Setup header
    /// away once when a run starts.
    pub fn set_progress(&mut self, progress: SearchProgress) {
        if progress.is_running() && !self.progress.is_running() {
            self.close_setup_fold = true;
        }
        self.progress = progress;
    }

    /// The run state, which `render` only ever draws. Test-only.
    #[cfg(feature = "test-support")]
    pub fn progress(&self) -> &SearchProgress {
        &self.progress
    }

    /// The prompt box's raw contents. Same reasoning: `render` feeds it to
    /// a widget and never hands it back. Test-only.
    #[cfg(feature = "test-support")]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// The attempt count. Test-only.
    #[cfg(feature = "test-support")]
    pub fn n(&self) -> u32 {
        self.n
    }

    /// Whether `attach` has wired the channel yet. Test-only.
    #[cfg(feature = "test-support")]
    pub fn has_channel(&self) -> bool {
        self.search_tx.is_some()
    }

    /// The run this form currently describes, at the session's current
    /// effort level.
    pub fn params(&self, effort: Effort) -> CascadeParams {
        CascadeParams {
            prompt: self.prompt.clone(),
            backend: self.backend.clone(),
            n: self.n,
            vote_k: self.vote_k,
            check_cmd: non_empty(&self.check_cmd),
            diversity_hints: parse_hints(&self.hints),
            escalate_backend: non_empty(&self.escalate_backend),
            effort,
        }
    }

    /// The whole tab. Returns true when a control changed something the
    /// settings file holds, so the caller saves once per frame.
    pub(crate) fn render(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        backends: &[String],
        effort: Effort,
    ) -> bool {
        let mut dirty = false;
        ui.heading("Cascade");
        ui.separator();

        let mut header = egui::CollapsingHeader::new("Setup").default_open(true);
        if self.close_setup_fold {
            header = header.open(Some(false));
            self.close_setup_fold = false;
        }
        header.show(ui, |ui| {
            dirty |= self.render_form(ui, settings, backends);
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            self.render_run_button(ui, effort);
            if ui
                .add_enabled(self.is_running(), egui::Button::new("Stop"))
                .clicked()
            {
                self.request_stop();
            }
            if let Some(text) = progress_label(&self.progress, SearchKind::Cascade) {
                ui.label(text);
            }
        });

        ui.add_space(4.0);
        render_progress(ui, &self.progress);
        dirty
    }

    /// Every input on the form. Split out to keep `render` short.
    fn render_form(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        backends: &[String],
    ) -> bool {
        let mut dirty = false;

        ui.label("Prompt");
        let prompt_response = ui.add(
            TextEdit::multiline(&mut self.prompt)
                .desired_rows(5)
                .desired_width(f32::INFINITY)
                .hint_text("The task every attempt runs"),
        );
        if prompt_response.lost_focus() {
            settings.cascade_mut().prompt = Some(self.prompt.clone());
            dirty = true;
        }

        ui.add_space(8.0);
        if backend_combo(ui, "Backend", &mut self.backend, backends, false) {
            settings.cascade_mut().backend = Some(self.backend.clone());
            dirty = true;
        }
        if backend_combo(
            ui,
            "Escalate to",
            &mut self.escalate_backend,
            backends,
            true,
        ) {
            settings.cascade_mut().escalate_backend = non_empty(&self.escalate_backend);
            dirty = true;
        }

        ui.add_space(8.0);
        let n_response = ui.add(egui::Slider::new(&mut self.n, 1..=MAX_ATTEMPTS).text("Attempts"));
        if n_response.drag_stopped() {
            settings.cascade_mut().n = Some(self.n);
            dirty = true;
        }
        let vote_response = ui.add(egui::Slider::new(&mut self.vote_k, 1..=8).text("Vote margin"));
        if vote_response.drag_stopped() {
            settings.cascade_mut().vote_k = Some(self.vote_k);
            dirty = true;
        }

        ui.add_space(8.0);
        ui.label("Check command");
        let check_response = ui.add(
            TextEdit::singleline(&mut self.check_cmd)
                .desired_width(f32::INFINITY)
                .hint_text("optional: runs per candidate, candidate text on stdin"),
        );
        if check_response.lost_focus() {
            settings.cascade_mut().check_cmd = non_empty(&self.check_cmd);
            dirty = true;
        }

        ui.add_space(8.0);
        ui.label("Diversity hints, one per line");
        let hints_response = ui.add(
            TextEdit::multiline(&mut self.hints)
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text(default_diversity_hints().join("\n")),
        );
        if hints_response.lost_focus() {
            let parsed = parse_hints(&self.hints);
            settings.cascade_mut().diversity_hints = if parsed.is_empty() {
                None
            } else {
                Some(parsed)
            };
            dirty = true;
        }
        ui.label(
            RichText::new("  Empty uses the four built-in hints.")
                .color(Color32::GRAY)
                .small(),
        );
        dirty
    }

    /// The Run button, disabled while a run is going or nothing can run.
    fn render_run_button(&mut self, ui: &mut egui::Ui, effort: Effort) {
        let can_run = !self.prompt.trim().is_empty()
            && !self.backend.trim().is_empty()
            && self.search_tx.is_some()
            && !self.is_running();
        if !ui.add_enabled(can_run, egui::Button::new("Run")).clicked() {
            return;
        }
        let Some(tx) = &self.search_tx else {
            return;
        };
        // A previous run may have set this, and nothing else clears it.
        // Without this the next run stops after its first attempt.
        if let Some(flag) = &self.interrupt {
            flag.store(false, Ordering::SeqCst);
        }
        let params = self.params(effort);
        info!(n = params.n, backend = %params.backend, "cascade run requested");
        let _ = tx.send(SearchCommand::Cascade(Box::new(params)));
    }
}

/// A backend picker over the names the `backends` map holds. `allow_none`
/// adds an empty choice, for the optional escalation backend. Returns true
/// when the selection changed.
pub fn backend_combo(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut String,
    backends: &[String],
    allow_none: bool,
) -> bool {
    let before = current.clone();
    let shown = if current.is_empty() {
        "(none)".to_string()
    } else {
        current.clone()
    };
    egui::ComboBox::from_label(label)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            if allow_none {
                ui.selectable_value(current, String::new(), "(none)");
            }
            for name in backends {
                ui.selectable_value(current, name.clone(), name);
            }
        });
    *current != before
}

/// `None` for a blank box, so an empty field means "not set" rather than
/// an empty command or an empty backend name.
pub fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Split a hint box into one hint per non-blank line.
pub fn parse_hints(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}
