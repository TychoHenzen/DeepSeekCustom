//! Actor-owned transcript projection and current-session state.

use crate::gui::session_state::SessionState;
use crate::gui::transcript::{Block, BlockKind, Severity, Span, Transcript};

use super::dto::{
    NoticeLevel, PendingSessionSwitch, SessionSummary, TranscriptBlock, TranscriptContent,
    TranscriptSpan,
};

/// Presentation-neutral conversation state temporarily rendered by the native GUI.
///
/// The GUI remains an adapter during migration. It no longer owns these values
/// independently, so a later web adapter can use the same serialized boundary.
pub struct ApplicationSession {
    pub transcript: Transcript,
    pub sessions: SessionState,
    pub turn_active: bool,
    pub pending_switch: Option<crate::gui::PendingSwitch>,
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
            crate::gui::PendingSwitch::New => PendingSessionSwitch::New,
            crate::gui::PendingSwitch::Load(id) => PendingSessionSwitch::Load(id.as_str()),
        })
    }

    pub fn transcript_projection(&self) -> Vec<TranscriptBlock> {
        self.transcript.blocks().iter().map(project_block).collect()
    }
}

fn project_block(block: &Block) -> TranscriptBlock {
    let content = match &block.kind {
        BlockKind::User { text } => TranscriptContent::User {
            text: text.clone(),
            has_image: false,
        },
        BlockKind::Assistant { spans } => TranscriptContent::Assistant {
            spans: spans
                .iter()
                .map(|span| match span {
                    Span::Text(text) => TranscriptSpan::Text(text.clone()),
                    Span::Reasoning(text) => TranscriptSpan::Reasoning(text.clone()),
                })
                .collect(),
        },
        BlockKind::ToolCall {
            tool,
            args,
            output,
            is_error,
        } => TranscriptContent::ToolCall {
            tool: tool.clone(),
            args: args.clone(),
            output: output.clone(),
            is_error: *is_error,
        },
        BlockKind::Notice { text, severity } => TranscriptContent::Notice {
            message: text.clone(),
            level: match severity {
                Severity::Error => NoticeLevel::Error,
                Severity::Warning => NoticeLevel::Warning,
                Severity::Info | Severity::Debug => NoticeLevel::Info,
            },
        },
        BlockKind::Image { image } => TranscriptContent::Image {
            media_type: image.media_type.clone(),
            data: image.data.clone(),
        },
        BlockKind::Subagent {
            subagent_id,
            backend,
            model,
            state,
            transcript,
            ..
        } => TranscriptContent::Subagent {
            name: format!("{subagent_id} ({backend}/{model})"),
            state: format!("{state:?}").to_lowercase(),
            blocks: transcript.blocks().iter().map(project_block).collect(),
        },
    };
    TranscriptBlock {
        id: block.id.as_u64(),
        content,
    }
}
