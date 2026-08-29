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

use tracing::{info, warn};

use super::transcript::Transcript;
use crate::agent::events::AgentCommand;
use crate::api::types::Message;
use crate::session::{
    SessionId, SessionMeta, SessionRecord, SessionSeq, SessionStore, now_timestamp,
};

/// The title a conversation carries until its first user message names it.
pub const PLACEHOLDER_TITLE: &str = "New conversation";

/// Which backend and model the running session is on. A save records
/// both. Either can change under the GUI. So every call that writes
/// metadata passes the current pair, rather than one captured at startup.
///
/// This owns its two strings rather than borrowing them. A borrowed
/// version would hold a shared borrow of the whole GUI while the call
/// runs. That blocks the borrows of the transcript and this state that
/// the same call needs. Two short clones per save sit next to a file
/// write, so the cost does not signify.
#[derive(Debug, Clone)]
pub struct SessionOrigin {
    pub backend: String,
    pub model: String,
}

/// The saved-conversation state behind the Chat and Sessions tabs.
pub struct SessionState {
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
    ///
    /// Numbers any older record that predates numbering first, so the list
    /// this reads is already complete and the fresh conversation's own
    /// number lands above every one of them.
    pub fn new(store: SessionStore, origin: SessionOrigin) -> Self {
        store.number_old_sessions();
        let saved = store.list();
        let current_id = SessionId::new();
        let seq = next_seq(&saved);
        Self {
            store,
            current_id,
            current_meta: fresh_meta(current_id, seq, &origin),
            saved,
            messages: Vec::new(),
            claude_session_id: None,
        }
    }

    /// The id the current conversation will save under.
    pub fn current_id(&self) -> SessionId {
        self.current_id
    }

    /// Current metadata for presentation-neutral session projection.
    pub fn current_meta(&self) -> &SessionMeta {
        &self.current_meta
    }

    /// Every saved conversation's metadata, newest first as the store
    /// returns it.
    pub fn saved(&self) -> &[SessionMeta] {
        &self.saved
    }

    /// The API history as of the last snapshot. Test-only: production
    /// code reads it through the save path, never directly.
    #[cfg(feature = "test-support")]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// The `claude` CLI's own session id as of the last snapshot.
    /// Test-only, for the same reason as `messages`.
    #[cfg(feature = "test-support")]
    pub fn claude_session_id(&self) -> Option<&str> {
        self.claude_session_id.as_deref()
    }

    /// The disk layer. Test-only: production code only ever writes
    /// through `autosave` and `load`, so handing the store out would be a
    /// second way into the same files.
    #[cfg(feature = "test-support")]
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// The current conversation's title. Test-only: the title is derived
    /// inside `autosave` and only ever leaves this type inside a written
    /// record, so there is no read seam production code would use.
    #[cfg(feature = "test-support")]
    pub fn title_for_test(&self) -> &str {
        &self.current_meta.title
    }

    /// Capture a `ConversationSnapshot` event: the latest API history and,
    /// on a `claude_cli` session, the child's own session id. Called before
    /// the paired `TurnEnd` triggers an autosave, so the save sees this
    /// turn's data.
    pub fn record_snapshot(&mut self, messages: &[Message], claude_session_id: &Option<String>) {
        self.messages = messages.to_vec();
        self.claude_session_id = claude_session_id.clone();
    }

    /// Save the current conversation to disk, refreshing its metadata
    /// first: title (derived only while it is still the placeholder),
    /// timestamp, message count, backend, and model. Called after every
    /// `TurnEnd`.
    pub fn autosave(&mut self, transcript: &mut Transcript, origin: SessionOrigin) {
        if self.current_meta.title == PLACEHOLDER_TITLE {
            self.current_meta.title = crate::session::derive_title(&self.messages, transcript);
        }
        self.current_meta.updated_at = now_timestamp();
        self.current_meta.message_count = self.messages.len();
        self.current_meta.backend = origin.backend.clone();
        self.current_meta.model = origin.model.clone();

        info!(
            session_id = self.current_id.as_str(),
            title = %self.current_meta.title,
            message_count = self.current_meta.message_count,
            block_count = transcript.blocks().len(),
            backend = %self.current_meta.backend,
            model = %self.current_meta.model,
            "session autosave"
        );
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

    /// Save the outgoing conversation before switching away from it.
    /// Skips the save when the transcript is empty. That way opening a new
    /// session twice in a row leaves no empty records behind.
    fn save_outgoing(&mut self, transcript: &mut Transcript, origin: SessionOrigin) {
        if transcript.blocks().is_empty() {
            info!(
                session_id = self.current_id.as_str(),
                "session save skipped: transcript empty"
            );
            return;
        }
        self.autosave(transcript, origin);
    }

    /// Start a fresh, empty conversation and hand back the command that
    /// tells the agent to start over.
    pub fn start_new(
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
    pub fn save_outgoing_and_start_new(
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
        // Read the number after the outgoing save, not before: that save
        // refreshed the saved list, and the outgoing conversation's own
        // number has to be in hand or this one would reuse it.
        let seq = next_seq(&self.saved);
        self.current_meta = fresh_meta(id, seq, &origin);
        self.refresh_saved();
        info!(
            session_id = id.as_str(),
            seq = self.current_meta.seq,
            "session: opened a fresh conversation"
        );
    }

    /// Load a saved conversation. This saves the outgoing one first. It
    /// then installs the loaded transcript and id. It hands back the
    /// command that tells the agent to replay its history. Returns `None`
    /// when the record will not load, leaving the current conversation
    /// alone.
    pub fn load(
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
        info!(
            session_id = self.current_id.as_str(),
            message_count = self.messages.len(),
            block_count = transcript.blocks().len(),
            "session loaded"
        );
        Some(AgentCommand::LoadSession {
            messages: record.messages,
            claude_session_id: record.claude_session_id,
        })
    }

    /// Delete one saved conversation and refresh the list. A failure is
    /// logged and otherwise ignored, matching every other disk error here.
    pub fn delete(&mut self, id: SessionId) {
        info!(session_id = id.as_str(), "session delete requested");
        if let Err(e) = self.store.delete(&id) {
            warn!(error = %e, "failed to delete session");
        }
        self.refresh_saved();
    }
}

/// The number a newly opened conversation takes: one past the highest
/// number on disk. Never reuses a number, so deleting a conversation leaves
/// a gap rather than moving another row's name onto it.
fn next_seq(saved: &[SessionMeta]) -> SessionSeq {
    saved.iter().map(|meta| meta.seq).max().unwrap_or(0) + 1
}

/// A fresh, empty conversation's metadata: its number, placeholder title,
/// message count zero, timestamps set to now, backend and model from the
/// running session.
fn fresh_meta(id: SessionId, seq: SessionSeq, origin: &SessionOrigin) -> SessionMeta {
    let now = now_timestamp();
    SessionMeta {
        id,
        seq,
        title: PLACEHOLDER_TITLE.to_string(),
        created_at: now,
        updated_at: now,
        backend: origin.backend.clone(),
        model: origin.model.clone(),
        message_count: 0,
    }
}
