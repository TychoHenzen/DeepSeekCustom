//! Unit tests for `deepseek_custom::backend::claude_cli::one_shot` (`src/backend/claude_cli/one_shot.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::collections::HashMap;
use std::path::PathBuf;

use deepseek_custom::backend::claude_cli::events::parse_line;
use deepseek_custom::backend::claude_cli::map::EventMapper;
use deepseek_custom::backend::claude_cli::one_shot::{
    accumulate_one_shot_event, build_one_shot_args, build_planning_one_shot_args,
    build_restricted_one_shot_args_for_test, spawn_one_shot_child,
};
use deepseek_custom::effort::Effort;

/// Matches `CLAUDE_CLI_PATH_KEY` in `src/backend/claude_cli/process.rs`, the
/// same way `tests/claude_cli_lifecycle.rs` does. That constant is
/// `pub(super)`, not reachable from an external integration test crate.
const CLAUDE_CLI_PATH_KEY: &str = "CLAUDE_CLI_PATH";

const TOOLS_FIXTURE: &str = include_str!("fixtures/claude_stream_json_tools.jsonl");

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
    let args = build_one_shot_args(
        "claude-opus-x",
        Some("acceptEdits"),
        "hello there",
        Effort::None,
    );
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
            "--thinking-display",
            "summarized",
        ]
    );
}

#[test]
fn one_shot_args_builder_asks_for_visible_thinking() {
    // A non-interactive run defaults to `omitted`, which makes the API
    // send `thinking_delta` events with an empty `thinking` field.
    let args = build_one_shot_args("claude-opus-x", None, "hello", Effort::Medium);
    let index = args
        .iter()
        .position(|a| a == "--thinking-display")
        .expect("expected a --thinking-display flag");
    assert_eq!(args[index + 1], "summarized");
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
fn restricted_one_shot_args_ignore_bypass_mode_and_disable_project_settings() {
    let args = build_restricted_one_shot_args_for_test(
        "claude-opus-x",
        Some("bypassPermissions"),
        "diagnose",
        Effort::None,
    );
    assert_eq!(args[9], "default");
    let restricted = args
        .iter()
        .position(|arg| arg == "--restricted")
        .expect("restricted one-shot args must use Claude safe mode");
    let tools = args
        .iter()
        .position(|arg| arg == "--tools")
        .expect("restricted one-shot args must carry the no-tools flag");
    assert!(restricted < tools);
    assert_eq!(args[tools + 1], "");
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
        assert_eq!(args[12], "--effort", "level {level:?}");
        assert_eq!(args[13], expected, "level {level:?}");
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

    assert!(
        !result.text.is_empty(),
        "expected non-empty accumulated text"
    );
    assert!(!result.is_error);
    assert_eq!(result.input_tokens, 20);
    assert_eq!(result.output_tokens, 260);
    assert_eq!(result.total_cost_usd, Some(0.143862));
}

#[test]
fn controlled_planning_one_shot_forces_fresh_read_only_structured_output() {
    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id"]
    })
    .to_string();
    let args =
        build_planning_one_shot_args("claude-opus-x", "produce one card", &schema, Effort::Medium);

    assert_eq!(args[0..2], ["-p", "produce one card"]);
    for required in [
        "--safe-mode",
        "--no-session-persistence",
        "--permission-mode",
        "plan",
        "--allowedTools",
        "Read,Glob,Grep",
        "--json-schema",
        schema.as_str(),
    ] {
        assert!(
            args.iter().any(|argument| argument == required),
            "missing {required}: {args:?}"
        );
    }
    for forbidden in ["--resume", "Task", "SendMessage", "CloseSession", "Bash"] {
        assert!(
            !args.iter().any(|argument| argument == forbidden),
            "unexpected {forbidden}: {args:?}"
        );
    }
}
