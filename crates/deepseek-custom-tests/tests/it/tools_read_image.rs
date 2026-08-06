//! Unit tests for `deepseek_custom::tools::read_image` (`src/tools/read_image.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::{Arc, Mutex};

use base64::Engine;

use deepseek_custom::tools::Tool;
use deepseek_custom::tools::read_image::ReadImageTool;

/// A real 1x1 PNG, not a placeholder string. Same fixture
/// `docs/notes/image-support.md` used to confirm image handling
/// against the live APIs.
const TEST_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

fn test_png_bytes() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(TEST_PNG_BASE64)
        .expect("valid base64 fixture")
}

fn dir_arc(p: std::path::PathBuf) -> Arc<Mutex<std::path::PathBuf>> {
    Arc::new(Mutex::new(p))
}

fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("dsc-read-image-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn reads_a_real_png_from_a_temp_directory() {
    let dir = unique_temp_dir("real-png");
    let file = dir.join("pixel.png");
    std::fs::write(&file, test_png_bytes()).unwrap();

    let tool = ReadImageTool::new(dir_arc(std::env::current_dir().unwrap()));
    let output = tool
        .execute(serde_json::json!({"file_path": file.to_string_lossy()}))
        .await
        .expect("execute");

    assert!(!output.is_error, "unexpected error: {}", output.content);
    let image = output.image.expect("image attachment present");
    assert_eq!(image.media_type, "image/png");
    assert_eq!(image.data, TEST_PNG_BASE64);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn relative_path_resolves_against_working_dir() {
    let dir_a = unique_temp_dir("a");
    let dir_b = unique_temp_dir("b");
    std::fs::write(dir_a.join("pixel.png"), test_png_bytes()).unwrap();
    std::fs::write(dir_b.join("pixel.png"), test_png_bytes()).unwrap();

    let shared = dir_arc(dir_a.clone());
    let tool = ReadImageTool::new(shared.clone());

    let first = tool
        .execute(serde_json::json!({"file_path": "pixel.png"}))
        .await
        .expect("execute");
    assert!(!first.is_error);
    assert!(first.content.contains(&dir_a.display().to_string()));

    *shared.lock().unwrap() = dir_b.clone();

    let second = tool
        .execute(serde_json::json!({"file_path": "pixel.png"}))
        .await
        .expect("execute");
    assert!(!second.is_error);
    assert!(second.content.contains(&dir_b.display().to_string()));

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[tokio::test]
async fn missing_path_is_a_tool_error_naming_the_path() {
    let dir = unique_temp_dir("missing");
    let missing = dir.join("does-not-exist.png");

    let tool = ReadImageTool::new(dir_arc(std::env::current_dir().unwrap()));
    let output = tool
        .execute(serde_json::json!({"file_path": missing.to_string_lossy()}))
        .await
        .expect("execute");

    assert!(output.is_error);
    assert!(output.image.is_none());
    assert!(output.content.contains(&missing.display().to_string()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_directory_is_a_tool_error_naming_the_path() {
    let dir = unique_temp_dir("a-directory");

    let tool = ReadImageTool::new(dir_arc(std::env::current_dir().unwrap()));
    let output = tool
        .execute(serde_json::json!({"file_path": dir.to_string_lossy()}))
        .await
        .expect("execute");

    assert!(output.is_error);
    assert!(output.image.is_none());
    assert!(output.content.contains(&dir.display().to_string()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_non_image_file_is_a_tool_error_naming_the_path() {
    let dir = unique_temp_dir("non-image");
    let file = dir.join("notes.txt");
    std::fs::write(&file, "just plain text, not an image").unwrap();

    let tool = ReadImageTool::new(dir_arc(std::env::current_dir().unwrap()));
    let output = tool
        .execute(serde_json::json!({"file_path": file.to_string_lossy()}))
        .await
        .expect("execute");

    assert!(output.is_error);
    assert!(output.image.is_none());
    assert!(output.content.contains(&file.display().to_string()));

    let _ = std::fs::remove_dir_all(&dir);
}
