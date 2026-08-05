//! The backend and model pickers: which entry the sidebar points at, which
//! model it offers, and the background discovery that fills the list.
//!
//! This owns the eight fields `DeepSeekGui` used to hold for the two
//! dropdowns, including the shared model handle the agent reads each turn.
//!
//! One distinction runs through the whole file and is easy to lose. The
//! picker's selection and the running session's backend are two different
//! things. A backend switch only takes effect on the next start, so the
//! picker can point at an entry the session is not on. A model change
//! writes the shared handle only while the two still agree. Sending
//! another entry's model name to the running backend would break every
//! following turn.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};
use tokio::sync::mpsc;
use tracing::info;

use crate::api::models::list_models;
use crate::config::settings::{BackendConfig, Settings};

/// The two dropdowns and the discovery channel behind them.
pub(crate) struct BackendPicker {
    /// Sorted backend names, the keys of `settings.backends()`.
    options: Vec<String>,
    /// Which of `options` the picker points at.
    selected_idx: usize,
    /// The backend the running session was actually built on, fixed at
    /// startup. `None` when the settings named no backend at all.
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
    pub(crate) fn new(settings: &Settings, model_flag: Arc<Mutex<String>>) -> Self {
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

    /// The backend the running session is on, empty when there is none.
    pub(crate) fn active_backend(&self) -> &str {
        self.active.as_deref().unwrap_or_default()
    }

    /// The model name the status bar shows and a save records.
    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    /// The name the picker points at, which may differ from the running
    /// backend until the next start.
    pub(crate) fn selected_name(&self) -> Option<&str> {
        self.options.get(self.selected_idx).map(String::as_str)
    }

    /// Every backend name the settings file declares, sorted.
    #[cfg(test)]
    pub(crate) fn options(&self) -> &[String] {
        &self.options
    }

    /// Point the picker at `idx` without applying it. Test-only: the
    /// dropdown moves this itself and then applies the change.
    #[cfg(test)]
    pub(crate) fn set_selected_idx(&mut self, idx: usize) {
        self.selected_idx = idx;
    }

    /// The model names the dropdown offers for the selected backend.
    #[cfg(test)]
    pub(crate) fn model_options(&self) -> &[String] {
        &self.model_options
    }

    /// Take every finished model-discovery result. Called once per frame.
    pub(crate) fn drain_fetched_lists(&mut self) -> Vec<(String, Vec<String>)> {
        let mut results = Vec::new();
        while let Ok(result) = self.list_rx.try_recv() {
            results.push(result);
        }
        results
    }

    /// Wait for the next model-discovery result. Test-only: the frame loop
    /// drains without blocking, but a test needs to await a real fetch.
    #[cfg(test)]
    pub(crate) async fn recv_fetched_list(&mut self) -> Option<(String, Vec<String>)> {
        self.list_rx.recv().await
    }

    /// Install a finished model list, or drop it when it was resolved for a
    /// backend the user has since switched away from.
    ///
    /// The current model is appended when discovery did not report it, so
    /// the dropdown always holds its own selection.
    pub(crate) fn apply_fetched_list(&mut self, backend_name: &str, models: Vec<String>) {
        if self.selected_name() != Some(backend_name) {
            return;
        }
        self.model_options = models;
        if !self.model_options.contains(&self.model) {
            self.model_options.push(self.model.clone());
        }
    }

    /// Both dropdowns and the two captions under them. Returns true when a
    /// change needs saving to the settings file.
    pub(crate) fn render(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> bool {
        let mut dirty = false;

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
            dirty |= self.switch_backend(new_backend, settings);
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
            dirty |= self.switch_model(picked, settings);
        }

        ui.label(
            RichText::new("Switching backends takes effect on the next app start.")
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

        dirty
    }

    /// Switch the picker to `new_backend`: adopt that entry's declared
    /// model, reseed the model dropdown, and start discovering its models.
    /// Returns true when the settings file needs saving.
    ///
    /// The shared model handle only moves while the picker still points at
    /// the running backend.
    pub(crate) fn switch_backend(&mut self, new_backend: String, settings: &mut Settings) -> bool {
        if let Some(cfg) = settings.resolve_backend(&new_backend) {
            let new_model = cfg.model().to_string();
            self.model = new_model.clone();
            self.model_options = vec![new_model.clone()];
            if self.active.as_deref() == Some(new_backend.as_str()) {
                self.write_model_flag(new_model);
            }
            spawn_model_list_fetch(self.list_tx.clone(), new_backend.clone(), cfg.clone());
        }
        info!(backend = %new_backend, "backend changed via settings panel");
        apply_default_backend(settings, &new_backend);
        true
    }

    /// Switch the selected backend's model. Returns true when the settings
    /// file needs saving, and false when the picker points at nothing.
    ///
    /// The shared model handle only moves while the picker still points at
    /// the running backend.
    pub(crate) fn switch_model(&mut self, new_model: String, settings: &mut Settings) -> bool {
        let Some(backend_name) = self.selected_name().map(str::to_string) else {
            return false;
        };
        self.model = new_model.clone();
        if self.active.as_deref() == Some(backend_name.as_str()) {
            self.write_model_flag(new_model.clone());
        }
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
pub(super) fn apply_default_backend(settings: &mut Settings, name: &str) {
    settings.default_backend = Some(name.to_string());
}

/// Store the model dropdown's selection onto the named backend's entry.
///
/// A name that does not resolve to any backend is a no-op. A stale
/// selection must never write to the wrong entry or panic.
pub(super) fn apply_backend_model(settings: &mut Settings, backend_name: &str, model: &str) {
    let Some(backends) = settings.backends.as_mut() else {
        return;
    };
    let Some(entry) = backends.get_mut(backend_name) else {
        return;
    };
    match entry {
        BackendConfig::Api { model: m, .. } => *m = model.to_string(),
        BackendConfig::ClaudeCli { model: m, .. } => *m = model.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::ApiProvider;
    use std::collections::HashMap;

    fn settings_with_two_backends() -> Settings {
        let mut backends = HashMap::new();
        backends.insert(
            "deepseek".to_string(),
            BackendConfig::Api {
                provider: ApiProvider::DeepSeek,
                model: "deepseek-v4-flash".to_string(),
                base_url: None,
                api_key: None,
                models: None,
            },
        );
        backends.insert(
            "claude".to_string(),
            BackendConfig::ClaudeCli {
                model: "opus".to_string(),
                permission_mode: None,
                env: None,
                models: None,
            },
        );
        Settings {
            backends: Some(backends),
            default_backend: Some("deepseek".to_string()),
            ..Settings::default()
        }
    }

    fn picker_on(settings: &Settings, model: &str) -> BackendPicker {
        BackendPicker::new(settings, Arc::new(Mutex::new(model.to_string())))
    }

    #[test]
    fn new_sorts_the_backend_names() {
        let picker = picker_on(&settings_with_two_backends(), "deepseek-v4-flash");
        assert_eq!(picker.options(), ["claude", "deepseek"]);
    }

    #[test]
    fn new_selects_the_default_backend() {
        let picker = picker_on(&settings_with_two_backends(), "deepseek-v4-flash");
        assert_eq!(picker.selected_name(), Some("deepseek"));
        assert_eq!(picker.active_backend(), "deepseek");
    }

    #[test]
    fn new_falls_back_to_the_first_backend_when_no_default_is_named() {
        let mut settings = settings_with_two_backends();
        settings.default_backend = None;
        let picker = picker_on(&settings, "deepseek-v4-flash");
        assert_eq!(picker.selected_name(), Some("claude"));
    }

    #[test]
    fn new_falls_back_to_the_first_backend_on_an_unknown_default() {
        let mut settings = settings_with_two_backends();
        settings.default_backend = Some("not_a_backend".to_string());
        let picker = picker_on(&settings, "deepseek-v4-flash");
        assert_eq!(picker.selected_name(), Some("claude"));
    }

    #[test]
    fn new_seeds_the_model_dropdown_with_the_running_model() {
        let picker = picker_on(&settings_with_two_backends(), "deepseek-v4-flash");
        assert_eq!(picker.model(), "deepseek-v4-flash");
        assert_eq!(picker.model_options(), ["deepseek-v4-flash"]);
    }

    #[test]
    fn switching_backend_adopts_that_entrys_declared_model() {
        let mut settings = settings_with_two_backends();
        let mut picker = picker_on(&settings, "deepseek-v4-flash");
        assert!(picker.switch_backend("claude".to_string(), &mut settings));
        assert_eq!(picker.model(), "opus");
        assert_eq!(
            picker.model_options(),
            ["opus"],
            "the dropdown reseeds with the new backend's declared model"
        );
        assert_eq!(settings.default_backend.as_deref(), Some("claude"));
    }

    #[test]
    fn switching_to_another_backend_leaves_the_running_model_alone() {
        let mut settings = settings_with_two_backends();
        let flag = Arc::new(Mutex::new("deepseek-v4-flash".to_string()));
        let mut picker = BackendPicker::new(&settings, Arc::clone(&flag));

        picker.switch_backend("claude".to_string(), &mut settings);

        assert_eq!(
            flag.lock().unwrap().as_str(),
            "deepseek-v4-flash",
            "the running backend must not be sent another entry's model"
        );
    }

    #[test]
    fn switching_model_on_the_running_backend_writes_the_shared_handle() {
        let mut settings = settings_with_two_backends();
        let flag = Arc::new(Mutex::new("deepseek-v4-flash".to_string()));
        let mut picker = BackendPicker::new(&settings, Arc::clone(&flag));

        assert!(picker.switch_model("deepseek-v4-pro".to_string(), &mut settings));

        assert_eq!(flag.lock().unwrap().as_str(), "deepseek-v4-pro");
        assert_eq!(picker.model(), "deepseek-v4-pro");
    }

    #[test]
    fn switching_model_on_another_backend_does_not_write_the_shared_handle() {
        let mut settings = settings_with_two_backends();
        let flag = Arc::new(Mutex::new("deepseek-v4-flash".to_string()));
        let mut picker = BackendPicker::new(&settings, Arc::clone(&flag));
        picker.switch_backend("claude".to_string(), &mut settings);
        picker.set_selected_idx(0); // now pointing at "claude"

        picker.switch_model("sonnet".to_string(), &mut settings);

        assert_eq!(
            flag.lock().unwrap().as_str(),
            "deepseek-v4-flash",
            "only the running backend's model reaches the agent"
        );
    }

    #[test]
    fn switching_model_persists_onto_the_selected_backends_entry() {
        let mut settings = settings_with_two_backends();
        let mut picker = picker_on(&settings, "deepseek-v4-flash");
        picker.switch_model("deepseek-v4-pro".to_string(), &mut settings);
        let cfg = settings.resolve_backend("deepseek").expect("must resolve");
        assert_eq!(cfg.model(), "deepseek-v4-pro");
    }

    #[test]
    fn a_fetched_list_for_the_selected_backend_replaces_the_options() {
        let settings = settings_with_two_backends();
        let mut picker = picker_on(&settings, "deepseek-v4-flash");
        picker.apply_fetched_list(
            "deepseek",
            vec!["deepseek-v4-flash".into(), "deepseek-v4-pro".into()],
        );
        assert_eq!(
            picker.model_options(),
            ["deepseek-v4-flash", "deepseek-v4-pro"]
        );
    }

    #[test]
    fn a_fetched_list_always_holds_the_current_model() {
        let settings = settings_with_two_backends();
        let mut picker = picker_on(&settings, "deepseek-v4-flash");
        picker.apply_fetched_list("deepseek", vec!["something-else".into()]);
        assert!(
            picker
                .model_options()
                .contains(&"deepseek-v4-flash".to_string()),
            "the dropdown must always offer its own selection"
        );
    }

    #[test]
    fn a_stale_fetched_list_is_dropped() {
        let settings = settings_with_two_backends();
        let mut picker = picker_on(&settings, "deepseek-v4-flash");
        picker.apply_fetched_list("claude", vec!["opus".into(), "sonnet".into()]);
        assert_eq!(
            picker.model_options(),
            ["deepseek-v4-flash"],
            "a result for another backend must not land here"
        );
    }

    #[test]
    fn draining_an_empty_channel_yields_nothing() {
        let settings = settings_with_two_backends();
        let mut picker = picker_on(&settings, "deepseek-v4-flash");
        assert!(picker.drain_fetched_lists().is_empty());
    }

    #[test]
    fn draining_takes_every_queued_result() {
        let settings = settings_with_two_backends();
        let mut picker = picker_on(&settings, "deepseek-v4-flash");
        picker
            .list_tx
            .send(("deepseek".to_string(), vec!["a".into()]))
            .expect("send");
        picker
            .list_tx
            .send(("claude".to_string(), vec!["b".into()]))
            .expect("send");
        assert_eq!(picker.drain_fetched_lists().len(), 2);
        assert!(picker.drain_fetched_lists().is_empty());
    }

    #[test]
    fn apply_backend_model_ignores_an_unknown_backend() {
        let mut settings = settings_with_two_backends();
        apply_backend_model(&mut settings, "not_a_backend", "whatever");
        let cfg = settings.resolve_backend("deepseek").expect("must resolve");
        assert_eq!(cfg.model(), "deepseek-v4-flash");
    }

    #[test]
    fn apply_backend_model_writes_a_claude_entry_too() {
        let mut settings = settings_with_two_backends();
        apply_backend_model(&mut settings, "claude", "haiku");
        let cfg = settings.resolve_backend("claude").expect("must resolve");
        assert_eq!(cfg.model(), "haiku");
    }
}
