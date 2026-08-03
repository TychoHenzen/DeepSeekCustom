use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, info};

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

/// Which shell to use for command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
    /// Auto-detect: PowerShell commands run directly, everything else via cmd.
    Auto,
    /// Force cmd.exe /C
    Cmd,
    /// Force powershell.exe -NoProfile -Command
    Ps,
}

/// Shell command execution tool.
pub struct BashTool {
    work_dir: std::path::PathBuf,
}

impl BashTool {
    pub fn new(work_dir: std::path::PathBuf) -> Self {
        Self { work_dir }
    }
}

#[derive(Deserialize)]
struct BashInput {
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
    fn resolve_shell(&self) -> Shell {
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

        let result = timeout(
            timeout_dur,
            run_command(&parsed.command, &self.work_dir, shell),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                info!("bash: exit_code={}, shell={:?}", output.exit_code, shell);
                Ok(ToolOutput {
                    content: format_output(&output),
                    is_error: output.exit_code != 0,
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
fn split_shell_words(input: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_schema_is_valid_json() {
        let tool = BashTool::new(std::env::current_dir().unwrap());
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["command"]["type"] == "string");
    }

    #[tokio::test]
    async fn echo_hello_returns_correct_stdout() {
        let tool = BashTool::new(std::env::current_dir().unwrap());
        let input = serde_json::json!({"command": "echo hello"});
        let output = tool.execute(input).await.expect("execute");
        assert!(!output.is_error);
        assert!(output.content.contains("hello"));
        assert!(output.content.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn timeout_kills_long_running_command() {
        let tool = BashTool::new(std::env::current_dir().unwrap());
        // A cmd builtin loop, so the test does not depend on any program being
        // on PATH. It runs for minutes, and the timeout must cut it short.
        let input = serde_json::json!({
            "command": "for /L %i in (1,1,200000000) do @rem",
            "timeout_ms": 500
        });
        let output = tool.execute(input).await.expect("execute");
        assert!(
            output.is_error,
            "expected an error, got: {}",
            output.content
        );
        assert!(
            output.content.contains("timed out"),
            "expected a timeout, got: {}",
            output.content
        );
    }

    #[tokio::test]
    async fn powershell_auto_detected_and_run_directly() {
        let tool = BashTool::new(std::env::current_dir().unwrap());
        let input = serde_json::json!({
            "command": "powershell -NoProfile -Command \"Write-Output 'ps_hello'\""
        });
        let output = tool.execute(input).await.expect("execute");
        assert!(!output.is_error);
        assert!(
            output.content.contains("ps_hello"),
            "expected 'ps_hello' in output, got: {}",
            output.content
        );
        assert!(output.content.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn explicit_shell_cmd_works() {
        let tool = BashTool::new(std::env::current_dir().unwrap());
        let input = serde_json::json!({
            "command": "echo cmd_explicit",
            "shell": "cmd"
        });
        let output = tool.execute(input).await.expect("execute");
        assert!(!output.is_error);
        assert!(output.content.contains("cmd_explicit"));
    }

    #[tokio::test]
    async fn explicit_shell_powershell_works() {
        let tool = BashTool::new(std::env::current_dir().unwrap());
        let input = serde_json::json!({
            "command": "powershell -NoProfile -Command \"Write-Output 'pwsh_explicit'\"",
            "shell": "powershell"
        });
        let output = tool.execute(input).await.expect("execute");
        assert!(!output.is_error);
        assert!(
            output.content.contains("pwsh_explicit"),
            "expected 'pwsh_explicit' in output, got: {}",
            output.content
        );
    }

    #[test]
    fn split_shell_words_handles_quotes() {
        let result =
            split_shell_words("powershell -NoProfile -Command \"Write-Output 'hello world'\"");
        assert_eq!(result.len(), 4);
        assert_eq!(result[0], "powershell");
        assert_eq!(result[1], "-NoProfile");
        assert_eq!(result[2], "-Command");
        assert_eq!(result[3], "Write-Output 'hello world'");
    }

    #[test]
    fn split_shell_words_simple_command() {
        let result = split_shell_words("echo hello world");
        assert_eq!(result, vec!["echo", "hello", "world"]);
    }

    #[test]
    fn resolve_shell_defaults_to_auto() {
        let input: BashInput = serde_json::from_value(serde_json::json!({
            "command": "echo hello"
        }))
        .unwrap();
        assert_eq!(input.resolve_shell(), Shell::Auto);
    }

    #[test]
    fn resolve_shell_explicit_cmd() {
        let input: BashInput = serde_json::from_value(serde_json::json!({
            "command": "echo hello",
            "shell": "cmd"
        }))
        .unwrap();
        assert_eq!(input.resolve_shell(), Shell::Cmd);
    }

    #[test]
    fn resolve_shell_explicit_powershell() {
        let input: BashInput = serde_json::from_value(serde_json::json!({
            "command": "echo hello",
            "shell": "powershell"
        }))
        .unwrap();
        assert_eq!(input.resolve_shell(), Shell::Ps);
    }
}
