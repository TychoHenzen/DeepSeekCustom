use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, info};

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

/// Which shell to use for command execution.
///
/// `pub`: `BashInput::resolve_shell` (below) returns this, and that method
/// is itself `pub` for a reason its own doc comment states. A caller of
/// that method needs this type's name to hold the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// Auto-detect: PowerShell commands run directly, everything else via cmd.
    Auto,
    /// Force cmd.exe /C
    Cmd,
    /// Force powershell.exe -NoProfile -Command
    Ps,
}

/// Shell command execution tool. `work_dir` is shared with every other tool
/// this harness registers, the same `Arc<Mutex<PathBuf>>` a `Cd` tool (a
/// later phase) will write into. It is read fresh on every `execute` call,
/// not captured at construction, so a change takes effect on the next
/// command run.
pub struct BashTool {
    work_dir: Arc<Mutex<std::path::PathBuf>>,
}

impl BashTool {
    pub fn new(work_dir: Arc<Mutex<std::path::PathBuf>>) -> Self {
        Self { work_dir }
    }
}

/// `pub`: its own tests build one straight from `serde_json::from_value`
/// and call `resolve_shell` on it directly, rather than through
/// `Tool::execute`, to check the shell-classification decision in
/// isolation. See `resolve_shell`'s own doc comment for why that decision
/// cannot be observed by running a real command.
#[derive(Deserialize)]
pub struct BashInput {
    command: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default = "default_shell")]
    shell: String,
}

fn default_shell() -> String {
    "auto".into()
}

impl BashInput {
    /// Maps the `shell` field onto how the command actually runs. `pub`,
    /// gated on nothing: this is the one internal flagged as needing a
    /// visibility change with no production caller left unconditional, and
    /// there is no side-effect-free public seam that exposes the
    /// classification alone. Driving this through `Tool::execute` cannot
    /// substitute: an `Auto`-classified non-PowerShell command and an
    /// explicit `Cmd` command both end up running through `cmd.exe`, so a
    /// real execution's output cannot tell which branch this method chose,
    /// only that both landed on the same shell. Only calling this method
    /// directly can test that decision itself, separate from its effect.
    pub fn resolve_shell(&self) -> Shell {
        match self.shell.as_str() {
            "cmd" => Shell::Cmd,
            "powershell" | "pwsh" => Shell::Ps,
            _ => Shell::Auto,
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the project working directory. \
         Defaults to cmd.exe on Windows; auto-detects powershell/pwsh commands \
         and runs them directly. Use shell='powershell' for PowerShell syntax \
         ($env:VAR, Get-ChildItem, etc.). Returns stdout, stderr, and exit code."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute. Use cmd.exe syntax by default, or start with 'powershell'/'pwsh' for PowerShell."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds (default: 120000)"
                },
                "shell": {
                    "type": "string",
                    "enum": ["auto", "cmd", "powershell"],
                    "description": "Which shell to use. 'auto' (default) detects PowerShell commands automatically."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: BashInput = serde_json::from_value(input)
            .map_err(|e| HarnessError::Tool(format!("Invalid bash input: {e}")))?;

        let timeout_dur = Duration::from_millis(parsed.timeout_ms.unwrap_or(120_000));
        let shell = parsed.resolve_shell();
        debug!("bash: command={}, shell={:?}", parsed.command, shell);

        // Read the shared working directory fresh on every call, so a
        // change made between two calls takes effect on the next one.
        let work_dir = self
            .work_dir
            .lock()
            .expect("work_dir mutex poisoned")
            .clone();

        let result = timeout(timeout_dur, run_command(&parsed.command, &work_dir, shell)).await;

        match result {
            Ok(Ok(output)) => {
                info!("bash: exit_code={}, shell={:?}", output.exit_code, shell);
                Ok(ToolOutput {
                    content: format_output(&output),
                    is_error: output.exit_code != 0,
                    image: None,
                })
            }
            Ok(Err(e)) => Err(HarnessError::Tool(format!("bash: {e}"))),
            Err(_elapsed) => Ok(ToolOutput {
                content: format!(
                    "Command timed out after {}ms\nCommand: {}",
                    timeout_dur.as_millis(),
                    parsed.command
                ),
                is_error: true,
                image: None,
            }),
        }
    }
}

struct CommandOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn format_output(out: &CommandOutput) -> String {
    let mut s = String::new();
    if !out.stdout.is_empty() {
        s.push_str(&out.stdout);
    }
    if !out.stderr.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("[stderr]\n");
        s.push_str(&out.stderr);
    }
    s.push_str(&format!("\n[exit code: {}]", out.exit_code));
    s
}

async fn run_command(
    cmd_str: &str,
    work_dir: &std::path::Path,
    shell: Shell,
) -> std::result::Result<CommandOutput, std::io::Error> {
    let use_powershell = match shell {
        Shell::Ps => true,
        Shell::Cmd => false,
        Shell::Auto => {
            let trimmed = cmd_str.trim().to_lowercase();
            trimmed.starts_with("powershell") || trimmed.starts_with("pwsh")
        }
    };

    if use_powershell {
        run_powershell_direct(cmd_str, work_dir).await
    } else {
        run_cmd(cmd_str, work_dir).await
    }
}

/// Run command via cmd.exe /C (default Windows shell).
async fn run_cmd(
    cmd_str: &str,
    work_dir: &std::path::Path,
) -> std::result::Result<CommandOutput, std::io::Error> {
    let output = Command::new("cmd")
        .args(["/C", cmd_str])
        .current_dir(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;

    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Run a PowerShell command directly (not wrapped in cmd /C).
/// Parses the command string to extract the executable and arguments,
/// avoiding cmd.exe's quote-mangling of inner double-quotes.
async fn run_powershell_direct(
    cmd_str: &str,
    work_dir: &std::path::Path,
) -> std::result::Result<CommandOutput, std::io::Error> {
    let args = split_shell_words(cmd_str);
    let (program, args) = if args.is_empty() {
        ("powershell.exe".to_string(), vec![])
    } else {
        (args[0].clone(), args[1..].to_vec())
    };

    let output = Command::new(&program)
        .args(&args)
        .current_dir(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;

    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Split a command string into shell words, respecting double-quote grouping.
/// "powershell -NoProfile -Command \"Write-Host 'Hello'\""
/// → ["powershell", "-NoProfile", "-Command", "Write-Host 'Hello'"]
///
/// `pub`: its own tests check the parsed word boundaries directly, which a
/// real process spawn cannot observe. `run_powershell_direct` only ever
/// sees the resulting `Command::new`/`args` call, not the intermediate
/// `Vec<String>`, so there is no output-based seam that reveals where this
/// function drew a word boundary.
pub fn split_shell_words(input: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
            }
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

