//! Actor-owned transcript projection and current-session state.

use super::session_state::SessionState;
use super::transcript::Transcript;

use super::dto::{PendingSessionSwitch, SessionSummary, TranscriptBlock};

/// Presentation-neutral conversation state rendered through the web adapter.
pub struct ApplicationSession {
    pub transcript: Transcript,
    pub sessions: SessionState,
    pub turn_active: bool,
    pub pending_switch: Option<PendingSwitch>,
}

impl ApplicationSession {
    pub fn new(sessions: SessionState) -> Self {
        Self {
            transcript: Transcript::new(),
            sessions,
            turn_active: false,
            pending_switch: None,
        }
    }

    pub fn session_summary(&self) -> SessionSummary {
        let meta = self.sessions.current_meta();
        SessionSummary {
            id: meta.id.as_str(),
            title: meta.title.clone(),
            backend: meta.backend.clone(),
            model: meta.model.clone(),
        }
    }

    pub fn saved_session_summaries(&self) -> Vec<SessionSummary> {
        self.sessions
            .saved()
            .iter()
            .map(|meta| SessionSummary {
                id: meta.id.as_str(),
                title: meta.title.clone(),
                backend: meta.backend.clone(),
                model: meta.model.clone(),
            })
            .collect()
    }

    pub fn pending_session_switch(&self) -> Option<PendingSessionSwitch> {
        self.pending_switch.as_ref().map(|pending| match pending {
            PendingSwitch::New => PendingSessionSwitch::New,
            PendingSwitch::Load(id) => PendingSessionSwitch::Load(id.as_str()),
        })
    }

    pub fn transcript_projection(&self) -> Vec<TranscriptBlock> {
        super::transcript_projection::project_transcript(&self.transcript)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PendingSwitch {
    New,
    Load(crate::session::SessionId),
}
