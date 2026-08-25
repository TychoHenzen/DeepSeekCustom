//! End-to-end construction and persistence of one non-mutating patch preview.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::effort::Effort;
use crate::error::HarnessError;

use super::{FrontierPatchDraftError, FrontierPatchDraftRequest};
use super::{
    LocalPatchDraftDispatch, LocalPatchDraftDispatcher, PatchApplyCheckError, PatchBoundaryError,
    PatchEnvelopeError, PatchPreviewInputError, PatchPreviewInputGate, PatchPreviewInputRequest,
    PatchRouteMetadata, ProcedureAttemptDisposition, RouteDecision, RouteOverride, RouteTier,
    apply_route_override, assess_route, check_patch_applicability, draft_frontier_patch,
    validate_patch_boundary,
};

const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Stable identity for one preview attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PatchPreviewId(Uuid);

impl PatchPreviewId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_str(self) -> String {
        self.0.to_string()
    }
}

impl Default for PatchPreviewId {
    fn default() -> Self {
        Self::new()
    }
}

/// GUI-selected inputs for one routed preview run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchPreviewRequest {
    pub localization_run_id: super::ProcedureRunId,
    pub change_id: String,
    pub task_id: String,
    pub route_override: RouteOverride,
    pub local_backend: String,
    pub local_model: String,
    pub frontier_backend: String,
    pub frontier_model: String,
}

/// Complete auditable output shown by the Procedure view and saved to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchPreview {
    pub id: PatchPreviewId,
    pub localization_run_id: super::ProcedureRunId,
    pub change_id: String,
    pub task_id: String,
    pub route: RouteDecision,
    pub backend: String,
    pub model: String,
    pub targets: Vec<String>,
    pub rationale: String,
    pub unified_diff: String,
}

/// File-backed storage for preview evidence.
#[derive(Debug, Clone)]
pub struct PatchPreviewStore {
    directory: PathBuf,
}

impl PatchPreviewStore {
    pub fn for_project(project_root: &Path) -> Self {
        Self {
            directory: project_root.join(".deepseek/procedure/previews"),
        }
    }

    pub fn report_path(&self, id: PatchPreviewId) -> PathBuf {
        self.directory.join(format!("{}.json", id.as_str()))
    }

    pub fn save(&self, preview: &PatchPreview) -> Result<PathBuf, HarnessError> {
        std::fs::create_dir_all(&self.directory)?;
        let path = self.report_path(preview.id);
        let bytes = serde_json::to_vec_pretty(preview)
            .map_err(|error| HarnessError::Parse(error.to_string()))?;
        std::fs::write(&path, bytes)?;
        Ok(path)
    }

    /// Load one persisted preview by its explicit identity.
    pub fn load(&self, id: PatchPreviewId) -> Result<PatchPreview, HarnessError> {
        let path = self.report_path(id);
        let json = std::fs::read_to_string(&path)?;
        serde_json::from_str(&json).map_err(|error| {
            HarnessError::Parse(format!(
                "could not parse patch preview {}: {error}",
                path.display()
            ))
        })
    }
}

/// Failure before a preview is eligible for display.
#[derive(Debug, Error)]
pub enum PatchPreviewError {
    #[error(transparent)]
    Input(#[from] PatchPreviewInputError),
    #[error("patch preview was interrupted")]
    Interrupted,
    #[error("patch preview has no accepted localization targets")]
    MissingTargets,
    #[error("could not serialize patch preview context: {0}")]
    Context(#[from] serde_json::Error),
    #[error("could not read localized target `{path}` for patch context: {message}")]
    TargetContext { path: String, message: String },
    #[error("could not resolve patch backend `{backend}`: {message}")]
    Backend { backend: String, message: String },
    #[error(transparent)]
    LocalDraft(#[from] super::LocalPatchDraftError),
    #[error(transparent)]
    FrontierDraft(#[from] FrontierPatchDraftError),
    #[error("patch envelope route metadata does not match the deterministic route")]
    RouteMismatch,
    #[error(transparent)]
    Envelope(#[from] PatchEnvelopeError),
    #[error(transparent)]
    Boundary(#[from] PatchBoundaryError),
    #[error(transparent)]
    ApplyCheck(#[from] PatchApplyCheckError),
    #[error("patch envelope targets do not match its diff paths")]
    TargetMismatch,
    #[error("could not save patch preview: {0}")]
    Save(#[from] HarnessError),
}

/// Production preview pipeline. Every drafting path finishes at the same gates.
pub struct PatchPreviewRunner {
    input_gate: PatchPreviewInputGate,
    source_root: PathBuf,
    factory: Arc<BackendFactory>,
    interrupt: Arc<AtomicBool>,
    effort: Effort,
    max_tokens: u32,
    store: PatchPreviewStore,
}

impl PatchPreviewRunner {
    pub fn new(
        input_gate: PatchPreviewInputGate,
        source_root: PathBuf,
        factory: Arc<BackendFactory>,
        interrupt: Arc<AtomicBool>,
        effort: Effort,
        max_tokens: u32,
    ) -> Self {
        let store = PatchPreviewStore::for_project(&source_root);
        Self {
            input_gate,
            source_root,
            factory,
            interrupt,
            effort,
            max_tokens,
            store,
        }
    }

    pub async fn run(
        &self,
        id: PatchPreviewId,
        request: PatchPreviewRequest,
    ) -> Result<(PatchPreview, PathBuf), PatchPreviewError> {
        self.check_interrupted()?;
        let input = self.input_gate.load(&PatchPreviewInputRequest {
            localization_run_id: request.localization_run_id,
            change_id: request.change_id.clone(),
            task_id: request.task_id.clone(),
            route_override: request.route_override,
        })?;
        self.check_interrupted()?;

        let targets =
            accepted_target_paths(&input.report).ok_or(PatchPreviewError::MissingTargets)?;
        let contract_text = serde_json::to_string_pretty(&input.contract.contract)?;
        let route = apply_route_override(
            assess_route(&contract_text, targets.len()),
            input.route_override,
        );
        let (backend, model) = selected_backend_and_model(&request, route.effective_tier);
        let prompt = build_patch_prompt(
            &self.source_root,
            &contract_text,
            &targets,
            &route,
            &backend,
            &model,
        )?;

        let candidate = match route.effective_tier {
            RouteTier::Local => {
                let resolved = self
                    .factory
                    .resolve(&backend, Some(&model))
                    .map_err(|message| PatchPreviewError::Backend {
                        backend: backend.clone(),
                        message,
                    })?;
                let dispatcher = LocalPatchDraftDispatcher::from_resolved_backend(
                    resolved,
                    Effort::None,
                    self.max_tokens,
                )?;
                self.await_interruptibly(dispatcher.draft(prompt)).await??
            }
            RouteTier::Frontier => {
                let registry = Arc::new(SubagentRegistry::new());
                let (events, _events_rx) = mpsc::unbounded_channel();
                let draft = draft_frontier_patch(
                    &self.factory,
                    &self.source_root,
                    FrontierPatchDraftRequest {
                        backend: backend.clone(),
                        model: Some(model.clone()),
                        prompt,
                        effort: self.effort,
                    },
                    events,
                    Arc::clone(&registry),
                );
                let result = self.await_interruptibly(draft).await;
                registry.close_all().await;
                result??
            }
        };
        self.check_interrupted()?;

        let expected_route = PatchRouteMetadata::from(route.clone());
        if candidate.envelope().route != expected_route {
            return Err(PatchPreviewError::RouteMismatch);
        }
        let boundary = validate_patch_boundary(candidate, &targets)?;
        let checked = check_patch_applicability(&self.source_root, boundary)?;
        let envelope = checked.envelope();
        let mut declared_targets = envelope.targets.clone();
        declared_targets.sort();
        let diff_targets = checked.boundary_patch().paths();
        if declared_targets != diff_targets {
            return Err(PatchPreviewError::TargetMismatch);
        }
        let preview = PatchPreview {
            id,
            localization_run_id: request.localization_run_id,
            change_id: request.change_id,
            task_id: request.task_id,
            route,
            backend,
            model,
            targets: diff_targets.to_vec(),
            rationale: envelope.rationale.clone(),
            unified_diff: envelope.unified_diff.clone(),
        };
        let path = self.store.save(&preview)?;
        Ok((preview, path))
    }

    fn check_interrupted(&self) -> Result<(), PatchPreviewError> {
        if self.interrupt.load(Ordering::SeqCst) {
            Err(PatchPreviewError::Interrupted)
        } else {
            Ok(())
        }
    }

    async fn await_interruptibly<F, T>(&self, future: F) -> Result<T, PatchPreviewError>
    where
        F: std::future::Future<Output = T>,
    {
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => return Ok(result),
                _ = tokio::time::sleep(INTERRUPT_POLL_INTERVAL) => self.check_interrupted()?,
            }
        }
    }
}

fn accepted_target_paths(report: &super::ProcedureRun) -> Option<Vec<String>> {
    let mut paths = report
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.disposition == ProcedureAttemptDisposition::Accepted)?
        .targets
        .iter()
        .map(|target| target.path.replace('\\', "/"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    (!paths.is_empty()).then_some(paths)
}

fn selected_backend_and_model(request: &PatchPreviewRequest, tier: RouteTier) -> (String, String) {
    match tier {
        RouteTier::Local => (request.local_backend.clone(), request.local_model.clone()),
        RouteTier::Frontier => (
            request.frontier_backend.clone(),
            request.frontier_model.clone(),
        ),
    }
}

fn build_patch_prompt(
    source_root: &Path,
    contract: &str,
    targets: &[String],
    route: &RouteDecision,
    backend: &str,
    model: &str,
) -> Result<String, PatchPreviewError> {
    let route = serde_json::to_string_pretty(&PatchRouteMetadata::from(route.clone()))?;
    let target_context = read_target_context(source_root, targets)?;
    let targets = serde_json::to_string_pretty(targets)?;
    Ok(format!(
        "Draft a patch preview for the selected OpenSpec task. Return exactly one JSON object and no commentary.\n\nThe object must contain targets, rationale, route, and unified_diff. Copy this route object exactly:\n{route}\n\nOnly these repository paths are allowed, and targets must equal the paths used by the diff:\n{targets}\n\nSelected backend: {backend}\nSelected model: {model}\n\nOpenSpec contract:\n{contract}\n\nCurrent localized target contents:\n{target_context}\n\nThe unified_diff must be a complete git-style unified diff that applies to the current contents above. Its first line must be exactly `diff --git a/<path> b/<path>` with the real allowed path substituted for both placeholders. Follow it immediately with `--- a/<path>`, `+++ b/<path>`, and valid `@@` hunks. Never put a blank line inside a diff section. Every hunk body line must begin with one space, `+`, `-`, or the exact no-newline marker. A one-line replacement has exactly one `-` line and one `+` line after `@@ -1 +1 @@`. The unified_diff string must end with a newline after its final hunk line. Do not wrap the diff in Markdown. Do not apply the patch."
    ))
}

fn read_target_context(
    source_root: &Path,
    targets: &[String],
) -> Result<String, PatchPreviewError> {
    let mut context = String::new();
    for target in targets {
        let path = source_root.join(target);
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                "<path does not exist>\n".to_string()
            }
            Err(error) => {
                return Err(PatchPreviewError::TargetContext {
                    path: target.clone(),
                    message: error.to_string(),
                });
            }
        };
        context.push_str(&format!("--- BEGIN {target} ---\n"));
        context.push_str(&contents);
        if !contents.ends_with('\n') {
            context.push('\n');
        }
        context.push_str(&format!("--- END {target} ---\n"));
    }
    Ok(context)
}
