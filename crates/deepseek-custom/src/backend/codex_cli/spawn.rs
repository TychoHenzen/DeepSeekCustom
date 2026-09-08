//! Argument assembly and one-shot child creation for `codex exec --json`.

use std::collections::HashMap;
use std::path::Path;
#[cfg(feature = "test-support")]
use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::{Child, ChildStderr, ChildStdout, Command};

use crate::effort::Effort;
use crate::error::{HarnessError, Result};
use crate::mcp::spawn::resolve_command;

/// Handles the driver needs from one `codex exec` invocation.
pub(super) struct SpawnedCodex {
    pub(super) child: Child,
    pub(super) stdout: ChildStdout,
    pub(super) stderr: ChildStderr,
}

/// Build arguments for one fresh or resumed Codex turn.
///
/// Codex places `resume <thread_id>` directly after `exec`. All flags stay
/// after that subcommand, and the prompt remains the final positional value.
pub(super) fn build_args(
    prompt: &str,
    thread_id: Option<&str>,
    sandbox: Option<&str>,
    model: Option<&str>,
    effort: Effort,
) -> Vec<String> {
    let mut args = vec!["exec".to_owned()];
    if let Some(thread_id) = thread_id {
        args.push("resume".to_owned());
        args.push(thread_id.to_owned());
    }
    args.push("--json".to_owned());
    // Disposable preview snapshots intentionally exclude `.git`. Codex must
    // still accept them as isolated working directories.
    args.push("--skip-git-repo-check".to_owned());

    if let Some(sandbox) = sandbox {
        args.push("--sandbox".to_owned());
        args.push(sandbox.to_owned());
    } else {
        args.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
    }

    if let Some(model) = model {
        args.push("-m".to_owned());
        args.push(model.to_owned());
    }

    if let Some(level) = effort.codex_cli_effort_level() {
        args.push("-c".to_owned());
        args.push(format!("reasoning.effort={level}"));
    }

    args.push(prompt.to_owned());
    args
}

/// Build one fresh Controlled Development planning invocation.
///
/// This profile is always read-only, ignores user configuration and rules,
/// never resumes a thread, and requires the complete final response to match
/// the supplied JSON Schema file.
pub(super) fn build_planning_args(
    prompt: &str,
    output_schema: &Path,
    model: Option<&str>,
    effort: Effort,
) -> Vec<String> {
    let mut args = vec![
        "exec".to_owned(),
        "--json".to_owned(),
        "--skip-git-repo-check".to_owned(),
        "--sandbox".to_owned(),
        "read-only".to_owned(),
        "--ephemeral".to_owned(),
        "--ignore-user-config".to_owned(),
        "--ignore-rules".to_owned(),
        "--output-schema".to_owned(),
        output_schema.display().to_string(),
    ];
    if let Some(model) = model {
        args.push("-m".to_owned());
        args.push(model.to_owned());
    }
    if let Some(level) = effort.codex_cli_effort_level() {
        args.push("-c".to_owned());
        args.push(format!("reasoning.effort={level}"));
    }
    args.push(prompt.to_owned());
    args
}

/// Build one fresh Controlled Development execution invocation.
///
/// The writable sandbox is limited to the child process working directory.
/// User configuration and rules stay unavailable, and both Codex subagent
/// implementations are disabled for this invocation.
pub(super) fn build_execution_args(
    prompt: &str,
    model: Option<&str>,
    effort: Effort,
) -> Vec<String> {
    let mut args = vec![
        "exec".to_owned(),
        "--json".to_owned(),
        "--skip-git-repo-check".to_owned(),
        "--sandbox".to_owned(),
        "workspace-write".to_owned(),
        "--ephemeral".to_owned(),
        "--ignore-user-config".to_owned(),
        "--ignore-rules".to_owned(),
        "--disable".to_owned(),
        "multi_agent".to_owned(),
        "--disable".to_owned(),
        "multi_agent_v2".to_owned(),
    ];
    if let Some(model) = model {
        args.push("-m".to_owned());
        args.push(model.to_owned());
    }
    if let Some(level) = effort.codex_cli_effort_level() {
        args.push("-c".to_owned());
        args.push(format!("reasoning.effort={level}"));
    }
    args.push(prompt.to_owned());
    args
}

/// Test-only access to the argument contract without widening production API.
#[cfg(feature = "test-support")]
pub fn build_args_for_test(
    prompt: &str,
    thread_id: Option<&str>,
    sandbox: Option<&str>,
    model: Option<&str>,
    effort: Effort,
) -> Vec<String> {
    build_args(prompt, thread_id, sandbox, model, effort)
}

#[cfg(feature = "test-support")]
pub fn build_planning_args_for_test(
    prompt: &str,
    output_schema: &Path,
    model: Option<&str>,
    effort: Effort,
) -> Vec<String> {
    build_planning_args(prompt, output_schema, model, effort)
}

#[cfg(feature = "test-support")]
pub fn build_execution_args_for_test(
    prompt: &str,
    model: Option<&str>,
    effort: Effort,
) -> Vec<String> {
    build_execution_args(prompt, model, effort)
}

fn build_command(args: &[String], working_dir: &Path) -> Command {
    let resolved = resolve_command("codex");
    let mut command = Command::new(&resolved.program);
    command
        .args(&resolved.prefix_args)
        .args(args)
        .current_dir(working_dir)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Returns the directory configured on the real spawn command.
#[cfg(feature = "test-support")]
pub fn command_working_dir_for_test(args: &[String], working_dir: &Path) -> Option<PathBuf> {
    build_command(args, working_dir)
        .as_std()
        .get_current_dir()
        .map(Path::to_path_buf)
}

/// Spawn one Codex turn in `working_dir`.
///
/// `codex` is resolved with the shared Windows `PATH` and `PATHEXT` logic.
/// Batch-file installations run through `cmd /c`. The returned stdout and
/// stderr handles remain readable by the driver.
pub(super) fn spawn_codex(
    args: &[String],
    working_dir: &Path,
    extra_env: Option<&HashMap<String, String>>,
) -> Result<SpawnedCodex> {
    let mut command = build_command(args, working_dir);

    if let Some(extra_env) = extra_env {
        command.envs(extra_env);
    }

    let mut child = command.spawn().map_err(HarnessError::Io)?;
    crate::process_group::adopt(&child);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::Tool("codex CLI child has no stdout".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| HarnessError::Tool("codex CLI child has no stderr".to_owned()))?;

    Ok(SpawnedCodex {
        child,
        stdout,
        stderr,
    })
}
