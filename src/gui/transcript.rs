//! Structured transcript data model for the Chat tab.
//!
//! Replaces the flat list of coloured lines the GUI used to hold. That
//! shape could only express a line's colour, not a message boundary, a
//! nested block, or a value that changes after it was appended (a tool
//! call's output arrives after its start line). This module holds plain
//! data with no GUI toolkit types at all, so it is testable without a
//! window. The GUI wires this model in later. Nothing here reaches into
//! the rendering layer.

use crate::agent::agent_loop::StreamEvent;
use serde::{Deserialize, Deserializer, Serialize};

/// Identifies a `Block` across appends. A newtype over a counter, not a
/// raw index, so a block already appended keeps a stable identity even
/// after later blocks are pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockId(u64);

/// How serious a `Notice` block is. Mirrors the distinct notice colours
/// the flat-line GUI already used. Red marked an error and orange an
/// interrupt. Cyan marked a session reset and grey the turn-end divider.
/// No new categories, just names for what the code already
/// distinguished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Debug,
}

/// One piece of assistant output. `Text` is the model's reply content,
/// `Reasoning` is a thinking-mode chunk. Kept separate so a stream of
/// deltas coalesces within a kind but never merges across kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Span {
    Text(String),
    Reasoning(String),
}

/// The content of one transcript block. Room is intentionally left for a
/// future `Subagent` and `Image` variant: nothing here exhaustively
/// matches `BlockKind` in a way a new variant would silently break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    User {
        text: String,
    },
    Assistant {
        spans: Vec<Span>,
    },
    ToolCall {
        tool: String,
        args: String,
        output: Option<String>,
        is_error: bool,
    },
    Notice {
        text: String,
        severity: Severity,
    },
}

/// One entry in the transcript: a stable id, its content, and whether the
/// GUI has collapsed it. `collapsed` lives on the model, not the view, so
/// a redraw does not need to remember which blocks the user folded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub collapsed: bool,
    pub kind: BlockKind,
}

/// The whole Chat tab's history, as a sequence of blocks instead of
/// coloured lines.
///
/// `Serialize` writes `blocks` and `next_id` only; `open_assistant` is
/// transient stream state and is never written to disk. `Deserialize` is
/// hand-written rather than derived: it recomputes `next_id` as at least
/// one past the highest id present in `blocks`, so a hand-edited or
/// truncated file can never hand out a colliding id on the next append.
/// `open_assistant` always comes back `None` on load, since the stream
/// that was filling it is long gone.
#[derive(Debug, Default, Serialize)]
pub struct Transcript {
    blocks: Vec<Block>,
    next_id: u64,
    /// The id of the `Assistant` block a text or reasoning delta should
    /// land in, if one is still open. Tracked as state instead of found
    /// by scanning, since a stream sends thousands of deltas per turn.
    #[serde(skip)]
    open_assistant: Option<BlockId>,
}

impl<'de> Deserialize<'de> for Transcript {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawTranscript {
            blocks: Vec<Block>,
            #[serde(default)]
            next_id: u64,
        }

        let raw = RawTranscript::deserialize(deserializer)?;
        let min_safe_next_id = raw
            .blocks
            .iter()
            .map(|block| block.id.0 + 1)
            .max()
            .unwrap_or(0);
        Ok(Transcript {
            blocks: raw.blocks,
            next_id: raw.next_id.max(min_safe_next_id),
            open_assistant: None,
        })
    }
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a new block, uncollapsed by default, and return its id.
    pub fn push(&mut self, kind: BlockKind) -> BlockId {
        self.push_collapsed(kind, false)
    }

    /// Append a new block with an explicit initial collapsed state. A tool
    /// call starts collapsed, since the renderer draws it as a one-line
    /// summary the reader opens on demand.
    pub fn push_collapsed(&mut self, kind: BlockKind, collapsed: bool) -> BlockId {
        let id = BlockId(self.next_id);
        self.next_id += 1;
        self.blocks.push(Block {
            id,
            collapsed,
            kind,
        });
        id
    }

    /// Set a block's collapsed flag. The renderer's disclosure toggle
    /// writes through this, so the open or closed choice lives in the
    /// model and survives a repaint.
    pub fn set_collapsed(&mut self, id: BlockId, collapsed: bool) {
        if let Some(block) = self.find_mut(id) {
            block.collapsed = collapsed;
        }
    }

    /// Find a block by id.
    pub fn find(&self, id: BlockId) -> Option<&Block> {
        self.blocks.iter().find(|block| block.id == id)
    }

    /// Find a block by id, for mutation.
    pub fn find_mut(&mut self, id: BlockId) -> Option<&mut Block> {
        self.blocks.iter_mut().find(|block| block.id == id)
    }

    /// Push a span onto an `Assistant` block, coalescing into the last
    /// span when it is the same kind as `span`. A stream of text deltas
    /// then grows one span instead of producing one per delta. A
    /// reasoning delta right after it still starts a new span. Does
    /// nothing if `id` is missing or does not name an `Assistant` block.
    pub fn push_span(&mut self, id: BlockId, span: Span) {
        let Some(block) = self.find_mut(id) else {
            return;
        };
        let BlockKind::Assistant { spans } = &mut block.kind else {
            return;
        };
        push_span_coalescing(spans, span);
    }

    /// Fill in a `ToolCall` block's result once it arrives. Does nothing
    /// if `id` is missing or does not name a `ToolCall` block.
    pub fn complete_tool_call(&mut self, id: BlockId, output: String, is_error: bool) {
        let Some(block) = self.find_mut(id) else {
            return;
        };
        let BlockKind::ToolCall {
            output: slot,
            is_error: error_slot,
            ..
        } = &mut block.kind
        else {
            return;
        };
        *slot = Some(output);
        *error_slot = is_error;
    }

    /// Drop every block, for a session reset. The id counter is not
    /// reset, so a block appended after a clear never collides with one
    /// appended before it.
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.open_assistant = None;
    }

    /// Read the blocks in append order, for rendering.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Apply one `StreamEvent`, mutating the transcript to match. Every
    /// variant is handled explicitly rather than through a catch-all.
    /// So a new variant, such as the `Subagent` case phase 2 adds, fails
    /// to compile here instead of being silently ignored.
    pub fn apply_stream_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Text { text, .. } => self.apply_delta(Span::Text(text)),
            StreamEvent::Reasoning { text, .. } => self.apply_delta(Span::Reasoning(text)),
            StreamEvent::ToolCallStart { tool, args, .. } => {
                self.apply_tool_call_start(tool, args)
            }
            StreamEvent::ToolCallEnd {
                output, is_error, ..
            } => self.apply_tool_call_end(output, is_error),
            StreamEvent::TurnEnd { .. } => self.close_open_assistant(),
            StreamEvent::Interrupted { message } => {
                self.push(BlockKind::Notice {
                    text: message,
                    severity: Severity::Warning,
                });
            }
            StreamEvent::Error { message } => {
                self.push(BlockKind::Notice {
                    text: message,
                    severity: Severity::Error,
                });
            }
            // A session reset is a direct `clear()` call from the GUI,
            // not routed through this method, so there is no block for
            // this variant to produce.
            StreamEvent::SessionReset => {}
            // Repeat iteration boundaries drive the Autopilot tab's own
            // progress readout, not the Chat transcript.
            StreamEvent::RepeatIterationStart { .. } => {}
            StreamEvent::RepeatFinished { .. } => {}
            // Consumed by the GUI's session-state layer before this event
            // reaches the transcript (see `apply_event_side_effects` in
            // `src/gui/mod.rs`). It carries no block to draw.
            StreamEvent::ConversationSnapshot { .. } => {}
        }
    }

    /// Append a text or reasoning delta to the open `Assistant` block,
    /// opening one first if none is open.
    fn apply_delta(&mut self, span: Span) {
        let id = self.open_assistant.unwrap_or_else(|| {
            let id = self.push(BlockKind::Assistant { spans: Vec::new() });
            self.open_assistant = Some(id);
            id
        });
        self.push_span(id, span);
    }

    /// Close the open `Assistant` block, if any, so the next text delta
    /// starts a fresh block instead of joining this one.
    fn close_open_assistant(&mut self) {
        self.open_assistant = None;
    }

    /// A tool call closes whatever `Assistant` block is open, then
    /// appends a new `ToolCall` block awaiting its result.
    fn apply_tool_call_start(&mut self, tool: String, args: String) {
        self.close_open_assistant();
        self.push_collapsed(
            BlockKind::ToolCall {
                tool,
                args,
                output: None,
                is_error: false,
            },
            true,
        );
    }

    /// `StreamEvent::ToolCallEnd` carries no id to match against its
    /// start, so the target is the most recent `ToolCall` block whose
    /// `output` is still `None`. Does nothing if every tool call already
    /// has a result, which should not happen in a well-formed stream.
    fn apply_tool_call_end(&mut self, output: String, is_error: bool) {
        let Some(id) = self.blocks.iter().rev().find_map(|block| match &block.kind {
            BlockKind::ToolCall { output: None, .. } => Some(block.id),
            _ => None,
        }) else {
            return;
        };
        self.complete_tool_call(id, output, is_error);
    }
}

/// Append `span` to `spans`, growing the last entry in place when it is
/// the same `Span` kind, so a delta stream does not produce one span per
/// delta.
fn push_span_coalescing(spans: &mut Vec<Span>, span: Span) {
    match (spans.last_mut(), span) {
        (Some(Span::Text(last)), Span::Text(text)) => last.push_str(&text),
        (Some(Span::Reasoning(last)), Span::Reasoning(text)) => last.push_str(&text),
        (_, span) => spans.push(span),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_stay_stable_across_appends() {
        let mut transcript = Transcript::new();
        let first = transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        let second = transcript.push(BlockKind::User {
            text: "world".into(),
        });
        assert_ne!(first, second);
        assert_eq!(
            transcript.find(first).unwrap().kind,
            BlockKind::User {
                text: "hello".into()
            }
        );
        assert_eq!(
            transcript.find(second).unwrap().kind,
            BlockKind::User {
                text: "world".into()
            }
        );
    }

    #[test]
    fn span_coalescing_merges_same_kind_text() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::Assistant { spans: Vec::new() });
        transcript.push_span(id, Span::Text("hel".into()));
        transcript.push_span(id, Span::Text("lo".into()));
        let BlockKind::Assistant { spans } = &transcript.find(id).unwrap().kind else {
            panic!("expected an Assistant block");
        };
        assert_eq!(spans, &[Span::Text("hello".into())]);
    }

    #[test]
    fn span_coalescing_merges_same_kind_reasoning() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::Assistant { spans: Vec::new() });
        transcript.push_span(id, Span::Reasoning("think".into()));
        transcript.push_span(id, Span::Reasoning("ing".into()));
        let BlockKind::Assistant { spans } = &transcript.find(id).unwrap().kind else {
            panic!("expected an Assistant block");
        };
        assert_eq!(spans, &[Span::Reasoning("thinking".into())]);
    }

    #[test]
    fn span_coalescing_does_not_merge_across_kinds() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::Assistant { spans: Vec::new() });
        transcript.push_span(id, Span::Text("said".into()));
        transcript.push_span(id, Span::Reasoning("thought".into()));
        transcript.push_span(id, Span::Text("said again".into()));
        let BlockKind::Assistant { spans } = &transcript.find(id).unwrap().kind else {
            panic!("expected an Assistant block");
        };
        assert_eq!(
            spans,
            &[
                Span::Text("said".into()),
                Span::Reasoning("thought".into()),
                Span::Text("said again".into()),
            ]
        );
    }

    #[test]
    fn tool_call_fills_in_place() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::ToolCall {
            tool: "Bash".into(),
            args: "ls".into(),
            output: None,
            is_error: false,
        });
        transcript.complete_tool_call(id, "file1\nfile2".into(), false);
        assert_eq!(
            transcript.find(id).unwrap().kind,
            BlockKind::ToolCall {
                tool: "Bash".into(),
                args: "ls".into(),
                output: Some("file1\nfile2".into()),
                is_error: false,
            }
        );
    }

    #[test]
    fn lookup_for_missing_id_returns_none() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        transcript.clear();
        assert!(transcript.find(id).is_none());
    }

    #[test]
    fn clear_empties_the_transcript() {
        let mut transcript = Transcript::new();
        transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        transcript.push(BlockKind::Notice {
            text: "Session reset".into(),
            severity: Severity::Info,
        });
        transcript.clear();
        assert!(transcript.blocks().is_empty());
    }

    #[test]
    fn push_span_on_missing_id_does_nothing() {
        let mut transcript = Transcript::new();
        transcript.push_span(BlockId(999), Span::Text("x".into()));
        assert!(transcript.blocks().is_empty());
    }

    #[test]
    fn complete_tool_call_on_non_tool_block_does_nothing() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        transcript.complete_tool_call(id, "output".into(), true);
        assert_eq!(
            transcript.find(id).unwrap().kind,
            BlockKind::User {
                text: "hello".into()
            }
        );
    }

    #[test]
    fn plain_text_turn_produces_one_assistant_block_with_one_span() {
        let mut transcript = Transcript::new();
        transcript.apply_stream_event(StreamEvent::Text {
            turn: 1,
            text: "hel".into(),
        });
        transcript.apply_stream_event(StreamEvent::Text {
            turn: 1,
            text: "lo".into(),
        });
        transcript.apply_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 10,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });
        let blocks = transcript.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("hello".into())],
            }
        );
    }

    #[test]
    fn interleaved_text_and_reasoning_stay_in_arrival_order() {
        let mut transcript = Transcript::new();
        transcript.apply_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: "thinking".into(),
        });
        transcript.apply_stream_event(StreamEvent::Text {
            turn: 1,
            text: "said".into(),
        });
        transcript.apply_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: "more".into(),
        });
        let blocks = transcript.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].kind,
            BlockKind::Assistant {
                spans: vec![
                    Span::Reasoning("thinking".into()),
                    Span::Text("said".into()),
                    Span::Reasoning("more".into()),
                ],
            }
        );
    }

    #[test]
    fn tool_call_produces_assistant_tool_call_then_new_assistant() {
        let mut transcript = Transcript::new();
        transcript.apply_stream_event(StreamEvent::Text {
            turn: 1,
            text: "before".into(),
        });
        transcript.apply_stream_event(StreamEvent::ToolCallStart {
            turn: 1,
            tool: "Bash".into(),
            args: "ls".into(),
        });
        transcript.apply_stream_event(StreamEvent::ToolCallEnd {
            turn: 1,
            tool: "Bash".into(),
            output: "file1".into(),
            is_error: false,
        });
        transcript.apply_stream_event(StreamEvent::Text {
            turn: 1,
            text: "after".into(),
        });
        let blocks = transcript.blocks();
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("before".into())],
            }
        );
        assert_eq!(
            blocks[1].kind,
            BlockKind::ToolCall {
                tool: "Bash".into(),
                args: "ls".into(),
                output: Some("file1".into()),
                is_error: false,
            }
        );
        assert_eq!(
            blocks[2].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("after".into())],
            }
        );
    }

    #[test]
    fn tool_call_end_sets_is_error() {
        let mut transcript = Transcript::new();
        transcript.apply_stream_event(StreamEvent::ToolCallStart {
            turn: 1,
            tool: "Bash".into(),
            args: "bad-command".into(),
        });
        transcript.apply_stream_event(StreamEvent::ToolCallEnd {
            turn: 1,
            tool: "Bash".into(),
            output: "command not found".into(),
            is_error: true,
        });
        let blocks = transcript.blocks();
        assert_eq!(
            blocks[0].kind,
            BlockKind::ToolCall {
                tool: "Bash".into(),
                args: "bad-command".into(),
                output: Some("command not found".into()),
                is_error: true,
            }
        );
    }

    #[test]
    fn interrupt_produces_a_notice_block_with_interrupt_severity() {
        let mut transcript = Transcript::new();
        transcript.apply_stream_event(StreamEvent::Interrupted {
            message: "Interrupted by user".into(),
        });
        let blocks = transcript.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].kind,
            BlockKind::Notice {
                text: "Interrupted by user".into(),
                severity: Severity::Warning,
            }
        );
    }

    #[test]
    fn a_tool_call_block_starts_collapsed() {
        let mut transcript = Transcript::new();
        transcript.apply_stream_event(StreamEvent::ToolCallStart {
            turn: 1,
            tool: "Bash".into(),
            args: "ls".into(),
        });
        assert!(transcript.blocks()[0].collapsed);
    }

    #[test]
    fn a_user_block_starts_uncollapsed() {
        let mut transcript = Transcript::new();
        transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        assert!(!transcript.blocks()[0].collapsed);
    }

    #[test]
    fn set_collapsed_flips_the_stored_flag() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        transcript.set_collapsed(id, true);
        assert!(transcript.find(id).unwrap().collapsed);
        transcript.set_collapsed(id, false);
        assert!(!transcript.find(id).unwrap().collapsed);
    }

    #[test]
    fn set_collapsed_on_a_missing_id_does_nothing() {
        let mut transcript = Transcript::new();
        transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        transcript.set_collapsed(BlockId(999), true);
        assert!(!transcript.blocks()[0].collapsed);
    }

    #[test]
    fn two_turns_produce_separate_assistant_blocks() {
        let mut transcript = Transcript::new();
        transcript.apply_stream_event(StreamEvent::Text {
            turn: 1,
            text: "first".into(),
        });
        transcript.apply_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 5,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });
        transcript.apply_stream_event(StreamEvent::Text {
            turn: 2,
            text: "second".into(),
        });
        let blocks = transcript.blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("first".into())],
            }
        );
        assert_eq!(
            blocks[1].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("second".into())],
            }
        );
        assert_ne!(blocks[0].id, blocks[1].id);
    }

    #[test]
    fn round_trips_one_block_of_every_kind_through_json() {
        let mut transcript = Transcript::new();
        transcript.push(BlockKind::User {
            text: "hi".into(),
        });
        transcript.push(BlockKind::Assistant {
            spans: vec![
                Span::Text("said".into()),
                Span::Reasoning("thought".into()),
            ],
        });
        transcript.push(BlockKind::ToolCall {
            tool: "Bash".into(),
            args: "ls".into(),
            output: Some("file1".into()),
            is_error: false,
        });
        transcript.push(BlockKind::ToolCall {
            tool: "Bash".into(),
            args: "ls".into(),
            output: None,
            is_error: false,
        });
        transcript.push(BlockKind::Notice {
            text: "warn".into(),
            severity: Severity::Warning,
        });
        transcript.push(BlockKind::Notice {
            text: "err".into(),
            severity: Severity::Error,
        });

        let json = serde_json::to_string(&transcript).unwrap();
        let restored: Transcript = serde_json::from_str(&json).unwrap();

        let original_kinds: Vec<&BlockKind> =
            transcript.blocks().iter().map(|b| &b.kind).collect();
        let restored_kinds: Vec<&BlockKind> =
            restored.blocks().iter().map(|b| &b.kind).collect();
        assert_eq!(original_kinds, restored_kinds);
        let original_ids: Vec<BlockId> = transcript.blocks().iter().map(|b| b.id).collect();
        let restored_ids: Vec<BlockId> = restored.blocks().iter().map(|b| b.id).collect();
        assert_eq!(original_ids, restored_ids);
    }

    #[test]
    fn appending_after_deserialize_never_collides_with_a_loaded_id() {
        let mut transcript = Transcript::new();
        transcript.push(BlockKind::User { text: "a".into() });
        transcript.push(BlockKind::User { text: "b".into() });
        let json = serde_json::to_string(&transcript).unwrap();
        let mut restored: Transcript = serde_json::from_str(&json).unwrap();

        let loaded_ids: Vec<BlockId> = restored.blocks().iter().map(|b| b.id).collect();
        let new_id = restored.push(BlockKind::User {
            text: "c".into(),
        });
        assert!(!loaded_ids.contains(&new_id));
    }

    #[test]
    fn a_bogus_next_id_in_the_json_still_yields_a_fresh_unused_id() {
        let json = r#"{"blocks":[{"id":5,"collapsed":false,"kind":{"User":{"text":"hi"}}}],"next_id":0}"#;
        let mut restored: Transcript = serde_json::from_str(json).unwrap();
        let new_id = restored.push(BlockKind::User {
            text: "next".into(),
        });
        assert_ne!(new_id, BlockId(5));
    }

    #[test]
    fn a_deserialize_with_no_next_id_field_still_yields_a_fresh_unused_id() {
        let json = r#"{"blocks":[{"id":7,"collapsed":false,"kind":{"User":{"text":"hi"}}}]}"#;
        let mut restored: Transcript = serde_json::from_str(json).unwrap();
        let new_id = restored.push(BlockKind::User {
            text: "next".into(),
        });
        assert_ne!(new_id, BlockId(7));
    }

    #[test]
    fn a_deserialized_transcript_has_no_open_assistant_block() {
        let mut transcript = Transcript::new();
        transcript.apply_stream_event(StreamEvent::Text {
            turn: 1,
            text: "first".into(),
        });
        let json = serde_json::to_string(&transcript).unwrap();
        let mut restored: Transcript = serde_json::from_str(&json).unwrap();

        restored.apply_stream_event(StreamEvent::Text {
            turn: 2,
            text: "second".into(),
        });
        let blocks = restored.blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("first".into())],
            }
        );
        assert_eq!(
            blocks[1].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("second".into())],
            }
        );
    }
}
