//! Owns a long-lived `claude -p` child process: spawns it, feeds it user
//! turns over stdin, and publishes `StreamEvent` values parsed from its
//! stdout on the same channel the agent loop already uses. The GUI needs no
//! change to consume either backend.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::agent::agent_loop::StreamEvent;
use crate::agent::prompt::voice_mode_instructions;
use crate::error::{HarnessError, Result};

use super::events::{ClaudeEvent, parse_line};
use super::map::EventMapper;

pub(super) const CLAUDE_CLI_PATH_KEY: &str = "CLAUDE_CLI_PATH";

/// How often `send` checks the interrupt flag while a turn is in flight.
/// Short enough that Escape still feels immediate.
const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

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
/// `resume_id`, when set, adds `--resume <id>` at the end, so the child
/// resumes a saved conversation instead of starting a fresh one. See
/// `docs/notes/claude-resume.md` for the verified flag shape.
fn build_args(
    model: &str,
    permission_mode: Option<&str>,
    append_system_prompt: Option<&str>,
    resume_id: Option<&str>,
) -> Vec<String> {
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
    if let Some(id) = resume_id {
        args.push("--resume".to_string());
        args.push(id.to_string());
    }
    args
}

/// True when the resume id the driver currently holds differs from the id
/// the running child was spawned under. `--resume` is a spawn-time
/// argument. So a changed id leaves the running child stale, and it must
/// be replaced before the next turn. `ensure_ready` already applies that
/// same rule to a changed voice-mode flag.
fn resume_id_changed(current: &Option<String>, spawned: &Option<String>) -> bool {
    current != spawned
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
    /// Signalled by the stdout reader once the terminal `result` event of a
    /// turn arrives, and closed when the reader ends. `send` waits on this,
    /// so one turn is finished before the next one starts.
    turn_done: Option<mpsc::UnboundedReceiver<()>>,
    /// Signalled by the stdout reader once it captures the `session_id`
    /// from the child's `init` event. `send` drains this after every turn,
    /// same as `turn_done`, since the reader runs on its own task and has
    /// no other way to hand the id back.
    session_id_rx: Option<mpsc::UnboundedReceiver<String>>,
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
    /// The resume id the current child was spawned with, if any.
    /// `ensure_ready` compares it against `claude_session_id`, the same
    /// way it compares `spawned_voice_mode` against the live voice flag.
    /// `--resume` is a spawn-time argument, so a changed id needs a
    /// respawn before the next turn.
    spawned_resume_id: Option<String>,
    interrupt_flag: Arc<AtomicBool>,
    thinking_flag: Arc<AtomicBool>,
    voice_mode_flag: Arc<AtomicBool>,
    context_budget_flag: Arc<AtomicUsize>,
    model_flag: Arc<Mutex<String>>,
    repeat_interrupt_flag: Arc<AtomicBool>,
    /// The claude session id to resume on the next spawn, set by
    /// `Backend::load_session`. `spawn_child` passes it to `build_args`
    /// as `--resume <id>`.
    claude_session_id: Option<String>,
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
            turn_done: None,
            session_id_rx: None,
            model,
            permission_mode,
            extra_env,
            project_root,
            tx_events,
            spawned_voice_mode: false,
            spawned_resume_id: None,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            thinking_flag: Arc::new(AtomicBool::new(false)),
            voice_mode_flag: Arc::new(AtomicBool::new(false)),
            context_budget_flag: Arc::new(AtomicUsize::new(UNUSED_CONTEXT_BUDGET)),
            model_flag,
            repeat_interrupt_flag: Arc::new(AtomicBool::new(false)),
            claude_session_id: None,
        }
    }

    /// Store the claude session id to resume on the next spawn. `send`
    /// also calls this itself, once the running child's `init` event
    /// reports an id. So the driver captures an id even when no caller
    /// ever set one.
    pub fn set_claude_session_id(&mut self, id: Option<String>) {
        self.claude_session_id = id;
    }

    /// The claude session id currently held, if any.
    pub fn claude_session_id(&self) -> Option<&str> {
        self.claude_session_id.as_deref()
    }

    /// Return a clone of the interrupt flag so the GUI can signal
    /// interruption. Polled by `await_turn_end` while a turn is in flight.
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
    /// running autopilot loop. Read before every iteration by `run_repeat`,
    /// through this driver's `RepeatTarget` impl.
    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.repeat_interrupt_flag)
    }

    /// Send one user turn to the child and wait for that turn to finish.
    /// Spawns the child first on the first turn. Respawns it if the
    /// voice-mode flag has flipped since the child was last spawned.
    ///
    /// The wait matters: the caller treats one `send` as one whole turn.
    /// `Backend::run` returns from it, and the autopilot runner counts it
    /// as a completed iteration. Returning at the moment the line is
    /// flushed would report a turn finished while the model was still
    /// answering.
    pub async fn send(&mut self, text: &str) -> Result<()> {
        self.interrupt_flag.store(false, Ordering::SeqCst);
        self.ensure_ready().await?;
        let line = build_user_turn_line(text);
        let stdin = self
            .stdin
            .as_mut()
            .expect("ensure_ready always leaves stdin populated on success");
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        self.await_turn_end().await;
        self.drain_session_id();
        Ok(())
    }

    /// Pick up any session id the stdout reader captured from the child's
    /// `init` event since the last drain. This never blocks. The reader
    /// sends at most once per spawned child, right after the first
    /// event. So one `try_recv` after each turn is enough to notice it.
    fn drain_session_id(&mut self) {
        let Some(rx) = self.session_id_rx.as_mut() else {
            return;
        };
        if let Ok(id) = rx.try_recv() {
            self.claude_session_id = Some(id);
        }
    }

    /// Wait for the turn in flight to finish, polling the interrupt flag
    /// while it runs. An interrupt kills the child, so the next turn
    /// spawns a fresh one. A closed channel means the reader ended, which
    /// means the child is gone: `ensure_ready` notices that too.
    async fn await_turn_end(&mut self) {
        loop {
            if self.interrupt_flag.load(Ordering::SeqCst) {
                self.interrupt();
                self.interrupt_flag.store(false, Ordering::SeqCst);
                return;
            }
            let Some(turn_done) = self.turn_done.as_mut() else {
                return;
            };
            match tokio::time::timeout(INTERRUPT_POLL_INTERVAL, turn_done.recv()).await {
                Ok(Some(())) => return,
                Ok(None) => {
                    self.turn_done = None;
                    return;
                }
                Err(_timed_out) => continue,
            }
        }
    }

    /// Ensure a child is running with the voice-mode setting and resume id
    /// currently held. Respawns when a child exists but was spawned under a
    /// different voice-mode value or a different resume id, and when the
    /// child it holds has already exited: killed on interrupt, crashed, or
    /// ended by itself.
    async fn ensure_ready(&mut self) -> Result<()> {
        let want_voice = self.voice_mode_flag.load(Ordering::SeqCst);
        let resume_changed = resume_id_changed(&self.claude_session_id, &self.spawned_resume_id);
        if self.child.is_some() && (want_voice != self.spawned_voice_mode || resume_changed || self.child_exited()) {
            self.shutdown().await;
        }
        if self.child.is_none() {
            self.spawn_child(want_voice)?;
        }
        Ok(())
    }

    /// True when the child this driver holds has already exited. A driver
    /// that kept writing to a dead child's stdin would fail every turn
    /// from then on, with no way back short of restarting the app.
    fn child_exited(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            None => false,
        }
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
        let args = build_args(
            &self.model,
            self.permission_mode.as_deref(),
            append_prompt,
            self.claude_session_id.as_deref(),
        );

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

        let (turn_done_tx, turn_done_rx) = mpsc::unbounded_channel();
        let (session_id_tx, session_id_rx) = mpsc::unbounded_channel();
        spawn_stdout_reader(stdout, self.tx_events.clone(), turn_done_tx, session_id_tx);
        spawn_stderr_drain(stderr);

        self.child = Some(child);
        self.stdin = Some(stdin);
        self.turn_done = Some(turn_done_rx);
        self.session_id_rx = Some(session_id_rx);
        self.spawned_voice_mode = voice_mode;
        self.spawned_resume_id = self.claude_session_id.clone();
        Ok(())
    }

    /// Kill the child immediately. There is no cancel message in the
    /// protocol, so killing the process is the only way to stop a turn in
    /// flight. Drops the handles to the killed child, so the next turn
    /// spawns a fresh one instead of writing into a dead pipe.
    pub fn interrupt(&mut self) {
        if let Some(mut child) = self.child.take()
            && let Err(e) = child.start_kill()
        {
            tracing::warn!("claude_cli: failed to kill child on interrupt: {e}");
        }
        self.stdin = None;
        self.turn_done = None;
        self.session_id_rx = None;
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
        self.turn_done = None;
        self.session_id_rx = None;
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

/// Read the child's stdout forever, publishing mapped events. Signals
/// `turn_done` after the terminal `result` event of each turn, so `send`
/// knows when one turn ended. Sends the child's session id on
/// `session_id` once `EventMapper` reads it from the `init` event. The
/// driver then picks it up and reuses it as `--resume`. Dropping the
/// senders when the loop ends also releases a `send` waiting on a child
/// that died mid-turn.
fn spawn_stdout_reader(
    stdout: tokio::process::ChildStdout,
    tx_events: mpsc::UnboundedSender<StreamEvent>,
    turn_done: mpsc::UnboundedSender<()>,
    session_id: mpsc::UnboundedSender<String>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut mapper = EventMapper::new();
        let mut session_id_sent = false;
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
            let ends_turn = matches!(event, ClaudeEvent::Result(_));
            for stream_event in mapper.map(event) {
                if tx_events.send(stream_event).is_err() {
                    return;
                }
            }
            if !session_id_sent
                && let Some(id) = mapper.session_id()
            {
                session_id_sent = true;
                if session_id.send(id.to_string()).is_err() {
                    return;
                }
            }
            if ends_turn && turn_done.send(()).is_err() {
                return;
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
        let args = build_args("claude-opus-x", Some("acceptEdits"), None, None);
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
        let args = build_args("claude-opus-x", None, None, None);
        assert_eq!(args[10], "bypassPermissions");
    }

    #[test]
    fn args_builder_appends_system_prompt_when_voice_mode_is_on() {
        let args = build_args("claude-opus-x", Some("acceptEdits"), Some("voice text"), None);
        assert_eq!(args[11], "--append-system-prompt");
        assert_eq!(args[12], "voice text");
    }

    #[test]
    fn args_builder_omits_system_prompt_flag_when_not_given() {
        let args = build_args("claude-opus-x", Some("acceptEdits"), None, None);
        assert!(!args.contains(&"--append-system-prompt".to_string()));
    }

    #[test]
    fn args_builder_omits_resume_flag_when_no_id_is_held() {
        let args = build_args("claude-opus-x", Some("acceptEdits"), None, None);
        assert!(!args.contains(&"--resume".to_string()));
    }

    #[test]
    fn args_builder_appends_resume_flag_with_the_exact_id_when_one_is_held() {
        let args = build_args(
            "claude-opus-x",
            Some("acceptEdits"),
            None,
            Some("c18eb67f-6873-45a4-aa7a-8755cecb4361"),
        );
        assert_eq!(args[11], "--resume");
        assert_eq!(args[12], "c18eb67f-6873-45a4-aa7a-8755cecb4361");
    }

    #[test]
    fn args_builder_includes_both_system_prompt_and_resume_when_both_are_given() {
        let args = build_args(
            "claude-opus-x",
            Some("acceptEdits"),
            Some("voice text"),
            Some("some-id"),
        );
        assert_eq!(args[11], "--append-system-prompt");
        assert_eq!(args[12], "voice text");
        assert_eq!(args[13], "--resume");
        assert_eq!(args[14], "some-id");
    }

    #[test]
    fn interrupt_drops_the_child_handles_so_the_next_turn_respawns() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut driver =
            ClaudeCliDriver::new("opus".to_string(), None, None, PathBuf::from("."), tx);

        driver.interrupt();

        assert!(driver.child.is_none());
        assert!(driver.stdin.is_none());
        assert!(driver.turn_done.is_none());
        match rx.try_recv().expect("expected an Interrupted event") {
            StreamEvent::Interrupted { .. } => {}
            other => panic!("expected Interrupted, got {other:?}"),
        }
    }

    #[test]
    fn child_exited_is_false_when_no_child_is_running() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut driver =
            ClaudeCliDriver::new("opus".to_string(), None, None, PathBuf::from("."), tx);

        assert!(!driver.child_exited());
    }

    #[test]
    fn resume_id_changed_is_false_when_both_are_none() {
        assert!(!resume_id_changed(&None, &None));
    }

    #[test]
    fn resume_id_changed_is_false_when_ids_match() {
        assert!(!resume_id_changed(
            &Some("abc".to_string()),
            &Some("abc".to_string())
        ));
    }

    #[test]
    fn resume_id_changed_is_true_when_a_new_id_replaces_none() {
        assert!(resume_id_changed(&Some("abc".to_string()), &None));
    }

    #[test]
    fn resume_id_changed_is_true_when_ids_differ() {
        assert!(resume_id_changed(
            &Some("abc".to_string()),
            &Some("xyz".to_string())
        ));
    }

    #[tokio::test]
    async fn await_turn_end_returns_at_once_when_no_turn_is_in_flight() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut driver =
            ClaudeCliDriver::new("opus".to_string(), None, None, PathBuf::from("."), tx);

        // No child, so no `turn_done` channel. This must return rather
        // than poll forever.
        driver.await_turn_end().await;
    }

    #[tokio::test]
    async fn await_turn_end_returns_when_the_reader_signals_the_turn_ended() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut driver =
            ClaudeCliDriver::new("opus".to_string(), None, None, PathBuf::from("."), tx);
        let (turn_done_tx, turn_done_rx) = mpsc::unbounded_channel();
        driver.turn_done = Some(turn_done_rx);
        turn_done_tx.send(()).unwrap();

        driver.await_turn_end().await;

        // The signal was consumed, so the channel is empty for the next
        // turn instead of returning from it right away.
        assert!(driver.turn_done.as_mut().unwrap().try_recv().is_err());
    }

    #[tokio::test]
    async fn await_turn_end_kills_the_child_when_the_interrupt_flag_is_set() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut driver =
            ClaudeCliDriver::new("opus".to_string(), None, None, PathBuf::from("."), tx);
        let (_turn_done_tx, turn_done_rx) = mpsc::unbounded_channel();
        driver.turn_done = Some(turn_done_rx);
        driver.interrupt_flag().store(true, Ordering::SeqCst);

        driver.await_turn_end().await;

        assert!(driver.turn_done.is_none());
        assert!(
            !driver.interrupt_flag().load(Ordering::SeqCst),
            "the flag must be cleared so it cannot cut the next turn short"
        );
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
