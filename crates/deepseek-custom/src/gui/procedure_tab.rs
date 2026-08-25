//! Procedure tab state and rendering.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, RichText};
use tokio::sync::mpsc;
use tracing::info;

use crate::api::models::list_models;
use crate::config::settings::{ApiProvider, BackendConfig, Settings};
use crate::procedure::{
    BoundedVerifierOutput, GitApplyPhase, GitApplyResult, OpenSpecChange, OpenSpecInput,
    PatchPreview, PatchPreviewId, PatchPreviewRequest, ProcedureApplyProgress,
    ProcedureAttemptDisposition, ProcedureCommand, ProcedureProgress, ProcedureReportStore,
    ProcedureReviewDecision, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureRunRequest, ProcedureScratchpad, ProcedureStage, ProcedureTerminalDisposition,
    PromotionRecoveryEvidence, PromotionResult, RouteOverride, SnapshotProgress,
    StalePromotionPath, VerifierGateDisposition, VerifierReport,
};

const MISSING_VERIFIER_COMMANDS_MESSAGE: &str =
    "Apply unavailable: configure at least one command in procedure.verifier_commands.";

use super::cascade_tab::backend_combo;

/// Current procedure-only UI state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcedureStatus {
    Idle,
    Running { message: String },
    Finished(ProcedureTerminalDisposition),
    Error { message: String },
}

/// State of the independent patch-preview action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchPreviewStatus {
    Idle,
    Running,
    Finished,
    Error { message: String },
}

/// Current state of the isolated Apply interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcedureApplyStatus {
    Idle,
    Snapshotting,
    PatchGate { phase: GitApplyPhase },
    Verifying { index: usize, command: String },
    Conflict,
    Promoting,
    Succeeded,
    Failed,
    Interrupted,
    Terminal(ProcedureTerminalDisposition),
}

impl ProcedureApplyStatus {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Idle => "ready",
            Self::Snapshotting => "snapshotting",
            Self::PatchGate { .. } => "patch gate",
            Self::Verifying { .. } => "verifying",
            Self::Conflict => "stale conflict",
            Self::Promoting => "promoting",
            Self::Succeeded => "promotion succeeded",
            Self::Failed => "promotion failed",
            Self::Interrupted => "interrupted",
            Self::Terminal(_) => "terminal",
        }
    }
}

/// Promotion failure evidence retained by the Apply view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionFailureView {
    pub message: String,
    pub recovery: Option<PromotionRecoveryEvidence>,
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
    local_backend_names: Vec<String>,
    frontier_backend_names: Vec<String>,
    local_backend: String,
    frontier_backend: String,
    local_model: String,
    frontier_model: String,
    model_options: HashMap<String, Vec<String>>,
    model_list_tx: mpsc::UnboundedSender<(String, Vec<String>)>,
    model_list_rx: mpsc::UnboundedReceiver<(String, Vec<String>)>,
    route_override: RouteOverride,
    status: ProcedureStatus,
    active_run: Option<ProcedureRunId>,
    dispatch_backend: Option<String>,
    dispatch_model: Option<String>,
    attempt_number: Option<u8>,
    latest_run: Option<ProcedureRun>,
    report_path: Option<PathBuf>,
    review_error: Option<String>,
    review_in_flight: bool,
    reports: ProcedureReportStore,
    preview_status: PatchPreviewStatus,
    active_preview: Option<PatchPreviewId>,
    latest_preview: Option<PatchPreview>,
    preview_report_path: Option<PathBuf>,
    apply_run_id: Option<ProcedureRunId>,
    apply_in_flight: bool,
    apply_requested: bool,
    apply_status: ProcedureApplyStatus,
    snapshot_progress: Option<SnapshotProgress>,
    patch_gate_results: Vec<GitApplyResult>,
    verifier_gate_evidence: Vec<(usize, crate::procedure::VerifierGateEvidence)>,
    verification_report: Option<VerifierReport>,
    conflicts: Vec<StalePromotionPath>,
    promotion_result: Option<PromotionResult>,
    promotion_failure: Option<PromotionFailureView>,
    apply_terminal: Option<ProcedureTerminalDisposition>,
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
        let local_backend_names = local_patch_backend_names(settings);
        let frontier_backend_names = frontier_patch_backend_names(settings);
        let procedure = settings.procedure();
        let local_backend = selected_backend(
            procedure.and_then(|value| value.local_patch_backend.as_deref()),
            &local_backend_names,
        );
        let frontier_backend = selected_backend(
            procedure.and_then(|value| value.frontier_patch_backend.as_deref()),
            &frontier_backend_names,
        );
        let local_model = configured_model(settings, &local_backend);
        let frontier_model = configured_model(settings, &frontier_backend);
        let mut model_options = HashMap::new();
        seed_model_option(&mut model_options, &local_backend, &local_model);
        seed_model_option(&mut model_options, &frontier_backend, &frontier_model);
        let (model_list_tx, model_list_rx) = mpsc::unbounded_channel();
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
            local_backend_names,
            frontier_backend_names,
            local_backend,
            frontier_backend,
            local_model,
            frontier_model,
            model_options,
            model_list_tx,
            model_list_rx,
            route_override: RouteOverride::Automatic,
            status: ProcedureStatus::Idle,
            active_run: None,
            dispatch_backend: None,
            dispatch_model: None,
            attempt_number: None,
            latest_run: None,
            report_path: None,
            review_error: None,
            review_in_flight: false,
            reports: ProcedureReportStore::for_project(project_root),
            preview_status: PatchPreviewStatus::Idle,
            active_preview: None,
            latest_preview: None,
            preview_report_path: None,
            apply_run_id: None,
            apply_in_flight: false,
            apply_requested: false,
            apply_status: ProcedureApplyStatus::Idle,
            snapshot_progress: None,
            patch_gate_results: Vec::new(),
            verifier_gate_evidence: Vec::new(),
            verification_report: None,
            conflicts: Vec::new(),
            promotion_result: None,
            promotion_failure: None,
            apply_terminal: None,
        };
        tab.spawn_model_fetches(settings);
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
            || self.preview_status == PatchPreviewStatus::Running
            || self.apply_in_flight
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

    pub fn local_backend_names(&self) -> &[String] {
        &self.local_backend_names
    }

    pub fn frontier_backend_names(&self) -> &[String] {
        &self.frontier_backend_names
    }

    pub fn local_model_options(&self) -> &[String] {
        self.model_options
            .get(&self.local_backend)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn frontier_model_options(&self) -> &[String] {
        self.model_options
            .get(&self.frontier_backend)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn route_override(&self) -> RouteOverride {
        self.route_override
    }

    pub fn preview_status(&self) -> &PatchPreviewStatus {
        &self.preview_status
    }

    pub fn latest_preview(&self) -> Option<&PatchPreview> {
        self.latest_preview.as_ref()
    }

    pub fn latest_preview_report_path(&self) -> Option<&Path> {
        self.preview_report_path.as_deref()
    }

    fn verifier_commands(settings: &Settings) -> &[String] {
        settings
            .procedure()
            .map(|procedure| procedure.verifier_commands.as_slice())
            .unwrap_or_default()
    }

    fn has_finished_preview(&self) -> bool {
        self.preview_status == PatchPreviewStatus::Finished && self.latest_preview.is_some()
    }

    fn apply_enabled(&self, settings: &Settings) -> bool {
        self.has_finished_preview()
            && !self.is_running()
            && !self.apply_requested
            && !Self::verifier_commands(settings).is_empty()
    }

    fn apply_missing_configuration(&self, settings: &Settings) -> bool {
        self.has_finished_preview() && Self::verifier_commands(settings).is_empty()
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

    pub fn apply_status(&self) -> &ProcedureApplyStatus {
        &self.apply_status
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
        self.drain_model_lists();
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
                if self.active_run.is_some_and(|active| active != run_id)
                    || (self.active_run.is_none() && self.latest_run.is_some())
                {
                    return;
                }
                self.active_run = Some(run_id);
                self.status = ProcedureStatus::Running {
                    message: format!("Validating {change_id} task {task_id}"),
                };
            }
            ProcedureProgress::StageStarted { run_id, stage } => {
                if !self.owns_active_run(run_id) {
                    return;
                }
                self.status = ProcedureStatus::Running {
                    message: stage_label(stage, "running"),
                };
            }
            ProcedureProgress::StageCompleted { run_id, stage } => {
                if !self.owns_active_run(run_id) {
                    return;
                }
                self.status = ProcedureStatus::Running {
                    message: stage_label(stage, "complete"),
                };
            }
            ProcedureProgress::AttemptStarted {
                run_id,
                number,
                backend,
                model,
            } => {
                if !self.owns_active_run(run_id) {
                    return;
                }
                self.attempt_number = Some(number);
                self.dispatch_backend = Some(backend);
                self.dispatch_model = Some(model);
                self.status = ProcedureStatus::Running {
                    message: format!("Localization attempt {number} of 2"),
                };
            }
            ProcedureProgress::AttemptRejected {
                run_id,
                number,
                error,
            } => {
                if !self.owns_active_run(run_id) {
                    return;
                }
                self.status = ProcedureStatus::Running {
                    message: format!("Attempt {number} rejected: {error}"),
                };
            }
            ProcedureProgress::AttemptAccepted {
                run_id,
                number,
                targets,
            } => {
                if !self.owns_active_run(run_id) {
                    return;
                }
                self.status = ProcedureStatus::Running {
                    message: format!("Attempt {number} accepted {targets} target(s)"),
                };
            }
            ProcedureProgress::RunFinished {
                run_id,
                disposition,
            } => {
                if !self.owns_active_run(run_id) {
                    return;
                }
                match self.reports.load(&run_id) {
                    Ok(run) => {
                        self.report_path = Some(self.reports.report_path(&run_id));
                        self.latest_run = Some(run);
                        self.review_error = None;
                        self.review_in_flight = false;
                        self.active_run = None;
                        self.status = ProcedureStatus::Finished(disposition);
                    }
                    Err(error) => {
                        self.active_run = None;
                        self.status = ProcedureStatus::Error {
                            message: format!("could not load completed procedure report: {error}"),
                        };
                    }
                }
            }
            ProcedureProgress::ReviewSucceeded {
                run_id,
                disposition: _,
            } => {
                if !self.displays_run(run_id) {
                    return;
                }
                self.review_in_flight = false;
                match self.reports.load(&run_id) {
                    Ok(run) => {
                        self.latest_run = Some(run);
                        self.review_error = None;
                    }
                    Err(error) => {
                        self.review_error = Some(format!(
                            "could not load reviewed procedure run {}: {error}",
                            run_id.as_str()
                        ));
                    }
                }
            }
            ProcedureProgress::ReviewFailed {
                run_id,
                disposition: _,
                error,
            } => {
                if !self.displays_run(run_id) {
                    return;
                }
                self.review_in_flight = false;
                self.review_error = Some(error);
            }
            ProcedureProgress::RunFailed { run_id, message } => {
                if !self.owns_active_run(run_id) {
                    return;
                }
                self.active_run = None;
                self.status = ProcedureStatus::Error { message };
            }
            ProcedureProgress::PreviewStarted { preview_id } => {
                if self.active_preview == Some(preview_id) {
                    self.preview_status = PatchPreviewStatus::Running;
                }
            }
            ProcedureProgress::PreviewFinished {
                preview_id,
                preview,
                report_path,
            } => {
                if self.active_preview != Some(preview_id) {
                    return;
                }
                self.active_preview = None;
                self.latest_preview = Some(*preview);
                self.preview_report_path = Some(report_path);
                self.preview_status = PatchPreviewStatus::Finished;
            }
            ProcedureProgress::PreviewFailed {
                preview_id,
                message,
            } => {
                if self.active_preview != Some(preview_id) {
                    return;
                }
                self.active_preview = None;
                self.preview_status = PatchPreviewStatus::Error { message };
            }
            ProcedureProgress::Apply { run_id, progress } => {
                self.handle_apply_progress(run_id, *progress);
            }
        }
    }

    fn handle_apply_progress(&mut self, run_id: ProcedureRunId, progress: ProcedureApplyProgress) {
        if matches!(&progress, ProcedureApplyProgress::Started) {
            if self.apply_in_flight && self.apply_run_id != Some(run_id) {
                return;
            }
            self.apply_run_id = Some(run_id);
            self.apply_in_flight = true;
            self.apply_requested = false;
            self.snapshot_progress = None;
            self.patch_gate_results.clear();
            self.verifier_gate_evidence.clear();
            self.verification_report = None;
            self.conflicts.clear();
            self.promotion_result = None;
            self.promotion_failure = None;
            self.apply_terminal = None;
            self.apply_status = ProcedureApplyStatus::Snapshotting;
            return;
        }
        if self.apply_run_id != Some(run_id) {
            return;
        }

        match progress {
            ProcedureApplyProgress::Started => unreachable!("handled before state matching"),
            ProcedureApplyProgress::SnapshotStarted => {
                self.apply_status = ProcedureApplyStatus::Snapshotting;
            }
            ProcedureApplyProgress::SnapshotProgress { progress } => {
                self.snapshot_progress = Some(progress);
                self.apply_status = ProcedureApplyStatus::Snapshotting;
            }
            ProcedureApplyProgress::PatchGateStarted { phase } => {
                self.apply_status = ProcedureApplyStatus::PatchGate { phase };
            }
            ProcedureApplyProgress::PatchGateCompleted { result } => {
                self.patch_gate_results.push(*result);
            }
            ProcedureApplyProgress::VerifierGateStarted { index, command } => {
                self.apply_status = ProcedureApplyStatus::Verifying { index, command };
            }
            ProcedureApplyProgress::VerifierGateCompleted { index, evidence } => {
                self.verifier_gate_evidence.push((index, *evidence));
            }
            ProcedureApplyProgress::VerificationFinished { report } => {
                self.verification_report = Some(report);
            }
            ProcedureApplyProgress::ConflictDetected { paths } => {
                self.conflicts = paths;
                self.apply_status = ProcedureApplyStatus::Conflict;
            }
            ProcedureApplyProgress::PromotionStarted => {
                self.apply_status = ProcedureApplyStatus::Promoting;
            }
            ProcedureApplyProgress::PromotionSucceeded { result } => {
                self.promotion_result = Some(result);
                self.apply_status = ProcedureApplyStatus::Succeeded;
            }
            ProcedureApplyProgress::PromotionFailed { message, recovery } => {
                self.promotion_failure = Some(PromotionFailureView { message, recovery });
                self.apply_status = ProcedureApplyStatus::Failed;
            }
            ProcedureApplyProgress::Finished { disposition } => {
                self.apply_in_flight = false;
                self.apply_terminal = Some(disposition.clone());
                self.apply_status = match disposition {
                    ProcedureTerminalDisposition::Interrupted => ProcedureApplyStatus::Interrupted,
                    ProcedureTerminalDisposition::Failed { .. } => ProcedureApplyStatus::Failed,
                    other => ProcedureApplyStatus::Terminal(other),
                };
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
        ui.label("OpenSpec localization and non-mutating patch preview.");
        ui.separator();

        ui.add_enabled_ui(!self.is_running(), |ui| {
            dirty |= self.render_selection(ui, settings, project_root);
        });

        ui.add_space(8.0);
        self.render_verifier_commands(ui, settings);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.can_run(), egui::Button::new("Run"))
                .clicked()
            {
                self.start_run();
            }
            if ui
                .add_enabled(self.can_preview(), egui::Button::new("Preview"))
                .clicked()
            {
                self.start_preview();
            }
            if ui
                .add_enabled(self.is_running(), egui::Button::new("Stop"))
                .clicked()
            {
                self.request_stop();
            }
            if ui
                .add_enabled(self.apply_enabled(settings), egui::Button::new("Apply"))
                .clicked()
            {
                self.start_apply(settings);
            }
        });
        self.render_status(ui);
        self.render_result(ui);
        self.render_preview(ui);
        self.render_apply(ui);
        if self.apply_missing_configuration(settings) {
            ui.label(RichText::new(MISSING_VERIFIER_COMMANDS_MESSAGE).color(Color32::LIGHT_RED));
        }
        dirty
    }

    fn render_verifier_commands(&self, ui: &mut egui::Ui, settings: &Settings) {
        if !self.has_finished_preview() {
            return;
        }
        let commands = Self::verifier_commands(settings);
        if commands.is_empty() {
            return;
        }
        ui.label("Verifier commands in execution order:");
        for command in Self::ordered_verifier_command_labels(settings) {
            ui.label(command);
        }
    }

    fn ordered_verifier_command_labels(settings: &Settings) -> Vec<String> {
        Self::verifier_commands(settings)
            .iter()
            .enumerate()
            .map(|(index, command)| format!("{}. {command}", index + 1))
            .collect()
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
        ui.separator();
        ui.label("Patch preview route");
        if backend_combo(
            ui,
            "Local backend",
            &mut self.local_backend,
            &self.local_backend_names,
            false,
        ) {
            self.local_model = configured_model(settings, &self.local_backend);
            seed_model_option(
                &mut self.model_options,
                &self.local_backend,
                &self.local_model,
            );
            spawn_model_list_fetch(self.model_list_tx.clone(), &self.local_backend, settings);
            settings.procedure_mut().local_patch_backend = Some(self.local_backend.clone());
            dirty = true;
        }
        let local_models = self
            .model_options
            .get(&self.local_backend)
            .cloned()
            .unwrap_or_default();
        simple_combo(ui, "Local model", &mut self.local_model, &local_models);

        if backend_combo(
            ui,
            "Frontier backend",
            &mut self.frontier_backend,
            &self.frontier_backend_names,
            false,
        ) {
            self.frontier_model = configured_model(settings, &self.frontier_backend);
            seed_model_option(
                &mut self.model_options,
                &self.frontier_backend,
                &self.frontier_model,
            );
            spawn_model_list_fetch(self.model_list_tx.clone(), &self.frontier_backend, settings);
            settings.procedure_mut().frontier_patch_backend = Some(self.frontier_backend.clone());
            dirty = true;
        }
        let frontier_models = self
            .model_options
            .get(&self.frontier_backend)
            .cloned()
            .unwrap_or_default();
        simple_combo(
            ui,
            "Frontier model",
            &mut self.frontier_model,
            &frontier_models,
        );
        route_override_combo(ui, &mut self.route_override);
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
                    self.apply_review(ProcedureReviewDecision::Approve);
                }
                if ui.button("Reject").clicked() {
                    self.apply_review(ProcedureReviewDecision::Reject);
                }
            });
        }
    }

    fn render_preview(&self, ui: &mut egui::Ui) {
        match &self.preview_status {
            PatchPreviewStatus::Idle => {}
            PatchPreviewStatus::Running => {
                ui.separator();
                ui.spinner();
                ui.label("Patch preview: running");
            }
            PatchPreviewStatus::Error { message } => {
                ui.separator();
                ui.label(
                    RichText::new(format!("Patch preview failed: {message}"))
                        .color(Color32::LIGHT_RED),
                );
            }
            PatchPreviewStatus::Finished => {}
        }
        let Some(preview) = &self.latest_preview else {
            return;
        };
        ui.separator();
        ui.heading("Patch preview");
        ui.label(format!("Change: {}", preview.change_id));
        ui.label(format!("Task: {}", preview.task_id));
        ui.label(format!(
            "Localization run: {}",
            preview.localization_run_id.as_str()
        ));
        ui.label(format!("Automatic route: {}", preview.route.automatic_tier));
        ui.label(format!("Override: {}", preview.route.selected_override));
        ui.label(format!("Effective route: {}", preview.route.effective_tier));
        ui.label(format!("Backend: {}", preview.backend));
        ui.label(format!("Model: {}", preview.model));
        ui.label("Signals:");
        for signal in &preview.route.signals {
            ui.label(format!("- {signal}"));
        }
        ui.label("Targets:");
        for target in &preview.targets {
            ui.label(format!("- {target}"));
        }
        ui.label(format!("Rationale: {}", preview.rationale));
        if let Some(path) = &self.preview_report_path {
            ui.label(format!("Preview report: {}", path.display()));
        }
        ui.label("Complete unified diff:");
        egui::ScrollArea::vertical()
            .id_salt("procedure-patch-preview-diff")
            .max_height(420.0)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(&preview.unified_diff).monospace())
                        .wrap()
                        .selectable(true),
                );
            });
    }

    fn render_apply(&self, ui: &mut egui::Ui) {
        if self.latest_preview.is_none()
            && !self.apply_requested
            && self.apply_status == ProcedureApplyStatus::Idle
        {
            return;
        }
        ui.separator();
        ui.heading("Apply");
        for line in self.apply_render_lines() {
            let is_error = line.contains("failed")
                || line.contains("interrupted")
                || line.contains("conflict")
                || line.contains("stale");
            if is_error {
                ui.label(RichText::new(line).color(Color32::LIGHT_RED));
            } else {
                ui.label(line);
            }
        }
    }

    fn apply_render_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let status = if self.apply_requested {
            "requested"
        } else if self.apply_status == ProcedureApplyStatus::Idle && self.has_finished_preview() {
            "ready"
        } else {
            self.apply_status.label()
        };
        lines.push(format!("Apply status: {status}"));
        if let Some(progress) = self.snapshot_progress {
            lines.push(format!(
                "Snapshotting verification workspace: {} file(s), {} / {} bytes",
                progress.files_copied, progress.bytes_copied, progress.total_bytes
            ));
        }
        for result in &self.patch_gate_results {
            lines.push(format!(
                "Patch gate {}: {} (exit {:?})",
                result.phase,
                pass_fail(result.success),
                result.status_code
            ));
            append_output_line(&mut lines, "Patch gate output", &result.stdout.text);
            append_output_line(&mut lines, "Patch gate error", &result.stderr.text);
            if let Some(error) = &result.error {
                lines.push(format!("Patch gate diagnostic: {error}"));
            }
        }
        if let Some(report) = &self.verification_report {
            for (index, gate) in report.gates.iter().enumerate() {
                append_verifier_gate_lines(
                    &mut lines,
                    index,
                    gate.command.as_str(),
                    &gate.disposition,
                    gate.result.as_ref().map(|result| &result.combined_output),
                );
            }
            lines.push(format!(
                "Verification eligibility: {}",
                pass_fail(report.eligibility.eligible)
            ));
            if let Some(reason) = &report.eligibility.reason {
                lines.push(format!("Verification failure evidence: {reason:?}"));
            }
            if report.stopped_after_failure {
                lines.push(format!(
                    "Verification stopped after gate {:?}",
                    report.first_failed_gate
                ));
            }
        } else {
            for (index, gate) in &self.verifier_gate_evidence {
                append_verifier_gate_lines(
                    &mut lines,
                    *index,
                    gate.command.as_str(),
                    &gate.disposition,
                    gate.result.as_ref().map(|result| &result.combined_output),
                );
            }
        }
        for conflict in &self.conflicts {
            lines.push(format!(
                "Stale conflict: {} (expected {:?}, actual {:?})",
                conflict.path, conflict.expected, conflict.actual
            ));
        }
        if let Some(result) = &self.promotion_result {
            lines.push(format!(
                "Promotion: succeeded ({} final path hash(es) verified)",
                result.final_fingerprints.len()
            ));
        }
        if let Some(failure) = &self.promotion_failure {
            lines.push(format!("Promotion: failed: {}", failure.message));
            match &failure.recovery {
                Some(recovery) if recovery.requires_recovery() => lines.push(format!(
                    "Recovery data retained: {}",
                    recovery
                        .recovery_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                Some(_) => lines.push("Rollback complete: recovery data removed".to_string()),
                None => {}
            }
        }
        if let Some(disposition) = &self.apply_terminal {
            lines.push(format!(
                "Apply terminal disposition: {}",
                terminal_disposition_label(disposition)
            ));
        }
        lines
    }

    fn can_review_latest(&self) -> bool {
        !self.review_in_flight
            && self.view_state() == ProcedureViewState::AwaitingReview
            && self.latest_run.as_ref().is_some_and(|run| {
                run.review_disposition == ProcedureReviewDisposition::Pending
                    && run.terminal_disposition
                        == Some(ProcedureTerminalDisposition::AwaitingReview)
            })
    }

    fn apply_review(&mut self, decision: ProcedureReviewDecision) {
        let Some(run_id) = self.latest_run.as_ref().map(|run| run.id) else {
            return;
        };
        let Some(command_tx) = &self.command_tx else {
            self.review_error = Some("procedure executor is unavailable".to_string());
            return;
        };
        if command_tx
            .send(ProcedureCommand::Review { run_id, decision })
            .is_err()
        {
            self.review_error = Some("procedure executor is unavailable".to_string());
            return;
        }
        self.review_in_flight = true;
        self.review_error = None;
    }

    fn owns_active_run(&self, run_id: ProcedureRunId) -> bool {
        self.active_run == Some(run_id)
    }

    fn displays_run(&self, run_id: ProcedureRunId) -> bool {
        self.active_run.is_none() && self.latest_run.as_ref().is_some_and(|run| run.id == run_id)
    }

    fn can_run(&self) -> bool {
        !self.is_running()
            && self.command_tx.is_some()
            && !self.selected_change.is_empty()
            && !self.selected_task.is_empty()
            && !self.backend.is_empty()
    }

    fn can_preview(&self) -> bool {
        !self.is_running()
            && self.command_tx.is_some()
            && !self.local_backend.is_empty()
            && !self.local_model.is_empty()
            && !self.frontier_backend.is_empty()
            && !self.frontier_model.is_empty()
            && self.latest_run.as_ref().is_some_and(|run| {
                run.change_id == self.selected_change
                    && run.selected_task.id == self.selected_task
                    && run.review_disposition == ProcedureReviewDisposition::Approved
            })
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
        let run_id = ProcedureRunId::new();
        let command = ProcedureCommand::Run {
            run_id,
            backend: self.backend.clone(),
            request: ProcedureRunRequest {
                change_id: self.selected_change.clone(),
                task_id: self.selected_task.clone(),
                scratchpad: ProcedureScratchpad::default(),
            },
        };
        info!(change = %self.selected_change, task = %self.selected_task, backend = %self.backend, "procedure run requested");
        if command_tx.send(command).is_err() {
            self.status = ProcedureStatus::Error {
                message: "procedure executor is unavailable".to_string(),
            };
            return;
        }
        self.active_run = Some(run_id);
        self.latest_run = None;
        self.report_path = None;
        self.dispatch_backend = None;
        self.dispatch_model = None;
        self.attempt_number = None;
        self.review_error = None;
        self.review_in_flight = false;
        self.status = ProcedureStatus::Running {
            message: "Queued".to_string(),
        };
    }

    fn start_preview(&mut self) {
        if !self.can_preview() {
            return;
        }
        let Some(command_tx) = &self.command_tx else {
            return;
        };
        let localization_run_id = self.latest_run.as_ref().map(|run| run.id).unwrap();
        let preview_id = PatchPreviewId::new();
        let command = ProcedureCommand::Preview {
            preview_id,
            request: PatchPreviewRequest {
                localization_run_id,
                change_id: self.selected_change.clone(),
                task_id: self.selected_task.clone(),
                route_override: self.route_override,
                local_backend: self.local_backend.clone(),
                local_model: self.local_model.clone(),
                frontier_backend: self.frontier_backend.clone(),
                frontier_model: self.frontier_model.clone(),
            },
        };
        if command_tx.send(command).is_err() {
            self.preview_status = PatchPreviewStatus::Error {
                message: "procedure executor is unavailable".to_string(),
            };
            return;
        }
        if let Some(interrupt) = &self.interrupt {
            interrupt.store(false, Ordering::SeqCst);
        }
        self.active_preview = Some(preview_id);
        self.latest_preview = None;
        self.preview_report_path = None;
        self.preview_status = PatchPreviewStatus::Running;
    }

    fn start_apply(&mut self, settings: &Settings) {
        if !self.apply_enabled(settings) {
            return;
        }
        let Some(command_tx) = &self.command_tx else {
            return;
        };
        let Some(preview) = self.latest_preview.as_ref() else {
            return;
        };
        let request = crate::procedure::ApplyRequest {
            localization_run_id: preview.localization_run_id,
            preview_id: preview.id,
            change_id: preview.change_id.clone(),
            task_id: preview.task_id.clone(),
        };
        let run_id = ProcedureRunId::new();
        if command_tx
            .send(ProcedureCommand::Apply { run_id, request })
            .is_err()
        {
            self.apply_status = ProcedureApplyStatus::Failed;
            return;
        }
        if let Some(interrupt) = &self.interrupt {
            interrupt.store(false, Ordering::SeqCst);
        }
        self.apply_run_id = Some(run_id);
        self.apply_in_flight = true;
        self.apply_requested = true;
        self.apply_status = ProcedureApplyStatus::Idle;
        self.snapshot_progress = None;
        self.patch_gate_results.clear();
        self.verifier_gate_evidence.clear();
        self.verification_report = None;
        self.conflicts.clear();
        self.promotion_result = None;
        self.promotion_failure = None;
        self.apply_terminal = None;
    }

    fn spawn_model_fetches(&self, settings: &Settings) {
        spawn_model_list_fetch(self.model_list_tx.clone(), &self.local_backend, settings);
        spawn_model_list_fetch(self.model_list_tx.clone(), &self.frontier_backend, settings);
    }

    fn drain_model_lists(&mut self) {
        while let Ok((backend, mut models)) = self.model_list_rx.try_recv() {
            let selected = if backend == self.local_backend {
                Some(self.local_model.clone())
            } else if backend == self.frontier_backend {
                Some(self.frontier_model.clone())
            } else {
                None
            };
            if let Some(selected) = selected
                && !models.contains(&selected)
            {
                models.push(selected);
            }
            self.model_options.insert(backend, models);
        }
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
    pub fn start_preview_for_test(&mut self) {
        self.start_preview();
    }

    #[cfg(feature = "test-support")]
    pub fn set_route_override_for_test(&mut self, route_override: RouteOverride) {
        self.route_override = route_override;
    }

    #[cfg(feature = "test-support")]
    pub fn send_model_list_for_test(&self, backend: impl Into<String>, models: Vec<String>) {
        let _ = self.model_list_tx.send((backend.into(), models));
    }

    #[cfg(feature = "test-support")]
    pub fn review_actions_available_for_test(&self) -> bool {
        self.can_review_latest()
    }

    #[cfg(feature = "test-support")]
    pub fn approve_for_test(&mut self) {
        self.apply_review(ProcedureReviewDecision::Approve);
    }

    #[cfg(feature = "test-support")]
    pub fn reject_for_test(&mut self) {
        self.apply_review(ProcedureReviewDecision::Reject);
    }

    #[cfg(feature = "test-support")]
    pub fn apply_enabled_for_test(&self, settings: &Settings) -> bool {
        self.apply_enabled(settings)
    }

    #[cfg(feature = "test-support")]
    pub fn apply_missing_configuration_for_test(
        &self,
        settings: &Settings,
    ) -> Option<&'static str> {
        self.apply_missing_configuration(settings)
            .then_some(MISSING_VERIFIER_COMMANDS_MESSAGE)
    }

    #[cfg(feature = "test-support")]
    pub fn verifier_command_labels_for_test(&self, settings: &Settings) -> Vec<String> {
        if !self.has_finished_preview() {
            return Vec::new();
        }
        Self::ordered_verifier_command_labels(settings)
    }

    /// Render the production Procedure view for deterministic external visual evidence.
    #[cfg(feature = "test-support")]
    pub fn render_for_test(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        project_root: &Path,
    ) -> bool {
        self.render(ui, settings, project_root)
    }

    #[cfg(feature = "test-support")]
    pub fn apply_render_lines_for_test(&self) -> Vec<String> {
        self.apply_render_lines()
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

/// Named backends that can enforce the local patch-envelope schema.
pub fn local_patch_backend_names(settings: &Settings) -> Vec<String> {
    localization_backend_names(settings)
}

/// Named CLI backends that can draft inside the disposable frontier workspace.
pub fn frontier_patch_backend_names(settings: &Settings) -> Vec<String> {
    let mut names = settings
        .backends()
        .into_iter()
        .flat_map(|backends| backends.iter())
        .filter_map(|(name, backend)| {
            matches!(
                backend,
                BackendConfig::ClaudeCli { .. } | BackendConfig::CodexCli { .. }
            )
            .then_some(name.clone())
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn selected_backend(saved: Option<&str>, available: &[String]) -> String {
    saved
        .filter(|name| available.iter().any(|candidate| candidate == name))
        .map(str::to_string)
        .or_else(|| available.first().cloned())
        .unwrap_or_default()
}

fn configured_model(settings: &Settings, backend: &str) -> String {
    settings
        .resolve_backend(backend)
        .map(|config| config.model().to_string())
        .unwrap_or_default()
}

fn seed_model_option(options: &mut HashMap<String, Vec<String>>, backend: &str, model: &str) {
    if !backend.is_empty() && !model.is_empty() {
        options
            .entry(backend.to_string())
            .or_default()
            .push(model.to_string());
    }
}

fn spawn_model_list_fetch(
    tx: mpsc::UnboundedSender<(String, Vec<String>)>,
    backend: &str,
    settings: &Settings,
) {
    let Some(config) = settings.resolve_backend(backend).cloned() else {
        return;
    };
    let backend = backend.to_string();
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let models = list_models(&config).await;
        let _ = tx.send((backend, models));
    });
}

fn route_override_combo(ui: &mut egui::Ui, current: &mut RouteOverride) {
    egui::ComboBox::from_label("Route override")
        .selected_text(current.to_string())
        .show_ui(ui, |ui| {
            for value in [
                RouteOverride::Automatic,
                RouteOverride::ForceLocal,
                RouteOverride::ForceFrontier,
            ] {
                ui.selectable_value(current, value, value.to_string());
            }
        });
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

fn pass_fail(success: bool) -> &'static str {
    if success { "passed" } else { "failed" }
}

fn append_output_line(lines: &mut Vec<String>, label: &str, output: &str) {
    if !output.is_empty() {
        lines.push(format!("{label}: {output}"));
    }
}

fn append_verifier_gate_lines(
    lines: &mut Vec<String>,
    index: usize,
    command: &str,
    disposition: &VerifierGateDisposition,
    output: Option<&BoundedVerifierOutput>,
) {
    lines.push(format!(
        "Verifier gate {}: {} ({})",
        index + 1,
        command,
        verifier_gate_label(disposition)
    ));
    if let Some(output) = output {
        append_output_line(lines, "Command output", &output.text);
        if output.truncated {
            lines.push(format!(
                "Command output was truncated after {} byte(s)",
                output.bytes_seen
            ));
        }
    }
}

fn verifier_gate_label(disposition: &VerifierGateDisposition) -> &'static str {
    match disposition {
        VerifierGateDisposition::Passed => "passed",
        VerifierGateDisposition::Failed => "failed",
        VerifierGateDisposition::SpawnFailed => "spawn failed",
        VerifierGateDisposition::Interrupted => "interrupted",
        VerifierGateDisposition::NotRun { .. } => "not run",
        VerifierGateDisposition::NotRunAfterPatch { .. } => "not run after patch gate",
    }
}

fn terminal_disposition_label(disposition: &ProcedureTerminalDisposition) -> String {
    match disposition {
        ProcedureTerminalDisposition::Succeeded => "succeeded".to_string(),
        ProcedureTerminalDisposition::AwaitingReview => "awaiting review".to_string(),
        ProcedureTerminalDisposition::Interrupted => "interrupted".to_string(),
        ProcedureTerminalDisposition::Failed { reason } => format!("failed: {reason}"),
    }
}
