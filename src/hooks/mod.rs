use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use crate::config::settings::HookDef;
use crate::error::{HarnessError, Result};
use crate::tools::ToolOutput;

/// Events that can trigger hooks.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum HookEvent {
    PreToolUse {
        tool: String,
        input: serde_json::Value,
    },
    PostToolUse {
        tool: String,
        input: serde_json::Value,
        output: HookToolOutput,
    },
    SessionStart,
    SessionEnd,
    SessionReset,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl From<&ToolOutput> for HookToolOutput {
    fn from(o: &ToolOutput) -> Self {
        Self {
            content: o.content.clone(),
            is_error: o.is_error,
        }
    }
}

/// Result returned from a hook script (JSON on stdout).
#[derive(Debug, Clone, Deserialize)]
pub struct HookResult {
    #[serde(default = "default_true")]
    pub approved: bool,
    #[serde(default)]
    pub modified_input: Option<serde_json::Value>,
    #[serde(default)]
    pub message: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Runs hook commands for a given event.
pub struct HookRunner;

impl HookRunner {
    /// Run all registered hooks for an event. Returns true if execution may proceed.
    pub async fn run(event: &HookEvent, hooks: &[HookDef]) -> Result<bool> {
        let event_json = serde_json::to_string(event)
            .map_err(|e| HarnessError::Hook(format!("failed to serialize event: {e}")))?;

        for hook in hooks {
            let hook_timeout = Duration::from_millis(hook.timeout.unwrap_or(30_000));
            debug!("hook: running {}", hook.command);

            let result = Self::run_one(&hook.command, &event_json, hook_timeout).await;

            match result {
                Ok(hook_result) => {
                    if let Some(ref msg) = hook_result.message {
                        info!("hook message: {msg}");
                    }
                    if !hook_result.approved {
                        warn!("hook blocked: {}", hook.command);
                        return Ok(false);
                    }
                }
                Err(e) => {
                    // Hook failures don't block execution
                    error!("hook failed (non-blocking): {e}");
                }
            }
        }

        Ok(true)
    }

    async fn run_one(command: &str, stdin_json: &str, dur: Duration) -> Result<HookResult> {
        let mut child = Command::new("cmd")
            .args(["/C", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| HarnessError::Hook(format!("failed to spawn hook: {e}")))?;

        // Write JSON to stdin
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(stdin_json.as_bytes()).await;
            let _ = stdin.write_all(b"\n").await;
        }

        let output = timeout(dur, child.wait_with_output())
            .await
            .map_err(|_| HarnessError::Hook("hook timed out".into()))?
            .map_err(|e| HarnessError::Hook(format!("hook process error: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("hook exited non-zero: {stderr}");
            // Non-zero exit → don't block
            return Ok(HookResult {
                approved: true,
                modified_input: None,
                message: None,
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Ok(HookResult {
                approved: true,
                modified_input: None,
                message: None,
            });
        }

        match serde_json::from_str::<HookResult>(stdout.trim()) {
            Ok(result) => Ok(result),
            Err(e) => {
                warn!("hook returned invalid JSON: {e}");
                Ok(HookResult {
                    approved: true,
                    modified_input: None,
                    message: Some(format!("invalid JSON from hook: {stdout}")),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_event_serializes_pre_tool() {
        let event = HookEvent::PreToolUse {
            tool: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"pretooluse\""));
        assert!(json.contains("\"tool\":\"bash\""));
    }

    #[test]
    fn hook_event_serializes_session_start() {
        let event = HookEvent::SessionStart;
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"sessionstart\""));
    }

    #[test]
    fn hook_result_deserializes_approved() {
        let json = r#"{"approved": false, "message": "blocked"}"#;
        let result: HookResult = serde_json::from_str(json).unwrap();
        assert!(!result.approved);
        assert_eq!(result.message.unwrap(), "blocked");
    }

    #[tokio::test]
    async fn hook_echo_returns_approved() {
        // Use echo as a simple hook that returns valid JSON
        let echo_cmd = r#"powershell -Command "Write-Output '{\"approved\": true}'""#;
        let result = HookRunner::run_one(echo_cmd, "{}", Duration::from_secs(5)).await;
        assert!(result.is_ok());
        assert!(result.unwrap().approved);
    }

    #[tokio::test]
    async fn hook_exit_nonzero_does_not_block() {
        let cmd = "exit 1";
        let result = HookRunner::run_one(cmd, "{}", Duration::from_secs(5)).await;
        // Non-zero exit returns Ok with approved=true (don't block on failure)
        assert!(result.is_ok());
        assert!(result.unwrap().approved);
    }
}
