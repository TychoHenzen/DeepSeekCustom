//! Unit tests for `deepseek_custom::hooks`, moved out of the production
//! module as part of the two-crate workspace split.
//!
//! The two process-spawning tests used to call `HookRunner::run_one`, a
//! private helper. They now drive the same code through the public seam,
//! `HookRunner::run`, with a one-entry `HookDef` list, so no production
//! visibility had to widen.

use deepseek_custom::config::settings::HookDef;
use deepseek_custom::hooks::{HookEvent, HookResult, HookRunner};

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
    let json =
        r#"{"approved": false, "modified_input": {"command": "safe"}, "message": "blocked"}"#;
    let result: HookResult = serde_json::from_str(json).unwrap();
    assert!(!result.approved);
    assert_eq!(result.modified_input.unwrap()["command"], "safe");
    assert_eq!(result.message.unwrap(), "blocked");
}

#[tokio::test]
async fn hook_echo_returns_approved() {
    // Use echo as a simple hook that returns valid JSON
    let echo_cmd = r#"powershell -Command "Write-Output '{\"approved\": true}'""#;
    let hooks = vec![HookDef {
        command: echo_cmd.into(),
        timeout: Some(5_000),
    }];
    let result = HookRunner::run(&HookEvent::SessionStart, &hooks).await;
    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[tokio::test]
async fn hook_exit_nonzero_does_not_block() {
    let hooks = vec![HookDef {
        command: "exit 1".into(),
        timeout: Some(5_000),
    }];
    // Non-zero exit is approved anyway (don't block on failure)
    let result = HookRunner::run(&HookEvent::SessionStart, &hooks).await;
    assert!(result.is_ok());
    assert!(result.unwrap());
}
