//! Unit tests for `deepseek_custom::tools::bash` (`src/tools/bash.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::{Arc, Mutex};

use deepseek_custom::tools::Tool;
use deepseek_custom::tools::bash::{BashInput, BashTool, Shell, split_shell_words};

fn dir_arc(p: std::path::PathBuf) -> Arc<Mutex<std::path::PathBuf>> {
    Arc::new(Mutex::new(p))
}

#[test]
fn input_schema_is_valid_json() {
    let tool = BashTool::new(dir_arc(std::env::current_dir().unwrap()));
    let schema = tool.input_schema();
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["command"]["type"] == "string");
}

#[tokio::test]
async fn echo_hello_returns_correct_stdout() {
    let tool = BashTool::new(dir_arc(std::env::current_dir().unwrap()));
    let input = serde_json::json!({"command": "echo hello"});
    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error);
    assert!(output.content.contains("hello"));
    assert!(output.content.contains("exit code: 0"));
}

#[tokio::test]
async fn timeout_kills_long_running_command() {
    let tool = BashTool::new(dir_arc(std::env::current_dir().unwrap()));
    // A cmd builtin loop, so the test does not depend on any program being
    // on PATH. It runs for minutes, and the timeout must cut it short.
    let input = serde_json::json!({
        "command": "for /L %i in (1,1,200000000) do @rem",
        "timeout_ms": 500
    });
    let output = tool.execute(input).await.expect("execute");
    assert!(
        output.is_error,
        "expected an error, got: {}",
        output.content
    );
    assert!(
        output.content.contains("timed out"),
        "expected a timeout, got: {}",
        output.content
    );
}

#[tokio::test]
async fn powershell_auto_detected_and_run_directly() {
    let tool = BashTool::new(dir_arc(std::env::current_dir().unwrap()));
    let input = serde_json::json!({
        "command": "powershell -NoProfile -Command \"Write-Output 'ps_hello'\""
    });
    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error);
    assert!(
        output.content.contains("ps_hello"),
        "expected 'ps_hello' in output, got: {}",
        output.content
    );
    assert!(output.content.contains("exit code: 0"));
}

#[tokio::test]
async fn explicit_shell_cmd_works() {
    let tool = BashTool::new(dir_arc(std::env::current_dir().unwrap()));
    let input = serde_json::json!({
        "command": "echo cmd_explicit",
        "shell": "cmd"
    });
    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error);
    assert!(output.content.contains("cmd_explicit"));
}

#[tokio::test]
async fn explicit_shell_powershell_works() {
    let tool = BashTool::new(dir_arc(std::env::current_dir().unwrap()));
    let input = serde_json::json!({
        "command": "powershell -NoProfile -Command \"Write-Output 'pwsh_explicit'\"",
        "shell": "powershell"
    });
    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error);
    assert!(
        output.content.contains("pwsh_explicit"),
        "expected 'pwsh_explicit' in output, got: {}",
        output.content
    );
}

#[test]
fn split_shell_words_handles_quotes() {
    let result = split_shell_words("powershell -NoProfile -Command \"Write-Output 'hello world'\"");
    assert_eq!(result.len(), 4);
    assert_eq!(result[0], "powershell");
    assert_eq!(result[1], "-NoProfile");
    assert_eq!(result[2], "-Command");
    assert_eq!(result[3], "Write-Output 'hello world'");
}

#[test]
fn split_shell_words_simple_command() {
    let result = split_shell_words("echo hello world");
    assert_eq!(result, vec!["echo", "hello", "world"]);
}

#[test]
fn resolve_shell_defaults_to_auto() {
    let input: BashInput = serde_json::from_value(serde_json::json!({
        "command": "echo hello"
    }))
    .unwrap();
    assert_eq!(input.resolve_shell(), Shell::Auto);
}

#[test]
fn resolve_shell_explicit_cmd() {
    let input: BashInput = serde_json::from_value(serde_json::json!({
        "command": "echo hello",
        "shell": "cmd"
    }))
    .unwrap();
    assert_eq!(input.resolve_shell(), Shell::Cmd);
}

#[test]
fn resolve_shell_explicit_powershell() {
    let input: BashInput = serde_json::from_value(serde_json::json!({
        "command": "echo hello",
        "shell": "powershell"
    }))
    .unwrap();
    assert_eq!(input.resolve_shell(), Shell::Ps);
}

/// Create a uniquely named directory under the system temp dir.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-bash-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn changing_shared_work_dir_moves_where_the_next_command_runs() {
    let dir_a = unique_temp_dir("a");
    let dir_b = unique_temp_dir("b");
    std::fs::write(dir_a.join("marker.txt"), "in_a").unwrap();
    std::fs::write(dir_b.join("marker.txt"), "in_b").unwrap();

    let shared = dir_arc(dir_a.clone());
    let tool = BashTool::new(shared.clone());

    let first = tool
        .execute(serde_json::json!({"command": "type marker.txt"}))
        .await
        .expect("execute");
    assert!(first.content.contains("in_a"), "got: {}", first.content);

    // Change the shared value between calls, same tool instance.
    *shared.lock().unwrap() = dir_b.clone();

    let second = tool
        .execute(serde_json::json!({"command": "type marker.txt"}))
        .await
        .expect("execute");
    assert!(second.content.contains("in_b"), "got: {}", second.content);

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}
