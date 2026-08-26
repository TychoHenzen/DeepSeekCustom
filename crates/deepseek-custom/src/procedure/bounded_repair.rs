//! Production orchestration for one complete bounded repair ladder.

use thiserror::Error;

use super::{
    FrontierRepairDispatch, FrontierRepairError, FrontierRepairOutcome, FrontierRepairRunner,
    LocalPatchDraftDispatch, LocalRepairError, LocalRepairOutcome, LocalRepairRun,
    LocalRepairRunner, ProcedureReportStore, RepairRequest,
};
use crate::config::settings::ValidatedProcedureRepairPolicy;
use crate::error::HarnessError;

/// Complete outcome from the local tier and its optional frontier escalation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedRepairRun {
    pub local: LocalRepairRun,
    pub frontier: Option<FrontierRepairOutcome>,
    pub persisted_events: Vec<super::RepairLadderEvent>,
}

/// Failure before a complete repair outcome can be persisted.
#[derive(Debug, Error)]
pub enum BoundedRepairError {
    #[error(transparent)]
    Local(#[from] LocalRepairError),
    #[error(transparent)]
    Frontier(#[from] FrontierRepairError),
    #[error("local repair exhausted but no frontier dispatcher was provided")]
    MissingFrontierDispatcher,
    #[error("could not persist bounded repair evidence: {0}")]
    Persist(#[from] HarnessError),
}

/// Runs the local and frontier tiers, then persists their complete event sequence.
pub struct BoundedRepairCoordinator<'a> {
    local: &'a LocalRepairRunner,
    frontier: &'a FrontierRepairRunner,
    reports: &'a ProcedureReportStore,
}

impl<'a> BoundedRepairCoordinator<'a> {
    pub fn new(
        local: &'a LocalRepairRunner,
        frontier: &'a FrontierRepairRunner,
        reports: &'a ProcedureReportStore,
    ) -> Self {
        Self {
            local,
            frontier,
            reports,
        }
    }

    /// Execute the approved request and persist every emitted transition.
    pub async fn run(
        &self,
        request: &RepairRequest,
        policy: ValidatedProcedureRepairPolicy,
        local_dispatcher: &dyn LocalPatchDraftDispatch,
        frontier_dispatcher: Option<&dyn FrontierRepairDispatch>,
        verifier_commands: &[String],
    ) -> Result<BoundedRepairRun, BoundedRepairError> {
        let mut local = self
            .local
            .run(request, policy, local_dispatcher, verifier_commands)
            .await?;
        let frontier = if local.outcome == LocalRepairOutcome::LocalExhausted {
            let dispatcher =
                frontier_dispatcher.ok_or(BoundedRepairError::MissingFrontierDispatcher)?;
            Some(
                self.frontier
                    .run(&mut local, dispatcher, verifier_commands)
                    .await?,
            )
        } else {
            None
        };
        local.save_repair_events(self.reports)?;
        let persisted_events = self
            .reports
            .load_with_fingerprints(&local.repair_input.report.run.id)?
            .repair_events;
        Ok(BoundedRepairRun {
            local,
            frontier,
            persisted_events,
        })
    }
}
