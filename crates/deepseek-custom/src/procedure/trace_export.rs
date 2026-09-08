//! Privacy-limited trace records for offline routing analysis.
//!
//! This module intentionally builds a new allowlisted document instead of
//! serializing [`StoredProcedureReport`] values. Procedure reports retain
//! review evidence such as prompts and verifier output. Trace exports must
//! never carry that workspace content.

use serde::{Deserialize, Serialize};

use super::{
    ProcedureAttemptDisposition, ProcedureMetricsDisposition, ProcedureRun, ProcedureRunMetrics,
    RouteSignal, RouteTier, StoredProcedureReport,
};

/// One normalized repository target included in a trace export.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LocalizationTraceTarget {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

/// Fixed route labels safe to include in a trace export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationTraceRoute {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_tier: Option<RouteTier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<RouteSignal>,
    pub escalated_to_frontier: bool,
}

/// Content-free outcomes recorded for one exported localization trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationTraceOutcomes {
    pub localization_attempts: Vec<ProcedureAttemptDisposition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_mechanical_success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_disposition: Option<ProcedureMetricsDisposition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidate_verifier_outcomes: Vec<Option<bool>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_outcomes: Vec<bool>,
}

/// Numeric measurements safe to include in a trace export.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationTraceMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage_durations_ms: Vec<u64>,
    pub localization_attempt_count: usize,
    pub schema_rejection_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidate_changed_line_counts: Vec<Option<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

/// One allowlisted record for offline localization tuning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationTraceExportRecord {
    pub targets: Vec<LocalizationTraceTarget>,
    pub route: LocalizationTraceRoute,
    pub outcomes: LocalizationTraceOutcomes,
    pub metrics: LocalizationTraceMetrics,
}

/// The complete JSON document emitted by an explicit trace export.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationTraceExport {
    pub records: Vec<LocalizationTraceExportRecord>,
}

impl LocalizationTraceExport {
    /// Serialize this already-allowlisted export document for the selected destination.
    pub fn to_pretty_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

impl From<&StoredProcedureReport> for LocalizationTraceExportRecord {
    fn from(report: &StoredProcedureReport) -> Self {
        Self::from_run_and_metrics(&report.run, report.metrics.as_ref())
    }
}

impl LocalizationTraceExportRecord {
    fn from_run_and_metrics(run: &ProcedureRun, metrics: Option<&ProcedureRunMetrics>) -> Self {
        let targets = normalized_targets(run);
        let route = LocalizationTraceRoute {
            selected_tier: metrics.and_then(|value| value.route.selected_tier),
            signals: metrics
                .map(|value| value.route.signals.clone())
                .unwrap_or_default(),
            escalated_to_frontier: metrics
                .is_some_and(|value| !value.route.escalation_triggers.is_empty()),
        };
        let outcomes = LocalizationTraceOutcomes {
            localization_attempts: run
                .attempts
                .iter()
                .map(|attempt| attempt.disposition)
                .collect(),
            local_mechanical_success: metrics
                .and_then(|value| value.route.local_mechanical_success),
            terminal_disposition: metrics.map(|value| value.terminal_disposition),
            candidate_verifier_outcomes: metrics
                .map(|value| {
                    value
                        .candidates
                        .iter()
                        .map(|candidate| candidate.verifier_passed)
                        .collect()
                })
                .unwrap_or_default(),
            gate_outcomes: metrics
                .map(|value| value.gate_outcomes.iter().map(|gate| gate.passed).collect())
                .unwrap_or_default(),
        };
        let metrics = LocalizationTraceMetrics {
            duration_ms: metrics.map(|value| value.duration_ms),
            stage_durations_ms: metrics
                .map(|value| {
                    value
                        .stage_timings
                        .iter()
                        .map(|timing| timing.duration_ms)
                        .collect()
                })
                .unwrap_or_default(),
            localization_attempt_count: metrics
                .map(|value| value.localization_attempt_count)
                .unwrap_or(run.attempts.len()),
            schema_rejection_count: metrics
                .map(|value| value.schema_rejection_count)
                .unwrap_or_else(|| {
                    run.attempts
                        .iter()
                        .filter(|attempt| {
                            attempt.disposition == ProcedureAttemptDisposition::Rejected
                        })
                        .count()
                }),
            candidate_changed_line_counts: metrics
                .map(|value| {
                    value
                        .candidates
                        .iter()
                        .map(|candidate| candidate.changed_line_count)
                        .collect()
                })
                .unwrap_or_default(),
            input_tokens: metrics.and_then(|value| value.token_usage.input_tokens),
            output_tokens: metrics.and_then(|value| value.token_usage.output_tokens),
            total_tokens: metrics.and_then(|value| value.token_usage.total_tokens),
        };
        Self {
            targets,
            route,
            outcomes,
            metrics,
        }
    }
}

fn normalized_targets(run: &ProcedureRun) -> Vec<LocalizationTraceTarget> {
    let mut targets = run
        .attempts
        .iter()
        .filter(|attempt| attempt.disposition == ProcedureAttemptDisposition::Accepted)
        .flat_map(|attempt| attempt.targets.iter())
        .map(|target| LocalizationTraceTarget {
            path: target.path.clone(),
            symbol: target.symbol.clone(),
        })
        .collect::<Vec<_>>();
    targets.sort_unstable();
    targets.dedup();
    targets
}
