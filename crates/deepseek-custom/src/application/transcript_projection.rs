use super::dto::{NoticeLevel, TranscriptBlock, TranscriptContent, TranscriptSpan};
use super::transcript::{Block, BlockKind, Severity, Span, Transcript};

pub(super) fn project_transcript(transcript: &Transcript) -> Vec<TranscriptBlock> {
    transcript.blocks().iter().map(project_block).collect()
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
