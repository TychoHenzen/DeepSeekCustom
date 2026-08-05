//! Plain data types for one saved conversation.
//!
//! This module holds the shapes that will eventually be written to and
//! read from disk under `.deepseek/sessions/` (a later step, not this
//! one). No disk IO and no GUI wiring happen here, only the types and
//! the title derivation logic that later steps depend on.

use crate::api::types::{Content, Message, Role};
use crate::gui::transcript::{BlockKind, Transcript};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

mod store;
pub use store::SessionStore;

/// The longest a derived conversation title is allowed to be, in
/// characters. A title that would run longer gets truncated and marked
/// with an ellipsis.
const MAX_TITLE_LEN: usize = 60;

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

/// The summary row the Sessions tab renders without loading the whole
/// conversation: title, timestamps, and which backend and model it ran
/// on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: SessionId,
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
        None => transcript.blocks().iter().find_map(|block| match &block.kind {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::transcript::BlockKind;

    fn user_message(content: &str) -> Message {
        Message {
            role: Role::User,
            content: Some(Content::text(content)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn assistant_message(content: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: Some(Content::text(content)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn system_message(content: &str) -> Message {
        Message {
            role: Role::System,
            content: Some(Content::text(content)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn title_from_plain_first_user_message() {
        let messages = vec![user_message("fix the bug in the parser")];
        assert_eq!(derive_title(&messages, &Transcript::new()), "fix the bug in the parser");
    }

    #[test]
    fn title_with_no_user_message_is_new_conversation() {
        let messages = vec![system_message("you are a helpful assistant")];
        assert_eq!(derive_title(&messages, &Transcript::new()), "New conversation");
    }

    #[test]
    fn title_from_empty_message_list_is_new_conversation() {
        let messages: Vec<Message> = Vec::new();
        assert_eq!(derive_title(&messages, &Transcript::new()), "New conversation");
    }

    #[test]
    fn title_from_long_first_user_message_is_capped() {
        let long_text = "word ".repeat(40);
        let messages = vec![user_message(&long_text)];
        let title = derive_title(&messages, &Transcript::new());
        assert!(title.chars().count() <= MAX_TITLE_LEN);
        assert!(title.ends_with('\u{2026}'));
    }

    #[test]
    fn title_from_multiline_first_user_message_is_one_line() {
        let messages = vec![user_message("first line\nsecond line\nthird line")];
        let title = derive_title(&messages, &Transcript::new());
        assert!(!title.contains('\n'));
        assert_eq!(title, "first line second line third line");
    }

    #[test]
    fn title_ignores_leading_assistant_and_system_messages() {
        let messages = vec![
            system_message("system prompt"),
            assistant_message("greeting"),
            user_message("the real question"),
        ];
        assert_eq!(derive_title(&messages, &Transcript::new()), "the real question");
    }

    #[test]
    fn title_falls_back_to_transcript_when_messages_are_empty() {
        // Shaped like a claude_cli conversation: the message vector is
        // always empty by design, but the transcript still holds the
        // user's turn.
        let messages: Vec<Message> = Vec::new();
        let mut transcript = Transcript::new();
        transcript.push(BlockKind::User {
            text: "fix the bug in the claude_cli path".into(),
        });

        assert_eq!(
            derive_title(&messages, &transcript),
            "fix the bug in the claude_cli path"
        );
    }

    #[test]
    fn title_stays_new_conversation_when_both_sources_are_empty() {
        let messages: Vec<Message> = Vec::new();
        let transcript = Transcript::new();

        assert_eq!(derive_title(&messages, &transcript), "New conversation");
    }

    #[test]
    fn title_prefers_message_history_over_transcript() {
        let messages = vec![user_message("from messages")];
        let mut transcript = Transcript::new();
        transcript.push(BlockKind::User {
            text: "from transcript".into(),
        });

        assert_eq!(derive_title(&messages, &transcript), "from messages");
    }

    #[test]
    fn session_record_round_trips_through_json() {
        let mut transcript = Transcript::new();
        transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        transcript.push(BlockKind::Assistant {
            spans: vec![],
        });

        let record = SessionRecord {
            meta: SessionMeta {
                id: SessionId::new(),
                title: "hello".into(),
                created_at: 1000,
                updated_at: 2000,
                backend: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                message_count: 2,
            },
            messages: vec![user_message("hello"), assistant_message("hi there")],
            transcript,
            claude_session_id: None,
        };

        let json = serde_json::to_string(&record).unwrap();
        let restored: SessionRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.meta.id, record.meta.id);
        assert_eq!(restored.meta.title, record.meta.title);
        assert_eq!(restored.messages.len(), record.messages.len());
        assert_eq!(
            restored.transcript.blocks().len(),
            record.transcript.blocks().len()
        );
        assert_eq!(restored.claude_session_id, record.claude_session_id);
    }

    #[test]
    fn two_fresh_session_ids_differ() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn session_id_string_form_is_safe_as_a_file_stem() {
        let id = SessionId::new();
        let s = id.as_str();
        assert!(!s.contains('/'));
        assert!(!s.contains('\\'));
        assert!(!s.contains(':'));
        assert!(!s.is_empty());
    }
}
