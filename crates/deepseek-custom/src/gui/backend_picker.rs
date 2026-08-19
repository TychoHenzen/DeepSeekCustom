//! The backend and model pickers: which entry the sidebar points at, which
//! model it offers, and the background discovery that fills the list.
//!
//! This owns the eight fields `DeepSeekGui` used to hold for the two
//! dropdowns, including the shared model handle the agent reads each turn.
//!
//! Picking a backend here replaces the running one. The picker asks for
//! the switch and the agent task performs it, so `active` moves in the
//! same frame the user picks and the status bar never names a backend that
//! is not the one answering turns.
//!
//! It did not always work that way. The dropdown used to write
//! `default_backend` into `settings.json` and stop there, leaving the
//! session on whatever backend it started with until the next launch. That
//! was not a documented limitation worth keeping. It meant a user could
//! select deepseek, watch the sidebar and the status bar both say deepseek,
//! and have every turn go to `claude -p` regardless.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};
use tokio::sync::mpsc;
use tracing::info;

use super::session_state::SessionOrigin;
use crate::agent::events::AgentCommand;
use crate::api::models::list_models;
use crate::config::settings::{BackendConfig, Settings};

/// What one frame of the two dropdowns asks the caller to do. `dirty` says
/// the settings file needs saving. `switch` carries the command the agent
/// task needs to replace the running backend, and is `None` on every frame
/// that did not change the backend.
///
/// The picker returns the command rather than sending it. Every type under
/// `src/gui/` follows that rule, so one place still talks to the agent.
#[derive(Default)]
pub struct PickerOutcome {
    pub dirty: bool,
    pub switch: Option<BackendSwitch>,
}

/// One backend switch, ready for the GUI to act on.
///
/// `outgoing` names the backend and model the conversation being replaced
/// actually ran on, captured before the picker moved. Without it the
/// outgoing conversation would be filed under the backend that is about to
/// take over, which is the one backend it never ran a single turn on.
pub struct BackendSwitch {
    pub command: AgentCommand,
    pub outgoing: SessionOrigin,
}

/// The two dropdowns and the discovery channel behind them.
pub struct BackendPicker {
    /// Sorted backend names, the keys of `settings.backends()`.
    options: Vec<String>,
    /// Which of `options` the picker points at.
    selected_idx: usize,
    /// The backend the running session is actually built on. Seeded at
    /// startup and moved by `switch_backend`, which is what makes it the
    /// running one. `None` when the settings named no backend at all.
    active: Option<String>,
    /// The model name the status bar shows and a save records.
    model: String,
    /// Shared with the agent, which re-reads it every turn.
    model_flag: Arc<Mutex<String>>,
    /// Options for the model dropdown, resolved for the selected backend.
    /// Seeded with that backend's declared model, so the dropdown is never
    /// empty. Replaced once the background fetch completes.
    model_options: Vec<String>,
    /// Background model-discovery results, tagged with the backend name
    /// they were resolved for.
    list_rx: mpsc::UnboundedReceiver<(String, Vec<String>)>,
    /// Sender half of `list_rx`. Cloned into each background fetch.
    list_tx: mpsc::UnboundedSender<(String, Vec<String>)>,
}

impl BackendPicker {
    /// Seed both dropdowns from `settings` and start discovering the
    /// selected backend's models in the background.
    pub fn new(settings: &Settings, model_flag: Arc<Mutex<String>>) -> Self {
        let mut options: Vec<String> = settings
            .backends()
            .map(|b| b.keys().cloned().collect())
            .unwrap_or_default();
        options.sort();
        let selected_idx = settings
            .default_backend
            .as_ref()
            .and_then(|name| options.iter().position(|b| b == name))
            .unwrap_or(0);
        let model = model_flag.lock().unwrap().clone();
        let (list_tx, list_rx) = mpsc::unbounded_channel::<(String, Vec<String>)>();

        let picker = Self {
            active: options.get(selected_idx).cloned(),
            model_options: vec![model.clone()],
            options,
            selected_idx,
            model,
            model_flag,
            list_rx,
            list_tx,
        };
        picker.spawn_fetch_for_selected(settings);
        picker
    }

    /// Every backend name the `backends` map holds, sorted. The two search
    /// tabs list these in their own backend dropdowns, so a run can be
    /// pointed at a cheap entry without moving the session's own backend.
    pub fn names(&self) -> &[String] {
        &self.options
    }

    /// The backend the running session is on, empty when there is none.
    pub fn active_backend(&self) -> &str {
        self.active.as_deref().unwrap_or_default()
    }

    /// The model name the status bar shows and a save records.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The name the dropdown points at. This tracks `active` for every
    /// name the `backends` map declares, since picking one switches the
    /// running backend in the same call.
    pub fn selected_name(&self) -> Option<&str> {
        self.options.get(self.selected_idx).map(String::as_str)
    }

    /// Every backend name the settings file declares, sorted. `render`
    /// draws these into a `ComboBox` but never returns the list itself,
    /// so a test confirming `new` sorted and populated it correctly has
    /// no other way to see it. Test-only.
    #[cfg(feature = "test-support")]
    pub fn options(&self) -> &[String] {
        &self.options
    }

    /// Point the picker at `idx` without applying it. Test-only: the
    /// dropdown moves this itself and then applies the change.
    #[cfg(feature = "test-support")]
    pub fn set_selected_idx(&mut self, idx: usize) {
        self.selected_idx = idx;
    }

    /// The model names the dropdown offers for the selected backend.
    /// `render` draws these into a `ComboBox` too, with no return path,
    /// so this is the only way a test can see what `apply_fetched_list`
    /// or `switch_backend` actually populated. Test-only.
    #[cfg(feature = "test-support")]
    pub fn model_options(&self) -> &[String] {
        &self.model_options
    }

    /// Push a discovery result directly onto the channel
    /// `drain_fetched_lists` reads, bypassing the real background fetch.
    /// Nothing in production ever writes to this channel except
    /// `spawn_model_list_fetch`'s own network call, so a test exercising
    /// `drain_fetched_lists` in isolation has no other way to fill it.
    /// Test-only.
    #[cfg(feature = "test-support")]
    pub fn send_fetched_list_for_test(&self, backend_name: String, models: Vec<String>) {
        let _ = self.list_tx.send((backend_name, models));
    }

    /// Take every finished model-discovery result. Called once per frame.
    pub fn drain_fetched_lists(&mut self) -> Vec<(String, Vec<String>)> {
        let mut results = Vec::new();
        while let Ok(result) = self.list_rx.try_recv() {
            results.push(result);
        }
        results
    }

    /// Wait for the next model-discovery result. Test-only: the frame loop
    /// drains without blocking, but a test needs to await a real fetch.
    #[cfg(feature = "test-support")]
    pub async fn recv_fetched_list(&mut self) -> Option<(String, Vec<String>)> {
        self.list_rx.recv().await
    }

    /// Install a finished model list, or drop it when it was resolved for a
    /// backend the user has since switched away from.
    ///
    /// The current model is appended when discovery did not report it, so
    /// the dropdown always holds its own selection.
    pub fn apply_fetched_list(&mut self, backend_name: &str, models: Vec<String>) {
        if self.selected_name() != Some(backend_name) {
            return;
        }
        self.model_options = models;
        if !self.model_options.contains(&self.model) {
            self.model_options.push(self.model.clone());
        }
    }

    /// Both dropdowns and the two captions under them. Returns what the
    /// caller has to do about this frame: save the settings file, send a
    /// backend switch, or neither.
    pub(crate) fn render(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> PickerOutcome {
        let mut outcome = PickerOutcome::default();

        let prev_idx = self.selected_idx;
        let selected_text = self
            .selected_name()
            .unwrap_or("(none configured)")
            .to_string();
        egui::ComboBox::from_label("Backend")
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                for (i, opt) in self.options.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_idx, i, opt);
                }
            });
        if self.selected_idx != prev_idx {
            let new_backend = self.options[self.selected_idx].clone();
            outcome.switch = self.switch_backend(new_backend, settings);
            outcome.dirty = true;
        }

        let prev_model = self.model.clone();
        let mut picked = self.model.clone();
        egui::ComboBox::from_label("Model")
            .selected_text(self.model.clone())
            .show_ui(ui, |ui| {
                for opt in &self.model_options {
                    ui.selectable_value(&mut picked, opt.clone(), opt);
                }
            });
        if picked != prev_model {
            outcome.dirty |= self.switch_model(picked, settings);
        }

        ui.label(
            RichText::new(
                "Switching backends replaces the running one and starts a new conversation.",
            )
            .color(Color32::GRAY)
            .small(),
        );
        ui.label(
            RichText::new(
                "Model changes apply next turn (DeepSeek, Ollama). Claude respawns its child.",
            )
            .color(Color32::GRAY)
            .small(),
        );

        outcome
    }

    /// Switch to `new_backend`: adopt that entry's declared model, reseed
    /// the model dropdown, start discovering its models, and return the
    /// command that replaces the running backend with it.
    ///
    /// `active` moves here, not on the next app start. The switch is what
    /// the returned command performs, and the agent task applies it before
    /// the next turn can run, so there is no window in which the sidebar
    /// names one backend and turns go to another.
    ///
    /// A name that resolves to no entry returns `None`: nothing is worth
    /// sending, since the agent could not build it either. The name is
    /// still written to settings, matching how the dropdown can only ever
    /// offer names that came out of that same map.
    pub fn switch_backend(
        &mut self,
        new_backend: String,
        settings: &mut Settings,
    ) -> Option<BackendSwitch> {
        let outgoing = SessionOrigin {
            backend: self.active_backend().to_string(),
            model: self.model.clone(),
        };
        let mut switch = None;
        if let Some(cfg) = settings.resolve_backend(&new_backend) {
            let new_model = cfg.model().to_string();
            self.model = new_model.clone();
            self.model_options = vec![new_model.clone()];
            self.active = Some(new_backend.clone());
            self.write_model_flag(new_model.clone());
            spawn_model_list_fetch(self.list_tx.clone(), new_backend.clone(), cfg.clone());
            switch = Some(BackendSwitch {
                command: AgentCommand::SwitchBackend {
                    name: new_backend.clone(),
                    model: Some(new_model),
                },
                outgoing,
            });
        }
        info!(backend = %new_backend, "backend changed via settings panel");
        apply_default_backend(settings, &new_backend);
        switch
    }

    /// Switch the selected backend's model. Returns true when the settings
    /// file needs saving, and false when the picker points at nothing.
    ///
    /// The shared model handle always moves with it. This used to be
    /// guarded on the picker still pointing at the running backend, back
    /// when a backend switch took effect only on the next start and the two
    /// could disagree for a whole session. A switch is applied immediately
    /// now, and the dropdown can only offer names that came out of the
    /// `backends` map, so the model picked here is always the running
    /// backend's.
    pub fn switch_model(&mut self, new_model: String, settings: &mut Settings) -> bool {
        let Some(backend_name) = self.selected_name().map(str::to_string) else {
            return false;
        };
        self.model = new_model.clone();
        self.write_model_flag(new_model.clone());
        info!(backend = %backend_name, model = %new_model, "model changed via settings panel");
        apply_backend_model(settings, &backend_name, &new_model);
        true
    }

    /// Write the shared handle the agent reads each turn. A poisoned lock
    /// leaves the model where it was rather than taking the GUI down.
    fn write_model_flag(&self, new_model: String) {
        if let Ok(mut model) = self.model_flag.lock() {
            *model = new_model;
        }
    }

    /// Start discovering the selected backend's models, if it resolves.
    fn spawn_fetch_for_selected(&self, settings: &Settings) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let Some(cfg) = settings.resolve_backend(name) else {
            return;
        };
        spawn_model_list_fetch(self.list_tx.clone(), name.to_string(), cfg.clone());
    }
}

/// Resolve a backend's model list on a background task, off the paint
/// loop. Sends it tagged with `backend_name`. That lets a result be told
/// apart from one resolved for a backend the user has since switched away
/// from. Does nothing outside a tokio runtime, which is what a unit test
/// building a GUI runs in.
pub(super) fn spawn_model_list_fetch(
    tx: mpsc::UnboundedSender<(String, Vec<String>)>,
    backend_name: String,
    cfg: BackendConfig,
) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let models = list_models(&cfg).await;
        let _ = tx.send((backend_name, models));
    });
}

/// Store the backend picker's selection.
pub fn apply_default_backend(settings: &mut Settings, name: &str) {
    settings.default_backend = Some(name.to_string());
}

/// Store the model dropdown's selection onto the named backend's entry.
///
/// A name that does not resolve to any backend is a no-op. A stale
/// selection must never write to the wrong entry or panic.
pub fn apply_backend_model(settings: &mut Settings, backend_name: &str, model: &str) {
    let Some(backends) = settings.backends.as_mut() else {
        return;
    };
    let Some(entry) = backends.get_mut(backend_name) else {
        return;
    };
    match entry {
        BackendConfig::Api { model: m, .. } => *m = model.to_string(),
        BackendConfig::ClaudeCli { model: m, .. } => *m = model.to_string(),
        BackendConfig::CodexCli { model: m, .. } => *m = model.to_string(),
    }
}
