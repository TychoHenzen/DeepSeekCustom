//! Bounded, provider-neutral blocker diagnosis and recovery.
//!
//! The module keeps provider identity, diagnostic dispatch, retry policy, and
//! durable evidence separate. A model may classify a failure, but only the
//! workflow owner supplies a permitted retry key and executes that safe action.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::agent::events::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::subagent::{SubagentRequest, run_subagent_without_tools};
use crate::effort::Effort;
use crate::error::HarnessError;

mod diagnosis;
mod identity;
mod state;
mod store;

pub use diagnosis::{
    DiagnosisOutcome, DiagnosticResponse, FailureContext, MAX_PERMITTED_RETRIES, SafeRetrySpec,
    parse_diagnostic_response, render_diagnostic_prompt, sanitize_text,
};
pub use identity::{
    IdentityError, IdentityInput, ProjectIdentity, ProjectReference, RepositoryIdentity,
    RepositoryProvider, ResolvedWorkIdentity, WorkItemIdentity, WorkItemKind, resolve_identity,
};
pub use state::{
    AttemptStatus, AttemptedAction, DiagnosticRecord, DiagnosticToken, RecoveryEvidence,
    RecoveryRun, RecoveryRunId, RecoveryRunRecord, RecoveryStateError, RecoveryStatus, RetryClaim,
    RetryResult,
};
pub use store::RecoveryStore;

/// An adapter owned by the guarded workflow. The diagnostic model never gets
/// a callable mutation tool. It can only select a key from `SafeRetrySpec`.
#[async_trait]
pub trait SafeRetryAction: Send + Sync {
    async fn retry(&self, claim: &RetryClaim) -> Result<String, String>;
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    State(#[from] RecoveryStateError),
    #[error(transparent)]
    Persistence(#[from] HarnessError),
    #[error("diagnostic session failed: {0}")]
    Diagnostic(String),
    #[error("diagnostic response was rejected: {0}")]
    InvalidDiagnostic(String),
}

/// Coordinates one diagnostic backend and one durable recovery store.
pub struct RecoveryCoordinator {
    factory: Arc<BackendFactory>,
    registry: Arc<SubagentRegistry>,
    store: RecoveryStore,
    diagnostic_backend: String,
    diagnostic_model: Option<String>,
}

impl RecoveryCoordinator {
    pub fn new(
        factory: Arc<BackendFactory>,
        store: RecoveryStore,
        diagnostic_backend: impl Into<String>,
        diagnostic_model: Option<String>,
    ) -> Self {
        Self::with_registry(
            factory,
            store,
            diagnostic_backend,
            diagnostic_model,
            Arc::new(SubagentRegistry::new()),
        )
    }

    pub fn with_registry(
        factory: Arc<BackendFactory>,
        store: RecoveryStore,
        diagnostic_backend: impl Into<String>,
        diagnostic_model: Option<String>,
        registry: Arc<SubagentRegistry>,
    ) -> Self {
        Self {
            factory,
            registry,
            store,
            diagnostic_backend: diagnostic_backend.into(),
            diagnostic_model,
        }
    }

    pub fn registry(&self) -> Arc<SubagentRegistry> {
        Arc::clone(&self.registry)
    }

    pub fn store(&self) -> &RecoveryStore {
        &self.store
    }

    pub fn start_run(
        &self,
        input: &IdentityInput,
        current_step: impl Into<String>,
        max_retry_attempts: u32,
    ) -> Result<RecoveryRun, RecoveryError> {
        let identity = resolve_identity(input)?;
        let run = RecoveryRun::new(identity, current_step, max_retry_attempts);
        self.store.save(&run)?;
        Ok(run)
    }

    pub fn load_run(&self, id: &RecoveryRunId) -> Result<RecoveryRun, RecoveryError> {
        Ok(self.store.load(id)?)
    }

    /// Run one fresh diagnostic session. The session is kept open only for
    /// the duration of this call, then explicitly closed through the existing
    /// `SubagentRegistry` lifetime boundary.
    pub async fn diagnose(
        &self,
        run: &mut RecoveryRun,
        failure: impl AsRef<str>,
        prior_evidence: &[String],
        permitted_retries: Vec<SafeRetrySpec>,
    ) -> Result<RecoveryStatus, RecoveryError> {
        let _lock = self.store.lock_run(&run.id())?;
        let authoritative = self.store.load(&run.id())?;
        run.replace_from(authoritative);
        let permitted_retries = permitted_retries
            .into_iter()
            .map(|retry| SafeRetrySpec::new(retry.key, retry.description))
            .collect::<Result<Vec<_>, _>>()
            .map_err(RecoveryError::InvalidDiagnostic)?;
        let context =
            FailureContext::new(run.record().current_step.clone(), failure, prior_evidence);
        let token = run.begin_diagnosis(context.clone(), permitted_retries.clone())?;
        self.store.save(run)?;

        let prompt = render_diagnostic_prompt(run.identity(), &context, &permitted_retries);
        let request = SubagentRequest {
            backend: self.diagnostic_backend.clone(),
            model: self.diagnostic_model.clone(),
            prompt,
            depth: 1,
            keep_open: true,
            working_dir_override: None,
            effort: Effort::None,
        };
        let (parent_tx, _parent_rx) = mpsc::unbounded_channel::<RoutedEvent>();
        let outcome = run_subagent_without_tools(
            &self.factory,
            request,
            parent_tx,
            Arc::clone(&self.registry),
        )
        .await;

        let (response, session_closed) = match outcome {
            Ok(outcome) => {
                let session_closed = self.close_diagnostic_session(outcome.session_id).await;
                let response = match parse_diagnostic_response(&outcome.text) {
                    Ok(response) => response,
                    Err(error) => DiagnosticResponse::needs_decision(
                        "The diagnostic response could not be validated.",
                        format!("Review the diagnostic output contract: {error}"),
                    ),
                };
                (response, session_closed)
            }
            Err(_error) => (
                DiagnosticResponse::needs_decision(
                    "The diagnostic session failed before it produced a decision.",
                    "Review the retained failure and choose an authorized recovery action.",
                ),
                true,
            ),
        };

        let status = run.apply_diagnosis(&token, response, session_closed)?;
        self.store.save(run)?;
        Ok(status)
    }

    /// Claim, persist, execute, and persist one permitted safe retry. The
    /// claim is written before the adapter runs, so a duplicate caller cannot
    /// invoke the same action twice for the same bounded attempt.
    pub async fn execute_retry(
        &self,
        run: &mut RecoveryRun,
        action: &dyn SafeRetryAction,
    ) -> Result<RetryResult, RecoveryError> {
        let _lock = self.store.lock_run(&run.id())?;
        let authoritative = self.store.load(&run.id())?;
        run.replace_from(authoritative);
        let claim = match run.claim_retry() {
            Ok(claim) => claim,
            Err(error) => {
                self.store.save(run)?;
                return Err(error.into());
            }
        };
        self.store.save(run)?;
        let result = action.retry(&claim).await;
        let outcome = run.finish_retry(&claim, result)?;
        self.store.save(run)?;
        Ok(outcome)
    }

    async fn close_diagnostic_session(
        &self,
        session_id: Option<crate::agent::events::SubagentId>,
    ) -> bool {
        let Some(session_id) = session_id else {
            return true;
        };
        self.registry.close(session_id).await;
        !self.registry.contains(session_id).await
    }
}
