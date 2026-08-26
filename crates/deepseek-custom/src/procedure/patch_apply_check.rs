//! Patch-hunk validation inside a disposable source snapshot.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mcp::spawn::resolve_command;

use super::{
    BoundaryValidatedPatch, BoundedVerifierOutput, DisposableDraftWorkspace,
    DisposableWorkspaceError, DisposableWorkspaceOptions, PatchEnvelope, SnapshotProgress,
};

/// The deterministic git command run inside a verification workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitApplyPhase {
    Check,
    Apply,
}

/// The terminal result of one attempted patch gate command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitApplyDisposition {
    Passed,
    Rejected,
    SpawnFailed,
    WriteFailed,
    WaitFailed,
}

/// A patch gate that did not run because an earlier patch gate stopped the sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchGateDisposition {
    Passed,
    Rejected,
    SpawnFailed,
    WriteFailed,
    WaitFailed,
    NotRun { blocked_by: GitApplyPhase },
}

impl GitApplyPhase {
    fn command(self) -> &'static str {
        match self {
            Self::Check => "git apply --check",
            Self::Apply => "git apply",
        }
    }
}

impl std::fmt::Display for GitApplyPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.command())
    }
}

/// Preserved status and output from one git apply command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitApplyResult {
    pub phase: GitApplyPhase,
    pub command: String,
    pub disposition: GitApplyDisposition,
    pub success: bool,
    pub status_code: Option<i32>,
    pub stdout: BoundedVerifierOutput,
    pub stderr: BoundedVerifierOutput,
    pub combined_output: BoundedVerifierOutput,
    pub duration_millis: u64,
    pub error: Option<String>,
}

impl GitApplyResult {
    pub fn evidence(&self) -> PatchGateEvidence {
        PatchGateEvidence {
            phase: self.phase,
            command: self.command.clone(),
            disposition: match self.disposition {
                GitApplyDisposition::Passed => PatchGateDisposition::Passed,
                GitApplyDisposition::Rejected => PatchGateDisposition::Rejected,
                GitApplyDisposition::SpawnFailed => PatchGateDisposition::SpawnFailed,
                GitApplyDisposition::WriteFailed => PatchGateDisposition::WriteFailed,
                GitApplyDisposition::WaitFailed => PatchGateDisposition::WaitFailed,
            },
            result: Some(self.clone()),
        }
    }
}

/// Serializable evidence for one executed or skipped patch gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchGateEvidence {
    pub phase: GitApplyPhase,
    pub command: String,
    pub disposition: PatchGateDisposition,
    pub result: Option<GitApplyResult>,
}

impl PatchGateEvidence {
    pub fn not_run(phase: GitApplyPhase, blocked_by: GitApplyPhase) -> Self {
        Self {
            phase,
            command: phase.command().to_string(),
            disposition: PatchGateDisposition::NotRun { blocked_by },
            result: None,
        }
    }
}

/// One truthful transition from snapshot creation or a patch gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchApplyProgress {
    Snapshot(SnapshotProgress),
    GateStarted(GitApplyPhase),
    GateCompleted(Box<GitApplyResult>),
}

/// A localization-boundary patch whose hunks match the current source snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCheckedPatch {
    patch: BoundaryValidatedPatch,
}

impl ApplyCheckedPatch {
    pub fn envelope(&self) -> &PatchEnvelope {
        self.patch.envelope()
    }

    pub fn boundary_patch(&self) -> &BoundaryValidatedPatch {
        &self.patch
    }

    pub fn into_boundary_patch(self) -> BoundaryValidatedPatch {
        self.patch
    }
}

/// A patch applied to an owned verification workspace.
///
/// The workspace remains available for later deterministic project commands.
/// Dropping this value removes the workspace and never changes the source root.
#[derive(Debug)]
pub struct AppliedPatchWorkspace {
    workspace: DisposableDraftWorkspace,
    patch: BoundaryValidatedPatch,
    check: GitApplyResult,
    apply: GitApplyResult,
}

impl AppliedPatchWorkspace {
    pub fn path(&self) -> &Path {
        self.workspace.path()
    }

    pub fn boundary_patch(&self) -> &BoundaryValidatedPatch {
        &self.patch
    }

    pub fn check_result(&self) -> &GitApplyResult {
        &self.check
    }

    pub fn apply_result(&self) -> &GitApplyResult {
        &self.apply
    }

    /// Remove the failed verification workspace before another attempt starts.
    pub fn close(self) -> Result<(), DisposableWorkspaceError> {
        self.workspace.close()
    }
}

/// Failure while checking patch hunks against an isolated source snapshot.
#[derive(Debug, Error)]
pub enum PatchApplyCheckError {
    #[error(transparent)]
    Workspace(#[from] DisposableWorkspaceError),
    #[error("could not start `{phase}` in draft workspace {workspace}: {source}")]
    Spawn {
        phase: GitApplyPhase,
        workspace: PathBuf,
        result: Box<GitApplyResult>,
        #[source]
        source: std::io::Error,
    },
    #[error("could not send the patch to `{phase}` in draft workspace {workspace}: {source}")]
    Write {
        phase: GitApplyPhase,
        workspace: PathBuf,
        result: Box<GitApplyResult>,
        #[source]
        source: std::io::Error,
    },
    #[error("could not wait for `{phase}` in draft workspace {workspace}: {source}")]
    Wait {
        phase: GitApplyPhase,
        workspace: PathBuf,
        result: Box<GitApplyResult>,
        #[source]
        source: std::io::Error,
    },
    #[error("patch hunks do not apply to the current source snapshot: {diagnostics}")]
    Rejected {
        result: Box<GitApplyResult>,
        diagnostics: String,
    },
}

impl PatchApplyCheckError {
    pub fn result(&self) -> Option<&GitApplyResult> {
        match self {
            Self::Workspace(_) => None,
            Self::Spawn { result, .. }
            | Self::Write { result, .. }
            | Self::Wait { result, .. }
            | Self::Rejected { result, .. } => Some(result),
        }
    }

    pub fn is_deterministic_rejection(&self) -> bool {
        matches!(self, Self::Rejected { .. })
    }
}

/// Check a boundary-validated patch without changing either source tree.
pub fn check_patch_applicability(
    source_root: &Path,
    patch: BoundaryValidatedPatch,
) -> Result<ApplyCheckedPatch, PatchApplyCheckError> {
    let workspace = DisposableDraftWorkspace::create_current_state(source_root)?;
    let check = run_git_apply_check(workspace.path(), patch.envelope().unified_diff.as_bytes());
    workspace.close()?;
    check?;
    Ok(ApplyCheckedPatch { patch })
}

/// Check and apply a patch inside a disposable current-state workspace.
pub fn apply_patch_in_workspace(
    source_root: &Path,
    patch: BoundaryValidatedPatch,
) -> Result<AppliedPatchWorkspace, PatchApplyCheckError> {
    apply_patch_in_workspace_with_progress(source_root, patch, |_| {})
}

/// Check and apply a patch while reporting snapshot and gate transitions.
pub fn apply_patch_in_workspace_with_progress(
    source_root: &Path,
    patch: BoundaryValidatedPatch,
    mut progress: impl FnMut(PatchApplyProgress),
) -> Result<AppliedPatchWorkspace, PatchApplyCheckError> {
    let workspace = DisposableDraftWorkspace::create_current_state_with_progress(
        source_root,
        &DisposableWorkspaceOptions::default(),
        &mut |snapshot| progress(PatchApplyProgress::Snapshot(snapshot)),
    )?;
    progress(PatchApplyProgress::GateStarted(GitApplyPhase::Check));
    let check = match run_git_apply(
        workspace.path(),
        patch.envelope().unified_diff.as_bytes(),
        GitApplyPhase::Check,
    ) {
        Ok(result) => {
            progress(PatchApplyProgress::GateCompleted(Box::new(result.clone())));
            result
        }
        Err(error) => {
            if let Some(result) = error.result() {
                progress(PatchApplyProgress::GateCompleted(Box::new(result.clone())));
            }
            drop(workspace);
            return Err(error);
        }
    };
    progress(PatchApplyProgress::GateStarted(GitApplyPhase::Apply));
    let apply = match run_git_apply(
        workspace.path(),
        patch.envelope().unified_diff.as_bytes(),
        GitApplyPhase::Apply,
    ) {
        Ok(result) => {
            progress(PatchApplyProgress::GateCompleted(Box::new(result.clone())));
            result
        }
        Err(error) => {
            if let Some(result) = error.result() {
                progress(PatchApplyProgress::GateCompleted(Box::new(result.clone())));
            }
            drop(workspace);
            return Err(error);
        }
    };
    Ok(AppliedPatchWorkspace {
        workspace,
        patch,
        check,
        apply,
    })
}

fn run_git_apply_check(workspace: &Path, diff: &[u8]) -> Result<(), PatchApplyCheckError> {
    run_git_apply(workspace, diff, GitApplyPhase::Check).map(|_| ())
}

fn run_git_apply(
    workspace: &Path,
    diff: &[u8],
    phase: GitApplyPhase,
) -> Result<GitApplyResult, PatchApplyCheckError> {
    let started = Instant::now();
    let resolved = resolve_command("git");
    let mut command = Command::new(&resolved.program);
    let args = match phase {
        GitApplyPhase::Check => vec![
            "-c",
            "core.autocrlf=false",
            "apply",
            "--check",
            "--no-index",
            "-",
        ],
        GitApplyPhase::Apply => vec!["-c", "core.autocrlf=false", "apply", "--no-index", "-"],
    };
    command
        .args(&resolved.prefix_args)
        .args(args)
        .current_dir(workspace)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|source| {
        let result = failed_git_apply_result(
            phase,
            GitApplyDisposition::SpawnFailed,
            started.elapsed(),
            source.to_string(),
        );
        PatchApplyCheckError::Spawn {
            phase,
            workspace: workspace.to_path_buf(),
            result: Box::new(result),
            source,
        }
    })?;
    let mut stdin = child
        .stdin
        .take()
        .expect("piped git stdin must be available");
    if let Err(source) = stdin.write_all(diff) {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        let result = failed_git_apply_result(
            phase,
            GitApplyDisposition::WriteFailed,
            started.elapsed(),
            source.to_string(),
        );
        return Err(PatchApplyCheckError::Write {
            phase,
            workspace: workspace.to_path_buf(),
            result: Box::new(result),
            source,
        });
    }
    drop(stdin);
    let output = child.wait_with_output().map_err(|source| {
        let result = failed_git_apply_result(
            phase,
            GitApplyDisposition::WaitFailed,
            started.elapsed(),
            source.to_string(),
        );
        PatchApplyCheckError::Wait {
            phase,
            workspace: workspace.to_path_buf(),
            result: Box::new(result),
            source,
        }
    })?;
    let stdout = BoundedVerifierOutput::from_bytes(&output.stdout);
    let stderr = BoundedVerifierOutput::from_bytes(&output.stderr);
    let mut combined = output.stdout.clone();
    combined.extend_from_slice(&output.stderr);
    let result = GitApplyResult {
        phase,
        command: phase.command().to_string(),
        disposition: if output.status.success() {
            GitApplyDisposition::Passed
        } else {
            GitApplyDisposition::Rejected
        },
        success: output.status.success(),
        status_code: output.status.code(),
        stdout,
        stderr,
        combined_output: BoundedVerifierOutput::from_bytes(&combined),
        duration_millis: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        error: None,
    };
    if result.success {
        return Ok(result);
    }

    let diagnostics = if !result.stderr.text.trim().is_empty() {
        result.stderr.text.trim().to_string()
    } else if !result.stdout.text.trim().is_empty() {
        result.stdout.text.trim().to_string()
    } else {
        format!(
            "{} exited with status {:?}",
            phase.command(),
            result.status_code
        )
    };
    Err(PatchApplyCheckError::Rejected {
        result: Box::new(result),
        diagnostics,
    })
}

fn failed_git_apply_result(
    phase: GitApplyPhase,
    disposition: GitApplyDisposition,
    duration: std::time::Duration,
    error: String,
) -> GitApplyResult {
    GitApplyResult {
        phase,
        command: phase.command().to_string(),
        disposition,
        success: false,
        status_code: None,
        stdout: BoundedVerifierOutput::empty(),
        stderr: BoundedVerifierOutput::empty(),
        combined_output: BoundedVerifierOutput::empty(),
        duration_millis: duration.as_millis().min(u64::MAX as u128) as u64,
        error: Some(error),
    }
}
