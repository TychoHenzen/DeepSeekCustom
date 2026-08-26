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
                message: None,
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Ok(HookResult {
                approved: true,
                message: None,
            });
        }

        match serde_json::from_str::<HookResult>(stdout.trim()) {
            Ok(result) => Ok(result),
            Err(e) => {
                warn!("hook returned invalid JSON: {e}");
                Ok(HookResult {
                    approved: true,
                    message: Some(format!("invalid JSON from hook: {stdout}")),
                })
            }
        }
    }
}
