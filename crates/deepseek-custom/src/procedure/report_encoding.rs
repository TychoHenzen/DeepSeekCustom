//! JSON encoding for procedure report documents.

use super::{
    ProcedureInputFingerprints, ProcedureRun, ProcedureRunMetrics, RepairLadderEvent,
    StoredProcedureReport, VerifierReport,
};
use crate::error::{HarnessError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct ProcedureReportDocument {
    #[serde(flatten)]
    run: ProcedureRun,
    #[serde(default, skip_serializing_if = "ProcedureInputFingerprints::is_empty")]
    input_fingerprints: ProcedureInputFingerprints,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verification: Option<VerifierReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    repair_events: Vec<RepairLadderEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metrics: Option<ProcedureRunMetrics>,
}

pub(super) fn encode_report(report: &StoredProcedureReport) -> Result<String> {
    let document = ProcedureReportDocument {
        run: report.run.clone(),
        input_fingerprints: report.input_fingerprints.clone(),
        verification: report.verification.clone(),
        repair_events: report.repair_events.clone(),
        metrics: report.metrics.clone(),
    };
    serde_json::to_string_pretty(&document).map_err(|error| {
        HarnessError::Parse(format!("could not serialize procedure report: {error}"))
    })
}

pub(super) fn decode_report(json: &str, path: &Path) -> Result<StoredProcedureReport> {
    let document: ProcedureReportDocument = serde_json::from_str(json).map_err(|error| {
        HarnessError::Parse(format!(
            "could not parse procedure report {}: {error}",
            path.display()
        ))
    })?;
    Ok(StoredProcedureReport {
        run: document.run,
        input_fingerprints: document.input_fingerprints,
        verification: document.verification,
        repair_events: document.repair_events,
        metrics: document.metrics,
    })
}
