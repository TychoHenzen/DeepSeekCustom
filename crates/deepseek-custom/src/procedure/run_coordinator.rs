//! Shared construction boundary for one localization procedure run.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use super::{
    LocalizationDispatch, OpenSpecInput, ProcedureProgress, ProcedureReportRepository,
    ProcedureRun, ProcedureRunId, ProcedureRunRequest, ProcedureRunner, ProcedureRunnerError,
};
use crate::config::settings::RepositoryIndexLimits;

/// Dependencies required to run and persist localization.
pub struct ProcedureRunCoordinatorParams<D> {
    pub input: OpenSpecInput,
    pub working_dir: PathBuf,
    pub index_limits: RepositoryIndexLimits,
    pub dispatcher: D,
    pub reports: ProcedureReportRepository,
    pub interrupt: Arc<AtomicBool>,
}

/// Owns localization runner construction for direct and composed procedure paths.
pub struct ProcedureRunCoordinator<D> {
    runner: ProcedureRunner<D>,
    input: OpenSpecInput,
    working_dir: PathBuf,
    index_limits: RepositoryIndexLimits,
    reports: ProcedureReportRepository,
    interrupt: Arc<AtomicBool>,
}

impl<D> ProcedureRunCoordinator<D>
where
    D: LocalizationDispatch,
{
    pub fn new(params: ProcedureRunCoordinatorParams<D>) -> Self {
        let input = params.input.clone();
        let working_dir = params.working_dir.clone();
        let index_limits = params.index_limits.clone();
        let reports = params.reports.clone();
        let interrupt = Arc::clone(&params.interrupt);
        Self {
            runner: ProcedureRunner::new(
                params.input,
                params.working_dir,
                params.index_limits,
                params.dispatcher,
                params.reports,
                params.interrupt,
            ),
            input,
            working_dir,
            index_limits,
            reports,
            interrupt,
        }
    }

    pub fn with_progress(mut self, progress: mpsc::UnboundedSender<ProcedureProgress>) -> Self {
        self.runner = self.runner.with_progress(progress);
        self
    }

    pub async fn run(
        &self,
        request: ProcedureRunRequest,
    ) -> Result<ProcedureRun, ProcedureRunnerError> {
        self.runner.run(request).await
    }

    pub async fn run_with_id(
        &self,
        run_id: ProcedureRunId,
        request: ProcedureRunRequest,
    ) -> Result<ProcedureRun, ProcedureRunnerError> {
        self.runner.run_with_id(run_id, request).await
    }

    pub fn input(&self) -> &OpenSpecInput {
        &self.input
    }

    pub fn reports(&self) -> &ProcedureReportRepository {
        &self.reports
    }

    pub fn project_root(&self) -> &PathBuf {
        &self.working_dir
    }

    pub fn index_limits(&self) -> &RepositoryIndexLimits {
        &self.index_limits
    }

    pub fn interrupt(&self) -> &Arc<AtomicBool> {
        &self.interrupt
    }
}
