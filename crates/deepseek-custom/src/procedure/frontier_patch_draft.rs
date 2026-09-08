//! Isolated one-shot patch drafting through Claude CLI or Codex CLI.

use std::path::Path;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::mpsc;

use crate::agent::events::RoutedEvent;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::resolved::ResolvedBackend;
use crate::backend::subagent::{SubagentRequest, run_subagent};
use crate::effort::Effort;

use super::{
    DisposableDraftWorkspace, DisposableWorkspaceError, PatchCandidate, PatchEnvelopeError,
    decode_frontier_patch_output,
};

/// Inputs for one frontier CLI patch draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontierPatchDraftRequest {
    pub backend: String,
    pub model: Option<String>,
    pub prompt: String,
    pub effort: Effort,
}

/// Failure before a frontier CLI draft becomes a validated patch candidate.
#[derive(Debug, Error)]
pub enum FrontierPatchDraftError {
    #[error(transparent)]
    Workspace(#[from] DisposableWorkspaceError),
    #[error(
        "backend `{backend}` cannot draft a frontier CLI patch because it is not Claude CLI or Codex CLI"
    )]
    UnsupportedBackend { backend: String },
    #[error("frontier patch dispatch on backend `{backend}` failed: {message}")]
    Dispatch { backend: String, message: String },
    #[error(transparent)]
    InvalidOutput(#[from] PatchEnvelopeError),
}

/// Draft one patch inside a disposable copy and validate only the returned final text.
pub async fn draft_frontier_patch(
    factory: &Arc<BackendFactory>,
    source_root: &Path,
    request: FrontierPatchDraftRequest,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    registry: Arc<SubagentRegistry>,
) -> Result<PatchCandidate, FrontierPatchDraftError> {
    ensure_cli_backend(factory, &request)?;
    let workspace = DisposableDraftWorkspace::create(source_root)?;
    let backend = request.backend.clone();
    let outcome = run_subagent(
        factory,
        SubagentRequest {
            backend: request.backend,
            model: request.model,
            prompt: request.prompt,
            depth: 1,
            keep_open: false,
            working_dir_override: Some(workspace.path().to_path_buf()),
            effort: request.effort,
        },
        parent_tx,
        registry,
    )
    .await
    .map_err(|message| FrontierPatchDraftError::Dispatch { backend, message })?;

    let final_text = outcome.text;
    workspace.close()?;
    decode_frontier_patch_output(&final_text).map_err(FrontierPatchDraftError::from)
}

fn ensure_cli_backend(
    factory: &Arc<BackendFactory>,
    request: &FrontierPatchDraftRequest,
) -> Result<(), FrontierPatchDraftError> {
    let resolved = factory
        .resolve(&request.backend, request.model.as_deref())
        .map_err(|message| FrontierPatchDraftError::Dispatch {
            backend: request.backend.clone(),
            message,
        })?;
    if matches!(
        resolved,
        ResolvedBackend::ClaudeCli { .. } | ResolvedBackend::CodexCli { .. }
    ) {
        return Ok(());
    }
    Err(FrontierPatchDraftError::UnsupportedBackend {
        backend: request.backend.clone(),
    })
}
