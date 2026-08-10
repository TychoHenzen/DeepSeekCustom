//! Unit tests for `deepseek_custom::tools::write` (`src/tools/write.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::{Arc, Mutex};

use deepseek_custom::tools::Tool;
use deepseek_custom::tools::write::WriteTool;

fn dir_arc(p: std::path::PathBuf) -> Arc<Mutex<std::path::PathBuf>> {
    Arc::new(Mutex::new(p))
}

/// Create a uniquely named directory under the system temp dir.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-write-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn writes_and_verifies_file() {
    let root = unique_temp_dir("verify-root");
    let tool = WriteTool::new(dir_arc(root.clone()));

    let test_path = "test_write_output.txt";
    let content = "hello from write tool";
    let input = serde_json::json!({"file_path": test_path, "content": content});
    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error);
    assert!(output.content.contains("Wrote"));

    // Verify file was written
    let written = std::fs::read_to_string(root.join(test_path)).unwrap();
    assert_eq!(written, content);

    // Cleanup
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn writes_via_absolute_path() {
    let dir = unique_temp_dir("absolute");
    let file = dir.join("hello.txt");

    let tool = WriteTool::new(dir_arc(std::env::current_dir().unwrap()));
    let input = serde_json::json!({"file_path": file.to_string_lossy(), "content": "hello world"});
    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn changing_shared_working_dir_moves_where_relative_writes_land() {
    let dir_a = unique_temp_dir("a");
    let dir_b = unique_temp_dir("b");

    let shared = dir_arc(dir_a.clone());
    let tool = WriteTool::new(shared.clone());

    let first = tool
        .execute(serde_json::json!({"file_path": "out.txt", "content": "in_a"}))
        .await
        .expect("execute");
    assert!(!first.is_error);
    assert_eq!(
        std::fs::read_to_string(dir_a.join("out.txt")).unwrap(),
        "in_a"
    );
    assert!(!dir_b.join("out.txt").exists());

    *shared.lock().unwrap() = dir_b.clone();

    let second = tool
        .execute(serde_json::json!({"file_path": "out.txt", "content": "in_b"}))
        .await
        .expect("execute");
    assert!(!second.is_error);
    assert_eq!(
        std::fs::read_to_string(dir_b.join("out.txt")).unwrap(),
        "in_b"
    );

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[tokio::test]
async fn overwriting_a_crlf_file_keeps_its_line_endings() {
    let root = unique_temp_dir("crlf");
    let path = root.join("sample.txt");
    std::fs::write(&path, "one\r\ntwo\r\n").unwrap();

    let tool = WriteTool::new(dir_arc(root.clone()));
    let output = tool
        .execute(serde_json::json!({"file_path": "sample.txt", "content": "one\nthree\n"}))
        .await
        .expect("execute");

    assert!(!output.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\r\nthree\r\n");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_new_file_is_written_with_the_line_endings_it_was_given() {
    let root = unique_temp_dir("lf");
    let path = root.join("fresh.txt");

    let tool = WriteTool::new(dir_arc(root.clone()));
    let output = tool
        .execute(serde_json::json!({"file_path": "fresh.txt", "content": "one\ntwo\n"}))
        .await
        .expect("execute");

    assert!(!output.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn overwriting_an_lf_file_does_not_add_carriage_returns() {
    let root = unique_temp_dir("keep-lf");
    let path = root.join("sample.txt");
    std::fs::write(&path, "one\ntwo\n").unwrap();

    let tool = WriteTool::new(dir_arc(root.clone()));
    let output = tool
        .execute(serde_json::json!({"file_path": "sample.txt", "content": "one\nthree\n"}))
        .await
        .expect("execute");

    assert!(!output.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nthree\n");
    let _ = std::fs::remove_dir_all(&root);
}
