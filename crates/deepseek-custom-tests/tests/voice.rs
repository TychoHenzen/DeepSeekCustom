//! Unit tests for `deepseek_custom::voice` (`src/voice/mod.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use deepseek_custom::voice::{
    KOKORO_MODEL_FILENAME, KOKORO_VOICES_DIRNAME, MAX_SPOKEN_CHARS, WHISPER_MODEL_FILENAME,
    filter_for_speech, resolve_kokoro_paths_in, resolve_whisper_model_path_in,
};

/// A directory unique to this test run under the OS temp dir, cleaned
/// up by the returned guard on drop. Mirrors the helper in
/// `voice::cuda_dlls`'s tests.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("voice_mod_test_{label}_{nanos}_{n}"));
        fs::create_dir_all(&root).expect("create temp tree root");
        Self { root }
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn whisper_resolves_configured_path_first() {
    let tree = TempTree::new("whisper_configured");
    let configured = tree.root.join("custom.bin");
    fs::write(&configured, b"x").unwrap();
    // Also place a project-root model, so a correct resolver proves it
    // prefers the configured path over this.
    fs::create_dir_all(tree.root.join("models")).unwrap();
    fs::write(tree.root.join("models").join(WHISPER_MODEL_FILENAME), b"x").unwrap();

    let found = resolve_whisper_model_path_in(Some(configured.to_str().unwrap()), &tree.root, None);
    assert_eq!(found, Some(configured));
}

#[test]
fn whisper_resolves_project_root_models_dir_when_unconfigured() {
    let tree = TempTree::new("whisper_project");
    fs::create_dir_all(tree.root.join("models")).unwrap();
    let expected = tree.root.join("models").join(WHISPER_MODEL_FILENAME);
    fs::write(&expected, b"x").unwrap();

    let found = resolve_whisper_model_path_in(None, &tree.root, None);
    assert_eq!(found, Some(expected));
}

#[test]
fn whisper_falls_back_to_local_app_data_when_project_root_lacks_it() {
    let tree = TempTree::new("whisper_lad");
    let local_app_data = tree.root.join("lad");
    let expected = local_app_data
        .join("DeepSeekCustom")
        .join("models")
        .join(WHISPER_MODEL_FILENAME);
    fs::create_dir_all(expected.parent().unwrap()).unwrap();
    fs::write(&expected, b"x").unwrap();

    let found = resolve_whisper_model_path_in(None, &tree.root, Some(&local_app_data));
    assert_eq!(found, Some(expected));
}

#[test]
fn whisper_returns_none_when_nothing_found() {
    let tree = TempTree::new("whisper_missing");
    let local_app_data = tree.root.join("lad");

    let found = resolve_whisper_model_path_in(None, &tree.root, Some(&local_app_data));
    assert!(found.is_none());
}

#[test]
fn kokoro_resolves_configured_paths_first() {
    let tree = TempTree::new("kokoro_configured");
    let model = tree.root.join("custom_model.onnx");
    let voices = tree.root.join("custom_voices");
    fs::write(&model, b"x").unwrap();
    fs::create_dir_all(&voices).unwrap();

    let found = resolve_kokoro_paths_in(
        Some(model.to_str().unwrap()),
        Some(voices.to_str().unwrap()),
        &tree.root,
        None,
    );
    assert_eq!(found, Some((model, voices)));
}

#[test]
fn kokoro_resolves_project_root_when_unconfigured() {
    let tree = TempTree::new("kokoro_project");
    fs::create_dir_all(tree.root.join("models")).unwrap();
    let model = tree.root.join("models").join(KOKORO_MODEL_FILENAME);
    fs::write(&model, b"x").unwrap();
    let voices = tree.root.join(KOKORO_VOICES_DIRNAME);
    fs::create_dir_all(&voices).unwrap();

    let found = resolve_kokoro_paths_in(None, None, &tree.root, None);
    assert_eq!(found, Some((model, voices)));
}

#[test]
fn kokoro_voices_dir_is_sibling_of_models_not_inside_it() {
    let tree = TempTree::new("kokoro_sibling");
    fs::create_dir_all(tree.root.join("models")).unwrap();
    let model = tree.root.join("models").join(KOKORO_MODEL_FILENAME);
    fs::write(&model, b"x").unwrap();
    // A voices dir nested under models/ must NOT satisfy resolution.
    fs::create_dir_all(tree.root.join("models").join(KOKORO_VOICES_DIRNAME)).unwrap();

    let found = resolve_kokoro_paths_in(None, None, &tree.root, None);
    assert!(found.is_none());
}

#[test]
fn kokoro_falls_back_to_local_app_data_when_project_root_lacks_it() {
    let tree = TempTree::new("kokoro_lad");
    let local_app_data = tree.root.join("lad");
    let model = local_app_data
        .join("DeepSeekCustom")
        .join("models")
        .join(KOKORO_MODEL_FILENAME);
    let voices = local_app_data
        .join("DeepSeekCustom")
        .join(KOKORO_VOICES_DIRNAME);
    fs::create_dir_all(model.parent().unwrap()).unwrap();
    fs::write(&model, b"x").unwrap();
    fs::create_dir_all(&voices).unwrap();

    let found = resolve_kokoro_paths_in(None, None, &tree.root, Some(&local_app_data));
    assert_eq!(found, Some((model, voices)));
}

#[test]
fn kokoro_returns_none_when_model_missing_even_if_voices_present() {
    let tree = TempTree::new("kokoro_no_model");
    let voices = tree.root.join(KOKORO_VOICES_DIRNAME);
    fs::create_dir_all(&voices).unwrap();

    let found = resolve_kokoro_paths_in(None, None, &tree.root, None);
    assert!(found.is_none());
}

#[test]
fn kokoro_returns_none_when_voices_missing_even_if_model_present() {
    let tree = TempTree::new("kokoro_no_voices");
    fs::create_dir_all(tree.root.join("models")).unwrap();
    fs::write(tree.root.join("models").join(KOKORO_MODEL_FILENAME), b"x").unwrap();

    let found = resolve_kokoro_paths_in(None, None, &tree.root, None);
    assert!(found.is_none());
}

#[test]
fn kokoro_returns_none_when_nothing_found() {
    let tree = TempTree::new("kokoro_missing");
    let local_app_data = tree.root.join("lad");

    let found = resolve_kokoro_paths_in(None, None, &tree.root, Some(&local_app_data));
    assert!(found.is_none());
}

#[test]
fn missing_whisper_and_missing_kokoro_degrade_independently() {
    let tree = TempTree::new("independent");
    // Only the whisper model exists. Kokoro must still resolve to
    // None without touching or invalidating the whisper result, and
    // vice versa.
    fs::create_dir_all(tree.root.join("models")).unwrap();
    fs::write(tree.root.join("models").join(WHISPER_MODEL_FILENAME), b"x").unwrap();

    let whisper = resolve_whisper_model_path_in(None, &tree.root, None);
    let kokoro = resolve_kokoro_paths_in(None, None, &tree.root, None);

    assert!(whisper.is_some());
    assert!(kokoro.is_none());
}

#[test]
fn filter_for_speech_passes_plain_text_through() {
    let out = filter_for_speech("hello there, how are you today?");
    assert_eq!(out, "hello there, how are you today?");
}

#[test]
fn filter_for_speech_drops_fenced_code_blocks() {
    let text = "Here is the fix:\n```rust\nfn main() {\n    panic!();\n}\n```\nThat should work.";
    let out = filter_for_speech(text);
    assert_eq!(out, "Here is the fix: That should work.");
}

#[test]
fn filter_for_speech_drops_unterminated_fence_to_the_end() {
    let text = "Before.\n```rust\nfn main() {}\n";
    let out = filter_for_speech(text);
    assert_eq!(out, "Before.");
}

#[test]
fn filter_for_speech_drops_inline_backtick_spans() {
    let out = filter_for_speech("Run `cargo test` to check it.");
    assert_eq!(out, "Run to check it.");
}

#[test]
fn filter_for_speech_drops_bare_urls() {
    let out = filter_for_speech("See https://example.com/docs for more.");
    assert_eq!(out, "See for more.");
}

#[test]
fn filter_for_speech_keeps_link_label_and_drops_url() {
    let out = filter_for_speech("Read the [project docs](https://example.com/docs) first.");
    assert_eq!(out, "Read the project docs first.");
}

#[test]
fn filter_for_speech_strips_headings() {
    let out = filter_for_speech("## Summary\nEverything passed.");
    assert_eq!(out, "Summary Everything passed.");
}

#[test]
fn filter_for_speech_strips_bullet_lists() {
    let text = "Steps:\n- first item\n- second item\n* third item";
    let out = filter_for_speech(text);
    assert_eq!(out, "Steps: first item second item third item");
}

#[test]
fn filter_for_speech_strips_numbered_lists() {
    let text = "1. do this\n2. do that";
    let out = filter_for_speech(text);
    assert_eq!(out, "do this do that");
}

#[test]
fn filter_for_speech_strips_emphasis_and_blockquote_markup() {
    let out = filter_for_speech("> **Note:** this is _important_.");
    assert_eq!(out, "Note: this is important.");
}

#[test]
fn filter_for_speech_drops_horizontal_rule_lines() {
    let text = "Before.\n---\nAfter.";
    let out = filter_for_speech(text);
    assert_eq!(out, "Before. After.");
}

#[test]
fn filter_for_speech_collapses_whitespace() {
    let out = filter_for_speech("too    many\n\n\nspaces   here");
    assert_eq!(out, "too many spaces here");
}

#[test]
fn filter_for_speech_caps_length_on_a_word_boundary() {
    let long = "word ".repeat(200);
    let out = filter_for_speech(&long);
    assert!(out.chars().count() <= MAX_SPOKEN_CHARS + 3);
    assert!(out.ends_with("..."));
    assert!(
        !out.trim_end_matches("...").ends_with(' '),
        "the cut must land on a word boundary, not mid-word or leave a trailing space before the ellipsis"
    );
}

#[test]
fn filter_for_speech_does_not_truncate_short_text() {
    let out = filter_for_speech("short reply");
    assert_eq!(out, "short reply");
    assert!(!out.ends_with("..."));
}

#[test]
fn filter_for_speech_handles_everything_together() {
    let text = "# Fix applied\n\
         Run `cargo test` to verify, see [the docs](https://example.com) too.\n\
         ```rust\n\
         fn broken() { panic!(); }\n\
         ```\n\
         - it now passes\n\
         - no more panics";
    let out = filter_for_speech(text);
    assert!(!out.contains('`'));
    assert!(!out.contains("https://"));
    assert!(!out.contains('#'));
    assert!(!out.contains('-'));
    assert!(out.contains("Fix applied"));
    assert!(out.contains("the docs"));
    assert!(out.contains("it now passes"));
    assert!(
        !out.contains("fn broken"),
        "the fenced code block's content must not survive, only the surrounding prose"
    );
}
