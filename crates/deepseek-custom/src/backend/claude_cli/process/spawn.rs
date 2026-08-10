//! Child-process spawning for `ClaudeCliDriver`. Extracted from
//! `mod.rs` so that file stays under the 300-line bound.

use std::path::PathBuf;

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::agent::prompt::voice_mode_instructions;
use crate::effort::Effort;
use crate::error::{HarnessError, Result};

use super::super::args::{
    CLAUDE_CLI_PATH_KEY, build_args, effort_changed, resolve_claude_binary, resume_id_changed,
    working_dir_changed,
};
use super::super::io::{spawn_stderr_drain, spawn_stdout_reader};

use super::ClaudeCliDriver;

/// Bundle of handles taken from a freshly spawned child, so
/// `spawn_and_take_handles` can return them as one value and
/// `attach_child_io` can accept them without blowing past the
/// parameter-count bound.
struct SpawnedChild {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
}

impl ClaudeCliDriver {
    /// Ensure a child is running with the voice-mode setting, resume id,
    /// working directory, and effort level currently held. Respawns when a
    /// child exists but was spawned under a different voice-mode value, a
    /// different resume id, a different working directory, or a different
    /// effort level, and when the child it holds has already exited:
    /// killed on interrupt, crashed, or ended by itself.
    pub(super) async fn ensure_ready(&mut self) -> Result<()> {
        let want_voice = self
            .voice_mode_flag
            .load(std::sync::atomic::Ordering::SeqCst);
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
    pub(super) fn child_exited(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    // ── spawn_child helpers ────────────────────────────────────────

    /// Resolve the binary and report failure as both an event and an
    /// error. Extracted so `spawn_child` stays under the function-length
    /// bound.
    fn resolve_binary_for_spawn(&self) -> Result<PathBuf> {
        let Some(binary) = resolve_claude_binary(self.extra_env.as_ref()) else {
            let message =
                format!("could not resolve the claude CLI binary; set {CLAUDE_CLI_PATH_KEY}");
            let _ = self.tx_events.send(RoutedEvent::own(StreamEvent::Error {
                message: message.clone(),
            }));
            return Err(HarnessError::Tool(message));
        };
        Ok(binary)
    }

    /// Assemble a `Command` with all the flags, env vars, and pipe
    /// plumbing before spawning.
    fn build_spawn_command(
        &self,
        binary: &std::path::Path,
        args: &[String],
        working_dir: &std::path::Path,
    ) -> Command {
        let mut command = Command::new(binary);
        command
            .args(args)
            .current_dir(working_dir)
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
        command
    }

    /// Spawn the command, adopt the child into the process group, and
    /// take stdin/stdout/stderr handles. Reports failure as both an
    /// event and an error, same as `resolve_binary_for_spawn`.
    fn spawn_and_take_handles(
        &self,
        mut command: Command,
        binary: &std::path::Path,
    ) -> Result<SpawnedChild> {
        let mut child = command.spawn().map_err(|e| {
            let message = format!(
                "failed to spawn claude CLI at {}: {e}; set {CLAUDE_CLI_PATH_KEY}",
                binary.display()
            );
            let _ = self.tx_events.send(RoutedEvent::own(StreamEvent::Error {
                message: message.clone(),
            }));
            HarnessError::Io(e)
        })?;
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
        Ok(SpawnedChild {
            child,
            stdin,
            stdout,
            stderr,
        })
    }

    /// Wire up the reader/drainer tasks, the turn-done and session-id
    /// channels, and record spawn-time metadata so `ensure_ready` can
    /// detect a stale child later.
    fn attach_child_io(
        &mut self,
        spawned: SpawnedChild,
        voice_mode: bool,
        working_dir: PathBuf,
        effort: Effort,
    ) {
        let (turn_done_tx, turn_done_rx) = mpsc::unbounded_channel();
        let (session_id_tx, session_id_rx) = mpsc::unbounded_channel();
        spawn_stdout_reader(
            spawned.stdout,
            self.tx_events.clone(),
            turn_done_tx,
            session_id_tx,
            self.last_reply.clone(),
        );
        spawn_stderr_drain(spawned.stderr);

        self.child = Some(spawned.child);
        self.stdin = Some(spawned.stdin);
        self.turn_done = Some(turn_done_rx);
        self.session_id_rx = Some(session_id_rx);
        self.spawned_voice_mode = voice_mode;
        self.spawned_resume_id = self.claude_session_id.clone();
        self.spawned_working_dir = Some(working_dir);
        self.spawned_effort = effort;
    }

    /// Spawn the child process and start reading its stdout and stderr in
    /// background tasks. Every parsed event is published on `tx_events`.
    pub(super) fn spawn_child(
        &mut self,
        voice_mode: bool,
        working_dir: PathBuf,
        effort: Effort,
    ) -> Result<()> {
        let binary = self.resolve_binary_for_spawn()?;
        let append_prompt = voice_mode.then(voice_mode_instructions);
        let args = build_args(
            &self.model,
            self.permission_mode.as_deref(),
            append_prompt,
            self.claude_session_id.as_deref(),
            effort,
        );
        let command = self.build_spawn_command(&binary, &args, &working_dir);
        let spawned = self.spawn_and_take_handles(command, &binary)?;
        self.attach_child_io(spawned, voice_mode, working_dir, effort);
        Ok(())
    }
}
