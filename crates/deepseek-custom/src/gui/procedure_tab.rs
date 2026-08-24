//! Procedure tab state and rendering.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, RichText};
use tokio::sync::mpsc;
use tracing::info;

use crate::config::settings::{ApiProvider, BackendConfig, Settings};
use crate::procedure::{
    OpenSpecChange, OpenSpecInput, ProcedureAttemptDisposition, ProcedureCommand,
    ProcedureProgress, ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun,
    ProcedureRunId, ProcedureRunRequest, ProcedureScratchpad, ProcedureStage,
    ProcedureTerminalDisposition,
};

use super::cascade_tab::backend_combo;

/// Current procedure-only UI state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcedureStatus {
    Idle,
    Running { message: String },
    Finished(ProcedureTerminalDisposition),
    Error { message: String },
}

/// Stable presentation states shown by the Procedure view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureViewState {
    Idle,
    Running,
    AwaitingReview,
    Approved,
    Rejected,
    Failed,
    Interrupted,
}

impl ProcedureViewState {
    /// Exact short label rendered for this state.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "ready",
            Self::Running => "running",
            Self::AwaitingReview => "awaiting review",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Selection, channel ownership, progress, and the latest report.
pub struct ProcedureTab {
    command_tx: Option<mpsc::UnboundedSender<ProcedureCommand>>,
    progress_rx: Option<mpsc::UnboundedReceiver<ProcedureProgress>>,
    interrupt: Option<Arc<AtomicBool>>,
    changes: Vec<OpenSpecChange>,
    change_error: Option<String>,
    selected_change: String,
    selected_task: String,
    backend_names: Vec<String>,
    backend: String,
    status: ProcedureStatus,
    active_run: Option<ProcedureRunId>,
    dispatch_backend: Option<String>,
    dispatch_model: Option<String>,
    attempt_number: Option<u8>,
    latest_run: Option<ProcedureRun>,
    report_path: Option<PathBuf>,
    review_error: Option<String>,
    reports: ProcedureReportStore,
}

impl ProcedureTab {
    /// Load pending OpenSpec tasks and schema-capable backend choices.
    pub fn new(settings: &Settings, project_root: &Path) -> Self {
        let backend_names = localization_backend_names(settings);
        let saved_backend = settings
            .procedure()
            .and_then(|procedure| procedure.localization_backend.clone());
        let backend = saved_backend
            .filter(|saved| backend_names.contains(saved))
            .or_else(|| backend_names.first().cloned())
            .unwrap_or_default();
        let mut tab = Self {
            command_tx: None,
            progress_rx: None,
            interrupt: None,
            changes: Vec::new(),
            change_error: None,
            selected_change: String::new(),
            selected_task: String::new(),
            backend_names,
            backend,
            status: ProcedureStatus::Idle,
            active_run: None,
            dispatch_backend: None,
            dispatch_model: None,
            attempt_number: None,
            latest_run: None,
            report_path: None,
            review_error: None,
            reports: ProcedureReportStore::for_project(project_root),
        };
        tab.refresh_changes(project_root);
        tab
    }

    /// Attach the dedicated command, progress, and interruption seams.
    pub fn attach(
        &mut self,
        command_tx: mpsc::UnboundedSender<ProcedureCommand>,
        progress_rx: mpsc::UnboundedReceiver<ProcedureProgress>,
        interrupt: Arc<AtomicBool>,
    ) {
        self.command_tx = Some(command_tx);
        self.progress_rx = Some(progress_rx);
        self.interrupt = Some(interrupt);
    }

    /// Request cancellation of the owned run.
    pub fn request_stop(&self) {
        if self.is_running()
            && let Some(interrupt) = &self.interrupt
        {
            interrupt.store(true, Ordering::SeqCst);
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status, ProcedureStatus::Running { .. })
    }

    pub fn status(&self) -> &ProcedureStatus {
        &self.status
    }

    pub fn changes(&self) -> &[OpenSpecChange] {
        &self.changes
    }

    pub fn backend_names(&self) -> &[String] {
        &self.backend_names
    }

    pub fn selected_change(&self) -> &str {
        &self.selected_change
    }

    pub fn selected_task(&self) -> &str {
        &self.selected_task
    }

    pub fn latest_run(&self) -> Option<&ProcedureRun> {
        self.latest_run.as_ref()
    }

    pub fn latest_report_path(&self) -> Option<&Path> {
        self.report_path.as_deref()
    }

    /// State label selected from progress and the persisted review decision.
    pub fn view_state(&self) -> ProcedureViewState {
        match &self.status {
            ProcedureStatus::Idle => ProcedureViewState::Idle,
            ProcedureStatus::Running { .. } => ProcedureViewState::Running,
            ProcedureStatus::Error { .. } => ProcedureViewState::Failed,
            ProcedureStatus::Finished(ProcedureTerminalDisposition::Interrupted) => {
                ProcedureViewState::Interrupted
            }
            ProcedureStatus::Finished(ProcedureTerminalDisposition::Failed { .. }) => {
                ProcedureViewState::Failed
            }
            ProcedureStatus::Finished(
                ProcedureTerminalDisposition::AwaitingReview
                | ProcedureTerminalDisposition::Succeeded,
            ) => match self.latest_run.as_ref().map(|run| run.review_disposition) {
                Some(ProcedureReviewDisposition::Approved) => ProcedureViewState::Approved,
                Some(ProcedureReviewDisposition::Rejected) => ProcedureViewState::Rejected,
                Some(
                    ProcedureReviewDisposition::Pending
                    | ProcedureReviewDisposition::LegacyUnreviewed,
                )
                | None => ProcedureViewState::AwaitingReview,
            },
        }
    }

    /// Most recent review failure, retained until a review succeeds or a new run starts.
    pub fn review_error(&self) -> Option<&str> {
        self.review_error.as_deref()
    }

    /// Receive progress without entering the routed chat event path.
    pub fn drain_progress(&mut self) {
        let Some(mut receiver) = self.progress_rx.take() else {
            return;
        };
        while let Ok(progress) = receiver.try_recv() {
            self.handle_progress(progress);
        }
        self.progress_rx = Some(receiver);
    }

    pub fn handle_progress(&mut self, progress: ProcedureProgress) {
        match progress {
            ProcedureProgress::RunStarted {
                run_id,
                change_id,
                task_id,
            } => {
                self.active_run = Some(run_id);
                self.status = ProcedureStatus::Running {
                    message: format!("Validating {change_id} task {task_id}"),
                };
            }
            ProcedureProgress::StageStarted { stage, .. } => {
                self.status = ProcedureStatus::Running {
                    message: stage_label(stage, "running"),
                };
            }
            ProcedureProgress::StageCompleted { stage, .. } => {
                self.status = ProcedureStatus::Running {
                    message: stage_label(stage, "complete"),
                };
            }
            ProcedureProgress::AttemptStarted {
                number,
                backend,
                model,
                ..
            } => {
                self.attempt_number = Some(number);
                self.dispatch_backend = Some(backend);
                self.dispatch_model = Some(model);
                self.status = ProcedureStatus::Running {
                    message: format!("Localization attempt {number} of 2"),
                };
            }
            ProcedureProgress::AttemptRejected { number, error, .. } => {
                self.status = ProcedureStatus::Running {
                    message: format!("Attempt {number} rejected: {error}"),
                };
            }
            ProcedureProgress::AttemptAccepted {
                number, targets, ..
            } => {
                self.status = ProcedureStatus::Running {
                    message: format!("Attempt {number} accepted {targets} target(s)"),
                };
            }
            ProcedureProgress::RunFinished {
                run_id,
                disposition,
            } => match self.reports.load(&run_id) {
                Ok(run) => {
                    self.report_path = Some(self.reports.report_path(&run_id));
                    self.latest_run = Some(run);
                    self.review_error = None;
                    self.active_run = None;
                    self.status = ProcedureStatus::Finished(disposition);
                }
                Err(error) => {
                    self.active_run = None;
                    self.status = ProcedureStatus::Error {
                        message: format!("could not load completed procedure report: {error}"),
                    };
                }
            },
            ProcedureProgress::RunFailed { message } => {
                self.active_run = None;
                self.status = ProcedureStatus::Error { message };
            }
        }
    }

    /// Render the form and latest result. Returns true when settings changed.
    pub(crate) fn render(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        project_root: &Path,
    ) -> bool {
        let mut dirty = false;
        ui.heading("Procedure");
        ui.label("Read-only OpenSpec validation and repository localization.");
        ui.separator();

        ui.add_enabled_ui(!self.is_running(), |ui| {
            dirty |= self.render_selection(ui, settings, project_root);
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.can_run(), egui::Button::new("Run"))
                .clicked()
            {
                self.start_run();
            }
            if ui
                .add_enabled(self.is_running(), egui::Button::new("Stop"))
                .clicked()
            {
                self.request_stop();
            }
        });
        self.render_status(ui);
        self.render_result(ui);
        dirty
    }

    fn render_selection(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        project_root: &Path,
    ) -> bool {
        let mut dirty = false;
        let change_ids: Vec<String> = self
            .changes
            .iter()
            .map(|change| change.id.clone())
            .collect();
        if simple_combo(ui, "Active change", &mut self.selected_change, &change_ids) {
            self.select_first_task();
        }

        let tasks: Vec<(String, String)> = self
            .selected_change_data()
            .map(|change| {
                change
                    .tasks
                    .iter()
                    .map(|task| (task.id.clone(), task.text.clone()))
                    .collect()
            })
            .unwrap_or_default();
        task_combo(ui, &mut self.selected_task, &tasks);

        if backend_combo(
            ui,
            "Localization backend",
            &mut self.backend,
            &self.backend_names,
            false,
        ) {
            settings.procedure_mut().localization_backend = Some(self.backend.clone());
            dirty = true;
        }
        if ui.button("Refresh changes").clicked() {
            self.refresh_changes(project_root);
        }
        dirty
    }

    fn render_status(&self, ui: &mut egui::Ui) {
        match &self.status {
            ProcedureStatus::Idle => {
                if let Some(error) = &self.change_error {
                    ui.label(RichText::new(error).color(Color32::LIGHT_RED));
                } else {
                    ui.label("Ready");
                }
            }
            ProcedureStatus::Running { message } => {
                ui.spinner();
                ui.label(format!("Status: {}", self.view_state().label()));
                ui.label(message);
            }
            ProcedureStatus::Finished(disposition) => {
                let label = self.view_state().label();
                match disposition {
                    ProcedureTerminalDisposition::Failed { reason } => {
                        ui.label(
                            RichText::new(format!("Final status: {label}: {reason}"))
                                .color(Color32::LIGHT_RED),
                        );
                    }
                    _ => {
                        ui.label(format!("Final status: {label}"));
                    }
                }
            }
            ProcedureStatus::Error { message } => {
                ui.label(
                    RichText::new(format!("Final status: failed: {message}"))
                        .color(Color32::LIGHT_RED),
                );
            }
        }
        if let (Some(backend), Some(model)) = (&self.dispatch_backend, &self.dispatch_model) {
            ui.label(format!("Dispatch: {backend} / {model}"));
        }
        if let Some(attempt) = self.attempt_number {
            ui.label(format!("Attempts observed: {attempt}"));
        }
    }

    fn render_result(&mut self, ui: &mut egui::Ui) {
        let Some(run) = &self.latest_run else {
            return;
        };
        ui.separator();
        ui.label(format!("Change: {}", run.change_id));
        ui.label(format!(
            "Task: {} {}",
            run.selected_task.id, run.selected_task.text
        ));
        if let Some(validation) = &run.validation {
            ui.label(format!(
                "OpenSpec validation: exit {:?}",
                validation.exit_code
            ));
        }
        ui.label(format!("Review: {}", run.review_disposition));
        ui.label(format!("Attempts: {}", run.attempts.len()));
        for attempt in &run.attempts {
            ui.label(format!(
                "Attempt {}: {} / {} ({:?})",
                attempt.number, attempt.backend, attempt.model, attempt.disposition
            ));
            if attempt.disposition == ProcedureAttemptDisposition::Accepted {
                ui.label("Proposed targets:");
                for target in &attempt.targets {
                    let symbol = target
                        .symbol
                        .as_deref()
                        .map(|symbol| format!("::{symbol}"))
                        .unwrap_or_default();
                    ui.label(format!("{}{}", target.path, symbol));
                    ui.label(RichText::new(&target.evidence).color(Color32::GRAY));
                }
            }
        }
        if let Some(path) = &self.report_path {
            ui.label(format!("Report: {}", path.display()));
        }
        if let Some(error) = &self.review_error {
            ui.label(RichText::new(error).color(Color32::LIGHT_RED));
        }
        if self.can_review_latest() {
            ui.horizontal(|ui| {
                if ui.button("Approve").clicked() {
                    self.apply_review(ProcedureReviewDisposition::Approved);
                }
                if ui.button("Reject").clicked() {
                    self.apply_review(ProcedureReviewDisposition::Rejected);
                }
            });
        }
    }

    fn can_review_latest(&self) -> bool {
        self.view_state() == ProcedureViewState::AwaitingReview
            && self.latest_run.as_ref().is_some_and(|run| {
                run.review_disposition == ProcedureReviewDisposition::Pending
                    && run.terminal_disposition
                        == Some(ProcedureTerminalDisposition::AwaitingReview)
            })
    }

    fn apply_review(&mut self, disposition: ProcedureReviewDisposition) {
        let Some(run_id) = self.latest_run.as_ref().map(|run| run.id) else {
            return;
        };
        let result = match disposition {
            ProcedureReviewDisposition::Approved => self.reports.approve(&run_id),
            ProcedureReviewDisposition::Rejected => self.reports.reject(&run_id),
            ProcedureReviewDisposition::Pending | ProcedureReviewDisposition::LegacyUnreviewed => {
                return;
            }
        };
        match result {
            Ok(run) => {
                self.latest_run = Some(run);
                self.review_error = None;
            }
            Err(error) => {
                self.review_error = Some(error.to_string());
            }
        }
    }

    fn can_run(&self) -> bool {
        !self.is_running()
            && self.command_tx.is_some()
            && !self.selected_change.is_empty()
            && !self.selected_task.is_empty()
            && !self.backend.is_empty()
    }

    fn start_run(&mut self) {
        if !self.can_run() {
            return;
        }
        let Some(command_tx) = &self.command_tx else {
            return;
        };
        if let Some(interrupt) = &self.interrupt {
            interrupt.store(false, Ordering::SeqCst);
        }
        let command = ProcedureCommand {
            backend: self.backend.clone(),
            request: ProcedureRunRequest {
                change_id: self.selected_change.clone(),
                task_id: self.selected_task.clone(),
                scratchpad: ProcedureScratchpad::default(),
            },
        };
        info!(change = %command.request.change_id, task = %command.request.task_id, backend = %command.backend, "procedure run requested");
        if command_tx.send(command).is_err() {
            self.status = ProcedureStatus::Error {
                message: "procedure executor is unavailable".to_string(),
            };
            return;
        }
        self.latest_run = None;
        self.report_path = None;
        self.dispatch_backend = None;
        self.dispatch_model = None;
        self.attempt_number = None;
        self.review_error = None;
        self.status = ProcedureStatus::Running {
            message: "Queued".to_string(),
        };
    }

    fn refresh_changes(&mut self, project_root: &Path) {
        match OpenSpecInput::new(project_root).active_changes() {
            Ok(changes) => {
                self.changes = changes
                    .into_iter()
                    .filter(|change| !change.tasks.is_empty())
                    .collect();
                self.change_error = None;
                if !self
                    .changes
                    .iter()
                    .any(|change| change.id == self.selected_change)
                {
                    self.selected_change = self
                        .changes
                        .first()
                        .map(|change| change.id.clone())
                        .unwrap_or_default();
                }
                self.select_first_task_if_missing();
            }
            Err(error) => {
                self.changes.clear();
                self.selected_change.clear();
                self.selected_task.clear();
                self.change_error = Some(error.to_string());
            }
        }
    }

    fn selected_change_data(&self) -> Option<&OpenSpecChange> {
        self.changes
            .iter()
            .find(|change| change.id == self.selected_change)
    }

    fn select_first_task(&mut self) {
        self.selected_task = self
            .selected_change_data()
            .and_then(|change| change.tasks.first())
            .map(|task| task.id.clone())
            .unwrap_or_default();
    }

    fn select_first_task_if_missing(&mut self) {
        let exists = self.selected_change_data().is_some_and(|change| {
            change
                .tasks
                .iter()
                .any(|task| task.id == self.selected_task)
        });
        if !exists {
            self.select_first_task();
        }
    }

    #[cfg(feature = "test-support")]
    pub fn start_for_test(&mut self) {
        self.start_run();
    }

    #[cfg(feature = "test-support")]
    pub fn review_actions_available_for_test(&self) -> bool {
        self.can_review_latest()
    }

    #[cfg(feature = "test-support")]
    pub fn approve_for_test(&mut self) {
        self.apply_review(ProcedureReviewDisposition::Approved);
    }

    #[cfg(feature = "test-support")]
    pub fn reject_for_test(&mut self) {
        self.apply_review(ProcedureReviewDisposition::Rejected);
    }
}

impl Drop for ProcedureTab {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Named backends that can enforce the localization JSON Schema.
pub fn localization_backend_names(settings: &Settings) -> Vec<String> {
    let mut names: Vec<String> = settings
        .backends()
        .into_iter()
        .flat_map(|backends| backends.iter())
        .filter_map(|(name, backend)| match backend {
            BackendConfig::Api {
                provider: ApiProvider::Ollama,
                ..
            } => Some(name.clone()),
            _ => None,
        })
        .collect();
    names.sort();
    names
}

fn simple_combo(ui: &mut egui::Ui, label: &str, current: &mut String, values: &[String]) -> bool {
    let before = current.clone();
    let selected = if current.is_empty() {
        "(none)".to_string()
    } else {
        current.clone()
    };
    egui::ComboBox::from_label(label)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for value in values {
                ui.selectable_value(current, value.clone(), value);
            }
        });
    *current != before
}

fn task_combo(ui: &mut egui::Ui, current: &mut String, tasks: &[(String, String)]) {
    let selected = tasks
        .iter()
        .find(|(id, _)| id == current)
        .map(|(id, text)| format!("{id} {text}"))
        .unwrap_or_else(|| "(none)".to_string());
    egui::ComboBox::from_label("Pending task")
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (id, text) in tasks {
                ui.selectable_value(current, id.clone(), format!("{id} {text}"));
            }
        });
}

fn stage_label(stage: ProcedureStage, suffix: &str) -> String {
    let name = match stage {
        ProcedureStage::SpecValidation => "OpenSpec validation",
        ProcedureStage::Localization => "Localization",
        ProcedureStage::Finished => "Procedure",
    };
    format!("{name} {suffix}")
}
