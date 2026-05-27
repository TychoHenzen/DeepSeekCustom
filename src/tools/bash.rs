use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, info};

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

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
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the project working directory. Returns stdout, stderr, and exit code."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds (default: 120000)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: BashInput = serde_json::from_value(input)
            .map_err(|e| HarnessError::Tool(format!("Invalid bash input: {e}")))?;

        let timeout_dur = Duration::from_millis(parsed.timeout_ms.unwrap_or(120_000));
        debug!("bash: command={}", parsed.command);

        let result = timeout(timeout_dur, run_command(&parsed.command, &self.work_dir)).await;

        match result {
            Ok(Ok(output)) => {
                info!("bash: exit_code={}", output.exit_code);
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

async fn run_command(cmd: &str, work_dir: &std::path::Path) -> std::result::Result<CommandOutput, std::io::Error> {
    let output = Command::new("cmd")
        .args(["/C", cmd])
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
        // Start a command that sleeps for 30 seconds, timeout at 500ms
        let input = serde_json::json!({"command": "ping -n 30 127.0.0.1 > nul", "timeout_ms": 500});
        let output = tool.execute(input).await.expect("execute");
        assert!(output.is_error);
        assert!(output.content.contains("timed out"));
    }
}
