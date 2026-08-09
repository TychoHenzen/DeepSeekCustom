//! Unit tests for `deepseek_custom::tools::grep` (`src/tools/grep.rs`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use deepseek_custom::tools::Tool;
use deepseek_custom::tools::grep::GrepTool;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-grep-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn tool_in(dir: &std::path::Path) -> GrepTool {
    GrepTool::new(Arc::new(Mutex::new(dir.to_path_buf())))
}

fn sample_tree(tag: &str) -> PathBuf {
    let dir = temp_dir(tag);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src").join("a.rs"), "fn hit() {}\nfn other() {}\n").unwrap();
    std::fs::write(dir.join("src").join("b.rs"), "fn miss() {}\n").unwrap();
    std::fs::write(dir.join("notes.md"), "hit in markdown\n").unwrap();
    dir
}

#[tokio::test]
async fn files_with_matches_is_the_default_output_mode() {
    let dir = sample_tree("default");

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "hit" }))
        .await
        .unwrap();

    assert!(!output.is_error, "{}", output.content);
    assert!(output.content.contains("a.rs"), "{}", output.content);
    assert!(output.content.contains("notes.md"), "{}", output.content);
    assert!(!output.content.contains("b.rs"), "{}", output.content);
    // A path alone, with no line number appended.
    assert!(!output.content.contains(":1:"), "{}", output.content);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn content_mode_reports_the_line_number_and_the_line() {
    let dir = sample_tree("content");

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "fn hit", "output_mode": "content" }))
        .await
        .unwrap();

    assert!(
        output.content.contains(":1:fn hit() {}"),
        "{}",
        output.content
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn count_mode_reports_one_line_per_file_with_a_total() {
    let dir = temp_dir("count");
    std::fs::write(dir.join("a.txt"), "x\nx\ny\n").unwrap();

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "x", "output_mode": "count" }))
        .await
        .unwrap();

    assert!(output.content.ends_with(":2"), "{}", output.content);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_glob_filter_narrows_which_files_are_read() {
    let dir = sample_tree("filter");

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "hit", "glob": "*.md" }))
        .await
        .unwrap();

    assert!(output.content.contains("notes.md"), "{}", output.content);
    assert!(!output.content.contains("a.rs"), "{}", output.content);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn case_insensitive_matches_a_different_case() {
    let dir = temp_dir("case");
    std::fs::write(dir.join("a.txt"), "Needle\n").unwrap();

    let sensitive = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "needle" }))
        .await
        .unwrap();
    assert!(
        sensitive.content.contains("No matches"),
        "{}",
        sensitive.content
    );

    let insensitive = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "needle", "case_insensitive": true }))
        .await
        .unwrap();
    assert!(
        insensitive.content.contains("a.txt"),
        "{}",
        insensitive.content
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn the_target_directory_is_never_searched() {
    // A search that walks `target/` reads millions of lines of build
    // output. The skip list is what keeps one lookup from taking minutes.
    let dir = temp_dir("skip");
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(dir.join("target").join("built.txt"), "needle\n").unwrap();
    std::fs::write(dir.join("kept.txt"), "needle\n").unwrap();

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "needle" }))
        .await
        .unwrap();

    assert!(output.content.contains("kept.txt"), "{}", output.content);
    assert!(!output.content.contains("built.txt"), "{}", output.content);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn head_limit_caps_the_result_and_says_so() {
    let dir = temp_dir("limit");
    std::fs::write(dir.join("a.txt"), "x\nx\nx\nx\n").unwrap();

    let output = tool_in(&dir)
        .execute(serde_json::json!({
            "pattern": "x",
            "output_mode": "content",
            "head_limit": 2
        }))
        .await
        .unwrap();

    assert_eq!(
        output.content.lines().filter(|l| l.contains(":x")).count(),
        2
    );
    assert!(output.content.contains("capped at 2"), "{}", output.content);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_bad_regex_is_a_tool_error_not_a_hard_failure() {
    let dir = temp_dir("bad-regex");

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "a(" }))
        .await
        .expect("a bad regex must not end the turn");

    assert!(output.is_error);
    assert!(
        output.content.contains("Invalid regex"),
        "{}",
        output.content
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn an_unknown_output_mode_is_reported_rather_than_defaulted() {
    let dir = temp_dir("bad-mode");

    let output = tool_in(&dir)
        .execute(serde_json::json!({ "pattern": "x", "output_mode": "lines" }))
        .await
        .unwrap();

    assert!(output.is_error);
    assert!(
        output.content.contains("Unknown output_mode"),
        "{}",
        output.content
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_tool_is_named_grep() {
    let dir = temp_dir("name");
    assert_eq!(tool_in(&dir).name(), "grep");
    std::fs::remove_dir_all(&dir).ok();
}
