//! GUI session state: the current session id and meta, the saved-session
//! list, and the save/switch/load/reset methods that keep them consistent
//! with the transcript and the disk-backed `SessionStore`.
//!
//! No UI lives here. This only maintains state so a later step can render
//! it in a Sessions tab.

use tracing::warn;

use crate::agent::agent_loop::AgentCommand;
use crate::api::types::Message;
use crate::session::{SessionId, SessionMeta, SessionRecord, now_timestamp};

use super::DeepSeekGui;

impl DeepSeekGui {
    /// Build a fresh, empty session's metadata: placeholder title, message
    /// count zero, timestamps set to now, backend and model taken from the
    /// running session.
    pub(super) fn fresh_session_meta(&self, id: SessionId) -> SessionMeta {
        let now = now_timestamp();
        SessionMeta {
            id,
            title: "New conversation".to_string(),
            created_at: now,
            updated_at: now,
            backend: self.active_backend.clone().unwrap_or_default(),
            model: self.model.clone(),
            message_count: 0,
        }
    }

    /// Capture a `ConversationSnapshot` event: the latest API history and,
    /// on a `claude_cli` session, the child's own session id. Called from
    /// `apply_event_side_effects` before the paired `TurnEnd` triggers an
    /// autosave, so the save sees this turn's data.
    pub(super) fn record_conversation_snapshot(
        &mut self,
        messages: &[Message],
        claude_session_id: &Option<String>,
    ) {
        self.current_messages = messages.to_vec();
        self.current_claude_session_id = claude_session_id.clone();
    }

    /// Save the current session to disk, refreshing its meta first: title
    /// (derived only while it is still the placeholder), timestamp, and
    /// message count. Called after every `TurnEnd`.
    pub(super) fn autosave_current_session(&mut self) {
        if self.current_session_meta.title == "New conversation" {
            self.current_session_meta.title =
                crate::session::derive_title(&self.current_messages, &self.transcript);
        }
        self.current_session_meta.updated_at = now_timestamp();
        self.current_session_meta.message_count = self.current_messages.len();
        self.current_session_meta.backend = self.active_backend.clone().unwrap_or_default();
        self.current_session_meta.model = self.model.clone();

        self.write_current_session_to_disk();
        self.refresh_saved_sessions();
    }

    /// Serialize the current session and write it, without touching meta.
    /// Moves the transcript out and back rather than cloning it, since
    /// `Transcript` carries no `Clone` impl.
    fn write_current_session_to_disk(&mut self) {
        let transcript = std::mem::take(&mut self.transcript);
        let record = SessionRecord {
            meta: self.current_session_meta.clone(),
            messages: self.current_messages.clone(),
            transcript,
            claude_session_id: self.current_claude_session_id.clone(),
        };
        let result = self.session_store.save(&record);
        self.transcript = record.transcript;
        if let Err(e) = result {
            warn!(error = %e, "failed to save session");
        }
    }

    /// Refresh the saved-session list from disk.
    fn refresh_saved_sessions(&mut self) {
        self.saved_sessions = self.session_store.list();
    }

    /// Save the outgoing conversation before switching away from it. Skips
    /// the save when the transcript is empty, so opening a new session
    /// twice in a row does not litter the sessions directory with nothing
    /// records.
    fn save_outgoing_session(&mut self) {
        if self.transcript.blocks().is_empty() {
            return;
        }
        self.autosave_current_session();
    }

    /// Start a fresh, empty conversation: save the outgoing one, clear the
    /// transcript and conversation state, generate a new session id, tell
    /// the agent to start over, and refresh the saved list.
    pub(super) fn start_new_session(&mut self) {
        self.save_outgoing_and_start_new();
        let _ = self.tx_input.send(AgentCommand::NewSession);
    }

    /// Load a saved conversation: save the outgoing one, install the
    /// loaded transcript and id, and tell the agent to replay its history.
    pub(super) fn load_session(&mut self, id: SessionId) {
        self.save_outgoing_session();
        let record = match self.session_store.load(&id) {
            Ok(record) => record,
            Err(e) => {
                warn!(error = %e, "failed to load session");
                return;
            }
        };

        self.transcript = record.transcript;
        self.current_messages = record.messages.clone();
        self.current_claude_session_id = record.claude_session_id.clone();
        self.current_session_id = record.meta.id;
        self.current_session_meta = record.meta;
        let _ = self.tx_input.send(AgentCommand::LoadSession {
            messages: record.messages,
            claude_session_id: record.claude_session_id,
        });
        self.refresh_saved_sessions();
    }

    /// Route a self-triggered session reset (the agent's `Reset` tool)
    /// through the same save-then-start-new path a manual New Chat takes,
    /// so the closed conversation lands in a saved session instead of
    /// being discarded outright.
    pub(super) fn save_outgoing_and_start_new(&mut self) {
        self.save_outgoing_session();
        self.transcript.clear();
        self.current_messages.clear();
        self.current_claude_session_id = None;
        let id = SessionId::new();
        self.current_session_id = id;
        self.current_session_meta = self.fresh_session_meta(id);
        self.refresh_saved_sessions();
    }
}
