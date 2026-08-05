//! Structured transcript data model for the Chat tab.
//!
//! Replaces the flat list of coloured lines the GUI used to hold. That
//! shape could only express a line's colour, not a message boundary, a
//! nested block, or a value that changes after it was appended (a tool
//! call's output arrives after its start line). This module holds plain
//! data with no GUI toolkit types at all, so it is testable without a
//! window. The GUI wires this model in later. Nothing here reaches into
//! the rendering layer.

use std::time::Instant;

use crate::agent::agent_loop::{RouteHop, RoutedEvent, StreamEvent, SubagentId, SubagentMeta};
use crate::api::types::ImageAttachment;
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

/// How a subagent dispatch is getting on. Advances only on the events that
/// mean something happened: a `TurnEnd` means `Done`, an `Error` means
/// `Failed`, an `Interrupted` means `Interrupted`. Every other event
/// (text, reasoning, a tool call, a nested dispatch) leaves it at
/// `Running`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentState {
    Running,
    Done,
    Failed,
    Interrupted,
}

/// The content of one transcript block.
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
    /// A pasted, dropped, or tool-read image. Reuses `ImageAttachment`
    /// (`src/api/types.rs`), the same backend-agnostic type a turn's
    /// outgoing content already carries, rather than a second image type
    /// for the transcript alone.
    ///
    /// The base64 `data` inside `ImageAttachment` is what actually lands on
    /// disk when a `Transcript` holding this block is saved to a session
    /// file (see `CLAUDE.md`'s "Session persistence"). That is a real cost:
    /// a screenshot-sized image can be a few hundred KB of base64 text per
    /// block, with no compression beyond what the source PNG or JPEG
    /// already applied. There is no separate on-disk blob store here; the
    /// simplicity of one self-contained JSON file per session, which
    /// `SessionStore` already relies on, was judged to outweigh that cost
    /// for now. A later phase can move image bytes to sidecar files if
    /// session sizes become a problem in practice.
    Image {
        image: ImageAttachment,
    },
    /// One `Task` dispatch. Holds a full transcript of its own, rendered
    /// with the same code as the top level, so a nested dispatch inside a
    /// subagent draws the same way a top-level one does. `subagent_id` is
    /// how `Transcript::apply_routed_event` finds this block again on the
    /// next event for the same dispatch, since a `BlockId` is assigned by
    /// this transcript and a `SubagentId` is assigned by the dispatcher,
    /// two different id spaces that never collide by construction.
    Subagent {
        subagent_id: SubagentId,
        backend: String,
        model: String,
        depth: u32,
        state: SubagentState,
        /// Milliseconds between block creation and the terminal event that
        /// set `state` away from `Running`. Stays `0` while running: there
        /// is no live clock in this plain-data model, only what the last
        /// applied event tells it. `started_at` is what makes the value
        /// possible to compute; it never round-trips to disk, so a
        /// reloaded session's still-running-looking blocks (there
        /// shouldn't be any, a session save happens between turns) would
        /// keep whatever `elapsed_ms` they last had.
        elapsed_ms: u64,
        #[serde(skip)]
        started_at: Option<Instant>,
        transcript: Box<Transcript>,
        /// How many turns this session has run so far, against
        /// `session_turn_cap`. Refreshed on every event that touches this
        /// block, not just at creation, so the header stays current across
        /// a `SendMessage` follow-up the same way it was for the turn that
        /// opened the session. See "both counts visible in the subagent
        /// block header" in the roadmap's Phase 3 section
        /// (`docs/plans/2026-08-04-long-term-roadmap.md`).
        session_turns: u32,
        session_turn_cap: u32,
        /// How many `SendMessage` calls this session's owner has made in
        /// total during its own current turn, across every session it has
        /// open, against `send_message_call_cap`. Same refresh rule as
        /// `session_turns`.
        send_message_calls: u32,
        send_message_call_cap: u32,
    },
}

/// One entry in the transcript: a stable id, its content, whether the GUI
/// has collapsed it, and whether the user has pinned it open. Both flags
/// live on the model, not the view, so a redraw does not need to remember
/// which blocks the user folded or pinned. `pinned` defaults to `false` on
/// a load from an older session file with no such field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub collapsed: bool,
    #[serde(default)]
    pub pinned: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
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
            pinned: false,
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

    /// Find a block by a path of ids, descending through nested `Subagent`
    /// transcripts. A one-element path behaves exactly like `find_mut`. A
    /// longer path treats every id but the last as a `Subagent` block to
    /// descend into: `path[0]` is looked up in `self`, then the remaining
    /// path is looked up in that block's own inner transcript, and so on.
    /// A `BlockId` is only unique within the `Transcript` that assigned
    /// it, since a nested transcript starts its own id counter at zero, so
    /// a path is what makes a block reachable from the top level. Returns
    /// `None` if any hop is missing or is not a `Subagent` block.
    pub fn find_mut_by_path(&mut self, path: &[BlockId]) -> Option<&mut Block> {
        let (first, rest) = path.split_first()?;
        if rest.is_empty() {
            return self.find_mut(*first);
        }
        let Some(Block {
            kind: BlockKind::Subagent { transcript, .. },
            ..
        }) = self.find_mut(*first)
        else {
            return None;
        };
        transcript.find_mut_by_path(rest)
    }

    /// Set the collapsed flag of the block named by `path`. Does nothing if
    /// the path does not resolve to a block. See `find_mut_by_path` for how
    /// a path descends through nested `Subagent` blocks.
    pub fn set_collapsed_by_path(&mut self, path: &[BlockId], collapsed: bool) {
        if let Some(block) = self.find_mut_by_path(path) {
            block.collapsed = collapsed;
        }
    }

    /// Set the pinned flag of the block named by `path`. Does nothing if
    /// the path does not resolve to a block. See `find_mut_by_path` for how
    /// a path descends through nested `Subagent` blocks.
    pub fn set_pinned_by_path(&mut self, path: &[BlockId], pinned: bool) {
        if let Some(block) = self.find_mut_by_path(path) {
            block.pinned = pinned;
        }
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
            StreamEvent::ToolCallStart { tool, args, .. } => self.apply_tool_call_start(tool, args),
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

    /// Apply one `RoutedEvent`. An empty route is a main-session event and
    /// behaves exactly as `apply_stream_event` does. A non-empty route
    /// names a chain of subagent dispatches to descend through: the first
    /// hop's `Subagent` block is found by `subagent_id`, created first if
    /// this is the first event ever seen for that dispatch, and the rest
    /// of the route (and the event) is applied to that block's own inner
    /// `Transcript`, recursively. Depth is driven entirely by the route's
    /// length, so nesting works to any depth with no special case.
    ///
    /// The state badge and elapsed time only advance on the hop the event
    /// is native to: the last hop in the route, where the remaining route
    /// is empty once that hop is consumed. A hop further out than that
    /// only knows a nested dispatch is still going, not that it finished.
    pub fn apply_routed_event(&mut self, routed: RoutedEvent) {
        let RoutedEvent { mut route, event } = routed;
        if route.is_empty() {
            self.apply_stream_event(event);
            return;
        }
        let RouteHop {
            id,
            meta,
            session_turns,
            session_turn_cap,
            send_message_calls,
            send_message_call_cap,
        } = route.remove(0);
        let block_id = self.find_or_create_subagent_block(id, meta);
        self.update_subagent_counts(
            block_id,
            session_turns,
            session_turn_cap,
            send_message_calls,
            send_message_call_cap,
        );
        if route.is_empty() {
            self.update_subagent_state(block_id, &event);
        }
        let Some(Block {
            kind: BlockKind::Subagent { transcript, .. },
            ..
        }) = self.find_mut(block_id)
        else {
            return;
        };
        transcript.apply_routed_event(RoutedEvent { route, event });
    }

    /// Find the `Subagent` block already tracking `subagent_id`, or create
    /// one, collapsed by default, seeded from `meta`. Every `RouteHop`
    /// carries `meta` fresh, so a block is fully formed the instant it is
    /// created: there is no "started" event to wait for separately.
    fn find_or_create_subagent_block(
        &mut self,
        subagent_id: SubagentId,
        meta: SubagentMeta,
    ) -> BlockId {
        let existing = self.blocks.iter().find(|block| {
            matches!(
                &block.kind,
                BlockKind::Subagent { subagent_id: id, .. } if *id == subagent_id
            )
        });
        if let Some(block) = existing {
            return block.id;
        }
        self.push_collapsed(
            BlockKind::Subagent {
                subagent_id,
                backend: meta.backend,
                model: meta.model,
                depth: meta.depth,
                state: SubagentState::Running,
                elapsed_ms: 0,
                started_at: Some(Instant::now()),
                transcript: Box::new(Transcript::new()),
                // Placeholder values: `apply_routed_event` calls
                // `update_subagent_counts` with the real counts from this
                // same hop right after this block is created, so these
                // never render as-is.
                session_turns: 0,
                session_turn_cap: 0,
                send_message_calls: 0,
                send_message_call_cap: 0,
            },
            true,
        )
    }

    /// Refresh a `Subagent` block's live turn and call counts from the
    /// `RouteHop` that named it. Runs on every event that touches the
    /// block, terminal or not, so the header reflects the session's
    /// current standing against both caps for as long as it stays open,
    /// not only once a cap trips. Does nothing if `id` is missing or does
    /// not name a `Subagent` block.
    fn update_subagent_counts(
        &mut self,
        id: BlockId,
        session_turns: u32,
        session_turn_cap: u32,
        send_message_calls: u32,
        send_message_call_cap: u32,
    ) {
        let Some(Block {
            kind:
                BlockKind::Subagent {
                    session_turns: turns,
                    session_turn_cap: turn_cap,
                    send_message_calls: calls,
                    send_message_call_cap: call_cap,
                    ..
                },
            ..
        }) = self.find_mut(id)
        else {
            return;
        };
        *turns = session_turns;
        *turn_cap = session_turn_cap;
        *calls = send_message_calls;
        *call_cap = send_message_call_cap;
    }

    /// Advance a `Subagent` block's state badge and elapsed time when the
    /// event applying to it directly (not to something it dispatched) is
    /// one that means the dispatch is over. Any other event leaves the
    /// block at `Running`.
    fn update_subagent_state(&mut self, id: BlockId, event: &StreamEvent) {
        let new_state = match event {
            StreamEvent::TurnEnd { .. } => SubagentState::Done,
            StreamEvent::Error { .. } => SubagentState::Failed,
            StreamEvent::Interrupted { .. } => SubagentState::Interrupted,
            _ => return,
        };
        let Some(Block {
            kind:
                BlockKind::Subagent {
                    state,
                    elapsed_ms,
                    started_at,
                    ..
                },
            ..
        }) = self.find_mut(id)
        else {
            return;
        };
        *state = new_state;
        if let Some(start) = started_at.take() {
            *elapsed_ms = start.elapsed().as_millis() as u64;
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
        let Some(id) = self
            .blocks
            .iter()
            .rev()
            .find_map(|block| match &block.kind {
                BlockKind::ToolCall { output: None, .. } => Some(block.id),
                _ => None,
            })
        else {
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
    fn an_image_block_round_trips_through_json() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::Image {
            image: ImageAttachment {
                data: "AAA".into(),
                media_type: "image/png".into(),
            },
        });

        let json = serde_json::to_string(&transcript).unwrap();
        let restored: Transcript = serde_json::from_str(&json).unwrap();

        assert_eq!(
            restored.find(id).unwrap().kind,
            BlockKind::Image {
                image: ImageAttachment {
                    data: "AAA".into(),
                    media_type: "image/png".into(),
                },
            }
        );
    }

    /// A session file saved before this variant existed has no `"Image"`
    /// key anywhere in it. It must still load, and the loaded transcript
    /// must still be able to take new blocks afterward.
    #[test]
    fn an_old_session_file_with_no_image_block_still_loads() {
        let json =
            r#"{"blocks":[{"id":3,"collapsed":false,"kind":{"User":{"text":"hi"}}}],"next_id":4}"#;
        let mut restored: Transcript = serde_json::from_str(json).unwrap();
        assert_eq!(restored.blocks().len(), 1);
        let new_id = restored.push(BlockKind::Image {
            image: ImageAttachment {
                data: "BBB".into(),
                media_type: "image/jpeg".into(),
            },
        });
        assert_eq!(restored.blocks().len(), 2);
        assert_ne!(new_id, BlockId(3));
    }

    #[test]
    fn round_trips_one_block_of_every_kind_through_json() {
        let mut transcript = Transcript::new();
        transcript.push(BlockKind::User { text: "hi".into() });
        transcript.push(BlockKind::Assistant {
            spans: vec![Span::Text("said".into()), Span::Reasoning("thought".into())],
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
        transcript.push(BlockKind::Image {
            image: ImageAttachment {
                data: "AAA".into(),
                media_type: "image/png".into(),
            },
        });

        let json = serde_json::to_string(&transcript).unwrap();
        let restored: Transcript = serde_json::from_str(&json).unwrap();

        let original_kinds: Vec<&BlockKind> = transcript.blocks().iter().map(|b| &b.kind).collect();
        let restored_kinds: Vec<&BlockKind> = restored.blocks().iter().map(|b| &b.kind).collect();
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
        let new_id = restored.push(BlockKind::User { text: "c".into() });
        assert!(!loaded_ids.contains(&new_id));
    }

    #[test]
    fn a_bogus_next_id_in_the_json_still_yields_a_fresh_unused_id() {
        let json =
            r#"{"blocks":[{"id":5,"collapsed":false,"kind":{"User":{"text":"hi"}}}],"next_id":0}"#;
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

    #[test]
    fn a_new_block_starts_unpinned() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        assert!(!transcript.find(id).unwrap().pinned);
    }

    #[test]
    fn set_pinned_by_path_flips_a_top_level_block() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::User {
            text: "hello".into(),
        });
        transcript.set_pinned_by_path(&[id], true);
        assert!(transcript.find(id).unwrap().pinned);
        transcript.set_pinned_by_path(&[id], false);
        assert!(!transcript.find(id).unwrap().pinned);
    }

    #[test]
    fn find_mut_by_path_on_a_missing_top_level_id_returns_none() {
        let mut transcript = Transcript::new();
        assert!(transcript.find_mut_by_path(&[BlockId(999)]).is_none());
    }

    fn test_hop(id: SubagentId, depth: u32) -> RouteHop {
        test_hop_with_counts(id, depth, 1, 20, 0, 10)
    }

    fn test_hop_with_counts(
        id: SubagentId,
        depth: u32,
        session_turns: u32,
        session_turn_cap: u32,
        send_message_calls: u32,
        send_message_call_cap: u32,
    ) -> RouteHop {
        RouteHop {
            id,
            meta: SubagentMeta {
                backend: "ollama".into(),
                model: "test-model".into(),
                depth,
            },
            session_turns,
            session_turn_cap,
            send_message_calls,
            send_message_call_cap,
        }
    }

    fn subagent_kind(block: &Block) -> (&str, &str, u32, SubagentState) {
        let BlockKind::Subagent {
            backend,
            model,
            depth,
            state,
            ..
        } = &block.kind
        else {
            panic!("expected a Subagent block");
        };
        (backend.as_str(), model.as_str(), *depth, *state)
    }

    #[test]
    fn an_empty_route_behaves_exactly_like_apply_stream_event() {
        let mut transcript = Transcript::new();
        transcript.apply_routed_event(RoutedEvent::own(StreamEvent::Text {
            turn: 1,
            text: "hello".into(),
        }));
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
    fn a_one_element_route_creates_a_subagent_block_and_fills_its_inner_transcript() {
        let mut transcript = Transcript::new();
        let id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(id, 1)],
            event: StreamEvent::Text {
                turn: 1,
                text: "hi from subagent".into(),
            },
        });

        let blocks = transcript.blocks();
        assert_eq!(blocks.len(), 1);
        let (backend, model, depth, state) = subagent_kind(&blocks[0]);
        assert_eq!(backend, "ollama");
        assert_eq!(model, "test-model");
        assert_eq!(depth, 1);
        assert_eq!(state, SubagentState::Running);

        let BlockKind::Subagent { transcript, .. } = &blocks[0].kind else {
            panic!("expected a Subagent block");
        };
        assert_eq!(
            transcript.blocks()[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("hi from subagent".into())],
            }
        );
    }

    /// The counts on a `RouteHop` land on the `Subagent` block's own
    /// fields, and a later event for the same subagent with different
    /// counts overwrites them: the block reflects the most recent hop, not
    /// just the one that created it. This is what makes the counts live
    /// across a `SendMessage` follow-up rather than frozen at whatever the
    /// session's opening turn reported.
    #[test]
    fn subagent_counts_travel_from_the_route_hop_onto_the_block_and_stay_live() {
        let mut transcript = Transcript::new();
        let id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop_with_counts(id, 1, 1, 20, 0, 10)],
            event: StreamEvent::Text {
                turn: 1,
                text: "opening turn".into(),
            },
        });
        let (turns, turn_cap, calls, call_cap) = subagent_counts(&transcript.blocks()[0]);
        assert_eq!((turns, turn_cap, calls, call_cap), (1, 20, 0, 10));

        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop_with_counts(id, 1, 2, 20, 1, 10)],
            event: StreamEvent::Text {
                turn: 2,
                text: "follow up".into(),
            },
        });
        let (turns, turn_cap, calls, call_cap) = subagent_counts(&transcript.blocks()[0]);
        assert_eq!((turns, turn_cap, calls, call_cap), (2, 20, 1, 10));
    }

    fn subagent_counts(block: &Block) -> (u32, u32, u32, u32) {
        let BlockKind::Subagent {
            session_turns,
            session_turn_cap,
            send_message_calls,
            send_message_call_cap,
            ..
        } = &block.kind
        else {
            panic!("expected a Subagent block");
        };
        (
            *session_turns,
            *session_turn_cap,
            *send_message_calls,
            *send_message_call_cap,
        )
    }

    #[test]
    fn a_turn_end_marks_the_subagent_block_done() {
        let mut transcript = Transcript::new();
        let id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(id, 1)],
            event: StreamEvent::Text {
                turn: 1,
                text: "hi".into(),
            },
        });
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(id, 1)],
            event: StreamEvent::TurnEnd {
                turn: 1,
                finish_reason: "stop".into(),
                total_tokens: 5,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
            },
        });
        let (_, _, _, state) = subagent_kind(&transcript.blocks()[0]);
        assert_eq!(state, SubagentState::Done);
    }

    #[test]
    fn an_error_marks_the_subagent_block_failed() {
        let mut transcript = Transcript::new();
        let id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(id, 1)],
            event: StreamEvent::Error {
                message: "boom".into(),
            },
        });
        let (_, _, _, state) = subagent_kind(&transcript.blocks()[0]);
        assert_eq!(state, SubagentState::Failed);
    }

    #[test]
    fn an_interrupt_marks_the_subagent_block_interrupted() {
        let mut transcript = Transcript::new();
        let id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(id, 1)],
            event: StreamEvent::Interrupted {
                message: "stopped".into(),
            },
        });
        let (_, _, _, state) = subagent_kind(&transcript.blocks()[0]);
        assert_eq!(state, SubagentState::Interrupted);
    }

    #[test]
    fn a_two_element_route_nests_a_subagent_block_inside_the_first() {
        let mut transcript = Transcript::new();
        let outer_id = SubagentId::next();
        let inner_id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(outer_id, 1), test_hop(inner_id, 2)],
            event: StreamEvent::Text {
                turn: 1,
                text: "hi from nested".into(),
            },
        });

        let blocks = transcript.blocks();
        assert_eq!(blocks.len(), 1);
        let (_, _, outer_depth, outer_state) = subagent_kind(&blocks[0]);
        assert_eq!(outer_depth, 1);
        // The outer dispatch is still running: only the innermost hop that
        // an event is native to advances a block's own state.
        assert_eq!(outer_state, SubagentState::Running);

        let BlockKind::Subagent {
            transcript: outer_transcript,
            ..
        } = &blocks[0].kind
        else {
            panic!("expected a Subagent block");
        };
        let inner_blocks = outer_transcript.blocks();
        assert_eq!(inner_blocks.len(), 1);
        let (_, _, inner_depth, _) = subagent_kind(&inner_blocks[0]);
        assert_eq!(inner_depth, 2);

        let BlockKind::Subagent {
            transcript: inner_transcript,
            ..
        } = &inner_blocks[0].kind
        else {
            panic!("expected a nested Subagent block");
        };
        assert_eq!(
            inner_transcript.blocks()[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("hi from nested".into())],
            }
        );
    }

    #[test]
    fn a_two_element_path_reaches_a_block_nested_inside_a_subagent() {
        let mut transcript = Transcript::new();
        let outer_id = SubagentId::next();
        let inner_id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(outer_id, 1), test_hop(inner_id, 2)],
            event: StreamEvent::Text {
                turn: 1,
                text: "hi from nested".into(),
            },
        });

        let outer_block_id = transcript.blocks()[0].id;
        let BlockKind::Subagent {
            transcript: outer_transcript,
            ..
        } = &transcript.blocks()[0].kind
        else {
            panic!("expected a Subagent block");
        };
        let inner_block_id = outer_transcript.blocks()[0].id;

        let path = [outer_block_id, inner_block_id];
        transcript.set_pinned_by_path(&path, true);
        transcript.set_collapsed_by_path(&path, false);

        let BlockKind::Subagent {
            transcript: outer_transcript,
            ..
        } = &transcript.blocks()[0].kind
        else {
            panic!("expected a Subagent block");
        };
        let inner_block = &outer_transcript.blocks()[0];
        assert!(inner_block.pinned);
        assert!(!inner_block.collapsed);
    }

    #[test]
    fn two_sibling_subagents_at_the_same_level_do_not_collide() {
        let mut transcript = Transcript::new();
        let first_id = SubagentId::next();
        let second_id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(first_id, 1)],
            event: StreamEvent::Text {
                turn: 1,
                text: "from first".into(),
            },
        });
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(second_id, 1)],
            event: StreamEvent::Text {
                turn: 1,
                text: "from second".into(),
            },
        });
        // A second event for the first subagent must land back in its own
        // block, not create a third one.
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(first_id, 1)],
            event: StreamEvent::Text {
                turn: 1,
                text: " again".into(),
            },
        });

        let blocks = transcript.blocks();
        assert_eq!(blocks.len(), 2);
        let BlockKind::Subagent {
            transcript: first_transcript,
            ..
        } = &blocks[0].kind
        else {
            panic!("expected a Subagent block");
        };
        assert_eq!(
            first_transcript.blocks()[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("from first again".into())],
            }
        );
        let BlockKind::Subagent {
            transcript: second_transcript,
            ..
        } = &blocks[1].kind
        else {
            panic!("expected a Subagent block");
        };
        assert_eq!(
            second_transcript.blocks()[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("from second".into())],
            }
        );
    }

    #[test]
    fn a_transcript_with_a_nested_subagent_block_round_trips_through_json() {
        let mut transcript = Transcript::new();
        let outer_id = SubagentId::next();
        let inner_id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(outer_id, 1), test_hop(inner_id, 2)],
            event: StreamEvent::Text {
                turn: 1,
                text: "nested text".into(),
            },
        });
        transcript.apply_routed_event(RoutedEvent {
            route: vec![test_hop(outer_id, 1), test_hop(inner_id, 2)],
            event: StreamEvent::TurnEnd {
                turn: 1,
                finish_reason: "stop".into(),
                total_tokens: 3,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
            },
        });

        let json = serde_json::to_string(&transcript).unwrap();
        let restored: Transcript = serde_json::from_str(&json).unwrap();

        let blocks = restored.blocks();
        assert_eq!(blocks.len(), 1);
        let (backend, model, depth, _) = subagent_kind(&blocks[0]);
        assert_eq!(backend, "ollama");
        assert_eq!(model, "test-model");
        assert_eq!(depth, 1);

        let BlockKind::Subagent {
            transcript: inner_transcript,
            ..
        } = &blocks[0].kind
        else {
            panic!("expected a Subagent block");
        };
        let inner_blocks = inner_transcript.blocks();
        assert_eq!(inner_blocks.len(), 1);
        let (_, _, inner_depth, inner_state) = subagent_kind(&inner_blocks[0]);
        assert_eq!(inner_depth, 2);
        assert_eq!(inner_state, SubagentState::Done);
    }
}
