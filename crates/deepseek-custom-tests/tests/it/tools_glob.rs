//! Unit tests for `deepseek_custom::tools::glob` (`src/tools/glob.rs`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use deepseek_custom::tools::Tool;
use deepseek_custom::tools::glob::GlobTool;

fn temp_dir(tag: &str) -> PathBuf {
    super::scratch_dir("dsc-glob", tag)
}

fn tool_in(dir: &std::path::Path) -> GlobTool {
    GlobTool::new(Arc::new(Mutex::new(dir.to_path_buf())))
}

#[tokio::test]
async fn finds_files_by_extension_across_nested_directories() {
    let dir = temp_dir("nested");
    std::fs::create_dir_all(dir.join("src").join("inner")).unwrap();
    std::fs::write(dir.join("src").join("a.rs"), "").unwrap();
    std::fs::write(dir.join("src").join("inner").join("b.rs"), "").unwrap();
    std::fs::write(dir.join("src").join("notes.md"), "").unwrap();

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "**/*.rs" }))
        .await
        .unwrap();

    assert!(!output.is_error, "{}", output.content);
    assert!(output.content.contains("a.rs"), "{}", output.content);
    assert!(output.content.contains("b.rs"), "{}", output.content);
    assert!(!output.content.contains("notes.md"), "{}", output.content);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_directory_that_matches_is_not_reported_as_a_file() {
    let dir = temp_dir("dirs");
    std::fs::create_dir_all(dir.join("build")).unwrap();
    std::fs::write(dir.join("keep"), "").unwrap();

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "*" }))
        .await
        .unwrap();

    assert!(output.content.contains("keep"), "{}", output.content);
    assert!(!output.content.contains("build"), "{}", output.content);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn no_match_reports_plainly_rather_than_erroring() {
    let dir = temp_dir("empty");

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "**/*.xyz" }))
        .await
        .unwrap();

    assert!(!output.is_error);
    assert!(
        output.content.contains("No files match"),
        "{}",
        output.content
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_path_argument_narrows_the_search_root() {
    let dir = temp_dir("root");
    std::fs::create_dir_all(dir.join("one")).unwrap();
    std::fs::create_dir_all(dir.join("two")).unwrap();
    std::fs::write(dir.join("one").join("hit.rs"), "").unwrap();
    std::fs::write(dir.join("two").join("miss.rs"), "").unwrap();

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "*.rs", "path": "one" }))
        .await
        .unwrap();

    assert!(output.content.contains("hit.rs"), "{}", output.content);
    assert!(!output.content.contains("miss.rs"), "{}", output.content);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_bad_pattern_is_a_tool_error_not_a_hard_failure() {
    let dir = temp_dir("bad");

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "a[" }))
        .await
        .expect("a bad pattern must not end the turn");

    assert!(output.is_error);
    assert!(
        output.content.contains("Invalid glob"),
        "{}",
        output.content
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_tool_is_named_glob() {
    let dir = temp_dir("name");
    assert_eq!(tool_in(&dir).name(), "glob");
    std::fs::remove_dir_all(&dir).ok();
}
