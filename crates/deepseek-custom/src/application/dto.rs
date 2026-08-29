//! Serializable contracts shared by application actors and presentation adapters.
//!
//! These types contain browser-visible state only. They deliberately do not embed
//! [`Settings`](crate::config::settings::Settings), backend configuration, channels,
//! flags, or service handles because those structures may contain credentials or
//! other process-private data.

use serde::{Deserialize, Serialize};

use crate::config::settings::Settings;

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
    pub pending_session_switch: Option<PendingSessionSwitch>,
    pub settings: VisibleSettings,
    pub operations: Vec<OperationState>,
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
    Reset(AppSnapshot),
    WorkspaceSelected(Workspace),
    TranscriptAppended(TranscriptBlock),
    SessionChanged(SessionSummary),
    PendingSessionSwitchChanged(Option<PendingSessionSwitch>),
    SettingsChanged(VisibleSettings),
    OperationChanged(OperationState),
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
    pub selected_backend: Option<String>,
    pub selected_model: Option<String>,
    pub effort: String,
    pub context_budget: usize,
    pub show_raw_output: bool,
    pub working_dir: Option<String>,
    pub style: VisibleStyleSettings,
    pub voice: VisibleVoiceSettings,
}

impl VisibleSettings {
    /// Projects the persisted schema without copying credential-bearing fields.
    pub fn from_settings(
        settings: &Settings,
        selected_backend: Option<String>,
        selected_model: Option<String>,
    ) -> Self {
        Self {
            selected_backend,
            selected_model,
            effort: format!("{:?}", settings.effort()).to_lowercase(),
            context_budget: settings.context_budget(),
            show_raw_output: settings.show_raw_output(),
            working_dir: settings.working_dir(),
            style: VisibleStyleSettings {
                plain_language: settings.style_plain_language_enabled(),
                target_grade: settings.style_target_grade(),
            },
            voice: VisibleVoiceSettings {
                enabled: settings.voice_enabled(),
                stt_enabled: settings.voice_stt_enabled(),
                tts_enabled: settings.voice_tts_enabled(),
                trigger_mode: format!("{:?}", settings.voice_trigger_mode()).to_lowercase(),
                wake_phrase: settings.voice_wake_phrase(),
                tts_voice: settings.voice_tts_voice(),
                tts_speed: settings.voice_tts_speed(),
            },
        }
    }
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
        settings: VisibleSettings,
    },
    StartAutopilot {
        task: String,
        iterations: u32,
    },
    StartCascade {
        prompt: String,
    },
    StartEvolve {
        prompt: String,
    },
    RunProcedure {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approve,
    Reject,
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
