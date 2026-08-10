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

impl BlockId {
    /// Build a `BlockId` with an arbitrary inner value. Production code
    /// only ever receives a `BlockId` back from `Transcript::push`, which
    /// keeps the inner counter private so no caller can forge a colliding
    /// id. A test still needs to construct one directly, to probe a
    /// lookup-miss path against an id nothing produced. That is the only
    /// reason this constructor exists.
    #[cfg(feature = "test-support")]
    pub fn new_for_test(id: u64) -> Self {
        Self(id)
    }
}

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
            // A new iteration closes whatever `Assistant` block is still
            // open, so the first text delta of the new iteration starts a
            // fresh block instead of joining the one before it, then marks
            // the boundary with an `Info` notice and draws the task as the
            // iteration's own `User` block. That block is the only record
            // of what an autopilot iteration was asked to do: the task
            // text never passes through the input box, so nothing else
            // would put it in the transcript, and `derive_title` would
            // find no user message to name the saved session after. The
            // Autopilot tab's own progress readout is still driven
            // separately, from `apply_event_side_effects` in
            // `src/gui/mod.rs`, as is the per-iteration session rotation.
            StreamEvent::RepeatIterationStart { index, total, task } => {
                self.close_open_assistant();
                self.push(BlockKind::Notice {
                    text: format!("Iteration {index} of {total}"),
                    severity: Severity::Info,
                });
                self.push(BlockKind::User { text: task });
            }
            // Marks where a repeat run ended with an `Info` notice. The
            // Autopilot tab's own progress readout is still driven
            // separately, from `apply_event_side_effects` in
            // `src/gui/mod.rs`.
            StreamEvent::RepeatFinished { completed, total } => {
                self.push(BlockKind::Notice {
                    text: format!("Autopilot finished: {completed} of {total} iterations"),
                    severity: Severity::Info,
                });
            }
            // Consumed by the GUI's session-state layer before this event
            // reaches the transcript (see `apply_event_side_effects` in
            // `src/gui/mod.rs`). It carries no block to draw.
            StreamEvent::ConversationSnapshot { .. } => {}
            StreamEvent::Info { message } => {
                self.push(BlockKind::Notice {
                    text: message,
                    severity: Severity::Info,
                });
            }
            // The running search's own tab draws this, live. Pushing a
            // block per scored candidate would bury the conversation under
            // a few hundred notices and grow the saved session with them.
            StreamEvent::SearchProgress(_) => {}
            // The result does belong in the conversation: a finished run is
            // a thing that happened, and the summary carries the winner and
            // the whole archive, so a saved session records it.
            StreamEvent::SearchFinished {
                kind,
                summary,
                is_error,
            } => {
                self.push(BlockKind::Notice {
                    text: format!("{} finished\n{summary}", kind.label()),
                    severity: if is_error {
                        Severity::Warning
                    } else {
                        Severity::Info
                    },
                });
            }
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
