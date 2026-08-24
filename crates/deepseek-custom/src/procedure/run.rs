use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::OpenSpecValidation;

/// Identifies one procedure run and supplies its report file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcedureRunId(Uuid);

impl ProcedureRunId {
    /// Generate an identifier for a new procedure run.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Return the hyphenated UUID used as the report file stem.
    pub fn as_str(&self) -> String {
        self.0.to_string()
    }
}

impl Default for ProcedureRunId {
    fn default() -> Self {
        Self::new()
    }
}

/// The OpenSpec task selected for one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureTask {
    pub id: String,
    pub text: String,
    pub covers: Option<String>,
}

/// The fixed procedure stage currently being executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureStage {
    SpecValidation,
    Localization,
    Finished,
}

/// Compact state copied into each fresh stage prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureScratchpad {
    pub goals: Vec<String>,
    pub files: Vec<String>,
    pub changes: Vec<String>,
    pub last_error: Option<String>,
}

/// One repository location selected by the localizer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationTarget {
    pub path: String,
    pub symbol: Option<String>,
    pub evidence: String,
}

/// Schema-constrained final content returned by a localization dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationEnvelope {
    pub targets: Vec<LocalizationTarget>,
}

/// The validation outcome of one model response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureAttemptDisposition {
    Accepted,
    Rejected,
    Interrupted,
}

/// The semantic review state persisted with one localization report.
///
/// `LegacyUnreviewed` is reserved for reports written before this field
/// existed. New runs start as `Pending` and must record an explicit review
/// decision before later procedure stages may trust their targets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureReviewDisposition {
    #[default]
    Pending,
    Approved,
    Rejected,
    LegacyUnreviewed,
}

const fn legacy_unreviewed() -> ProcedureReviewDisposition {
    ProcedureReviewDisposition::LegacyUnreviewed
}

/// One bounded localization dispatch and its validated result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationAttempt {
    pub number: u8,
    pub backend: String,
    pub model: String,
    pub disposition: ProcedureAttemptDisposition,
    pub targets: Vec<LocalizationTarget>,
    pub validation_error: Option<String>,
}

/// The final result of a completed or stopped run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProcedureTerminalDisposition {
    Succeeded,
    AwaitingReview,
    Failed { reason: String },
    Interrupted,
}

/// All persistent state for a staged procedure run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureRun {
    pub id: ProcedureRunId,
    pub change_id: String,
    pub selected_task: ProcedureTask,
    pub spec_fingerprint: Option<String>,
    pub repository_fingerprint: Option<String>,
    /// Exact successful Stage 0 command evidence, when validation completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<OpenSpecValidation>,
    pub scratchpad: ProcedureScratchpad,
    pub stage: ProcedureStage,
    pub attempts: Vec<LocalizationAttempt>,
    /// Semantic review state. A missing field identifies a legacy report.
    #[serde(default = "legacy_unreviewed")]
    pub review_disposition: ProcedureReviewDisposition,
    pub terminal_disposition: Option<ProcedureTerminalDisposition>,
}
