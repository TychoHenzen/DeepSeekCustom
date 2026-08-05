//! The current conversation's identity on disk: its id, its metadata, the
//! saved-session list, and the API history a save needs.
//!
//! This owns the six fields `DeepSeekGui` used to hold for saved
//! conversations. An earlier split moved these methods into this file but
//! left them as an `impl DeepSeekGui`, so the fields stayed on the GUI and
//! every method could still reach the whole of it. They are now methods on
//! a type that owns its own state.
//!
//! No UI lives here. The Sessions tab renders what this holds, and the
//! frame loop drives it.
//!
//! The seam runs one way, like `gui::voice_ui`. Nothing here sends on the
//! agent channel. A method that needs the agent told something returns the
//! command, and the caller sends it. That keeps one place in the GUI that
//! talks to the agent.

use tracing::warn;

use super::transcript::Transcript;
use crate::agent::agent_loop::AgentCommand;
use crate::api::types::Message;
use crate::session::{SessionId, SessionMeta, SessionRecord, SessionStore, now_timestamp};

/// The title a conversation carries until its first user message names it.
const PLACEHOLDER_TITLE: &str = "New conversation";

/// Which backend and model the running session is on. A save records both,
/// and they can change under the GUI, so every call that writes metadata
/// passes the current pair rather than trusting one captured at startup.
///
/// This owns its two strings rather than borrowing them. A borrowed
/// version would hold a shared borrow of the whole GUI for as long as the
/// call runs, which blocks the mutable borrows of the transcript and this
/// state that the same call needs. Two short clones per save sit next to a
/// file write, so the cost does not signify.
#[derive(Debug, Clone)]
pub(crate) struct SessionOrigin {
    pub backend: String,
    pub model: String,
}

/// The saved-conversation state behind the Chat and Sessions tabs.
pub(crate) struct SessionState {
    /// Disk layer for saved conversations, rooted under the project root.
    store: SessionStore,
    /// The session id the current conversation will save under.
    current_id: SessionId,
    /// Summary metadata for the current conversation. Kept in step with
    /// `current_id` by every method that changes either.
    current_meta: SessionMeta,
    /// Every saved session's metadata, refreshed after each save.
    saved: Vec<SessionMeta>,
    /// The API history as of the latest `ConversationSnapshot` event.
    /// Always empty on a `claude_cli` session.
    messages: Vec<Message>,
    /// The `claude` CLI's own session id, for `--resume`, as of the latest
    /// `ConversationSnapshot` event. Always `None` on an `Api` session.
    claude_session_id: Option<String>,
}

impl SessionState {
    /// Open a fresh, empty conversation and read the saved list off disk.
    pub(crate) fn new(store: SessionStore, origin: SessionOrigin) -> Self {
        let saved = store.list();
        let current_id = SessionId::new();
        Self {
            store,
            current_id,
            current_meta: fresh_meta(current_id, &origin),
            saved,
            messages: Vec::new(),
            claude_session_id: None,
        }
    }

    /// The id the current conversation will save under.
    pub(crate) fn current_id(&self) -> SessionId {
        self.current_id
    }

    /// Every saved conversation's metadata, newest first as the store
    /// returns it.
    pub(crate) fn saved(&self) -> &[SessionMeta] {
        &self.saved
    }

    /// The API history as of the last snapshot. Test-only: production
    /// code reads it through the save path, never directly.
    #[cfg(test)]
    pub(crate) fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// The `claude` CLI's own session id as of the last snapshot.
    /// Test-only, for the same reason as `messages`.
    #[cfg(test)]
    pub(crate) fn claude_session_id(&self) -> Option<&str> {
        self.claude_session_id.as_deref()
    }

    /// The disk layer, so a test can read back what a save wrote.
    #[cfg(test)]
    pub(crate) fn store(&self) -> &SessionStore {
        &self.store
    }

    /// The current conversation's title, for tests that check a title is
    /// derived once and then left alone.
    #[cfg(test)]
    pub(crate) fn title_for_test(&self) -> &str {
        &self.current_meta.title
    }

    /// Capture a `ConversationSnapshot` event: the latest API history and,
    /// on a `claude_cli` session, the child's own session id. Called before
    /// the paired `TurnEnd` triggers an autosave, so the save sees this
    /// turn's data.
    pub(crate) fn record_snapshot(
        &mut self,
        messages: &[Message],
        claude_session_id: &Option<String>,
    ) {
        self.messages = messages.to_vec();
        self.claude_session_id = claude_session_id.clone();
    }

    /// Save the current conversation to disk, refreshing its metadata
    /// first: title (derived only while it is still the placeholder),
    /// timestamp, message count, backend, and model. Called after every
    /// `TurnEnd`.
    pub(crate) fn autosave(&mut self, transcript: &mut Transcript, origin: SessionOrigin) {
        if self.current_meta.title == PLACEHOLDER_TITLE {
            self.current_meta.title = crate::session::derive_title(&self.messages, transcript);
        }
        self.current_meta.updated_at = now_timestamp();
        self.current_meta.message_count = self.messages.len();
        self.current_meta.backend = origin.backend.clone();
        self.current_meta.model = origin.model.clone();

        self.write_to_disk(transcript);
        self.refresh_saved();
    }

    /// Serialize the current conversation and write it, without touching
    /// metadata. Moves the transcript out and back rather than cloning it,
    /// since `Transcript` carries no `Clone` impl.
    fn write_to_disk(&mut self, transcript: &mut Transcript) {
        let record = SessionRecord {
            meta: self.current_meta.clone(),
            messages: self.messages.clone(),
            transcript: std::mem::take(transcript),
            claude_session_id: self.claude_session_id.clone(),
        };
        let result = self.store.save(&record);
        *transcript = record.transcript;
        if let Err(e) = result {
            warn!(error = %e, "failed to save session");
        }
    }

    /// Refresh the saved-conversation list from disk.
    fn refresh_saved(&mut self) {
        self.saved = self.store.list();
    }

    /// Save the outgoing conversation before switching away from it. Skips
    /// the save when the transcript is empty, so opening a new session
    /// twice in a row does not litter the sessions directory with empty
    /// records.
    fn save_outgoing(&mut self, transcript: &mut Transcript, origin: SessionOrigin) {
        if transcript.blocks().is_empty() {
            return;
        }
        self.autosave(transcript, origin);
    }

    /// Start a fresh, empty conversation and hand back the command that
    /// tells the agent to start over.
    pub(crate) fn start_new(
        &mut self,
        transcript: &mut Transcript,
        origin: SessionOrigin,
    ) -> AgentCommand {
        self.save_outgoing_and_start_new(transcript, origin);
        AgentCommand::NewSession
    }

    /// Save the outgoing conversation and open a fresh one in its place,
    /// without telling the agent. This is the path the agent's own `Reset`
    /// tool takes, since the agent already knows it reset itself.
    pub(crate) fn save_outgoing_and_start_new(
        &mut self,
        transcript: &mut Transcript,
        origin: SessionOrigin,
    ) {
        self.save_outgoing(transcript, origin.clone());
        transcript.clear();
        self.messages.clear();
        self.claude_session_id = None;
        let id = SessionId::new();
        self.current_id = id;
        self.current_meta = fresh_meta(id, &origin);
        self.refresh_saved();
    }

    /// Load a saved conversation: save the outgoing one, install the loaded
    /// transcript and id, and hand back the command that tells the agent to
    /// replay its history. Returns `None` when the record will not load, in
    /// which case the current conversation is left alone.
    pub(crate) fn load(
        &mut self,
        id: SessionId,
        transcript: &mut Transcript,
        origin: SessionOrigin,
    ) -> Option<AgentCommand> {
        self.save_outgoing(transcript, origin);
        let record = match self.store.load(&id) {
            Ok(record) => record,
            Err(e) => {
                warn!(error = %e, "failed to load session");
                return None;
            }
        };

        *transcript = record.transcript;
        self.messages = record.messages.clone();
        self.claude_session_id = record.claude_session_id.clone();
        self.current_id = record.meta.id;
        self.current_meta = record.meta;
        self.refresh_saved();
        Some(AgentCommand::LoadSession {
            messages: record.messages,
            claude_session_id: record.claude_session_id,
        })
    }

    /// Delete one saved conversation and refresh the list. A failure is
    /// logged and otherwise ignored, matching every other disk error here.
    pub(crate) fn delete(&mut self, id: SessionId) {
        if let Err(e) = self.store.delete(&id) {
            warn!(error = %e, "failed to delete session");
        }
        self.refresh_saved();
    }
}

/// A fresh, empty conversation's metadata: placeholder title, message
/// count zero, timestamps set to now, backend and model from the running
/// session.
fn fresh_meta(id: SessionId, origin: &SessionOrigin) -> SessionMeta {
    let now = now_timestamp();
    SessionMeta {
        id,
        title: PLACEHOLDER_TITLE.to_string(),
        created_at: now,
        updated_at: now,
        backend: origin.backend.clone(),
        model: origin.model.clone(),
        message_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::transcript::BlockKind;

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
        assert_eq!(state.current_meta.title, PLACEHOLDER_TITLE);
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
        let derived = state.current_meta.title.clone();
        assert_ne!(derived, PLACEHOLDER_TITLE);

        transcript.push(BlockKind::User {
            text: "a later question".into(),
        });
        state.autosave(&mut transcript, origin());
        assert_eq!(
            state.current_meta.title, derived,
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
}
