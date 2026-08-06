//! Unit tests for `deepseek_custom::tools::read` (`src/tools/read.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::{Arc, Mutex};

use deepseek_custom::tools::Tool;
use deepseek_custom::tools::read::{ReadTool, format_with_line_numbers};

fn dir_arc(p: std::path::PathBuf) -> Arc<Mutex<std::path::PathBuf>> {
    Arc::new(Mutex::new(p))
}

/// Create a uniquely named directory under the system temp dir.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-read-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn reads_known_file_correctly() {
    let dir = unique_temp_dir("known-file");
    let file = dir.join("known.txt");
    std::fs::write(
        &file,
        "line one\nline two\nline three\nline four\nline five\nline six\nline seven",
    )
    .unwrap();

    let tool = ReadTool::new(dir_arc(dir.clone()));

    let input = serde_json::json!({"file_path": "known.txt", "limit": 5});
    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error);
    // The content itself, and the 1-based line numbering `format_with_line_numbers`
    // (already covered directly below) is known to produce.
    assert!(output.content.contains("1\tline one"));
    assert!(output.content.contains("5\tline five"));
    // The limit of 5 actually truncated the output: lines six and seven
    // must not appear.
    assert!(!output.content.contains("line six"));
    assert!(!output.content.contains("line seven"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn reads_via_absolute_path() {
    let dir = unique_temp_dir("absolute");
    let file = dir.join("hello.txt");
    std::fs::write(&file, "hello world").unwrap();

    let tool = ReadTool::new(dir_arc(std::env::current_dir().unwrap()));
    let input = serde_json::json!({"file_path": file.to_string_lossy()});
    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error);
    assert!(output.content.contains("hello world"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn changing_shared_working_dir_moves_where_relative_reads_resolve() {
    let dir_a = unique_temp_dir("a");
    let dir_b = unique_temp_dir("b");
    std::fs::write(dir_a.join("marker.txt"), "in_a").unwrap();
    std::fs::write(dir_b.join("marker.txt"), "in_b").unwrap();

    let shared = dir_arc(dir_a.clone());
    let tool = ReadTool::new(shared.clone());

    let first = tool
        .execute(serde_json::json!({"file_path": "marker.txt"}))
        .await
        .expect("execute");
    assert!(first.content.contains("in_a"), "got: {}", first.content);

    *shared.lock().unwrap() = dir_b.clone();

    let second = tool
        .execute(serde_json::json!({"file_path": "marker.txt"}))
        .await
        .expect("execute");
    assert!(second.content.contains("in_b"), "got: {}", second.content);

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[test]
fn format_line_numbers_correctly() {
    let lines = vec!["line one", "line two", "line three"];
    let result = format_with_line_numbers(&lines, 10);
    // Should have line numbers 10, 11, 12
    assert!(result.starts_with("10"));
    assert!(result.contains("11\tline two"));
    assert!(result.contains("12\tline three"));
}
