//! A single `claude -p` invocation that runs to completion and exits, in
//! place of the long-lived stdin-fed child `process.rs` owns for the GUI
//! session. Meant for a subagent call: one prompt in, one answer out, then
//! the process is gone. `run_once` is an associated function on
//! `ClaudeCliDriver` rather than a method, since a one-shot run owns nothing
//! and outlives nothing that a constructed driver would give it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::agent::agent_loop::StreamEvent;
use crate::effort::Effort;

use super::events::{parse_line, ClaudeEvent};
use super::map::EventMapper;
use super::process::{resolve_claude_binary, spawn_stderr_drain, ClaudeCliDriver, CLAUDE_CLI_PATH_KEY};

/// How often the read loop checks the interrupt flag between lines. A
/// shorter poll interval kills a stuck run faster. 100ms is short enough
/// that a user-facing interrupt still feels immediate.
const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The outcome of one `run_once` call, filled from the final `result` event
/// of the stream-json protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct OneShotResult {
    /// The concatenation of every `StreamEvent::Text` chunk the run
    /// produced, in order.
    pub text: String,
    pub is_error: bool,
    /// From the final `result` event.
    pub total_cost_usd: Option<f64>,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Build the argument vector for a one-shot `claude -p <prompt>` run. The
/// prompt is a positional argument instead of going over stdin. There is no
/// `--input-format` flag, since there is no streaming input to declare a
/// format for. `effort`, when it maps to a CLI value, adds `--effort
/// <level>` right after the base flags, the same rule `build_args` in
/// `process.rs` applies for the long-lived driver: `Effort::None` omits
/// the flag entirely, since the CLI has no `none` value of its own.
fn build_one_shot_args(
    model: &str,
    permission_mode: Option<&str>,
    prompt: &str,
    effort: Effort,
) -> Vec<String> {
    let mode = permission_mode.unwrap_or("bypassPermissions");
    let mut args = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--include-partial-messages".to_string(),
        "--verbose".to_string(),
        "--model".to_string(),
        model.to_string(),
        "--permission-mode".to_string(),
        mode.to_string(),
    ];
    if let Some(level) = effort.claude_cli_effort() {
        args.push("--effort".to_string());
        args.push(level.to_string());
    }
    args
}

/// Fold one `ClaudeEvent` into the running one-shot accumulation state.
/// Returns `Some` only when `event` was the terminal `result` event.
/// Every other event is passed through `mapper`, and any `StreamEvent::Text`
/// it emits is appended to `text`. Shared by `run_once`'s live read loop and
/// by the test below, so the fold logic is exercised without spawning a
/// process.
fn accumulate_one_shot_event(
    mapper: &mut EventMapper,
    event: ClaudeEvent,
    text: &mut String,
) -> Option<OneShotResult> {
    let data = match event {
        ClaudeEvent::Result(data) => data,
        other => {
            for stream_event in mapper.map(other) {
                if let StreamEvent::Text { text: chunk, .. } = stream_event {
                    text.push_str(&chunk);
                }
            }
            return None;
        }
    };
    let usage = data.usage.unwrap_or_default();
    Some(OneShotResult {
        text: text.clone(),
        is_error: data.is_error,
        total_cost_usd: data.total_cost_usd,
        input_tokens: usage.input_tokens as u32,
        output_tokens: usage.output_tokens as u32,
    })
}

/// Spawn the one-shot child: resolve the binary, build the argument vector,
/// and pipe stdout/stderr. Stdin is closed immediately, since the prompt
/// travels as a positional argument and no turn ever follows it. `working_dir`
/// is where the child spawns, the harness's own working directory, not
/// necessarily `project_root`: see `Task`'s `working_dir` override in
/// `src/tools/task.rs`.
fn spawn_one_shot_child(
    model: &str,
    permission_mode: Option<&str>,
    extra_env: Option<&HashMap<String, String>>,
    working_dir: &Path,
    prompt: &str,
    effort: Effort,
) -> Result<Child, String> {
    let binary = resolve_claude_binary(extra_env).ok_or_else(|| {
        format!("could not resolve the claude CLI binary; set {CLAUDE_CLI_PATH_KEY}")
    })?;
    let args = build_one_shot_args(model, permission_mode, prompt, effort);

    let mut command = Command::new(&binary);
    command
        .args(&args)
        .current_dir(working_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if let Some(env) = extra_env {
        for (key, value) in env {
            if key == CLAUDE_CLI_PATH_KEY {
                continue;
            }
            command.env(key, value);
        }
    }

    command
        .spawn()
        .map_err(|e| format!("failed to spawn claude CLI at {}: {e}", binary.display()))
}

/// Read the child's stdout line by line until the terminal `result` event
/// arrives, polling `interrupt_flag` between lines. Killing the child on
/// interrupt happens here. The caller is still responsible for `wait`ing on
/// it afterward, so no zombie is left behind.
async fn read_one_shot_events(
    stdout: tokio::process::ChildStdout,
    interrupt_flag: &Arc<AtomicBool>,
    child: &mut Child,
) -> Result<OneShotResult, String> {
    let mut lines = BufReader::new(stdout).lines();
    let mut mapper = EventMapper::new();
    let mut text = String::new();

    loop {
        if interrupt_flag.load(Ordering::SeqCst) {
            let _ = child.start_kill();
            return Err("claude CLI one-shot run was interrupted".to_string());
        }
        match tokio::time::timeout(INTERRUPT_POLL_INTERVAL, lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                let Some(event) = parse_line(&line) else {
                    continue;
                };
                if let Some(result) = accumulate_one_shot_event(&mut mapper, event, &mut text) {
                    return Ok(result);
                }
            }
            Ok(Ok(None)) => {
                return Err("claude CLI one-shot run ended without a result event".to_string());
            }
            Ok(Err(e)) => return Err(format!("claude CLI stdout read error: {e}")),
            Err(_timed_out) => continue,
        }
    }
}

impl ClaudeCliDriver {
    /// Run one prompt against `claude -p` to completion, then exit. This is
    /// an associated function, not a method. A one-shot run owns nothing and
    /// outlives nothing, so it needs no constructed driver. Stderr drains on
    /// its own task, so a full stderr pipe cannot deadlock the child. The
    /// child is always waited on before this returns, whether it finished,
    /// errored, or was interrupted.
    pub async fn run_once(
        model: &str,
        permission_mode: Option<&str>,
        extra_env: Option<&HashMap<String, String>>,
        working_dir: &Path,
        prompt: &str,
        interrupt_flag: Arc<AtomicBool>,
        effort: Effort,
    ) -> Result<OneShotResult, String> {
        let mut child = spawn_one_shot_child(
            model,
            permission_mode,
            extra_env,
            working_dir,
            prompt,
            effort,
        )?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "claude CLI child has no stdout".to_string())?;
        if let Some(stderr) = child.stderr.take() {
            spawn_stderr_drain(stderr);
        }

        let outcome = read_one_shot_events(stdout, &interrupt_flag, &mut child).await;

        if let Err(e) = child.wait().await {
            tracing::warn!("claude_cli: failed waiting for one-shot child exit: {e}");
        }

        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const TOOLS_FIXTURE: &str =
        include_str!("../../../tests/fixtures/claude_stream_json_tools.jsonl");

    /// `spawn_one_shot_child` builds its `Command` with `working_dir`, not
    /// `project_root`: there is no `project_root` in this function's scope
    /// at all, `working_dir` is the only directory it is ever given. This
    /// proves the wiring the same way `process.rs`'s spawn-wiring tests do,
    /// without ever running a real `claude` binary: `cmd.exe`, always
    /// present at this path on Windows (this is a Windows-only project, see
    /// CLAUDE.md), starts as a plain shell on unrecognized switches, and
    /// Windows refuses to start any process at all when its working
    /// directory does not exist. So a spawn into a missing directory only
    /// fails if `working_dir` really reached `Command::current_dir`.
    #[tokio::test]
    async fn spawn_one_shot_child_honors_the_given_working_dir() {
        let cmd_exe = PathBuf::from(r"C:\Windows\System32\cmd.exe");
        if !cmd_exe.is_file() {
            return;
        }
        let mut env = HashMap::new();
        env.insert(
            CLAUDE_CLI_PATH_KEY.to_string(),
            cmd_exe.to_string_lossy().to_string(),
        );

        let missing_dir = std::env::temp_dir().join(format!(
            "one_shot_missing_dir_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!missing_dir.exists());

        let err = spawn_one_shot_child(
            "opus",
            None,
            Some(&env),
            &missing_dir,
            "hello",
            Effort::None,
        )
        .expect_err("spawning into a nonexistent working directory must fail");
        assert!(err.contains("failed to spawn"), "unexpected error: {err}");

        let existing_dir = std::env::temp_dir();
        let mut child = spawn_one_shot_child(
            "opus",
            None,
            Some(&env),
            &existing_dir,
            "hello",
            Effort::None,
        )
        .expect("spawning into an existing directory should succeed");
        let _ = child.start_kill();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
    }

    #[test]
    fn one_shot_args_builder_produces_exact_flag_list_with_prompt_positional() {
        let args = build_one_shot_args("claude-opus-x", Some("acceptEdits"), "hello there", Effort::None);
        assert_eq!(
            args,
            vec![
                "-p",
                "hello there",
                "--output-format",
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
    fn one_shot_args_builder_omits_input_format() {
        let args = build_one_shot_args("claude-opus-x", Some("acceptEdits"), "hello", Effort::None);
        assert!(!args.contains(&"--input-format".to_string()));
    }

    #[test]
    fn one_shot_args_builder_defaults_permission_mode_to_bypass_permissions() {
        let args = build_one_shot_args("claude-opus-x", None, "hello", Effort::None);
        assert_eq!(args[9], "bypassPermissions");
    }

    #[test]
    fn one_shot_args_builder_omits_effort_flag_for_effort_none() {
        let args = build_one_shot_args("claude-opus-x", None, "hello", Effort::None);
        assert!(!args.contains(&"--effort".to_string()));
    }

    #[test]
    fn one_shot_args_builder_appends_effort_flag_for_every_other_level() {
        for (level, expected) in [
            (Effort::Low, "low"),
            (Effort::Medium, "medium"),
            (Effort::High, "high"),
            (Effort::Max, "max"),
        ] {
            let args = build_one_shot_args("claude-opus-x", None, "hello", level);
            assert_eq!(args[10], "--effort", "level {level:?}");
            assert_eq!(args[11], expected, "level {level:?}");
        }
    }

    #[test]
    fn one_shot_result_folds_from_tools_fixture_result_event() {
        let mut mapper = EventMapper::new();
        let mut text = String::new();
        let mut result = None;
        for line in TOOLS_FIXTURE.lines() {
            let Some(event) = parse_line(line) else {
                continue;
            };
            if let Some(r) = accumulate_one_shot_event(&mut mapper, event, &mut text) {
                result = Some(r);
                break;
            }
        }
        let result = result.expect("expected a result event in the fixture");

        assert!(!result.text.is_empty(), "expected non-empty accumulated text");
        assert!(!result.is_error);
        assert_eq!(result.input_tokens, 20);
        assert_eq!(result.output_tokens, 260);
        assert_eq!(result.total_cost_usd, Some(0.143862));
    }
}
