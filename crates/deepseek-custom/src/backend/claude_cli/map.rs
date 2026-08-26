//! Maps `ClaudeEvent` values, parsed from a `claude -p` stream-json session,
//! onto the `StreamEvent` values the GUI already knows how to render.
//!
//! This module is pure: no I/O, no process spawning. It only tracks the
//! small bit of state needed to turn one protocol into the other: a turn
//! counter, a map from tool_use id to tool name, and the session id from
//! the init event.

use std::collections::HashMap;

use crate::agent::events::StreamEvent;

use super::events::ClaudeEvent;
use super::stream::{ContentBlock, ContentDelta, InnerStreamEvent};

/// A tool call announced by `content_block_start`, whose arguments are still
/// arriving as `input_json_delta` fragments keyed by content block index.
struct PendingToolCall {
    name: String,
    buffer: String,
}

/// The arguments a `content_block_start` announced, if it announced any.
/// An empty object, a missing `input`, and any non-object value all yield
/// an empty buffer, since only a filled object is usable as arguments.
fn announced_arguments(input: &serde_json::Value) -> String {
    match input.as_object() {
        Some(map) if !map.is_empty() => input.to_string(),
        _ => String::new(),
    }
}

/// Converts `ClaudeEvent`s into `StreamEvent`s, tracking the turn counter,
/// the tool_use id to tool name map, and the session id along the way.
pub struct EventMapper {
    turn: u32,
    tool_names: HashMap<String, String>,
    session_id: Option<String>,
    pending_tool_calls: HashMap<u32, PendingToolCall>,
}

impl EventMapper {
    pub fn new() -> Self {
        Self {
            turn: 0,
            tool_names: HashMap::new(),
            session_id: None,
            pending_tool_calls: HashMap::new(),
        }
    }

    /// The session id captured from the `system`/`init` event, if one has
    /// been seen yet.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Map one `ClaudeEvent` to zero or more `StreamEvent`s.
    pub fn map(&mut self, event: ClaudeEvent) -> Vec<StreamEvent> {
        match event {
            ClaudeEvent::StreamEvent(envelope) => self.map_stream_event(envelope.event),
            ClaudeEvent::Assistant(data) => self.map_assistant(data),
            ClaudeEvent::User(data) => self.map_user(data),
            ClaudeEvent::Result(data) => self.map_result(data),
            ClaudeEvent::System(super::events::SystemEvent::Init(init)) => {
                self.session_id = init.session_id;
                Vec::new()
            }
            ClaudeEvent::System(_) | ClaudeEvent::RateLimitEvent(_) | ClaudeEvent::Unknown(_) => {
                Vec::new()
            }
        }
    }

    fn map_stream_event(&mut self, inner: InnerStreamEvent) -> Vec<StreamEvent> {
        match inner {
            InnerStreamEvent::ContentBlockDelta { index, delta } => match delta {
                ContentDelta::TextDelta { text } => vec![StreamEvent::Text {
                    turn: self.turn,
                    text,
                }],
                // A `thinking_delta` can carry no text at all. During a
                // redacted-thinking phase the API sends only pings, which
                // arrive here as an empty `thinking` field beside an
                // `estimated_tokens` count. Emitting a `Reasoning` event
                // for one drew an empty "Reasoning" fold in the
                // transcript, promising content that does not exist. The
                // spawn flags cover the other cause of an empty field:
                // see `--thinking-display` in `args.rs`'s `build_args`.
                ContentDelta::ThinkingDelta { thinking } if thinking.is_empty() => Vec::new(),
                ContentDelta::ThinkingDelta { thinking } => vec![StreamEvent::Reasoning {
                    turn: self.turn,
                    text: thinking,
                }],
                ContentDelta::InputJsonDelta { partial_json } => {
                    if let Some(pending) = self.pending_tool_calls.get_mut(&index) {
                        pending.buffer.push_str(&partial_json);
                    }
                    Vec::new()
                }
                ContentDelta::SignatureDelta { .. } => Vec::new(),
                ContentDelta::Unknown => Vec::new(),
            },
            InnerStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => self.map_content_block_start(index, content_block),
            InnerStreamEvent::ContentBlockStop { index } => self.map_content_block_stop(index),
            _ => Vec::new(),
        }
    }

    fn map_content_block_start(&mut self, index: u32, block: ContentBlock) -> Vec<StreamEvent> {
        match block {
            ContentBlock::ToolUse { id, name, input } => {
                self.tool_names.insert(id, name.clone());
                // The arguments are still an empty object at this point in
                // the protocol. Buffer the call and emit `ToolCallStart` at
                // `content_block_stop`, once `input_json_delta` fragments
                // have accumulated into the real arguments. Fall back to the
                // announced `input` in case no delta ever arrives, but only
                // when it already holds arguments. Seeding the buffer with
                // anything else, `null` above all, would leave that text in
                // front of the fragments and make the arguments unparseable.
                self.pending_tool_calls.insert(
                    index,
                    PendingToolCall {
                        name,
                        buffer: announced_arguments(&input),
                    },
                );
                Vec::new()
            }
            ContentBlock::Text { .. } | ContentBlock::Thinking { .. } | ContentBlock::Unknown => {
                Vec::new()
            }
        }
    }

    fn map_content_block_stop(&mut self, index: u32) -> Vec<StreamEvent> {
        let Some(pending) = self.pending_tool_calls.remove(&index) else {
            return Vec::new();
        };
        let args = if pending.buffer.is_empty() {
            "{}".to_string()
        } else {
            pending.buffer
        };
        vec![StreamEvent::ToolCallStart {
            turn: self.turn,
            tool: pending.name,
            args,
        }]
    }

    /// The completed `assistant` message repeats content already streamed
    /// as deltas via `content_block_start`/`content_block_delta`, so this
    /// only records id-to-name mappings for any `tool_use` block it carries.
    /// It never emits a `ToolCallStart`, to avoid a duplicate: that event is
    /// only ever emitted from `content_block_start`, where `input` is still
    /// an empty object.
    fn map_assistant(&mut self, data: super::events::AssistantEventData) -> Vec<StreamEvent> {
        for block in data.message.content {
            if let ContentBlock::ToolUse { id, name, .. } = block {
                self.tool_names.insert(id, name);
            }
        }
        Vec::new()
    }

    fn map_user(&mut self, data: super::events::UserEventData) -> Vec<StreamEvent> {
        data.message
            .content
            .into_iter()
            .map(|block| {
                let tool = self
                    .tool_names
                    .get(&block.tool_use_id)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let output = block.content_as_string();
                StreamEvent::ToolCallEnd {
                    turn: self.turn,
                    tool,
                    output,
                    is_error: block.is_error,
                }
            })
            .collect()
    }

    fn map_result(&mut self, data: super::events::ResultEventData) -> Vec<StreamEvent> {
        let turn = self.turn;
        let mut events = Vec::new();
        if data.is_error {
            let message = data
                .stop_reason
                .clone()
                .or_else(|| data.subtype.clone())
                .unwrap_or_else(|| "unknown error".to_string());
            events.push(StreamEvent::Error { message });
        }
        let usage = data.usage.unwrap_or_default();
        // No `MessageHistory` exists on this path, so `messages` is always
        // empty here. The display transcript plus `claude_session_id` is
        // the whole conversation for a `claude_cli` session, so an empty
        // vector is correct, not a gap.
        events.push(StreamEvent::ConversationSnapshot {
            messages: Vec::new(),
            claude_session_id: self.session_id.clone(),
        });
        events.push(StreamEvent::TurnEnd {
            turn,
            finish_reason: data.stop_reason.unwrap_or_else(|| "end_turn".to_string()),
            total_tokens: (usage.input_tokens + usage.output_tokens) as usize,
            prompt_cache_hit_tokens: usage.cache_read_input_tokens as u32,
            prompt_cache_miss_tokens: usage.cache_creation_input_tokens as u32,
        });
        self.turn += 1;
        events
    }
}

impl Default for EventMapper {
    fn default() -> Self {
        Self::new()
    }
}
