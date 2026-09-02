//! Serializable contracts shared by application actors and presentation adapters.
//!
//! These types contain browser-visible state only. They deliberately do not embed
//! [`Settings`](crate::config::settings::Settings), backend configuration, channels,
//! flags, or service handles because those structures may contain credentials or
//! other process-private data.

use serde::{Deserialize, Serialize};

use crate::config::settings::Settings;
use crate::controlled_development::{
    ControlledDevelopmentCoordinator, ControlledDevelopmentPhase, WorkCard, WorkCardValidationError,
};

/// Monotonic version of the authoritative application state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppRevision(pub u64);

impl AppRevision {
    pub const INITIAL: Self = Self(0);

    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(revision) => Some(Self(revision)),
            None => None,
        }
    }
}

/// Complete state required to render a newly connected client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub revision: AppRevision,
    pub workspace: Workspace,
    pub transcript: Vec<TranscriptBlock>,
    pub session: SessionSummary,
    pub saved_sessions: Vec<SessionSummary>,
    pub pending_session_switch: Option<PendingSessionSwitch>,
    pub settings: VisibleSettings,
    pub operations: Vec<OperationState>,
    #[serde(default)]
    pub controlled_development: ControlledDevelopmentView,
    #[serde(default)]
    pub tests: super::test_control::TestControlSnapshot,
}

/// One ordered update emitted after a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppChange {
    pub revision: AppRevision,
    #[serde(flatten)]
    pub change: AppChangeKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AppChangeKind {
    Reset(Box<AppSnapshot>),
    WorkspaceSelected(Workspace),
    TranscriptAppended(TranscriptBlock),
    SessionChanged(SessionSummary),
    SavedSessionsChanged(Vec<SessionSummary>),
    PendingSessionSwitchChanged(Option<PendingSessionSwitch>),
    SettingsChanged(VisibleSettings),
    OperationChanged(OperationState),
    ControlledDevelopmentChanged(ControlledDevelopmentView),
    TestsChanged(Box<super::test_control::TestControlSnapshot>),
    Error(AppError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workspace {
    Chat,
    Autopilot,
    Cascade,
    Evolve,
    Procedure,
    Sessions,
    Tests,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub backend: String,
    pub model: String,
}

/// Browser-safe projection of the selected session's controlled workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlledDevelopmentView {
    pub enabled: bool,
    pub phase: ControlledDevelopmentPhase,
    pub packet_id: Option<String>,
    pub card: Option<WorkCard>,
    pub structural_errors: Vec<WorkCardValidationError>,
    pub changed_paths: Vec<String>,
    pub proof_results: Vec<ControlledDevelopmentProofResult>,
    pub compact_result: Option<String>,
    pub blocker: Option<String>,
    pub retained_evidence: bool,
    pub limitation: String,
}

impl Default for ControlledDevelopmentView {
    fn default() -> Self {
        Self {
            enabled: false,
            phase: ControlledDevelopmentPhase::Off,
            packet_id: None,
            card: None,
            structural_errors: Vec::new(),
            changed_paths: Vec::new(),
            proof_results: Vec::new(),
            compact_result: None,
            blocker: None,
            retained_evidence: false,
            limitation: controlled_development_limitation().to_string(),
        }
    }
}

impl ControlledDevelopmentView {
    pub fn from_coordinator(coordinator: &ControlledDevelopmentCoordinator) -> Self {
        let state = coordinator.state();
        Self {
            enabled: state.is_enabled(),
            phase: state.phase(),
            packet_id: state.packet_id().map(str::to_string),
            card: state.work_card().cloned(),
            structural_errors: state.structural_errors().to_vec(),
            changed_paths: coordinator.changed_paths().to_vec(),
            proof_results: coordinator
                .proof_evidence()
                .iter()
                .map(ControlledDevelopmentProofResult::from)
                .collect(),
            compact_result: coordinator.compact_summary().map(str::to_string),
            blocker: coordinator.blocker().map(str::to_string),
            retained_evidence: coordinator.has_retained_workspace(),
            limitation: controlled_development_limitation().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlledDevelopmentProofResult {
    pub command: String,
    pub disposition: String,
    pub success: Option<bool>,
    pub exit_code: Option<i32>,
}

impl From<&crate::procedure::VerifierGateEvidence> for ControlledDevelopmentProofResult {
    fn from(evidence: &crate::procedure::VerifierGateEvidence) -> Self {
        Self {
            command: evidence.command.clone(),
            disposition: verifier_disposition_label(&evidence.disposition),
            success: evidence.result.as_ref().map(|result| result.success),
            exit_code: evidence.result.as_ref().and_then(|result| result.exit_code),
        }
    }
}

fn verifier_disposition_label(disposition: &crate::procedure::VerifierGateDisposition) -> String {
    match disposition {
        crate::procedure::VerifierGateDisposition::Passed => "passed".into(),
        crate::procedure::VerifierGateDisposition::Failed => "failed".into(),
        crate::procedure::VerifierGateDisposition::SpawnFailed => "spawn_failed".into(),
        crate::procedure::VerifierGateDisposition::Interrupted => "interrupted".into(),
        crate::procedure::VerifierGateDisposition::NotRun { blocked_by } => {
            format!("not_run_after_gate_{blocked_by}")
        }
        crate::procedure::VerifierGateDisposition::NotRunAfterPatch { blocked_by } => {
            format!("not_run_after_patch_{blocked_by:?}")
        }
    }
}

fn controlled_development_limitation() -> &'static str {
    "Approved proof commands run in the disposable workspace, but can still address absolute paths outside it."
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "session_id", rename_all = "snake_case")]
pub enum PendingSessionSwitch {
    New,
    Load(String),
}

/// Stable transcript representation independent of native paint types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptBlock {
    pub id: u64,
    #[serde(flatten)]
    pub content: TranscriptContent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptContent {
    User {
        text: String,
        has_image: bool,
    },
    Assistant {
        spans: Vec<TranscriptSpan>,
    },
    ToolCall {
        tool: String,
        args: String,
        output: Option<String>,
        is_error: bool,
    },
    Notice {
        message: String,
        level: NoticeLevel,
    },
    Error {
        message: String,
        recoverable: bool,
    },
    Image {
        media_type: String,
        data: String,
    },
    Terminal {
        outcome: OperationPhase,
        message: String,
    },
    Subagent {
        name: String,
        state: String,
        blocks: Vec<TranscriptBlock>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "text", rename_all = "snake_case")]
pub enum TranscriptSpan {
    Text(String),
    Reasoning(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// The subset of settings safe and useful for presentation clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisibleSettings {
    pub backends: Vec<VisibleBackend>,
    pub selected_backend: Option<String>,
    pub selected_model: Option<String>,
    pub effort: String,
    pub context_budget: usize,
    pub show_raw_output: bool,
    pub max_tokens: u32,
    pub working_dir: Option<String>,
    pub style: VisibleStyleSettings,
    pub voice: VisibleVoiceSettings,
    pub procedure: VisibleProcedureSettings,
}

impl VisibleSettings {
    /// Projects the persisted schema without copying credential-bearing fields.
    pub fn from_settings(
        settings: &Settings,
        selected_backend: Option<String>,
        selected_model: Option<String>,
    ) -> Self {
        Self {
            backends: visible_backends(settings),
            selected_backend,
            selected_model,
            effort: format!("{:?}", settings.effort()).to_lowercase(),
            context_budget: settings.context_budget(),
            show_raw_output: settings.show_raw_output(),
            max_tokens: settings.max_tokens(),
            working_dir: settings.working_dir(),
            style: VisibleStyleSettings {
                plain_language: settings.style_plain_language_enabled(),
                target_grade: settings.style_target_grade(),
            },
            voice: VisibleVoiceSettings {
                enabled: settings.voice_enabled(),
                stt_enabled: settings.voice_stt_enabled(),
                tts_enabled: settings.voice_tts_enabled(),
                trigger_mode: match settings.voice_trigger_mode() {
                    crate::config::settings::TriggerMode::PushToTalk => "push_to_talk",
                    crate::config::settings::TriggerMode::WakeWord => "wake_word",
                }
                .into(),
                wake_phrase: settings.voice_wake_phrase(),
                tts_voice: settings.voice_tts_voice(),
                tts_speed: settings.voice_tts_speed(),
            },
            procedure: VisibleProcedureSettings::from_settings(settings),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisibleBackend {
    pub name: String,
    pub configured_model: String,
    pub models: Vec<String>,
}

fn visible_backends(settings: &Settings) -> Vec<VisibleBackend> {
    let mut values = settings
        .backends()
        .into_iter()
        .flatten()
        .map(|(name, config)| {
            let configured_model = config.model().to_owned();
            let models = match config {
                crate::config::settings::BackendConfig::Api { models, .. }
                | crate::config::settings::BackendConfig::ClaudeCli { models, .. }
                | crate::config::settings::BackendConfig::CodexCli { models, .. } => models
                    .clone()
                    .unwrap_or_else(|| vec![configured_model.clone()]),
            };
            VisibleBackend {
                name: name.clone(),
                configured_model,
                models,
            }
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.name.cmp(&right.name));
    values
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisibleStyleSettings {
    pub plain_language: bool,
    pub target_grade: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisibleVoiceSettings {
    pub enabled: bool,
    pub stt_enabled: bool,
    pub tts_enabled: bool,
    pub trigger_mode: String,
    pub wake_phrase: String,
    pub tts_voice: String,
    pub tts_speed: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisibleProcedureSettings {
    pub localization_backend: Option<String>,
    pub local_patch_backend: Option<String>,
    pub frontier_patch_backend: Option<String>,
    pub index_max_files: usize,
    pub index_max_total_bytes: u64,
    pub verifier_commands: Vec<String>,
}

impl VisibleProcedureSettings {
    fn from_settings(settings: &Settings) -> Self {
        let procedure = settings.procedure().cloned().unwrap_or_default();
        Self {
            localization_backend: procedure.localization_backend,
            local_patch_backend: procedure.local_patch_backend,
            frontier_patch_backend: procedure.frontier_patch_backend,
            index_max_files: procedure.repository_index.max_files,
            index_max_total_bytes: procedure.repository_index.max_total_bytes,
            verifier_commands: procedure.verifier_commands,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationState {
    pub kind: OperationKind,
    pub operation_id: Option<String>,
    pub phase: OperationPhase,
    pub progress: Option<OperationProgress>,
    pub message: Option<String>,
    pub error: Option<AppError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Chat,
    Autopilot,
    Cascade,
    Evolve,
    Procedure,
    Voice,
    Tests,
    FolderPicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationPhase {
    Idle,
    Running,
    AwaitingReview,
    Completed,
    Failed,
    Interrupted,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationProgress {
    pub completed: u64,
    pub total: Option<u64>,
}

/// A state-changing request paired with the state revision seen by its caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppCommandRequest {
    pub revision: AppRevision,
    #[serde(flatten)]
    pub command: AppCommand,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", content = "payload", rename_all = "snake_case")]
pub enum AppCommand {
    SelectWorkspace {
        workspace: Workspace,
    },
    SendMessage {
        text: String,
        attachment_id: Option<String>,
    },
    SetControlledDevelopmentEnabled {
        session_id: String,
        enabled: bool,
    },
    ApproveControlledDevelopment {
        session_id: String,
        card_id: String,
    },
    RejectControlledDevelopment {
        session_id: String,
        card_id: String,
    },
    StopControlledDevelopment {
        session_id: String,
        packet_id: String,
    },
    DiscardControlledDevelopmentEvidence {
        session_id: String,
    },
    StopOperation {
        kind: OperationKind,
    },
    NewSession,
    LoadSession {
        session_id: String,
    },
    DeleteSession {
        session_id: String,
    },
    SelectBackend {
        backend: String,
        model: String,
    },
    UpdateSettings {
        settings: Box<VisibleSettings>,
    },
    StartAutopilot {
        task: String,
        iterations: u32,
    },
    StartCascade {
        prompt: String,
        backend: String,
        n: u32,
        vote_k: u32,
        check_cmd: Option<String>,
        diversity_hints: Vec<String>,
        escalate_backend: Option<String>,
    },
    StartEvolve {
        prompt: String,
        backend: String,
        generations: u32,
        population: u32,
        fitness_cmd: String,
        feature_cmd: Option<String>,
        islands: u32,
        migration_interval: u32,
        mutation_hints: Vec<String>,
    },
    RunProcedure {
        change_id: String,
        task_id: String,
    },
    PreviewProcedure {
        localization_run_id: String,
        change_id: String,
        task_id: String,
        route: ProcedureRouteOverride,
        local_backend: String,
        local_model: String,
        frontier_backend: String,
        frontier_model: String,
    },
    RunWholeChangeProcedure {
        change_id: String,
        route: ProcedureRouteOverride,
        localization_backend: String,
        local_backend: String,
        local_model: String,
        frontier_backend: String,
        frontier_model: String,
    },
    ApplyProcedure {
        localization_run_id: String,
        preview_id: String,
        change_id: String,
        task_id: String,
    },
    ReviewProcedure {
        run_id: String,
        decision: ReviewDecision,
    },
    StartVoiceCapture,
    StopVoiceCapture,
    PickWorkingDirectory,
    RefreshTests,
    StartTestRun {
        request: super::test_control::TestRunRequest,
    },
    CancelTestRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approve,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureRouteOverride {
    Automatic,
    ForceLocal,
    ForceFrontier,
}

/// Stable response shape for every state-changing request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AppCommandResult {
    Applied { revision: AppRevision },
    Conflict { current_revision: AppRevision },
    Rejected { error: AppError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppError {
    pub code: AppErrorCode,
    pub message: String,
    pub recoverable: bool,
    pub field: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppErrorCode {
    InvalidCommand,
    InvalidInput,
    Conflict,
    OperationActive,
    NotFound,
    Unavailable,
    PersistenceFailed,
    ServiceFailed,
}
