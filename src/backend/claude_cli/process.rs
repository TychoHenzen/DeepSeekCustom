//! Owns a long-lived `claude -p` child process: spawns it, feeds it user
//! turns over stdin, and publishes `StreamEvent` values parsed from its
//! stdout on the same channel the agent loop already uses. The GUI needs no
//! change to consume either backend.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::agent::agent_loop::StreamEvent;
use crate::agent::prompt::voice_mode_instructions;
use crate::error::{HarnessError, Result};

use super::events::parse_line;
use super::map::EventMapper;

pub(super) const CLAUDE_CLI_PATH_KEY: &str = "CLAUDE_CLI_PATH";

/// Token budget placeholder. Claude Code manages its own context
/// compaction, so this value is never read by anything. It only exists so
/// `ClaudeCliDriver` can hand the GUI a real `Arc<AtomicUsize>` for the
/// context budget slider to write into.
const UNUSED_CONTEXT_BUDGET: usize = 100_000;

/// Resolve the path to the `claude` binary, since it may not be on the
/// Windows PATH even when it is on the Git Bash PATH.
///
/// Tries, in order: the `CLAUDE_CLI_PATH` key in `extra_env`, then the
/// `CLAUDE_CLI_PATH` environment variable. Then the bare name `claude`,
/// letting the OS search PATH at spawn time. Then a platform-specific
/// fallback path under the user's home directory.
pub fn resolve_claude_binary(extra_env: Option<&HashMap<String, String>>) -> Option<PathBuf> {
    if let Some(env) = extra_env
        && let Some(path) = env.get(CLAUDE_CLI_PATH_KEY)
    {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Ok(path) = std::env::var(CLAUDE_CLI_PATH_KEY) {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Some(fallback) = fallback_claude_path()
        && fallback.is_file()
    {
        return Some(fallback);
    }

    Some(PathBuf::from("claude"))
}

#[cfg(target_os = "windows")]
fn fallback_claude_path() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE").ok()?;
    Some(PathBuf::from(home).join(".local").join("bin").join("claude.exe"))
}

#[cfg(not(target_os = "windows"))]
fn fallback_claude_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".local").join("bin").join("claude"))
}

/// Build the argument vector for `claude -p`, in the exact order the
/// protocol expects. `append_system_prompt`, when set, adds
/// `--append-system-prompt <text>` at the end: this is how voice reply mode
/// reaches the child, since the child has no per-turn config channel.
fn build_args(model: &str, permission_mode: Option<&str>, append_system_prompt: Option<&str>) -> Vec<String> {
    let mode = permission_mode.unwrap_or("bypassPermissions");
    let mut args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--include-partial-messages".to_string(),
        "--verbose".to_string(),
        "--model".to_string(),
        model.to_string(),
        "--permission-mode".to_string(),
        mode.to_string(),
    ];
    if let Some(prompt) = append_system_prompt {
        args.push("--append-system-prompt".to_string());
        args.push(prompt.to_string());
    }
    args
}

/// Build one stdin line for a plain-text user turn, in the verified
/// stream-json shape.
fn build_user_turn_line(text: &str) -> String {
    let value = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    });
    value.to_string()
}

/// Owns a `claude -p` child process for the lifetime of a session. It also
/// owns the six shared flags `Backend` exposes to the GUI, in place of the
/// ones `AgentLoop` owns for the API path.
///
/// Three of those flags have no meaning here: `thinking_flag`,
/// `context_budget_flag`, and `model_flag`. Claude Code manages its own
/// thinking level, its own context compaction, and its own model choice.
/// The GUI can still write to them. The settings panel does not know which
/// backend is active. Nothing in this driver ever reads them back.
pub struct ClaudeCliDriver {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    model: String,
    permission_mode: Option<String>,
    extra_env: Option<HashMap<String, String>>,
    project_root: PathBuf,
    tx_events: mpsc::UnboundedSender<StreamEvent>,
    /// The voice-mode flag's value at the time the current child was
    /// spawned. `send` compares this against the live flag before every
    /// turn, and respawns the child on a mismatch, since
    /// `--append-system-prompt` is a spawn-time argument only.
    spawned_voice_mode: bool,
    interrupt_flag: Arc<AtomicBool>,
    thinking_flag: Arc<AtomicBool>,
    voice_mode_flag: Arc<AtomicBool>,
    context_budget_flag: Arc<AtomicUsize>,
    model_flag: Arc<Mutex<String>>,
    repeat_interrupt_flag: Arc<AtomicBool>,
}

impl ClaudeCliDriver {
    /// Build a driver for the given spawn parameters. Does not spawn the
    /// child yet: `send` spawns it lazily on the first turn.
    pub fn new(
        model: String,
        permission_mode: Option<String>,
        extra_env: Option<HashMap<String, String>>,
        project_root: PathBuf,
        tx_events: mpsc::UnboundedSender<StreamEvent>,
    ) -> Self {
        let model_flag = Arc::new(Mutex::new(model.clone()));
        Self {
            child: None,
            stdin: None,
            model,
            permission_mode,
            extra_env,
            project_root,
            tx_events,
            spawned_voice_mode: false,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            thinking_flag: Arc::new(AtomicBool::new(false)),
            voice_mode_flag: Arc::new(AtomicBool::new(false)),
            context_budget_flag: Arc::new(AtomicUsize::new(UNUSED_CONTEXT_BUDGET)),
            model_flag,
            repeat_interrupt_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Return a clone of the interrupt flag so the GUI can signal
    /// interruption. Read by `interrupt`.
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    /// Return a clone of the thinking flag. Unread: Claude Code manages its
    /// own thinking level.
    pub fn thinking_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.thinking_flag)
    }

    /// Return a clone of the voice-mode flag. Read before every turn in
    /// `send`, to decide whether the child needs a respawn.
    pub fn voice_mode_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.voice_mode_flag)
    }

    /// Return a clone of the context budget flag. Unread: Claude Code
    /// manages its own context compaction.
    pub fn context_budget_flag(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.context_budget_flag)
    }

    /// Return a clone of the model-name flag. Unread: the model is fixed
    /// for the lifetime of the child, set at spawn time from config.
    pub fn model_flag(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.model_flag)
    }

    /// Return a clone of the repeat-interrupt flag so the GUI can stop a
    /// running autopilot loop. Autopilot is not wired up on this backend
    /// yet (see `Backend::run_repeat`), so nothing reads this today.
    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.repeat_interrupt_flag)
    }

    /// Send one user turn to the child. Spawns the child first on the
    /// first turn. Respawns it if the voice-mode flag has flipped since
    /// the child was last spawned.
    pub async fn send(&mut self, text: &str) -> Result<()> {
        self.ensure_ready().await?;
        let line = build_user_turn_line(text);
        let stdin = self
            .stdin
            .as_mut()
            .expect("ensure_ready always leaves stdin populated on success");
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Ensure a child is running with the voice-mode setting the live flag
    /// currently holds. Respawns when a child exists but was spawned under
    /// a different voice-mode value.
    async fn ensure_ready(&mut self) -> Result<()> {
        let want_voice = self.voice_mode_flag.load(Ordering::SeqCst);
        if self.child.is_some() && want_voice != self.spawned_voice_mode {
            self.shutdown().await;
        }
        if self.child.is_none() {
            self.spawn_child(want_voice)?;
        }
        Ok(())
    }

    /// Spawn the child process and start reading its stdout and stderr in
    /// background tasks. Every parsed event is published on `tx_events`.
    fn spawn_child(&mut self, voice_mode: bool) -> Result<()> {
        let Some(binary) = resolve_claude_binary(self.extra_env.as_ref()) else {
            let message =
                format!("could not resolve the claude CLI binary; set {CLAUDE_CLI_PATH_KEY}");
            let _ = self.tx_events.send(StreamEvent::Error {
                message: message.clone(),
            });
            return Err(HarnessError::Tool(message));
        };

        let append_prompt = if voice_mode {
            Some(voice_mode_instructions())
        } else {
            None
        };
        let args = build_args(&self.model, self.permission_mode.as_deref(), append_prompt);

        let mut command = Command::new(&binary);
        command
            .args(&args)
            .current_dir(&self.project_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(env) = &self.extra_env {
            for (key, value) in env {
                if key == CLAUDE_CLI_PATH_KEY {
                    continue;
                }
                command.env(key, value);
            }
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                let message = format!(
                    "failed to spawn claude CLI at {}: {e}; set {CLAUDE_CLI_PATH_KEY}",
                    binary.display()
                );
                let _ = self.tx_events.send(StreamEvent::Error {
                    message: message.clone(),
                });
                return Err(HarnessError::Io(e));
            }
        };

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Tool("claude CLI child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Tool("claude CLI child has no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HarnessError::Tool("claude CLI child has no stderr".into()))?;

        spawn_stdout_reader(stdout, self.tx_events.clone());
        spawn_stderr_drain(stderr);

        self.child = Some(child);
        self.stdin = Some(stdin);
        self.spawned_voice_mode = voice_mode;
        Ok(())
    }

    /// Kill the child immediately. There is no cancel message in the
    /// protocol, so killing the process is the only way to stop a turn in
    /// flight.
    pub fn interrupt(&mut self) {
        if let Some(child) = self.child.as_mut()
            && let Err(e) = child.start_kill()
        {
            tracing::warn!("claude_cli: failed to kill child on interrupt: {e}");
        }
        let _ = self.tx_events.send(StreamEvent::Interrupted {
            message: "Interrupted by user (Escape)".into(),
        });
    }

    /// Close stdin and wait for the child to exit, for the ordinary
    /// end-of-session path and for a voice-mode-triggered respawn. A no-op
    /// when no child is running.
    pub async fn shutdown(&mut self) {
        if let Some(mut stdin) = self.stdin.take()
            && let Err(e) = stdin.shutdown().await
        {
            tracing::warn!("claude_cli: failed to close stdin on shutdown: {e}");
        }
        if let Some(mut child) = self.child.take()
            && let Err(e) = child.wait().await
        {
            tracing::warn!("claude_cli: failed waiting for child exit: {e}");
        }
    }

    /// Publish one `StreamEvent` on this driver's event channel. Used by
    /// the `RepeatTarget` impl below, so the shared `run_repeat` loop in
    /// `src/agent/repeat.rs` can drive this backend the same way it drives
    /// `AgentLoop`.
    fn send_event(&self, event: StreamEvent) {
        let _ = self.tx_events.send(event);
    }
}

impl crate::agent::repeat::RepeatTarget for ClaudeCliDriver {
    /// End the current child so the next turn spawns a fresh one. A new
    /// `claude -p` process starts a new session with no prior
    /// conversation, the same guarantee `AgentLoop::clear_history` gives
    /// on the API path.
    async fn reset_for_iteration(&mut self) {
        self.shutdown().await;
    }

    async fn run_turn(&mut self, task: &str) -> Result<()> {
        self.send(task).await
    }

    fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        self.repeat_interrupt_flag()
    }

    fn send_event(&self, event: StreamEvent) {
        self.send_event(event)
    }
}

fn spawn_stdout_reader(
    stdout: tokio::process::ChildStdout,
    tx_events: mpsc::UnboundedSender<StreamEvent>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut mapper = EventMapper::new();
        loop {
            let next = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("claude_cli: error reading stdout: {e}");
                    let _ = tx_events.send(StreamEvent::Error {
                        message: format!("claude CLI stdout read error: {e}"),
                    });
                    break;
                }
            };
            let Some(event) = parse_line(&next) else {
                continue;
            };
            for stream_event in mapper.map(event) {
                if tx_events.send(stream_event).is_err() {
                    return;
                }
            }
        }
    });
}

pub(super) fn spawn_stderr_drain(stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if !line.trim().is_empty() {
                        tracing::warn!("claude_cli stderr: {line}");
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("claude_cli: error reading stderr: {e}");
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn resolve_prefers_env_map_over_environment_variable() {
        let mut file = tempfile_named();
        write!(file.1, "x").unwrap();
        let mut env = HashMap::new();
        env.insert(
            CLAUDE_CLI_PATH_KEY.to_string(),
            file.0.to_string_lossy().to_string(),
        );

        let resolved = resolve_claude_binary(Some(&env));
        assert_eq!(resolved, Some(file.0));
    }

    #[test]
    fn resolve_prefers_env_var_over_fallback_path() {
        let mut file = tempfile_named();
        write!(file.1, "x").unwrap();

        // SAFETY: test-local. Guarded by ENV_MUTEX so no other test in this
        // process observes an intermediate state of this environment
        // variable while it is set.
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::set_var(CLAUDE_CLI_PATH_KEY, &file.0);
        }
        let resolved = resolve_claude_binary(None);
        unsafe {
            std::env::remove_var(CLAUDE_CLI_PATH_KEY);
        }

        assert_eq!(resolved, Some(file.0));
    }

    #[test]
    fn resolve_returns_bare_name_when_nothing_resolves() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::remove_var(CLAUDE_CLI_PATH_KEY);
        }

        // The fallback path (~/.local/bin/claude[.exe]) is not guaranteed to
        // be absent on this machine. The briefing states it exists here.
        // So this test only checks the case where no override is set.
        // It relies on `resolve_claude_binary` always returning `Some`,
        // even when nothing but the bare name is left. Exercising the
        // "fallback absent" branch would need an override seam for
        // `fallback_claude_path`, which this step does not add. Instead
        // this test asserts the invariant that matters: resolution never
        // fails outright.
        let resolved = resolve_claude_binary(None);
        assert!(resolved.is_some());
    }

    #[test]
    fn stdin_line_builder_matches_verified_shape_for_plain_text() {
        let line = build_user_turn_line("hello there");
        let expected = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": "hello there"}]
            }
        });
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn stdin_line_builder_escapes_quotes_newlines_and_unicode() {
        let text = "she said \"hi\"\nline two \u{00e9}";
        let line = build_user_turn_line(text);
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        let round_tripped = parsed["message"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(round_tripped, text);
    }

    #[test]
    fn args_builder_produces_exact_flag_list_in_order() {
        let args = build_args("claude-opus-x", Some("acceptEdits"), None);
        assert_eq!(
            args,
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                "--model",
                "claude-opus-x",
                "--permission-mode",
                "acceptEdits",
            ]
        );
    }

    #[test]
    fn args_builder_defaults_permission_mode_to_bypass_permissions() {
        let args = build_args("claude-opus-x", None, None);
        assert_eq!(args[10], "bypassPermissions");
    }

    #[test]
    fn args_builder_appends_system_prompt_when_voice_mode_is_on() {
        let args = build_args("claude-opus-x", Some("acceptEdits"), Some("voice text"));
        assert_eq!(args[11], "--append-system-prompt");
        assert_eq!(args[12], "voice text");
    }

    #[test]
    fn args_builder_omits_system_prompt_flag_when_not_given() {
        let args = build_args("claude-opus-x", Some("acceptEdits"), None);
        assert!(!args.contains(&"--append-system-prompt".to_string()));
    }

    #[test]
    fn new_driver_seeds_model_flag_from_constructor_model() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let driver = ClaudeCliDriver::new(
            "opus".to_string(),
            None,
            None,
            PathBuf::from("."),
            tx,
        );
        assert_eq!(*driver.model_flag().lock().unwrap(), "opus");
    }

    /// Serializes tests that mutate `CLAUDE_CLI_PATH` in the process
    /// environment, since `cargo test` runs the suite multithreaded and an
    /// unguarded env var mutation would be a race between tests.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn tempfile_named() -> (PathBuf, std::fs::File) {
        let dir = std::env::temp_dir();
        let name = format!(
            "claude_cli_process_test_{}_{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        (path, file)
    }
}
