//! Unit tests for `deepseek_custom::tools::edit` (`src/tools/edit.rs`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use deepseek_custom::tools::Tool;
use deepseek_custom::tools::edit::{EditTool, apply_edit};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-edit-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn tool_in(dir: &std::path::Path) -> EditTool {
    EditTool::new(Arc::new(Mutex::new(dir.to_path_buf())))
}

#[test]
fn replaces_a_single_occurrence() {
    let (out, count) = apply_edit("let x = 1;\nlet y = 2;\n", "x = 1", "x = 42", false).unwrap();
    assert_eq!(out, "let x = 42;\nlet y = 2;\n");
    assert_eq!(count, 1);
}

#[test]
fn refuses_an_ambiguous_match_unless_replace_all_is_set() {
    let source = "a\na\n";
    let err = apply_edit(source, "a", "b", false).unwrap_err();
    assert!(err.contains("appears 2 times"), "{err}");

    let (out, count) = apply_edit(source, "a", "b", true).unwrap();
    assert_eq!(out, "b\nb\n");
    assert_eq!(count, 2);
}

#[test]
fn a_missing_match_is_an_error_not_a_silent_no_op() {
    let err = apply_edit("hello\n", "goodbye", "hi", false).unwrap_err();
    assert!(err.contains("not found"), "{err}");
}

#[test]
fn an_edit_that_changes_nothing_is_an_error() {
    let err = apply_edit("hello\n", "hello", "hello", false).unwrap_err();
    assert!(err.contains("identical"), "{err}");
}

#[test]
fn an_empty_old_string_is_an_error() {
    let err = apply_edit("hello\n", "", "hi", false).unwrap_err();
    assert!(err.contains("empty"), "{err}");
}

#[test]
fn replace_all_counts_every_replacement_it_made() {
    let (_, count) = apply_edit("x x x", "x", "y", true).unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
async fn execute_writes_the_change_to_disk() {
    let dir = temp_dir("write");
    let path = dir.join("sample.txt");
    std::fs::write(&path, "before\nkeep\n").unwrap();

    let tool = tool_in(&dir);
    let output = tool
        .execute(serde_json::json!({
            "file_path": "sample.txt",
            "old_string": "before",
            "new_string": "after"
        }))
        .await
        .unwrap();

    assert!(!output.is_error, "{}", output.content);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\nkeep\n");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_relative_path_resolves_against_the_working_directory() {
    let dir = temp_dir("relative");
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    let path = dir.join("nested").join("sample.txt");
    std::fs::write(&path, "one\n").unwrap();

    let tool = tool_in(&dir);
    let output = tool
        .execute(serde_json::json!({
            "file_path": "nested/sample.txt",
            "old_string": "one",
            "new_string": "two"
        }))
        .await
        .unwrap();

    assert!(!output.is_error, "{}", output.content);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\n");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_missing_file_is_a_tool_error_not_a_hard_failure() {
    let dir = temp_dir("missing");
    let tool = tool_in(&dir);

    let output = tool
        .execute(serde_json::json!({
            "file_path": "nope.txt",
            "old_string": "a",
            "new_string": "b"
        }))
        .await
        .expect("a missing file must not end the turn");

    assert!(output.is_error);
    assert!(
        output.content.contains("Failed to read"),
        "{}",
        output.content
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn an_ambiguous_match_leaves_the_file_untouched() {
    let dir = temp_dir("ambiguous");
    let path = dir.join("sample.txt");
    std::fs::write(&path, "dup\ndup\n").unwrap();

    let tool = tool_in(&dir);
    let output = tool
        .execute(serde_json::json!({
            "file_path": "sample.txt",
            "old_string": "dup",
            "new_string": "one"
        }))
        .await
        .unwrap();

    assert!(output.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "dup\ndup\n");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_tool_is_named_edit() {
    let dir = temp_dir("name");
    assert_eq!(tool_in(&dir).name(), "edit");
    std::fs::remove_dir_all(&dir).ok();
}
