//! Unit tests for `deepseek_custom::session` (`src/session/mod.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::api::types::{Content, Message, Role};
use deepseek_custom::gui::transcript::{BlockKind, Transcript};
use deepseek_custom::session::{MAX_TITLE_LEN, SessionId, SessionMeta, SessionRecord, derive_title};

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
