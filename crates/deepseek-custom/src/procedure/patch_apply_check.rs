//! Patch-hunk validation inside a disposable source snapshot.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;

use crate::mcp::spawn::resolve_command;

use super::{
    BoundaryValidatedPatch, DisposableDraftWorkspace, DisposableWorkspaceError, PatchEnvelope,
};

/// The deterministic git command run inside a verification workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitApplyPhase {
    Check,
    Apply,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitApplyResult {
    pub phase: GitApplyPhase,
    pub success: bool,
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
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
        #[source]
        source: std::io::Error,
    },
    #[error("could not send the patch to `{phase}` in draft workspace {workspace}: {source}")]
    Write {
        phase: GitApplyPhase,
        workspace: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not wait for `{phase}` in draft workspace {workspace}: {source}")]
    Wait {
        phase: GitApplyPhase,
        workspace: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("patch hunks do not apply to the current source snapshot: {diagnostics}")]
    Rejected {
        phase: GitApplyPhase,
        status_code: Option<i32>,
        stdout: String,
        stderr: String,
        diagnostics: String,
    },
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
    let workspace = DisposableDraftWorkspace::create_current_state(source_root)?;
    let check = match run_git_apply(
        workspace.path(),
        patch.envelope().unified_diff.as_bytes(),
        GitApplyPhase::Check,
    ) {
        Ok(result) => result,
        Err(error) => {
            drop(workspace);
            return Err(error);
        }
    };
    let apply = match run_git_apply(
        workspace.path(),
        patch.envelope().unified_diff.as_bytes(),
        GitApplyPhase::Apply,
    ) {
        Ok(result) => result,
        Err(error) => {
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

    let mut child = command
        .spawn()
        .map_err(|source| PatchApplyCheckError::Spawn {
            phase,
            workspace: workspace.to_path_buf(),
            source,
        })?;
    let mut stdin = child
        .stdin
        .take()
        .expect("piped git stdin must be available");
    if let Err(source) = stdin.write_all(diff) {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return Err(PatchApplyCheckError::Write {
            phase,
            workspace: workspace.to_path_buf(),
            source,
        });
    }
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|source| PatchApplyCheckError::Wait {
            phase,
            workspace: workspace.to_path_buf(),
            source,
        })?;
    let result = GitApplyResult {
        phase,
        success: output.status.success(),
        status_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    if result.success {
        return Ok(result);
    }

    let diagnostics = if !result.stderr.trim().is_empty() {
        result.stderr.trim().to_string()
    } else if !result.stdout.trim().is_empty() {
        result.stdout.trim().to_string()
    } else {
        format!(
            "{} exited with status {:?}",
            phase.command(),
            result.status_code
        )
    };
    Err(PatchApplyCheckError::Rejected {
        phase,
        status_code: result.status_code,
        stdout: result.stdout,
        stderr: result.stderr,
        diagnostics,
    })
}
