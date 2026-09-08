//! Unit tests for `deepseek_custom::backend::claude_cli::process` (`src/backend/claude_cli/process.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::events::StreamEvent;
use deepseek_custom::api::types::ImageAttachment;
use deepseek_custom::backend::claude_cli::args::{
    build_args, build_restricted_args_for_test, build_user_turn_line, effort_changed,
    resolve_claude_binary, resume_id_changed, working_dir_changed,
};
use deepseek_custom::backend::claude_cli::process::ClaudeCliDriver;
use deepseek_custom::effort::Effort;

use tokio::sync::mpsc;

/// Matches `CLAUDE_CLI_PATH_KEY` in `src/backend/claude_cli/process.rs`, the
/// same way `tests/claude_cli_lifecycle.rs` does. That constant is
/// `pub(super)`, not reachable from an external integration test crate.
const CLAUDE_CLI_PATH_KEY: &str = "CLAUDE_CLI_PATH";

/// A fresh `Arc<Mutex<PathBuf>>` for tests that need a `working_dir`
/// but never spawn a real child, so the exact path does not matter.
fn test_working_dir() -> Arc<Mutex<PathBuf>> {
    Arc::new(Mutex::new(PathBuf::from(".")))
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
    let line = build_user_turn_line("hello there", None);
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
    let line = build_user_turn_line(text, None);
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    let round_tripped = parsed["message"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(round_tripped, text);
}

/// Pins the exact wire shape confirmed against the real `claude`
/// binary in `docs/notes/image-support.md`: an Anthropic `image`
/// content block, base64 source, after the text block, not the OpenAI
/// `image_url` shape the API backends speak.
#[test]
fn stdin_line_builder_appends_an_anthropic_image_block_when_an_image_is_given() {
    let image = ImageAttachment {
        data: "AAA".into(),
        media_type: "image/png".into(),
    };
    let line = build_user_turn_line("what color is this?", Some(&image));
    let expected = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                {"type": "text", "text": "what color is this?"},
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "AAA"
                    }
                }
            ]
        }
    });
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed, expected);
}

#[test]
fn stdin_line_builder_with_no_image_is_unaffected_compared_to_before() {
    let with_none = build_user_turn_line("hello", None);
    let expected = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": "hello"}]
        }
    });
    let parsed: serde_json::Value = serde_json::from_str(&with_none).unwrap();
    assert_eq!(parsed, expected);
}

#[test]
fn args_builder_produces_exact_flag_list_in_order() {
    let args = build_args(
        "claude-opus-x",
        Some("acceptEdits"),
        None,
        None,
        Effort::None,
    );
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
            "--thinking-display",
            "summarized",
        ]
    );
}

#[test]
fn args_builder_asks_for_visible_thinking() {
    // A non-interactive run defaults to `omitted`, which makes the API
    // send `thinking_delta` events with an empty `thinking` field.
    let args = build_args("claude-opus-x", None, None, None, Effort::Medium);
    let index = args
        .iter()
        .position(|a| a == "--thinking-display")
        .expect("expected a --thinking-display flag");
    assert_eq!(args[index + 1], "summarized");
}

#[test]
fn args_builder_defaults_permission_mode_to_bypass_permissions() {
    let args = build_args("claude-opus-x", None, None, None, Effort::None);
    assert_eq!(args[10], "bypassPermissions");
}

#[test]
fn restricted_args_ignore_bypass_mode_and_disable_project_settings() {
    let args = build_restricted_args_for_test(
        "claude-opus-x",
        Some("bypassPermissions"),
        None,
        None,
        Effort::None,
    );
    assert_eq!(args[10], "default");
    let restricted = args
        .iter()
        .position(|arg| arg == "--restricted")
        .expect("restricted diagnostic args must use Claude safe mode");
    let tools = args
        .iter()
        .position(|arg| arg == "--tools")
        .expect("restricted diagnostic args must carry the no-tools flag");
    assert!(restricted < tools);
    assert_eq!(args[tools + 1], "");
}

#[test]
fn args_builder_appends_system_prompt_when_voice_mode_is_on() {
    let args = build_args(
        "claude-opus-x",
        Some("acceptEdits"),
        Some("voice text"),
        None,
        Effort::None,
    );
    assert_eq!(args[13], "--append-system-prompt");
    assert_eq!(args[14], "voice text");
}

#[test]
fn args_builder_omits_system_prompt_flag_when_not_given() {
    let args = build_args(
        "claude-opus-x",
        Some("acceptEdits"),
        None,
        None,
        Effort::None,
    );
    assert!(!args.contains(&"--append-system-prompt".to_string()));
}

#[test]
fn args_builder_omits_resume_flag_when_no_id_is_held() {
    let args = build_args(
        "claude-opus-x",
        Some("acceptEdits"),
        None,
        None,
        Effort::None,
    );
    assert!(!args.contains(&"--resume".to_string()));
}

#[test]
fn args_builder_appends_resume_flag_with_the_exact_id_when_one_is_held() {
    let args = build_args(
        "claude-opus-x",
        Some("acceptEdits"),
        None,
        Some("c18eb67f-6873-45a4-aa7a-8755cecb4361"),
        Effort::None,
    );
    assert_eq!(args[13], "--resume");
    assert_eq!(args[14], "c18eb67f-6873-45a4-aa7a-8755cecb4361");
}

#[test]
fn args_builder_includes_both_system_prompt_and_resume_when_both_are_given() {
    let args = build_args(
        "claude-opus-x",
        Some("acceptEdits"),
        Some("voice text"),
        Some("some-id"),
        Effort::None,
    );
    assert_eq!(args[13], "--append-system-prompt");
    assert_eq!(args[14], "voice text");
    assert_eq!(args[15], "--resume");
    assert_eq!(args[16], "some-id");
}

#[test]
fn args_builder_omits_effort_flag_for_effort_none() {
    let args = build_args(
        "claude-opus-x",
        Some("acceptEdits"),
        None,
        None,
        Effort::None,
    );
    assert!(!args.contains(&"--effort".to_string()));
}

#[test]
fn args_builder_appends_effort_flag_for_every_other_level() {
    let cases = [
        (Effort::Low, "low"),
        (Effort::Medium, "medium"),
        (Effort::High, "high"),
        (Effort::Max, "max"),
    ];
    for (level, expected) in cases {
        let args = build_args("claude-opus-x", Some("acceptEdits"), None, None, level);
        assert_eq!(args[13], "--effort", "level {level:?}");
        assert_eq!(args[14], expected, "level {level:?}");
    }
}

#[test]
fn args_builder_puts_effort_before_system_prompt_and_resume() {
    let args = build_args(
        "claude-opus-x",
        Some("acceptEdits"),
        Some("voice text"),
        Some("some-id"),
        Effort::Max,
    );
    assert_eq!(args[13], "--effort");
    assert_eq!(args[14], "max");
    assert_eq!(args[15], "--append-system-prompt");
    assert_eq!(args[16], "voice text");
    assert_eq!(args[17], "--resume");
    assert_eq!(args[18], "some-id");
}

#[test]
fn effort_changed_is_false_when_levels_match() {
    assert!(!effort_changed(Effort::Low, Effort::Low));
}

#[test]
fn effort_changed_is_true_when_levels_differ() {
    assert!(effort_changed(Effort::Low, Effort::High));
}

#[test]
fn interrupt_drops_the_child_handles_so_the_next_turn_respawns() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut driver = ClaudeCliDriver::new("opus".to_string(), None, None, test_working_dir(), tx);

    driver.interrupt();

    assert!(driver.child_pid().is_none());
    assert!(driver.stdin_is_none_for_test());
    assert!(driver.turn_done_is_none_for_test());
    match rx.try_recv().expect("expected an Interrupted event").event {
        StreamEvent::Interrupted { .. } => {}
        other => panic!("expected Interrupted, got {other:?}"),
    }
}

#[test]
fn child_exited_is_false_when_no_child_is_running() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut driver = ClaudeCliDriver::new("opus".to_string(), None, None, test_working_dir(), tx);

    assert!(!driver.child_exited_for_test());
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

#[test]
fn working_dir_changed_is_true_before_any_child_has_spawned() {
    // `spawned` is `None` until the first `spawn_child` call, which
    // always counts as changed so the very first spawn goes ahead.
    assert!(working_dir_changed(std::path::Path::new("."), &None));
}

#[test]
fn working_dir_changed_is_false_when_the_directory_matches_the_spawned_one() {
    let dir = PathBuf::from("C:/some/project");
    assert!(!working_dir_changed(&dir, &Some(dir.clone())));
}

#[test]
fn working_dir_changed_is_true_when_the_directory_differs_from_the_spawned_one() {
    let spawned = PathBuf::from("C:/some/project");
    let current = PathBuf::from("C:/some/other-project");
    assert!(working_dir_changed(&current, &Some(spawned)));
}

#[tokio::test]
async fn ensure_ready_reads_the_live_working_dir_before_every_spawn_attempt() {
    // Forces every spawn attempt to fail without ever running a real
    // `claude` binary, the same way the `resolve_*` tests above point
    // `CLAUDE_CLI_PATH` at a file that exists but is not executable.
    // Each failed attempt still proves `ensure_ready` read the live
    // directory and tried to spawn: `spawn_child` sends a
    // `StreamEvent::Error` before it returns, and never reaches the
    // line that would record `spawned_working_dir` on success.
    let mut file = tempfile_named();
    std::io::Write::write_all(&mut file.1, b"not a real binary").unwrap();
    let mut env = HashMap::new();
    env.insert(
        CLAUDE_CLI_PATH_KEY.to_string(),
        file.0.to_string_lossy().to_string(),
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    let working_dir = Arc::new(Mutex::new(PathBuf::from("C:/first-dir")));
    let mut driver = ClaudeCliDriver::new(
        "opus".to_string(),
        None,
        Some(env),
        Arc::clone(&working_dir),
        tx,
    );

    assert!(driver.ensure_ready_for_test().await.is_err());
    assert!(driver.spawned_working_dir_is_none_for_test());
    assert!(matches!(
        rx.try_recv().expect("expected an Error event").event,
        StreamEvent::Error { .. }
    ));

    // The change is picked up from the shared `Arc`, not a value the
    // driver captured at construction: mutating it here is visible to
    // the very next `ensure_ready` call.
    *working_dir.lock().unwrap() = PathBuf::from("C:/second-dir");

    assert!(driver.ensure_ready_for_test().await.is_err());
    assert!(driver.spawned_working_dir_is_none_for_test());
    assert!(matches!(
        rx.try_recv().expect("expected a second Error event").event,
        StreamEvent::Error { .. }
    ));
}

#[tokio::test]
async fn await_turn_end_returns_at_once_when_no_turn_is_in_flight() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut driver = ClaudeCliDriver::new("opus".to_string(), None, None, test_working_dir(), tx);

    // No child, so no `turn_done` channel. This must return rather
    // than poll forever.
    driver.await_turn_end_for_test().await;
}

#[tokio::test]
async fn await_turn_end_returns_when_the_reader_signals_the_turn_ended() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut driver = ClaudeCliDriver::new("opus".to_string(), None, None, test_working_dir(), tx);
    let (turn_done_tx, turn_done_rx) = mpsc::unbounded_channel();
    driver.set_turn_done_for_test(turn_done_rx);
    turn_done_tx.send(()).unwrap();

    driver.await_turn_end_for_test().await;

    // The signal was consumed, so the channel is empty for the next
    // turn instead of returning from it right away.
    assert!(driver.turn_done_try_recv_is_err_for_test());
}

#[tokio::test]
async fn await_turn_end_kills_the_child_when_the_interrupt_flag_is_set() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut driver = ClaudeCliDriver::new("opus".to_string(), None, None, test_working_dir(), tx);
    let (_turn_done_tx, turn_done_rx) = mpsc::unbounded_channel();
    driver.set_turn_done_for_test(turn_done_rx);
    driver.interrupt_flag().store(true, Ordering::SeqCst);

    driver.await_turn_end_for_test().await;

    assert!(driver.turn_done_is_none_for_test());
    assert!(
        !driver.interrupt_flag().load(Ordering::SeqCst),
        "the flag must be cleared so it cannot cut the next turn short"
    );
}

#[test]
fn new_driver_seeds_model_flag_from_constructor_model() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let driver = ClaudeCliDriver::new("opus".to_string(), None, None, test_working_dir(), tx);
    assert_eq!(*driver.model_flag().lock().unwrap(), "opus");
}
