//! Review policy for persisted procedure reports.

use super::{ProcedureReviewDisposition, ProcedureRun, ProcedureTerminalDisposition};
use crate::error::HarnessError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProcedureReviewError {
    #[error("could not load procedure run {run_id} for review: {source}")]
    ReportLoad {
        run_id: String,
        #[source]
        source: HarnessError,
    },
    #[error(
        "cannot record {requested} review for procedure run {run_id}: terminal disposition is {actual}; expected awaiting_review"
    )]
    NotAwaitingReview {
        run_id: String,
        requested: ProcedureReviewDisposition,
        actual: String,
    },
    #[error(
        "cannot record {requested} review for procedure run {run_id}: review disposition is already {actual}; first terminal review decision wins"
    )]
    DecisionConflict {
        run_id: String,
        requested: ProcedureReviewDisposition,
        actual: ProcedureReviewDisposition,
    },
    #[error("could not save reviewed procedure run {run_id}: {source}")]
    ReportSave {
        run_id: String,
        #[source]
        source: HarnessError,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error(
    "procedure run {run_id} cannot enter a downstream procedure stage: review disposition is {disposition}"
)]
pub struct ProcedureApprovedReportError {
    pub run_id: String,
    pub disposition: ProcedureReviewDisposition,
}

pub fn require_approved_report(
    report: &ProcedureRun,
) -> std::result::Result<&ProcedureRun, ProcedureApprovedReportError> {
    if report.review_disposition == ProcedureReviewDisposition::Approved {
        return Ok(report);
    }
    Err(ProcedureApprovedReportError {
        run_id: report.id.as_str(),
        disposition: report.review_disposition,
    })
}

pub(super) fn terminal_disposition_name(
    disposition: Option<&ProcedureTerminalDisposition>,
) -> String {
    match disposition {
        Some(ProcedureTerminalDisposition::Succeeded) => "succeeded".into(),
        Some(ProcedureTerminalDisposition::AwaitingReview) => "awaiting_review".into(),
        Some(ProcedureTerminalDisposition::Failed { .. }) => "failed".into(),
        Some(ProcedureTerminalDisposition::Interrupted) => "interrupted".into(),
        None => "none".into(),
    }
}
