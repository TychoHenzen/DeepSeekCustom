//! Run one shell command with a candidate's text piped to its stdin.
//!
//! Both scoring tools need this and neither can use `bash::run_command`,
//! which closes stdin. A `fitness_cmd` and a `check_cmd` are only useful
//! when they can read the candidate they are judging, so the text has to
//! reach the child somehow, and stdin is the one channel that needs no
//! temporary file and no quoting.
//!
//! Shell selection matches `bash::run_command`: a command starting with
//! `powershell` or `pwsh` runs directly, everything else goes through
//! `cmd /C`. Without that, a PowerShell scoring command pays cmd.exe's
//! inner-quote mangling that the Bash tool already learned to avoid.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::tools::bash::split_shell_words;

/// How long one scoring or check command may run before it is killed.
const COMMAND_TIMEOUT: Duration = Duration::from_millis(120_000);

/// Raw output from a command run through [`run_command_with_stdin`].
pub struct StdinCmdOutput {
    pub stdout: Vec<u8>,
    pub exit_code: i32,
}

/// Run `cmd_str` in `work_dir` with `stdin_text` piped to its stdin.
///
/// Returns the raw stdout and the exit code, or an error string on a spawn
/// failure or a timeout. The caller decides what a non-zero exit means.
pub async fn run_command_with_stdin(
    cmd_str: &str,
    stdin_text: &str,
    work_dir: &std::path::Path,
) -> std::result::Result<StdinCmdOutput, String> {
    match timeout(
        COMMAND_TIMEOUT,
        spawn_and_wait(cmd_str, stdin_text, work_dir),
    )
    .await
    {
        Ok(result) => result,
        Err(_elapsed) => Err("command timed out after 120s".to_string()),
    }
}

/// The command and arguments for `cmd_str`, matching `bash::run_command`'s
/// own auto-detection.
fn program_and_args(cmd_str: &str) -> (String, Vec<String>) {
    let trimmed = cmd_str.trim().to_lowercase();
    if trimmed.starts_with("powershell") || trimmed.starts_with("pwsh") {
        let words = split_shell_words(cmd_str);
        return match words.split_first() {
            Some((program, rest)) => (program.clone(), rest.to_vec()),
            None => ("powershell.exe".to_string(), Vec::new()),
        };
    }
    (
        "cmd".to_string(),
        vec!["/C".to_string(), cmd_str.to_string()],
    )
}

async fn spawn_and_wait(
    cmd_str: &str,
    stdin_text: &str,
    work_dir: &std::path::Path,
) -> std::result::Result<StdinCmdOutput, String> {
    let (program, args) = program_and_args(cmd_str);
    let mut child = Command::new(&program)
        .args(&args)
        .current_dir(work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to spawn command: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_text.as_bytes())
            .await
            .map_err(|e| format!("failed to write to command stdin: {e}"))?;
    }
    // stdin is dropped here, closing the pipe.

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("command failed: {e}"))?;
    Ok(StdinCmdOutput {
        stdout: output.stdout,
        exit_code: output.status.code().unwrap_or(-1),
    })
}
