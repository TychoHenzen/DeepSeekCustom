//! Owns a long-lived `claude -p` child process: spawns it, feeds it user
//! turns over stdin, and publishes `StreamEvent` values parsed from its
//! stdout on the same channel the agent loop already uses. The GUI needs no
//! change to consume either backend.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::agent::agent_loop::{RoutedEvent, StreamEvent};
use crate::agent::prompt::voice_mode_instructions;
use crate::api::types::ImageAttachment;
use crate::effort::Effort;
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
pub fn fallback_claude_path() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE").ok()?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("bin")
            .join("claude.exe"),
    )
}

#[cfg(not(target_os = "windows"))]
pub fn fallback_claude_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("bin")
            .join("claude"),
    )
}

/// Build the argument vector for `claude -p`, in the exact order the
/// protocol expects. `effort`, when it maps to a CLI value, adds
/// `--effort <level>` right after the base flags: `Effort::None` omits it
/// entirely, since the CLI has no `none` value of its own. See
/// `docs/notes/claude-effort.md` for the verified value set.
/// `append_system_prompt`, when set, adds `--append-system-prompt <text>`
/// next: this is how voice reply mode reaches the child, since the child
/// has no per-turn config channel. `resume_id`, when set, adds `--resume
/// <id>` at the end, so the child resumes a saved conversation instead of
/// starting a fresh one. See `docs/notes/claude-resume.md` for the
/// verified flag shape.
pub fn build_args(
    model: &str,
    permission_mode: Option<&str>,
    append_system_prompt: Option<&str>,
    resume_id: Option<&str>,
    effort: Effort,
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
        // Without this, a non-interactive run asks the API for no thinking
        // text at all: `thinking_delta` events still arrive, but every one
        // carries an empty `thinking` field beside an encrypted signature.
        // The gate is on the request, not on the renderer, so no amount of
        // parsing on this side recovers the text. `summarized` and
        // `omitted` are the only two values the flag takes.
        "--thinking-display".to_string(),
        "summarized".to_string(),
    ];
    if let Some(level) = effort.claude_cli_effort() {
        args.push("--effort".to_string());
        args.push(level.to_string());
    }
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
pub fn resume_id_changed(current: &Option<String>, spawned: &Option<String>) -> bool {
    current != spawned
}

/// True when the live working directory differs from the one the running
/// child was spawned under. The child's cwd is a spawn-time argument, so a
/// changed directory leaves the running child stale, and it must be
/// replaced before the next turn. `ensure_ready` applies the same rule to a
/// changed voice-mode flag and a changed resume id. `spawned` is `None`
/// before the first child is ever spawned, which always counts as changed.
pub fn working_dir_changed(current: &std::path::Path, spawned: &Option<PathBuf>) -> bool {
    Some(current) != spawned.as_deref()
}

/// True when the live effort level differs from the one the running child
/// was spawned under. `--effort` is a spawn-time argument, same as
/// `--resume` and the working directory, so a changed level leaves the
/// running child stale and it must be replaced before the next turn.
pub fn effort_changed(current: Effort, spawned: Effort) -> bool {
    current != spawned
}

/// Build one stdin line for a user turn, in the verified stream-json shape.
/// With no image this is exactly the plain-text shape confirmed against the
/// real `claude` binary. With one, an Anthropic `image` content block
/// follows the text block: `{"type":"image","source":{"type":"base64",
/// "media_type":"...","data":"..."}}`. That shape, not the OpenAI
/// `image_url` shape the API backends speak, is what a real turn against
/// this stdin protocol actually accepts; see `docs/notes/image-support.md`.
pub fn build_user_turn_line(text: &str, image: Option<&ImageAttachment>) -> String {
    let mut content = vec![serde_json::json!({"type": "text", "text": text})];
    if let Some(image) = image {
        content.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.media_type,
                "data": image.data,
            }
        }));
    }
    let value = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content
        }
    });
    value.to_string()
}

/// Owns a `claude -p` child process for the lifetime of a session. It also
/// owns the six shared flags `Backend` exposes to the GUI, in place of the
/// ones `AgentLoop` owns for the API path.
///
/// Two of those flags have no meaning here: `context_budget_flag` and
/// `model_flag`. Claude Code manages its own context compaction, and the
/// model is fixed for the lifetime of the child. The GUI can still write to
/// them. The settings panel does not know which backend is active. Nothing
/// in this driver ever reads them back. `effort_flag` is different: `send`
/// reads it before every turn through `ensure_ready`, and respawns the
/// child on a change, the same way it already does for voice mode, the
/// resume id, and the working directory.
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
    /// Where the child is spawned. Shared with `BackendFactory` and every
    /// tool it builds, the same `Arc<Mutex<PathBuf>>` `BashTool`, `ReadTool`,
    /// and `WriteTool` read per call. Read fresh in `ensure_ready` before
    /// every turn, since the child's cwd is a spawn-time argument that a
    /// later write to this value cannot change on a running child.
    working_dir: Arc<Mutex<PathBuf>>,
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
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
    /// The working directory the current child was spawned with. `ensure_ready`
    /// compares it against the live value in `working_dir`, the same way it
    /// compares `spawned_voice_mode` and `spawned_resume_id`. `None` until
    /// the first child is spawned.
    spawned_working_dir: Option<PathBuf>,
    /// The effort level the current child was spawned with. `ensure_ready`
    /// compares it against the live value in `effort_flag`, the same way it
    /// compares `spawned_voice_mode`, `spawned_resume_id`, and
    /// `spawned_working_dir`. `--effort` is a spawn-time argument.
    spawned_effort: Effort,
    interrupt_flag: Arc<AtomicBool>,
    effort_flag: Arc<AtomicU8>,
    voice_mode_flag: Arc<AtomicBool>,
    context_budget_flag: Arc<AtomicUsize>,
    model_flag: Arc<Mutex<String>>,
    repeat_interrupt_flag: Arc<AtomicBool>,
    /// The claude session id to resume on the next spawn, set by
    /// `Backend::load_session`. `spawn_child` passes it to `build_args`
    /// as `--resume <id>`.
    claude_session_id: Option<String>,
    /// Accumulated reply text from the most recent turn, updated by the
    /// stdout reader and cleared at the start of each turn.
    last_reply: Arc<Mutex<String>>,
}

impl ClaudeCliDriver {
    /// Build a driver for the given spawn parameters. Does not spawn the
    /// child yet: `send` spawns it lazily on the first turn.
    pub fn new(
        model: String,
        permission_mode: Option<String>,
        extra_env: Option<HashMap<String, String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        tx_events: mpsc::UnboundedSender<RoutedEvent>,
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
            working_dir,
            tx_events,
            spawned_voice_mode: false,
            spawned_resume_id: None,
            spawned_working_dir: None,
            spawned_effort: Effort::None,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            effort_flag: Arc::new(AtomicU8::new(Effort::None.to_u8())),
            voice_mode_flag: Arc::new(AtomicBool::new(false)),
            context_budget_flag: Arc::new(AtomicUsize::new(UNUSED_CONTEXT_BUDGET)),
            model_flag,
            repeat_interrupt_flag: Arc::new(AtomicBool::new(false)),
            claude_session_id: None,
            last_reply: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Store the claude session id to resume on the next spawn. `send`
    /// also calls this itself, once the running child's `init` event
    /// reports an id. So the driver captures an id even when no caller
    /// ever set one.
    pub fn set_claude_session_id(&mut self, id: Option<String>) {
        self.claude_session_id = id;
    }

    /// Replace all six shared handles with the GUI's own, so this driver
    /// answers to the controls the user already has on screen. Called by
    /// `BackendFactory::build` on a depth-0 backend only.
    ///
    /// `effort` is adopted here, unlike on the `Api` path, because nothing
    /// else holds this driver's effort flag. `ensure_ready` reads it before
    /// every turn and respawns the child on a change, so a level the user
    /// picked before the switch takes effect on this driver's first spawn.
    pub fn adopt_flags(&mut self, flags: &crate::backend::SharedFlags) {
        self.interrupt_flag = Arc::clone(&flags.interrupt);
        self.effort_flag = Arc::clone(&flags.effort);
        self.voice_mode_flag = Arc::clone(&flags.voice_mode);
        self.context_budget_flag = Arc::clone(&flags.context_budget);
        self.model_flag = Arc::clone(&flags.model);
        self.repeat_interrupt_flag = Arc::clone(&flags.repeat_interrupt);
    }

    /// The claude session id currently held, if any.
    pub fn claude_session_id(&self) -> Option<&str> {
        self.claude_session_id.as_deref()
    }

    /// The OS process id of the currently running child, if any. Exists so
    /// a test can prove a respawn happened (or did not) by process identity
    /// directly, rather than inferring it indirectly from the session id,
    /// which a genuine `--resume` deliberately keeps stable across a
    /// respawn.
    pub fn child_pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|child| child.id())
    }

    /// Return a clone of the interrupt flag so the GUI can signal
    /// interruption. Polled by `await_turn_end` while a turn is in flight.
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    /// Return a clone of the effort flag. Read before every turn in `send`,
    /// through `ensure_ready`, to decide whether the child needs a respawn.
    pub fn effort_flag(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.effort_flag)
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
        self.send_with_image(text, None).await
    }

    /// Same as `send`, with an optional image attachment mapped onto the
    /// Anthropic content-block shape `build_user_turn_line` builds. `send`
    /// is this method called with no image, so a turn with no attachment is
    /// unaffected.
    pub async fn send_with_image(
        &mut self,
        text: &str,
        image: Option<&ImageAttachment>,
    ) -> Result<()> {
        self.interrupt_flag.store(false, Ordering::SeqCst);
        if let Ok(mut reply) = self.last_reply.lock() {
            reply.clear();
        }
        self.ensure_ready().await?;
        let line = build_user_turn_line(text, image);
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
    ///
    /// The running child IS the session it just reported, so this also
    /// updates `spawned_resume_id` to match. Without that, the next
    /// `ensure_ready` would compare a freshly-learned id against the `None`
    /// (or older id) the child was actually spawned under, read that as a
    /// change, and kill a perfectly healthy child to "resume" it into
    /// itself. Setting only `claude_session_id` here, and leaving
    /// `spawned_resume_id` for `spawn_child` alone to write, was the bug: it
    /// let bookkeeping about what to pass on the next spawn look identical
    /// to a signal that the current child is stale.
    fn drain_session_id(&mut self) {
        let Some(rx) = self.session_id_rx.as_mut() else {
            return;
        };
        if let Ok(id) = rx.try_recv() {
            self.claude_session_id = Some(id.clone());
            self.spawned_resume_id = Some(id);
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

    /// Ensure a child is running with the voice-mode setting, resume id,
    /// working directory, and effort level currently held. Respawns when a
    /// child exists but was spawned under a different voice-mode value, a
    /// different resume id, a different working directory, or a different
    /// effort level, and when the child it holds has already exited:
    /// killed on interrupt, crashed, or ended by itself.
    async fn ensure_ready(&mut self) -> Result<()> {
        let want_voice = self.voice_mode_flag.load(Ordering::SeqCst);
        let want_effort = Effort::load(&self.effort_flag);
        let resume_changed = resume_id_changed(&self.claude_session_id, &self.spawned_resume_id);
        let current_dir = self.working_dir.lock().unwrap().clone();
        let dir_changed = working_dir_changed(&current_dir, &self.spawned_working_dir);
        if self.child.is_some()
            && (want_voice != self.spawned_voice_mode
                || resume_changed
                || dir_changed
                || effort_changed(want_effort, self.spawned_effort)
                || self.child_exited())
        {
            self.shutdown().await;
        }
        if self.child.is_none() {
            self.spawn_child(want_voice, current_dir, want_effort)?;
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
    /// `working_dir` is the directory to spawn it in, read from the shared
    /// flag by `ensure_ready` just before the call, since the child's cwd
    /// is fixed at spawn time. `effort` is the level to spawn under, read
    /// from `effort_flag` the same way, since `--effort` is also fixed at
    /// spawn time.
    fn spawn_child(
        &mut self,
        voice_mode: bool,
        working_dir: PathBuf,
        effort: Effort,
    ) -> Result<()> {
        let Some(binary) = resolve_claude_binary(self.extra_env.as_ref()) else {
            let message =
                format!("could not resolve the claude CLI binary; set {CLAUDE_CLI_PATH_KEY}");
            let _ = self.tx_events.send(RoutedEvent::own(StreamEvent::Error {
                message: message.clone(),
            }));
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
            effort,
        );

        let mut command = Command::new(&binary);
        command
            .args(&args)
            .current_dir(&working_dir)
            // Second line of defence behind `process_group::adopt` below.
            // This one only fires when the `Child` is really dropped, which
            // the exit path does not guarantee. The job object does.
            .kill_on_drop(true)
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
                let _ = self.tx_events.send(RoutedEvent::own(StreamEvent::Error {
                    message: message.clone(),
                }));
                return Err(HarnessError::Io(e));
            }
        };
        // The child outlives this harness otherwise. See
        // `src/process_group.rs` for what that cost once.
        crate::process_group::adopt(&child);

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
        spawn_stdout_reader(
            stdout,
            self.tx_events.clone(),
            turn_done_tx,
            session_id_tx,
            self.last_reply.clone(),
        );
        spawn_stderr_drain(stderr);

        self.child = Some(child);
        self.stdin = Some(stdin);
        self.turn_done = Some(turn_done_rx);
        self.session_id_rx = Some(session_id_rx);
        self.spawned_voice_mode = voice_mode;
        self.spawned_resume_id = self.claude_session_id.clone();
        self.spawned_working_dir = Some(working_dir);
        self.spawned_effort = effort;
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
        let _ = self
            .tx_events
            .send(RoutedEvent::own(StreamEvent::Interrupted {
                message: "Interrupted by user (Escape)".into(),
            }));
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
        let _ = self.tx_events.send(RoutedEvent::own(event));
    }

    /// Test seam for `ensure_ready`. `send_with_image` calls the real
    /// method unconditionally, so it keeps its own visibility; this thin
    /// wrapper is the only way a test outside the module can drive it
    /// directly, with no real child process, the way the respawn-trigger
    /// tests need.
    #[cfg(feature = "test-support")]
    pub async fn ensure_ready_for_test(&mut self) -> Result<()> {
        self.ensure_ready().await
    }

    /// Test seam for `child_exited`. `ensure_ready` calls the real method
    /// unconditionally, so it keeps its own visibility.
    #[cfg(feature = "test-support")]
    pub fn child_exited_for_test(&mut self) -> bool {
        self.child_exited()
    }

    /// Test seam for `await_turn_end`. `send_with_image` calls the real
    /// method unconditionally, so it keeps its own visibility.
    #[cfg(feature = "test-support")]
    pub async fn await_turn_end_for_test(&mut self) {
        self.await_turn_end().await
    }

    /// Test seam over the private `stdin` field, which has no other
    /// observable effect a test could check without a real child process.
    #[cfg(feature = "test-support")]
    pub fn stdin_is_none_for_test(&self) -> bool {
        self.stdin.is_none()
    }

    /// Test seam over the private `turn_done` field. Paired with
    /// `set_turn_done_for_test` and `turn_done_try_recv_is_err_for_test`
    /// below, so a test can set up and inspect the turn-boundary channel
    /// directly, without a real child process.
    #[cfg(feature = "test-support")]
    pub fn turn_done_is_none_for_test(&self) -> bool {
        self.turn_done.is_none()
    }

    /// Test seam to install a `turn_done` receiver directly, so a test can
    /// drive `await_turn_end` without spawning a real child.
    #[cfg(feature = "test-support")]
    pub fn set_turn_done_for_test(&mut self, rx: mpsc::UnboundedReceiver<()>) {
        self.turn_done = Some(rx);
    }

    /// Test seam to check the `turn_done` channel is empty after a signal
    /// was consumed, proving `await_turn_end` drained it rather than
    /// returning early.
    #[cfg(feature = "test-support")]
    pub fn turn_done_try_recv_is_err_for_test(&mut self) -> bool {
        self.turn_done
            .as_mut()
            .expect("turn_done must be set before calling this")
            .try_recv()
            .is_err()
    }

    /// Test seam over the private `spawned_working_dir` field, checked by
    /// the respawn-trigger tests to confirm a failed spawn attempt never
    /// records a directory.
    #[cfg(feature = "test-support")]
    pub fn spawned_working_dir_is_none_for_test(&self) -> bool {
        self.spawned_working_dir.is_none()
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

    async fn run_turn(&mut self, task: &str) -> Result<String> {
        self.send(task).await?;
        let reply = self
            .last_reply
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default();
        Ok(reply)
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
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
    turn_done: mpsc::UnboundedSender<()>,
    session_id: mpsc::UnboundedSender<String>,
    last_reply: Arc<Mutex<String>>,
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
                    let _ = tx_events.send(RoutedEvent::own(StreamEvent::Error {
                        message: format!("claude CLI stdout read error: {e}"),
                    }));
                    break;
                }
            };
            let Some(event) = parse_line(&next) else {
                continue;
            };
            let ends_turn = matches!(event, ClaudeEvent::Result(_));
            for stream_event in mapper.map(event) {
                if let StreamEvent::Text { text, .. } = &stream_event
                    && let Ok(mut reply) = last_reply.lock()
                {
                    reply.push_str(text.as_str());
                }
                if tx_events.send(RoutedEvent::own(stream_event)).is_err() {
                    return;
                }
            }
            if !session_id_sent && let Some(id) = mapper.session_id() {
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
