//! CLI argument assembly, binary resolution, and wire-format helpers for
//! `claude -p`. Pure functions with no dependency on `ClaudeCliDriver`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::api::types::ImageAttachment;
use crate::effort::Effort;

pub(super) const CLAUDE_CLI_PATH_KEY: &str = "CLAUDE_CLI_PATH";

/// How often `send` checks the interrupt flag while a turn is in flight.
/// Short enough that Escape still feels immediate.
pub(super) const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Token budget placeholder. Claude Code manages its own context
/// compaction, so this value is never read by anything. It only exists so
/// `ClaudeCliDriver` can hand the GUI a real `Arc<AtomicUsize>` for the
/// context budget slider to write into.
pub(super) const UNUSED_CONTEXT_BUDGET: usize = 100_000;

/// Resolve the path to the `claude` binary, since it may not be on the
/// Windows PATH even when it is on the Git Bash PATH.
///
/// Tries, in order: the `CLAUDE_CLI_PATH` key in `extra_env`, then the
/// `CLAUDE_CLI_PATH` environment variable. Then the bare name `claude`,
/// letting the OS search PATH at spawn time. Then a platform-specific
/// fallback path under the user's home directory.
pub fn resolve_claude_binary(extra_env: Option<&HashMap<String, String>>) -> Option<PathBuf> {
    if let Some(env) = extra_env
        && let Some(path) = env.get(CLAUDE_CLI_PATH_KEY)
    {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Ok(path) = std::env::var(CLAUDE_CLI_PATH_KEY) {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Some(fallback) = fallback_claude_path()
        && fallback.is_file()
    {
        return Some(fallback);
    }

    Some(PathBuf::from("claude"))
}

#[cfg(target_os = "windows")]
pub fn fallback_claude_path() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE").ok()?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("bin")
            .join("claude.exe"),
    )
}

#[cfg(not(target_os = "windows"))]
pub fn fallback_claude_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("bin")
            .join("claude"),
    )
}

/// Build the argument vector for `claude -p`, in the exact order the
/// protocol expects. `effort`, when it maps to a CLI value, adds
/// `--effort <level>` right after the base flags: `Effort::None` omits it
/// entirely, since the CLI has no `none` value of its own. See
/// `docs/notes/claude-effort.md` for the verified value set.
/// `append_system_prompt`, when set, adds `--append-system-prompt <text>`
/// next: this is how voice reply mode reaches the child, since the child
/// has no per-turn config channel. `resume_id`, when set, adds `--resume
/// <id>` at the end, so the child resumes a saved conversation instead of
/// starting a fresh one. See `docs/notes/claude-resume.md` for the
/// verified flag shape.
pub fn build_args(
    model: &str,
    permission_mode: Option<&str>,
    append_system_prompt: Option<&str>,
    resume_id: Option<&str>,
    effort: Effort,
) -> Vec<String> {
    build_args_with_tools(
        model,
        permission_mode,
        append_system_prompt,
        resume_id,
        effort,
        true,
    )
}

pub(super) fn build_args_with_tools(
    model: &str,
    permission_mode: Option<&str>,
    append_system_prompt: Option<&str>,
    resume_id: Option<&str>,
    effort: Effort,
    tools_enabled: bool,
) -> Vec<String> {
    let mode = match (tools_enabled, permission_mode) {
        (false, None | Some("bypassPermissions")) => "default",
        (_, Some(mode)) => mode,
        (_, None) => "bypassPermissions",
    };
    let mut args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--include-partial-messages".to_string(),
        "--verbose".to_string(),
        "--model".to_string(),
        model.to_string(),
        "--permission-mode".to_string(),
        mode.to_string(),
        // Without this, a non-interactive run asks the API for no thinking
        // text at all: `thinking_delta` events still arrive, but every one
        // carries an empty `thinking` field beside an encrypted signature.
        // The gate is on the request, not on the renderer, so no amount of
        // parsing on this side recovers the text. `summarized` and
        // `omitted` are the only two values the flag takes.
        "--thinking-display".to_string(),
        "summarized".to_string(),
    ];
    if !tools_enabled {
        args.push("--restricted".to_string());
        args.push("--tools".to_string());
        args.push(String::new());
    }
    if let Some(level) = effort.claude_cli_effort() {
        args.push("--effort".to_string());
        args.push(level.to_string());
    }
    if let Some(prompt) = append_system_prompt {
        args.push("--append-system-prompt".to_string());
        args.push(prompt.to_string());
    }
    if let Some(id) = resume_id {
        args.push("--resume".to_string());
        args.push(id.to_string());
    }
    args
}

#[cfg(feature = "test-support")]
pub fn build_restricted_args_for_test(
    model: &str,
    permission_mode: Option<&str>,
    append_system_prompt: Option<&str>,
    resume_id: Option<&str>,
    effort: Effort,
) -> Vec<String> {
    build_args_with_tools(
        model,
        permission_mode,
        append_system_prompt,
        resume_id,
        effort,
        false,
    )
}

/// True when the resume id the driver currently holds differs from the id
/// the running child was spawned under. `--resume` is a spawn-time
/// argument. So a changed id leaves the running child stale, and it must
/// be replaced before the next turn. `ensure_ready` already applies that
/// same rule to a changed voice-mode flag.
pub fn resume_id_changed(current: &Option<String>, spawned: &Option<String>) -> bool {
    current != spawned
}

/// True when the live working directory differs from the one the running
/// child was spawned under. The child's cwd is a spawn-time argument, so a
/// changed directory leaves the running child stale, and it must be
/// replaced before the next turn. `ensure_ready` applies the same rule to a
/// changed voice-mode flag and a changed resume id. `spawned` is `None`
/// before the first child is ever spawned, which always counts as changed.
pub fn working_dir_changed(current: &std::path::Path, spawned: &Option<PathBuf>) -> bool {
    Some(current) != spawned.as_deref()
}

/// True when the live effort level differs from the one the running child
/// was spawned under. `--effort` is a spawn-time argument, same as
/// `--resume` and the working directory, so a changed level leaves the
/// running child stale and it must be replaced before the next turn.
pub fn effort_changed(current: Effort, spawned: Effort) -> bool {
    current != spawned
}

/// Build one stdin line for a user turn, in the verified stream-json shape.
/// With no image this is exactly the plain-text shape confirmed against the
/// real `claude` binary. With one, an Anthropic `image` content block
/// follows the text block: `{"type":"image","source":{"type":"base64",
/// "media_type":"...","data":"..."}}`. That shape, not the OpenAI
/// `image_url` shape the API backends speak, is what a real turn against
/// this stdin protocol actually accepts; see `docs/notes/image-support.md`.
pub fn build_user_turn_line(text: &str, image: Option<&ImageAttachment>) -> String {
    let mut content = vec![serde_json::json!({"type": "text", "text": text})];
    if let Some(image) = image {
        content.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.media_type,
                "data": image.data,
            }
        }));
    }
    let value = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content
        }
    });
    value.to_string()
}
