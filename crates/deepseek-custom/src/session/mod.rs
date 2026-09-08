//! Plain data types for one saved conversation.
//!
//! This file holds the shapes and the title derivation logic, and does no
//! disk IO of its own. `store` below reads and writes them as one JSON
//! file per session under `.deepseek/sessions/`, and
//! `src/application/session_state.rs` drives saving and loading for the application actor.

use crate::api::types::{Content, Message, Role};
use crate::application::transcript::{BlockKind, Transcript};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// `pub`: the moved `SessionStore` tests in `deepseek-custom-tests` import
/// `deepseek_custom::session::store::SessionStore` directly rather than
/// through this module's re-export.
pub mod store;
pub use store::SessionStore;

/// The longest a derived conversation title is allowed to be, in
/// characters. A title that would run longer gets truncated and marked
/// with an ellipsis.
///
/// `pub`: the moved test suite in `deepseek-custom-tests` pins the cap
/// directly (`title.chars().count() <= MAX_TITLE_LEN`), and there is no
/// side-effect-free public seam that reports the configured cap back to
/// a caller.
pub const MAX_TITLE_LEN: usize = 60;

/// Identifies one saved session. A newtype over a UUID rather than the
/// UUID type itself. That way a session id cannot be mixed up with any
/// other identifier the crate carries.
///
/// Its string form is the UUID's hyphenated form. That form is already
/// safe as a file stem. It holds no path separator and no character
/// illegal in a Windows filename. The store uses it as the session
/// file's name on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    /// Generate a fresh id for a new session.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The id's string form, safe to use as a file stem.
    pub fn as_str(&self) -> String {
        self.0.to_string()
    }

    /// Parse the stable string form accepted by presentation adapters.
    pub fn parse(value: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(value).map(Self)
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

/// A Unix timestamp in seconds. Kept as a plain integer rather than a
/// `SystemTime` in the stored types, since a `SystemTime` has no stable
/// serialized form across platforms. The Sessions tab derives a relative
/// display ("2h ago") from this at render time.
pub type Timestamp = u64;

/// The current time as a `Timestamp`. Falls back to 0 on a clock set
/// before the Unix epoch, which should not happen on a real machine.
pub fn now_timestamp() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// A conversation's display number: 1 for the first session ever opened in
/// a project, counting up from there. Assigned once, when the conversation
/// opens, and never changed again.
///
/// The Sessions tab used to be ordered by `updated_at` alone, which meant
/// the list reshuffled under the reader every time a running turn autosaved
/// and moved its own row to the top. A number that never moves gives a row
/// a name that survives the next save.
pub type SessionSeq = u64;

/// The summary row the Sessions tab renders without loading the whole
/// conversation: number, title, timestamps, and which backend and model it
/// ran on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: SessionId,
    /// This conversation's display number. `serde(default)` gives 0 to a
    /// record written before numbering existed. `SessionStore::number_old_sessions`
    /// replaces every such 0 once, at startup, so a 0 never reaches the
    /// Sessions tab.
    #[serde(default)]
    pub seq: SessionSeq,
    pub title: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// The name of the backend entry (from `settings.json`'s `backends`
    /// map) the conversation ran on.
    pub backend: String,
    pub model: String,
    pub message_count: usize,
}

/// A whole saved conversation: its summary, the API message history, and
/// the display transcript. It also holds the `claude` CLI's own session
/// id, for a conversation that ran on a `claude_cli` backend.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionRecord {
    pub meta: SessionMeta,
    /// The API history, replayed to the backend on resume.
    pub messages: Vec<Message>,
    /// The display transcript, rendered back into the Chat tab on
    /// resume.
    pub transcript: Transcript,
    /// The `claude` CLI's own session id, for use with `--resume`. Always
    /// `None` for an API-backend conversation. `None` on a `claude_cli`
    /// conversation until its first turn completes.
    pub claude_session_id: Option<String>,
    /// Session-owned Controlled Development state. Older records omit it.
    #[serde(default)]
    pub controlled_development: crate::controlled_development::ControlledDevelopmentSessionRecord,
}

/// Derive a conversation's title from its message history. It takes the
/// first `Role::User` message's content, trimmed, with any newline
/// collapsed to a space, capped at `MAX_TITLE_LEN` characters. A cap
/// that lands mid-word gets a trailing ellipsis, so the reader can tell
/// the title was cut short. The marked result stays within
/// `MAX_TITLE_LEN` characters too.
///
/// A `claude_cli` conversation always has an empty `messages` vector, by
/// design: Claude Code owns that history inside its own child process.
/// When no user message is found, this falls back to the first
/// `BlockKind::User` block in `transcript`, applying the same one-line
/// and length rules.
///
/// Returns `"New conversation"` when both sources are empty.
pub fn derive_title(messages: &[Message], transcript: &Transcript) -> String {
    let first_user_text = messages
        .iter()
        .find(|message| message.role == Role::User)
        .and_then(|message| message.content.as_ref())
        .and_then(Content::as_text);

    let text = match first_user_text {
        Some(text) => Some(text.to_string()),
        None => transcript
            .blocks()
            .iter()
            .find_map(|block| match &block.kind {
                BlockKind::User { text } => Some(text.clone()),
                _ => None,
            }),
    };

    let Some(text) = text else {
        return "New conversation".to_string();
    };

    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let one_line = one_line.trim();
    if one_line.is_empty() {
        return "New conversation".to_string();
    }

    truncate_title(one_line)
}

/// Cap `text` at `MAX_TITLE_LEN` characters, appending an ellipsis when
/// truncation happened so the cut is visible. The ellipsis counts toward
/// the cap, so the result is never longer than `MAX_TITLE_LEN`.
fn truncate_title(text: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= MAX_TITLE_LEN {
        return text.to_string();
    }

    let keep = MAX_TITLE_LEN.saturating_sub(1);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push('\u{2026}');
    truncated
}
