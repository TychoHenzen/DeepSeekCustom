//! Durable, content-free routing metrics for completed procedure runs.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{
    ProcedureAttemptDisposition, ProcedureRun, ProcedureTerminalDisposition, RouteSignal, RouteTier,
};

/// Timing recorded for one named procedure stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureStageTiming {
    pub stage: String,
    pub duration_ms: u64,
}

/// One backend and model pair observed while completing a procedure run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProcedureBackendModel {
    pub backend: String,
    pub model: String,
}

/// Content-free result for one generated local candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureCandidateMetric {
    pub index: u8,
    pub changed_line_count: Option<usize>,
    pub verifier_passed: Option<bool>,
}

/// Content-free result for one named deterministic gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureGateOutcome {
    pub gate: String,
    pub passed: bool,
}

/// Available token counts reported by a backend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureTokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// Route evidence that does not retain prompts, source content, or raw output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureRouteMetrics {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<RouteSignal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tier: Option<RouteTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_mechanical_success: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalation_triggers: Vec<String>,
}

/// Terminal state stored as a stable metrics label without a failure message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureMetricsDisposition {
    Succeeded,
    AwaitingReview,
    Failed,
    Interrupted,
}

/// Inspectable facts gathered for one terminal procedure run.
///
/// This deliberately stores operational labels and counts only. The complete
/// report remains the source for review and debugging details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureRunMetrics {
    pub completed_at_unix_ms: u64,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage_timings: Vec<ProcedureStageTiming>,
    #[serde(default)]
    pub route: ProcedureRouteMetrics,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backends: Vec<ProcedureBackendModel>,
    pub localization_attempt_count: usize,
    pub schema_rejection_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<ProcedureCandidateMetric>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_outcomes: Vec<ProcedureGateOutcome>,
    #[serde(default)]
    pub token_usage: ProcedureTokenUsage,
    pub terminal_disposition: ProcedureMetricsDisposition,
}

impl ProcedureRunMetrics {
    /// Create baseline metrics when a legacy localization runner reaches a terminal state.
    pub fn from_terminal_run(run: &ProcedureRun) -> Option<Self> {
        Self::from_terminal_run_with_timing(run, Duration::ZERO, Vec::new())
    }

    /// Add the timings captured by the owning runner to one terminal report.
    pub fn from_terminal_run_with_timing(
        run: &ProcedureRun,
        elapsed: Duration,
        stage_timings: Vec<ProcedureStageTiming>,
    ) -> Option<Self> {
        let disposition = run.terminal_disposition.as_ref()?;
        let mut backends = BTreeSet::new();
        for attempt in &run.attempts {
            backends.insert(ProcedureBackendModel {
                backend: attempt.backend.clone(),
                model: attempt.model.clone(),
            });
        }
        Some(Self {
            completed_at_unix_ms: now_unix_ms(),
            duration_ms: duration_to_ms(elapsed),
            stage_timings,
            route: ProcedureRouteMetrics::default(),
            backends: backends.into_iter().collect(),
            localization_attempt_count: run.attempts.len(),
            schema_rejection_count: run
                .attempts
                .iter()
                .filter(|attempt| attempt.disposition == ProcedureAttemptDisposition::Rejected)
                .count(),
            candidates: Vec::new(),
            gate_outcomes: Vec::new(),
            token_usage: ProcedureTokenUsage::default(),
            terminal_disposition: ProcedureMetricsDisposition::from(disposition),
        })
    }
}

fn duration_to_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

impl From<&ProcedureTerminalDisposition> for ProcedureMetricsDisposition {
    fn from(value: &ProcedureTerminalDisposition) -> Self {
        match value {
            ProcedureTerminalDisposition::Succeeded => Self::Succeeded,
            ProcedureTerminalDisposition::AwaitingReview => Self::AwaitingReview,
            ProcedureTerminalDisposition::Failed { .. } => Self::Failed,
            ProcedureTerminalDisposition::Interrupted => Self::Interrupted,
        }
    }
}

/// Recent aggregate values shown by the Procedure view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcedureMetricsSummary {
    pub completed_run_count: usize,
    pub local_mechanical_run_count: usize,
    pub local_mechanical_success_count: usize,
    pub frontier_escalation_count: usize,
}

impl ProcedureMetricsSummary {
    /// Percentage of local mechanical runs with a selected passing candidate.
    pub fn local_mechanical_success_percent(&self) -> Option<u8> {
        percentage(
            self.local_mechanical_success_count,
            self.local_mechanical_run_count,
        )
    }

    pub fn record(&mut self, metrics: &ProcedureRunMetrics) {
        self.completed_run_count += 1;
        if let Some(success) = metrics.route.local_mechanical_success {
            self.local_mechanical_run_count += 1;
            self.local_mechanical_success_count += usize::from(success);
        }
        if !metrics.route.escalation_triggers.is_empty() {
            self.frontier_escalation_count += 1;
        }
    }
}

fn percentage(numerator: usize, denominator: usize) -> Option<u8> {
    (denominator != 0).then(|| (numerator.saturating_mul(100) / denominator) as u8)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
