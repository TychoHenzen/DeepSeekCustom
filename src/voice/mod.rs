//! Voice subsystem: local speech to text, Kokoro text to speech, and
//! trigger handling (push-to-talk / wake-word), so the harness can act as
//! a hands-free personal assistant.
//!
//! Planned layout (files land as later steps implement them):
//! - `tts` - Kokoro text to speech synthesis, plus the threaded `TtsHandle`.
//! - `playback` - cpal audio output used by `tts`.
//! - `capture` - microphone audio capture via `cpal`.
//! - `vad` - voice activity detection over captured audio.
//! - `stt` - local speech to text via `whisper-rs`.
//! - `service` - ties capture, VAD, and STT into one pipeline for the agent.
//! - `wake` - wake-word detection, switchable against push-to-talk.
//!
//! This file also resolves where the two local models live on disk. See
//! [`resolve_whisper_model_path`] and [`resolve_kokoro_paths`].

pub mod capture;
mod cuda_dlls;
pub mod playback;
pub mod service;
pub mod stt;
pub mod tts;
pub mod vad;
pub mod wake;

use std::path::{Path, PathBuf};

use tracing::warn;

/// Filename of the whisper GGML speech-to-text model under `models/`.
const WHISPER_MODEL_FILENAME: &str = "ggml-base.en.bin";

/// Filename of the default (fp32) Kokoro ONNX text-to-speech model under
/// `models/`. Fastest and best-sounding of the variants tried on this
/// machine. Never default to `model_quantized.onnx` instead.
const KOKORO_MODEL_FILENAME: &str = "model.onnx";

/// Directory name holding Kokoro `<voice_id>.bin` voice packs. Sits at the
/// project root, a sibling of `models/`, not inside it.
const KOKORO_VOICES_DIRNAME: &str = "voices";

const WHISPER_DOWNLOAD_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
const KOKORO_DOWNLOAD_URL: &str = "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX";

/// Resolve the whisper GGML speech-to-text model path.
///
/// Tries, in order: `configured`, then `models/ggml-base.en.bin` under
/// `project_root`, then `%LOCALAPPDATA%/DeepSeekCustom/models/`. If none
/// exist, logs a warning naming every path tried plus the download URL and
/// returns `None` instead of failing. A missing whisper model disables
/// speech to text only. It must not stop speech output.
pub fn resolve_whisper_model_path(
    configured: Option<&str>,
    project_root: &Path,
) -> Option<PathBuf> {
    resolve_whisper_model_path_in(configured, project_root, local_app_data_dir().as_deref())
}

fn resolve_whisper_model_path_in(
    configured: Option<&str>,
    project_root: &Path,
    local_app_data: Option<&Path>,
) -> Option<PathBuf> {
    let candidates = model_candidates(
        configured,
        project_root,
        local_app_data,
        WHISPER_MODEL_FILENAME,
    );
    let found = first_existing(&candidates);
    if found.is_none() {
        warn_missing(
            "whisper speech-to-text model",
            &candidates,
            WHISPER_DOWNLOAD_URL,
        );
    }
    found
}

/// Resolve the Kokoro text-to-speech model file and voice packs directory.
///
/// Each half is resolved independently, in the same order as
/// [`resolve_whisper_model_path`]: the configured path, then the project
/// root (`models/model.onnx` and `voices/`), then
/// `%LOCALAPPDATA%/DeepSeekCustom/`. Both must resolve for text to speech
/// to come up. If either is missing, a warning names every path tried for
/// that half plus the download URL, and `None` is returned so speech
/// recognition stays unaffected.
pub fn resolve_kokoro_paths(
    model_configured: Option<&str>,
    voices_configured: Option<&str>,
    project_root: &Path,
) -> Option<(PathBuf, PathBuf)> {
    resolve_kokoro_paths_in(
        model_configured,
        voices_configured,
        project_root,
        local_app_data_dir().as_deref(),
    )
}

fn resolve_kokoro_paths_in(
    model_configured: Option<&str>,
    voices_configured: Option<&str>,
    project_root: &Path,
    local_app_data: Option<&Path>,
) -> Option<(PathBuf, PathBuf)> {
    let model_paths = model_candidates(
        model_configured,
        project_root,
        local_app_data,
        KOKORO_MODEL_FILENAME,
    );
    let voices_paths = voices_candidates(voices_configured, project_root, local_app_data);

    let model = first_existing(&model_paths);
    if model.is_none() {
        warn_missing(
            "Kokoro text-to-speech model",
            &model_paths,
            KOKORO_DOWNLOAD_URL,
        );
    }
    let voices = first_existing(&voices_paths);
    if voices.is_none() {
        warn_missing(
            "Kokoro voice packs directory",
            &voices_paths,
            KOKORO_DOWNLOAD_URL,
        );
    }

    Some((model?, voices?))
}

/// Build the ordered candidate list for a model file under `models/`: the
/// configured path, `models/<filename>` under the project root, then
/// `%LOCALAPPDATA%/DeepSeekCustom/models/<filename>`.
fn model_candidates(
    configured: Option<&str>,
    project_root: &Path,
    local_app_data: Option<&Path>,
    filename: &str,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = configured {
        candidates.push(PathBuf::from(configured));
    }
    candidates.push(project_root.join("models").join(filename));
    if let Some(dir) = local_app_data {
        candidates.push(dir.join("DeepSeekCustom").join("models").join(filename));
    }
    candidates
}

/// Build the ordered candidate list for the Kokoro voices directory: the
/// configured path, `voices/` under the project root (a sibling of
/// `models/`, not inside it), then `%LOCALAPPDATA%/DeepSeekCustom/voices/`.
fn voices_candidates(
    configured: Option<&str>,
    project_root: &Path,
    local_app_data: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = configured {
        candidates.push(PathBuf::from(configured));
    }
    candidates.push(project_root.join(KOKORO_VOICES_DIRNAME));
    if let Some(dir) = local_app_data {
        candidates.push(dir.join("DeepSeekCustom").join(KOKORO_VOICES_DIRNAME));
    }
    candidates
}

/// The first candidate that exists on disk, if any.
fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.exists()).cloned()
}

/// Log a warning naming every path tried for `what`, plus where to
/// download it. Called once resolution has exhausted every candidate.
fn warn_missing(what: &str, candidates: &[PathBuf], download_url: &str) {
    let tried: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
    warn!(
        "{what} not found, tried: {}. Download it from {download_url}. That half of the voice subsystem stays disabled.",
        tried.join(", ")
    );
}

/// `%LOCALAPPDATA%`, if set. Absent on non-Windows platforms and in some
/// minimal environments.
fn local_app_data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

/// Maximum length, in characters, of text handed to the speaker.
/// The cut runs after filtering, not before. That way it lands on real
/// content, not on markdown noise left over from an earlier pass.
const MAX_SPOKEN_CHARS: usize = 500;

/// Turn model output into plain text worth speaking.
/// Drops fenced code blocks, inline code spans, and URLs. Strips markdown
/// markup, collapses whitespace, and caps the length. This is the only
/// path from an agent reply to `VoiceCommand::Speak`. A code block should
/// never get read aloud character by character.
pub fn filter_for_speech(text: &str) -> String {
    let text = strip_fenced_code_blocks(text);
    let text = strip_inline_code(&text);
    let text = strip_markdown_links(&text);
    // Markup stripping reads line structure: headings, bullets,
    // blockquotes. It must run before anything that turns newlines into
    // spaces. Bare-URL stripping does that as a side effect. It runs
    // after, not before.
    let text = strip_markdown_markup(&text);
    let text = strip_bare_urls(&text);
    let text = collapse_whitespace(&text);
    truncate_for_speech(&text, MAX_SPOKEN_CHARS)
}

/// Drop every fenced code block (` ``` ` or `~~~` delimited), fence lines
/// included. An unterminated fence drops everything from its opening line
/// to the end of the text, rather than leaking the rest of the reply.
fn strip_fenced_code_blocks(text: &str) -> String {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push(line);
        }
    }
    out.join("\n")
}

/// Drop inline backtick spans, backticks and content both. An unmatched
/// trailing backtick has nothing to pair with, so its text is kept and
/// only the stray backtick itself is dropped.
fn strip_inline_code(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('`') {
            Some(end) => rest = &after[end + 1..],
            None => {
                result.push_str(after);
                rest = "";
            }
        }
    }
    result.push_str(rest);
    result
}

/// Replace `[label](url)` with just `label`. A bracket that is not
/// actually a markdown link (no following `(url)`) is left as plain text,
/// brackets included.
fn strip_markdown_links(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        result.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let Some(label_end) = rest.find(']') else {
            result.push('[');
            result.push_str(rest);
            return result;
        };
        let label = &rest[..label_end];
        let after_label = &rest[label_end + 1..];
        let target = after_label
            .strip_prefix('(')
            .and_then(|s| s.find(')').map(|end| (s, end)));
        match target {
            Some((after_paren, url_end)) => {
                result.push_str(label);
                rest = &after_paren[url_end + 1..];
            }
            None => {
                result.push('[');
                result.push_str(label);
                result.push(']');
                rest = after_label;
            }
        }
    }
    result.push_str(rest);
    result
}

/// Drop bare `http(s)://` and `www.` URLs entirely, word by word. Also
/// collapses inter-word whitespace to single spaces as a side effect,
/// which is harmless since [`collapse_whitespace`] runs again afterward.
fn strip_bare_urls(text: &str) -> String {
    text.split_whitespace()
        .filter(|word| !looks_like_url(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `word` looks like a URL once common leading/trailing
/// punctuation around it (parens, quotes, sentence punctuation) is
/// ignored.
fn looks_like_url(word: &str) -> bool {
    let trimmed = word.trim_matches(|c: char| "([{\"'.,!?;:)]}".contains(c));
    trimmed.starts_with("http://") || trimmed.starts_with("https://") || trimmed.starts_with("www.")
}

/// Strip markdown markup characters line by line: heading `#` runs,
/// blockquote `>`, list bullets (`-`, `*`, `+`, or `N.`/`N)`), horizontal
/// rules, table pipes, and emphasis markers (`*`, `_`, `~`). A pure
/// horizontal-rule line is dropped outright rather than emitted empty.
fn strip_markdown_markup(text: &str) -> String {
    text.lines()
        .filter_map(strip_line_markup)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strip markdown markup from a single line. See [`strip_markdown_markup`].
fn strip_line_markup(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if is_horizontal_rule(trimmed) {
        return None;
    }
    let without_heading = trimmed.trim_start_matches('#').trim_start();
    let without_quote = without_heading.trim_start_matches('>').trim_start();
    let without_bullet = strip_leading_bullet(without_quote);
    let cleaned: String = without_bullet
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '~' | '#' | '|'))
        .collect();
    Some(cleaned)
}

/// True for a line that is only `-`, `*`, or `_` characters (and
/// whitespace) repeated three or more times: a markdown horizontal rule.
fn is_horizontal_rule(trimmed: &str) -> bool {
    let stripped: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    stripped.len() >= 3
        && (stripped.chars().all(|c| c == '-')
            || stripped.chars().all(|c| c == '*')
            || stripped.chars().all(|c| c == '_'))
}

/// Strip a leading list bullet: `-`, `*`, `+` followed by a space, or a
/// numbered marker like `1.` or `2)` followed by a space. Anything else is
/// returned unchanged.
fn strip_leading_bullet(text: &str) -> &str {
    let plain_bullet = text
        .strip_prefix("- ")
        .or_else(|| text.strip_prefix("* "))
        .or_else(|| text.strip_prefix("+ "));
    match plain_bullet {
        Some(rest) => rest,
        None => strip_numbered_bullet(text),
    }
}

/// Strip a leading `N.` or `N)` numbered list marker followed by a space.
fn strip_numbered_bullet(text: &str) -> &str {
    let digits_end = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digits_end == 0 {
        return text;
    }
    let after_digits = &text[digits_end..];
    after_digits
        .strip_prefix(". ")
        .or_else(|| after_digits.strip_prefix(") "))
        .unwrap_or(text)
}

/// Collapse runs of whitespace (including newlines) to single spaces, and
/// trim the ends.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cap `text` at `max_chars` characters, cutting on a word boundary and
/// appending "..." when a cut happened. Character-counted, not
/// byte-counted, so the cut never lands inside a multi-byte character.
fn truncate_for_speech(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    let cut = match truncated.rfind(' ') {
        Some(idx) if idx > 0 => &truncated[..idx],
        _ => truncated.as_str(),
    };
    format!("{cut}...")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

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

        let found =
            resolve_whisper_model_path_in(Some(configured.to_str().unwrap()), &tree.root, None);
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
        let text =
            "Here is the fix:\n```rust\nfn main() {\n    panic!();\n}\n```\nThat should work.";
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
}
