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
// `pub`, not the bare private `mod` this used to be: the moved
// `tests/voice_cuda_dlls.rs` in the external test crate needs to name
// `deepseek_custom::voice::cuda_dlls` directly, and a private module
// segment blocks that regardless of how its own items are marked.
pub mod cuda_dlls;
pub mod playback;
pub mod service;
pub mod stt;
pub mod tts;
pub mod vad;
pub mod wake;

use std::path::{Path, PathBuf};

use tracing::warn;

/// Filename of the whisper GGML speech-to-text model under `models/`.
/// `pub`: the moved resolver tests in the external test crate build this
/// exact path against a temp directory tree.
pub const WHISPER_MODEL_FILENAME: &str = "ggml-base.en.bin";

/// Filename of the default (fp32) Kokoro ONNX text-to-speech model under
/// `models/`. Fastest and best-sounding of the variants tried on this
/// machine. Never default to `model_quantized.onnx` instead.
/// `pub`: same reason as [`WHISPER_MODEL_FILENAME`].
pub const KOKORO_MODEL_FILENAME: &str = "model.onnx";

/// Directory name holding Kokoro `<voice_id>.bin` voice packs. Sits at the
/// project root, a sibling of `models/`, not inside it.
/// `pub`: same reason as [`WHISPER_MODEL_FILENAME`].
pub const KOKORO_VOICES_DIRNAME: &str = "voices";

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

/// `pub`: the moved resolver tests drive resolution through this seam
/// directly, against a temp `local_app_data` path, rather than through
/// [`resolve_whisper_model_path`], which always reads the real
/// `%LOCALAPPDATA%` and would make the fallback tier untestable.
pub fn resolve_whisper_model_path_in(
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

/// `pub`: same reason as [`resolve_whisper_model_path_in`].
pub fn resolve_kokoro_paths_in(
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
/// `pub`: the moved truncation test checks the cap against this constant
/// rather than hardcoding 500 a second time.
pub const MAX_SPOKEN_CHARS: usize = 500;

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
