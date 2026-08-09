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
