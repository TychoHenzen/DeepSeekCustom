//! One MCP server, spawned as a child process and driven over stdio.
//!
//! The child's stdout is read by a background task for the whole life of
//! the client, not polled per request. A JSON-RPC response carries the `id`
//! of the request it answers, and a server is free to interleave answers or
//! push notifications between them, so matching a reply to a caller means
//! keeping a table of what is outstanding rather than reading the next line
//! and hoping.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};
use tracing::{debug, warn};

use super::config::ServerConfig;
use super::protocol::{
    McpToolDef, Request, Response, ToolsCallResult, ToolsListResult, flatten_content,
    initialize_params,
};
use super::spawn::resolve_command;
use crate::error::{HarnessError, Result};
use crate::process_group;

/// How long any one request waits for its answer. Generous on purpose. A
/// server that shells out to `npx` may fetch a package first. It answers
/// nothing at all until that finishes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// A live connection to one MCP server.
pub struct McpClient {
    name: String,
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Pending,
    next_id: AtomicU64,
    /// Set by `shutdown`, read by the stdout reader. A server's stdout
    /// closing on its own is worth a `warn`. The same event right after
    /// this process killed it is worth nothing. Every clean exit kills
    /// every server.
    closing: Arc<AtomicBool>,
}

/// Requests sent and not yet answered, keyed by JSON-RPC id.
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

/// What one `tools/call` produced: text, plus whether the server itself
/// called it a failure.
#[derive(Debug)]
pub struct CallOutcome {
    pub text: String,
    pub is_error: bool,
}

impl McpClient {
    /// Spawn the server, then run `initialize` and the notification that
    /// follows it. Both are required before any other request.
    ///
    /// The child joins this process's job object through
    /// `process_group::adopt`, so closing the window kills it even if this
    /// process never runs a destructor. `kill_on_drop` is set too, as the
    /// same second line of defence the `claude -p` path uses.
    pub async fn connect(config: &ServerConfig) -> Result<McpClient> {
        // Not `Command::new(&config.command)` directly: a bare `npx` on
        // Windows is a batch file that only `cmd.exe` can start. See
        // `super::spawn`.
        let resolved = resolve_command(&config.command);
        let mut command = Command::new(&resolved.program);
        command
            .args(&resolved.prefix_args)
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherited rather than piped: a server that logs to stderr
            // would otherwise fill an unread pipe and block forever.
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // CREATE_NO_WINDOW. Without it every stdio server started from a
        // GUI process flashes its own console window.
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);

        let mut child = command.spawn().map_err(|e| {
            HarnessError::Config(format!(
                "mcp: failed to spawn server {}: {e} (command: {})",
                config.name, config.command
            ))
        })?;
        process_group::adopt(&child);

        let stdin = child.stdin.take().ok_or_else(|| {
            HarnessError::Config(format!("mcp: server {} gave no stdin", config.name))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            HarnessError::Config(format!("mcp: server {} gave no stdout", config.name))
        })?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closing = Arc::new(AtomicBool::new(false));
        spawn_reader(
            config.name.clone(),
            stdout,
            Arc::clone(&pending),
            Arc::clone(&closing),
        );

        let client = McpClient {
            name: config.name.clone(),
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            pending,
            next_id: AtomicU64::new(1),
            closing,
        };

        client.request("initialize", initialize_params()).await?;
        client.notify("notifications/initialized").await?;
        Ok(client)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Ask the server what tools it has.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>> {
        let value = self.request("tools/list", Value::Object(Default::default())).await?;
        let listed: ToolsListResult = serde_json::from_value(value)
            .map_err(|e| HarnessError::Parse(format!("mcp: {}: bad tools/list: {e}", self.name)))?;
        Ok(listed.tools)
    }

    /// Run one of the server's tools.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<CallOutcome> {
        let params = serde_json::json!({ "name": tool, "arguments": arguments });
        let value = self.request("tools/call", params).await?;
        let result: ToolsCallResult = serde_json::from_value(value)
            .map_err(|e| HarnessError::Parse(format!("mcp: {}: bad tools/call: {e}", self.name)))?;
        Ok(CallOutcome {
            text: flatten_content(&result.content),
            is_error: result.is_error,
        })
    }

    /// Kill the child. Called when the harness shuts a server down. The job
    /// object covers the case where this never runs.
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let mut child = self.child.lock().await;
        if let Err(e) = child.kill().await {
            debug!("mcp: {}: kill failed: {e}", self.name);
        }
    }

    /// Send a request and wait for the matching response.
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        if let Err(e) = self.write_line(&Request::call(id, method, params)).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => Err(HarnessError::Tool(format!(
                "mcp: {}: {method} failed: {message}",
                self.name
            ))),
            // The reader task dropped the sender, which only happens when
            // the child's stdout closed: the server died.
            Ok(Err(_)) => Err(HarnessError::Tool(format!(
                "mcp: {}: server closed while waiting for {method}",
                self.name
            ))),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(HarnessError::Tool(format!(
                    "mcp: {}: {method} timed out after {}s",
                    self.name,
                    REQUEST_TIMEOUT.as_secs()
                )))
            }
        }
    }

    /// Send a notification, which by definition gets no answer.
    async fn notify(&self, method: &str) -> Result<()> {
        self.write_line(&Request::notify(method)).await
    }

    async fn write_line(&self, request: &Request) -> Result<()> {
        let mut line = serde_json::to_string(request)
            .map_err(|e| HarnessError::Parse(format!("mcp: {}: {e}", self.name)))?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}

/// Read the child's stdout for the life of the connection, completing each
/// pending request as its answer arrives.
///
/// A line that is not JSON, or that carries no `id`, is dropped. A server
/// may write a banner or push a notification. Neither is an error.
///
/// When stdout closes, every still-pending sender is dropped. That turns a
/// dead server into an error at each waiting caller, rather than a hang.
fn spawn_reader(
    name: String,
    stdout: tokio::process::ChildStdout,
    pending: Pending,
    closing: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(response) = serde_json::from_str::<Response>(&line) else {
                debug!("mcp: {name}: ignoring non-JSON-RPC line");
                continue;
            };
            let Some(id) = response.id else {
                continue;
            };
            let Some(sender) = pending.lock().await.remove(&id) else {
                debug!("mcp: {name}: response {id} matched no pending request");
                continue;
            };
            let payload = match response.error {
                Some(e) => Err(format!("{} (code {})", e.message, e.code)),
                None => Ok(response.result.unwrap_or(Value::Null)),
            };
            let _ = sender.send(payload);
        }
        if closing.load(Ordering::SeqCst) {
            debug!("mcp: {name}: server stopped");
        } else {
            warn!("mcp: {name}: server stdout closed");
        }
        pending.lock().await.clear();
    });
}
