//! Unit tests for `deepseek_custom::gui::session_state` (`src/gui/session_state.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::agent::agent_loop::AgentCommand;
use deepseek_custom::api::types::Message;
use deepseek_custom::gui::session_state::{PLACEHOLDER_TITLE, SessionOrigin, SessionState};
use deepseek_custom::gui::transcript::{BlockKind, Transcript};
use deepseek_custom::session::{SessionId, SessionStore};

fn origin() -> SessionOrigin {
    SessionOrigin {
        backend: "deepseek".to_string(),
        model: "deepseek-v4-flash".to_string(),
    }
}

/// A unique temporary project root, the same way `session::store`'s
/// own tests make one. No temp-directory crate is a dependency here.
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dsc-gui-session-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn state_in(dir: &std::path::Path) -> SessionState {
    SessionState::new(SessionStore::for_project(dir), origin())
}

fn transcript_with_user_text(text: &str) -> Transcript {
    let mut transcript = Transcript::default();
    transcript.push(BlockKind::User { text: text.into() });
    transcript
}

#[test]
fn a_new_state_starts_empty_with_a_placeholder_title() {
    let dir = temp_dir("a_new_state_starts_empty");
    let state = state_in(&dir);
    assert!(state.saved().is_empty());
    assert!(state.messages().is_empty());
    assert_eq!(state.claude_session_id(), None);
    assert_eq!(state.title_for_test(), PLACEHOLDER_TITLE);
}

#[test]
fn recording_a_snapshot_keeps_the_history_and_the_claude_id() {
    let dir = temp_dir("recording_a_snapshot_kee");
    let mut state = state_in(&dir);
    let messages = vec![Message::user("hello".to_string())];
    state.record_snapshot(&messages, &Some("abc-123".into()));
    assert_eq!(state.messages().len(), 1);
    assert_eq!(state.claude_session_id(), Some("abc-123"));
}

#[test]
fn autosave_writes_the_record_and_lists_it() {
    let dir = temp_dir("autosave_writes_the_reco");
    let mut state = state_in(&dir);
    let mut transcript = transcript_with_user_text("first question");
    state.record_snapshot(&[Message::user("first question".to_string())], &None);
    state.autosave(&mut transcript, origin());
    assert_eq!(state.saved().len(), 1);
    assert_eq!(state.saved()[0].message_count, 1);
    assert_eq!(state.saved()[0].backend, "deepseek");
}

#[test]
fn autosave_returns_the_transcript_it_borrowed() {
    let dir = temp_dir("autosave_returns_the_tra");
    let mut state = state_in(&dir);
    let mut transcript = transcript_with_user_text("keep me");
    state.autosave(&mut transcript, origin());
    assert_eq!(
        transcript.blocks().len(),
        1,
        "the transcript must survive being moved through the record"
    );
}

#[test]
fn autosave_derives_the_title_from_the_transcript_once() {
    let dir = temp_dir("autosave_derives_the_tit");
    let mut state = state_in(&dir);
    let mut transcript = transcript_with_user_text("what is the plan");
    state.autosave(&mut transcript, origin());
    let derived = state.title_for_test().to_string();
    assert_ne!(derived, PLACEHOLDER_TITLE);

    transcript.push(BlockKind::User {
        text: "a later question".into(),
    });
    state.autosave(&mut transcript, origin());
    assert_eq!(
        state.title_for_test(),
        derived,
        "a title is derived once, not rewritten every save"
    );
}

#[test]
fn starting_a_new_session_saves_the_outgoing_one_and_clears_the_transcript() {
    let dir = temp_dir("starting_a_new_session_s");
    let mut state = state_in(&dir);
    let mut transcript = transcript_with_user_text("outgoing");
    let first_id = state.current_id();

    let command = state.start_new(&mut transcript, origin());

    assert!(matches!(command, AgentCommand::NewSession));
    assert!(transcript.blocks().is_empty());
    assert_ne!(state.current_id(), first_id);
    assert_eq!(state.saved().len(), 1, "the outgoing session was saved");
}

#[test]
fn starting_a_new_session_twice_saves_no_empty_record() {
    let dir = temp_dir("starting_a_new_session_t");
    let mut state = state_in(&dir);
    let mut transcript = Transcript::default();
    let _ = state.start_new(&mut transcript, origin());
    let _ = state.start_new(&mut transcript, origin());
    assert!(
        state.saved().is_empty(),
        "an empty conversation must not be written"
    );
}

#[test]
fn loading_a_saved_session_restores_it_and_replays_the_history() {
    let dir = temp_dir("loading_a_saved_session_");
    let mut state = state_in(&dir);
    let mut transcript = transcript_with_user_text("original question");
    state.record_snapshot(
        &[Message::user("original question".to_string())],
        &Some("cli-1".into()),
    );
    state.autosave(&mut transcript, origin());
    let saved_id = state.current_id();

    let _ = state.start_new(&mut transcript, origin());
    let command = state
        .load(saved_id, &mut transcript, origin())
        .expect("a saved session must load");

    assert_eq!(state.current_id(), saved_id);
    assert_eq!(transcript.blocks().len(), 1);
    assert!(matches!(
        command,
        AgentCommand::LoadSession {
            messages,
            claude_session_id,
        } if messages.len() == 1 && claude_session_id.as_deref() == Some("cli-1")
    ));
}

#[test]
fn loading_an_unknown_session_leaves_the_current_one_alone() {
    let dir = temp_dir("loading_an_unknown_sessi");
    let mut state = state_in(&dir);
    let mut transcript = transcript_with_user_text("still here");
    let before = state.current_id();

    let command = state.load(SessionId::new(), &mut transcript, origin());

    assert!(command.is_none());
    assert_eq!(state.current_id(), before);
    assert_eq!(transcript.blocks().len(), 1);
}

#[test]
fn deleting_a_saved_session_drops_it_from_the_list() {
    let dir = temp_dir("deleting_a_saved_session");
    let mut state = state_in(&dir);
    let mut transcript = transcript_with_user_text("doomed");
    state.autosave(&mut transcript, origin());
    let id = state.current_id();
    assert_eq!(state.saved().len(), 1);

    state.delete(id);

    assert!(state.saved().is_empty());
}

#[test]
fn deleting_an_unknown_session_is_harmless() {
    let dir = temp_dir("deleting_an_unknown_sess");
    let mut state = state_in(&dir);
    state.delete(SessionId::new());
    assert!(state.saved().is_empty());
}
