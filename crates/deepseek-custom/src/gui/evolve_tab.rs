//! The Evolve tab: the seed prompt, the backend, the generation and
//! population counts, the island settings, the fitness and feature
//! commands, the mutation hints, a Run button, and the live archive.
//!
//! Same reason the Cascade tab exists rather than a tool. An evolutionary
//! run's shape decides its whole cost, and a person fixes that shape here
//! before the first dispatch goes out.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, RichText, TextEdit};
use tokio::sync::mpsc;
use tracing::info;

use crate::application::services::DomainCommandPort;
use crate::config::settings::Settings;
use crate::effort::Effort;
use crate::search::evolve::{EvolveParams, MAX_TOTAL_DISPATCHES, default_mutation_hints};
use crate::search::{SearchCommand, SearchKind};

use super::cascade_tab::{backend_combo, non_empty, parse_hints};
use super::search_view::{SearchProgress, progress_label, render_progress};

/// The Evolve tab's form, channels, and run state.
pub struct EvolveTab {
    /// Sends a run to the agent task. `None` until `attach`, and Run stays
    /// disabled while it is.
    search_tx: Option<DomainCommandPort<SearchCommand>>,
    /// Set by Stop, and by Escape, to end a run between generations.
    interrupt: Option<Arc<AtomicBool>>,
    prompt: String,
    backend: String,
    generations: u32,
    population: u32,
    islands: u32,
    migration_interval: u32,
    fitness_cmd: String,
    feature_cmd: String,
    /// One hint per line. Empty means the run uses the built-in defaults.
    hints: String,
    progress: SearchProgress,
    close_setup_fold: bool,
}

impl EvolveTab {
    /// Seed the form from `settings`, with no channels attached yet.
    pub fn new(settings: &Settings) -> Self {
        let saved = settings.evolve();
        Self {
            search_tx: None,
            interrupt: None,
            prompt: saved.and_then(|e| e.prompt.clone()).unwrap_or_default(),
            backend: saved.and_then(|e| e.backend.clone()).unwrap_or_default(),
            generations: saved.and_then(|e| e.generations).unwrap_or(10),
            population: saved.and_then(|e| e.population).unwrap_or(6),
            islands: saved.and_then(|e| e.islands).unwrap_or(1),
            migration_interval: saved.and_then(|e| e.migration_interval).unwrap_or(5),
            fitness_cmd: saved
                .and_then(|e| e.fitness_cmd.clone())
                .unwrap_or_default(),
            feature_cmd: saved
                .and_then(|e| e.feature_cmd.clone())
                .unwrap_or_default(),
            hints: saved
                .and_then(|e| e.mutation_hints.clone())
                .map(|h| h.join("\n"))
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
        self.search_tx = Some(DomainCommandPort::new(search_tx));
        self.interrupt = Some(interrupt);
    }

    /// Stop a running search.
    pub fn request_stop(&self) {
        if let Some(flag) = &self.interrupt {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Whether a run is in flight.
    pub fn is_running(&self) -> bool {
        self.progress.is_running()
    }

    /// Move the readout to a newly reported state, folding Setup away once
    /// when a run starts.
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

    /// The seed prompt box's raw contents. Test-only.
    #[cfg(feature = "test-support")]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// The generation count. Test-only.
    #[cfg(feature = "test-support")]
    pub fn generations(&self) -> u32 {
        self.generations
    }

    /// Whether `attach` has wired the channel yet. Test-only.
    #[cfg(feature = "test-support")]
    pub fn has_channel(&self) -> bool {
        self.search_tx.is_some()
    }

    /// The run this form currently describes, at the session's current
    /// effort level.
    pub fn params(&self, effort: Effort) -> EvolveParams {
        EvolveParams {
            prompt: self.prompt.clone(),
            backend: self.backend.clone(),
            generations: self.generations,
            population: self.population,
            fitness_cmd: self.fitness_cmd.trim().to_string(),
            feature_cmd: non_empty(&self.feature_cmd),
            islands: self.islands,
            migration_interval: self.migration_interval,
            mutation_hints: parse_hints(&self.hints),
            effort,
        }
    }

    /// The whole tab. Returns true when a control changed something the
    /// settings file holds.
    pub(crate) fn render(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        backends: &[String],
        effort: Effort,
    ) -> bool {
        let mut dirty = false;
        ui.heading("Evolve");
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
            if let Some(text) = progress_label(&self.progress, SearchKind::Evolve) {
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

        ui.label("Seed prompt");
        let prompt_response = ui.add(
            TextEdit::multiline(&mut self.prompt)
                .desired_rows(5)
                .desired_width(f32::INFINITY)
                .hint_text("The task the first generation starts from"),
        );
        if prompt_response.lost_focus() {
            settings.evolve_mut().prompt = Some(self.prompt.clone());
            dirty = true;
        }

        ui.add_space(8.0);
        if backend_combo(ui, "Backend", &mut self.backend, backends, false) {
            settings.evolve_mut().backend = Some(self.backend.clone());
            dirty = true;
        }

        ui.add_space(8.0);
        dirty |= self.render_counts(ui, settings);

        ui.add_space(8.0);
        ui.label("Fitness command (required)");
        let fitness_response = ui.add(
            TextEdit::singleline(&mut self.fitness_cmd)
                .desired_width(f32::INFINITY)
                .hint_text("candidate text on stdin, one number on stdout"),
        );
        if fitness_response.lost_focus() {
            settings.evolve_mut().fitness_cmd = non_empty(&self.fitness_cmd);
            dirty = true;
        }

        ui.label("Feature command");
        let feature_response = ui.add(
            TextEdit::singleline(&mut self.feature_cmd)
                .desired_width(f32::INFINITY)
                .hint_text("optional: comma-separated numbers, drives the MAP-Elites grid"),
        );
        if feature_response.lost_focus() {
            settings.evolve_mut().feature_cmd = non_empty(&self.feature_cmd);
            dirty = true;
        }

        ui.add_space(8.0);
        ui.label("Mutation hints, one per line");
        let hints_response = ui.add(
            TextEdit::multiline(&mut self.hints)
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text(default_mutation_hints().join("\n")),
        );
        if hints_response.lost_focus() {
            let parsed = parse_hints(&self.hints);
            settings.evolve_mut().mutation_hints = if parsed.is_empty() {
                None
            } else {
                Some(parsed)
            };
            dirty = true;
        }

        ui.label(
            RichText::new(format!(
                "  Up to {} dispatches, then the run stops and reports its best.",
                MAX_TOTAL_DISPATCHES
            ))
            .color(Color32::GRAY)
            .small(),
        );
        ui.label(
            RichText::new(format!(
                "  This run would use {}.",
                self.planned_dispatches()
            ))
            .color(if self.planned_dispatches() > MAX_TOTAL_DISPATCHES {
                Color32::from_rgb(255, 180, 120)
            } else {
                Color32::GRAY
            })
            .small(),
        );
        dirty
    }

    /// The four count sliders, split out so `render_form` stays readable.
    fn render_counts(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> bool {
        let mut dirty = false;
        let gen_response =
            ui.add(egui::Slider::new(&mut self.generations, 1..=50).text("Generations"));
        if gen_response.drag_stopped() {
            settings.evolve_mut().generations = Some(self.generations);
            dirty = true;
        }
        let pop_response =
            ui.add(egui::Slider::new(&mut self.population, 1..=20).text("Population"));
        if pop_response.drag_stopped() {
            settings.evolve_mut().population = Some(self.population);
            dirty = true;
        }
        let island_response = ui.add(egui::Slider::new(&mut self.islands, 1..=8).text("Islands"));
        if island_response.drag_stopped() {
            settings.evolve_mut().islands = Some(self.islands);
            dirty = true;
        }
        let migrate_response =
            ui.add(egui::Slider::new(&mut self.migration_interval, 0..=20).text("Migrate every"));
        if migrate_response.drag_stopped() {
            settings.evolve_mut().migration_interval = Some(self.migration_interval);
            dirty = true;
        }
        ui.label(
            RichText::new("  Migrate every 0 means never.")
                .color(Color32::GRAY)
                .small(),
        );
        dirty
    }

    /// How many dispatches this form would make if nothing stopped it. Shown
    /// next to the cap, since the three counts multiply and a run that looks
    /// small on each slider is not.
    pub fn planned_dispatches(&self) -> u32 {
        self.generations
            .saturating_mul(self.population)
            .saturating_mul(self.islands)
    }

    /// The Run button, disabled while a run is going or the form is not
    /// runnable. A fitness command is required: without one nothing ranks a
    /// candidate, so the run has no meaning.
    fn render_run_button(&mut self, ui: &mut egui::Ui, effort: Effort) {
        let can_run = !self.prompt.trim().is_empty()
            && !self.backend.trim().is_empty()
            && !self.fitness_cmd.trim().is_empty()
            && self.search_tx.is_some()
            && !self.is_running();
        if !ui.add_enabled(can_run, egui::Button::new("Run")).clicked() {
            return;
        }
        let Some(tx) = &self.search_tx else {
            return;
        };
        // A previous run may have set this, and nothing else clears it.
        if let Some(flag) = &self.interrupt {
            flag.store(false, Ordering::SeqCst);
        }
        let params = self.params(effort);
        info!(
            generations = params.generations,
            backend = %params.backend,
            "evolve run requested"
        );
        let _ = tx.send(SearchCommand::Evolve(Box::new(params)));
    }
}
