//! Unit tests for `deepseek_custom::agent::history`, moved out of the
//! production module as part of the two-crate workspace split.

use deepseek_custom::agent::history::MessageHistory;
use deepseek_custom::api::types::{Content, Message, Role};

#[test]
fn push_increases_count_and_tokens() {
    let mut h = MessageHistory::new("You are helpful.".into());
    assert_eq!(h.len(), 0);
    h.push(Message::user("hello".into()));
    assert_eq!(h.len(), 1);
    assert!(h.estimated_tokens() > 0);
}

#[test]
fn clear_drops_messages_keeps_system() {
    let mut h = MessageHistory::new("system".into());
    h.push(Message::user("hi".into()));
    h.clear();
    assert_eq!(h.len(), 0);
    assert!(h.is_empty());
}

#[test]
fn to_api_messages_includes_system_first() {
    let h = MessageHistory::new("sys".into());
    let api = h.to_api_messages();
    assert_eq!(api.len(), 1);
    assert_eq!(api[0].role, Role::System);
}

#[test]
fn estimated_tokens_increases_with_content() {
    let mut h = MessageHistory::new("short".into());
    let before = h.estimated_tokens();
    h.push(Message::user(
        "a long message with many characters and more words".into(),
    ));
    assert!(h.estimated_tokens() > before);
}

#[test]
fn no_suffix_leaves_system_message_unchanged() {
    let h = MessageHistory::new("You are helpful.".into());
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.")
    );
}

#[test]
fn set_suffix_appears_in_system_message() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.set_system_suffix(Some("Reply briefly.".into()));
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.\n\nReply briefly.")
    );
}

#[test]
fn set_suffix_none_removes_it() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.set_system_suffix(Some("Reply briefly.".into()));
    h.set_system_suffix(None);
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.")
    );
}

#[test]
fn suffix_survives_clear() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.set_system_suffix(Some("Reply briefly.".into()));
    h.push(Message::user("hi".into()));
    h.clear();
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.\n\nReply briefly.")
    );
}

#[test]
fn no_working_dir_leaves_system_message_unchanged() {
    let h = MessageHistory::new("You are helpful.".into());
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.")
    );
}

#[test]
fn set_working_dir_appears_in_system_message() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.set_working_dir(Some("C:\\proj".into()));
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.\n\nWorking directory: C:\\proj.")
    );
}

#[test]
fn set_working_dir_none_removes_it() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.set_working_dir(Some("C:\\proj".into()));
    h.set_working_dir(None);
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.")
    );
}

#[test]
fn working_dir_and_suffix_both_appear_together() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.set_working_dir(Some("C:\\proj".into()));
    h.set_system_suffix(Some("Reply briefly.".into()));
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.\n\nWorking directory: C:\\proj.\n\nReply briefly.")
    );
}

#[test]
fn changing_working_dir_replaces_the_reported_directory() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.set_working_dir(Some("C:\\proj".into()));
    h.set_working_dir(Some("C:\\other".into()));
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.\n\nWorking directory: C:\\other.")
    );
}

#[test]
fn working_dir_survives_clear() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.set_working_dir(Some("C:\\proj".into()));
    h.push(Message::user("hi".into()));
    h.clear();
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.\n\nWorking directory: C:\\proj.")
    );
}

#[test]
fn estimated_tokens_reflects_set_working_dir() {
    let mut h = MessageHistory::new("short".into());
    let before = h.estimated_tokens();
    h.set_working_dir(Some("a long directory path with many characters".into()));
    assert!(h.estimated_tokens() > before);
}

#[test]
fn estimated_tokens_reflects_set_suffix() {
    let mut h = MessageHistory::new("short".into());
    let before = h.estimated_tokens();
    h.set_system_suffix(Some(
        "a long suffix with many characters and more words".into(),
    ));
    assert!(h.estimated_tokens() > before);
}

#[test]
fn restore_replaces_rather_than_appends() {
    let mut h = MessageHistory::new("sys".into());
    h.push(Message::user("old1".into()));
    h.push(Message::user("old2".into()));
    let restored = vec![
        Message::user("new1".into()),
        Message::assistant("new2".into()),
    ];
    h.restore(restored.clone());
    assert_eq!(h.len(), restored.len());
    let got: Vec<&Message> = h.iter().collect();
    for (g, r) in got.iter().zip(restored.iter()) {
        assert_eq!(g.content, r.content);
        assert_eq!(g.role, r.role);
    }
}

#[test]
fn restore_token_count_matches_pushed_one_at_a_time() {
    let messages = vec![
        Message::user("hello there".into()),
        Message::assistant("a fairly long reply with several words".into()),
    ];

    let mut restored_h = MessageHistory::new("system prompt".into());
    restored_h.restore(messages.clone());

    let mut pushed_h = MessageHistory::new("system prompt".into());
    for m in messages {
        pushed_h.push(m);
    }

    assert_eq!(restored_h.estimated_tokens(), pushed_h.estimated_tokens());
}

#[test]
fn restore_leaves_system_prompt_unchanged() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.restore(vec![Message::user("hi".into())]);
    let api = h.to_api_messages();
    assert_eq!(api[0].role, Role::System);
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.")
    );
}

#[test]
fn restore_leaves_system_suffix_in_place() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.set_system_suffix(Some("Reply briefly.".into()));
    let before_suffix_tokens = h.estimated_tokens();
    h.restore(vec![Message::user("hi".into())]);
    let api = h.to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.\n\nReply briefly.")
    );
    // suffix contribution still reflected in the token count
    let mut baseline = MessageHistory::new("You are helpful.".into());
    baseline.set_system_suffix(Some("Reply briefly.".into()));
    assert!(before_suffix_tokens > 0);
    assert!(h.estimated_tokens() >= baseline.estimated_tokens());
}

#[test]
fn restore_empty_vector_leaves_history_empty_with_system_prompt() {
    let mut h = MessageHistory::new("You are helpful.".into());
    h.push(Message::user("hi".into()));
    h.restore(Vec::new());
    assert!(h.is_empty());
    assert_eq!(h.len(), 0);
    let api = h.to_api_messages();
    assert_eq!(api.len(), 1);
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("You are helpful.")
    );
}

/// `recompute_token_count` itself stays private: it is bookkeeping, not a
/// real interface method. `restore` already recomputes the token count
/// from scratch the same way `prune_to_budget` does, so restoring the
/// pruned messages into a fresh, independent `MessageHistory` is a public
/// seam for the same check the original test made through the private
/// method directly.
#[test]
fn estimated_tokens_matches_fresh_recompute_after_prune() {
    let mut h = MessageHistory::new("system prompt".into());
    for i in 0..8 {
        h.push(Message::user(format!("question {i}")));
        h.push(Message::assistant(format!("answer {i}")));
    }
    h.prune_to_budget(1, None);
    let tracked = h.estimated_tokens();

    let remaining: Vec<Message> = h.iter().cloned().collect();
    let mut fresh = MessageHistory::new("system prompt".into());
    fresh.restore(remaining);

    assert_eq!(tracked, fresh.estimated_tokens());
}
