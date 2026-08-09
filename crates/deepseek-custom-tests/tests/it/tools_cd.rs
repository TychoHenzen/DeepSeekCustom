//! Unit tests for `deepseek_custom::tools::cd` (`src/tools/cd.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::{Arc, Mutex};

use deepseek_custom::tools::Tool;
use deepseek_custom::tools::bash::BashTool;
use deepseek_custom::tools::cd::CdTool;

fn dir_arc(p: std::path::PathBuf) -> Arc<Mutex<std::path::PathBuf>> {
    Arc::new(Mutex::new(p))
}

/// Create a uniquely named directory under the system temp dir.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-cd-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn successful_change_moves_the_shared_value() {
    let start = unique_temp_dir("start");
    let target = unique_temp_dir("target");
    let shared = dir_arc(start.clone());
    let tool = CdTool::new(shared.clone());

    let output = tool
        .execute(serde_json::json!({"path": target.to_string_lossy()}))
        .await
        .expect("execute");
    assert!(!output.is_error, "unexpected error: {}", output.content);

    let expected = std::fs::canonicalize(&target).unwrap();
    assert_eq!(*shared.lock().unwrap(), expected);

    let _ = std::fs::remove_dir_all(&start);
    let _ = std::fs::remove_dir_all(&target);
}

#[tokio::test]
async fn following_bash_call_runs_in_the_new_directory() {
    let start = unique_temp_dir("bash-start");
    let target = unique_temp_dir("bash-target");
    let shared = dir_arc(start.clone());
    let cd = CdTool::new(shared.clone());
    let bash = BashTool::new(shared.clone());

    let cd_output = cd
        .execute(serde_json::json!({"path": target.to_string_lossy()}))
        .await
        .expect("execute");
    assert!(!cd_output.is_error);

    let bash_output = bash
        .execute(serde_json::json!({"command": "cd"}))
        .await
        .expect("execute");
    assert!(!bash_output.is_error);
    let canonical_target = std::fs::canonicalize(&target).unwrap();
    // cmd's own `cd` builtin prints the current directory; strip any
    // Windows extended-length prefix quirk by just checking containment
    // of the target's file name, which is unique to this test run.
    let target_name = canonical_target.file_name().unwrap().to_string_lossy();
    assert!(
        bash_output.content.contains(target_name.as_ref()),
        "expected bash cwd to contain {}, got: {}",
        target_name,
        bash_output.content
    );

    let _ = std::fs::remove_dir_all(&start);
    let _ = std::fs::remove_dir_all(&target);
}

#[tokio::test]
async fn two_relative_changes_compose() {
    let root = unique_temp_dir("compose-root");
    let inner_a = root.join("a");
    let inner_b = inner_a.join("b");
    std::fs::create_dir_all(&inner_b).unwrap();

    let shared = dir_arc(root.clone());
    let tool = CdTool::new(shared.clone());

    let first = tool
        .execute(serde_json::json!({"path": "a"}))
        .await
        .expect("execute");
    assert!(!first.is_error, "unexpected error: {}", first.content);

    let second = tool
        .execute(serde_json::json!({"path": "b"}))
        .await
        .expect("execute");
    assert!(!second.is_error, "unexpected error: {}", second.content);

    let expected = std::fs::canonicalize(&inner_b).unwrap();
    assert_eq!(*shared.lock().unwrap(), expected);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn missing_path_is_a_tool_error_naming_the_path() {
    let start = unique_temp_dir("missing-start");
    let shared = dir_arc(start.clone());
    let tool = CdTool::new(shared.clone());

    let missing = start.join("does-not-exist");
    let output = tool
        .execute(serde_json::json!({"path": missing.to_string_lossy()}))
        .await
        .expect("execute");
    assert!(output.is_error);
    assert!(output.content.contains("no such path"));
    assert_eq!(*shared.lock().unwrap(), start);

    let _ = std::fs::remove_dir_all(&start);
}

#[tokio::test]
async fn path_that_is_a_file_is_a_tool_error() {
    let start = unique_temp_dir("file-start");
    let file = start.join("not_a_dir.txt");
    std::fs::write(&file, "hello").unwrap();
    let shared = dir_arc(start.clone());
    let tool = CdTool::new(shared.clone());

    let output = tool
        .execute(serde_json::json!({"path": file.to_string_lossy()}))
        .await
        .expect("execute");
    assert!(output.is_error);
    assert!(output.content.contains("not a directory"));
    assert_eq!(*shared.lock().unwrap(), start);

    let _ = std::fs::remove_dir_all(&start);
}

#[tokio::test]
async fn invalid_input_is_a_tool_error_not_a_hard_err() {
    let shared = dir_arc(std::env::current_dir().unwrap());
    let tool = CdTool::new(shared);

    let output = tool
        .execute(serde_json::json!({"not_path": "whatever"}))
        .await
        .expect("execute");
    assert!(output.is_error);
}
