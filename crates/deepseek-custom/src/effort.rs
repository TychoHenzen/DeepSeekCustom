//! `Effort`: the harness's own five-level reasoning-effort control, shared
//! across every backend. A single control replaces the old per-backend
//! guesswork: a boolean thinking toggle for DeepSeek and Ollama, and no
//! control at all for `claude_cli`.
//!
//! Each backend maps `Effort` to whatever its own API or CLI expects, at the
//! edge, right before a request goes out or a child gets spawned:
//! `ApiClient::prepare_request` in `src/api/client.rs` for DeepSeek and
//! Ollama, `build_args` in `src/backend/claude_cli/process.rs` for the
//! `claude` CLI. See phase 5 of
//! `docs/plans/2026-08-04-long-term-roadmap.md` and
//! `docs/notes/claude-effort.md` for the mapping decisions and their
//! evidence.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

/// Five levels, ordered lowest to highest. `None` means no reasoning
/// effort at all, the same meaning the old `thinking: false` boolean had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Effort {
    #[default]
    None,
    Low,
    Medium,
    High,
    Max,
}

impl Effort {
    /// Encode as the `u8` a shared `AtomicU8` flag carries.
    pub fn to_u8(self) -> u8 {
        match self {
            Effort::None => 0,
            Effort::Low => 1,
            Effort::Medium => 2,
            Effort::High => 3,
            Effort::Max => 4,
        }
    }

    /// Decode from the `u8` a shared flag carries. A value `to_u8` never
    /// produces (5 or above) falls back to `None`: a defensive default, not
    /// a case any writer in this codebase is expected to hit.
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Effort::None,
            1 => Effort::Low,
            2 => Effort::Medium,
            3 => Effort::High,
            4 => Effort::Max,
            _ => Effort::None,
        }
    }

    /// Read the current level off a shared flag.
    pub fn load(flag: &AtomicU8) -> Self {
        Effort::from_u8(flag.load(Ordering::SeqCst))
    }

    /// Write this level into a shared flag.
    pub fn store(self, flag: &AtomicU8) {
        flag.store(self.to_u8(), Ordering::SeqCst);
    }

    /// DeepSeek's V4 `thinking_mode` value for this level. DeepSeek has
    /// three levels for the harness's five: `None` maps to
    /// `"non-thinking"`, `Low` through `High` all map to `"thinking"`, and
    /// `Max` maps to `"thinking_max"`, until DeepSeek offers more levels of
    /// its own.
    pub fn deepseek_thinking_mode(self) -> &'static str {
        match self {
            Effort::None => "non-thinking",
            Effort::Low | Effort::Medium | Effort::High => "thinking",
            Effort::Max => "thinking_max",
        }
    }

    /// Ollama's `reasoning_effort` value for this level: a direct
    /// one-to-one mapping, since Ollama also has five levels.
    pub fn ollama_reasoning_effort(self) -> &'static str {
        match self {
            Effort::None => "none",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Max => "max",
        }
    }

    /// The `claude` CLI's `--effort <level>` value for this level, or
    /// `None` to omit the flag entirely. The CLI's accepted values are
    /// `low, medium, high, xhigh, max`: no `none`, and no slot that lines
    /// up with `xhigh`. Omitting the flag for `Effort::None` reproduces the
    /// CLI's own default, which is exactly the behaviour before this
    /// control existed. See `docs/notes/claude-effort.md`.
    pub fn claude_cli_effort(self) -> Option<&'static str> {
        match self {
            Effort::None => None,
            Effort::Low => Some("low"),
            Effort::Medium => Some("medium"),
            Effort::High => Some("high"),
            Effort::Max => Some("max"),
        }
    }
}
