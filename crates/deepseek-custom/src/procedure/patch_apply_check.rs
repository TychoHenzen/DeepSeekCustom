//! Patch-hunk validation inside a disposable source snapshot.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;

use crate::mcp::spawn::resolve_command;

use super::{
    BoundaryValidatedPatch, DisposableDraftWorkspace, DisposableWorkspaceError, PatchEnvelope,
};

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

/// Failure while checking patch hunks against an isolated source snapshot.
#[derive(Debug, Error)]
pub enum PatchApplyCheckError {
    #[error(transparent)]
    Workspace(#[from] DisposableWorkspaceError),
    #[error("could not start `git apply --check` in draft workspace {workspace}: {source}")]
    Spawn {
        workspace: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "could not send the patch to `git apply --check` in draft workspace {workspace}: {source}"
    )]
    Write {
        workspace: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not wait for `git apply --check` in draft workspace {workspace}: {source}")]
    Wait {
        workspace: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("patch hunks do not apply to the current source snapshot: {diagnostics}")]
    Rejected { diagnostics: String },
}

/// Check a boundary-validated patch without changing either source tree.
pub fn check_patch_applicability(
    source_root: &Path,
    patch: BoundaryValidatedPatch,
) -> Result<ApplyCheckedPatch, PatchApplyCheckError> {
    let workspace = DisposableDraftWorkspace::create(source_root)?;
    let check = run_git_apply_check(workspace.path(), patch.envelope().unified_diff.as_bytes());
    workspace.close()?;
    check?;
    Ok(ApplyCheckedPatch { patch })
}

fn run_git_apply_check(workspace: &Path, diff: &[u8]) -> Result<(), PatchApplyCheckError> {
    let resolved = resolve_command("git");
    let mut command = Command::new(&resolved.program);
    command
        .args(&resolved.prefix_args)
        .args(["apply", "--check", "--no-index", "-"])
        .current_dir(workspace)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|source| PatchApplyCheckError::Spawn {
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
            workspace: workspace.to_path_buf(),
            source,
        });
    }
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|source| PatchApplyCheckError::Wait {
            workspace: workspace.to_path_buf(),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }

    let diagnostics = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(PatchApplyCheckError::Rejected {
        diagnostics: if diagnostics.is_empty() {
            format!("git exited with {}", output.status)
        } else {
            diagnostics
        },
    })
}
