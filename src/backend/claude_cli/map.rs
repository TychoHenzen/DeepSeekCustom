//! Maps `ClaudeEvent` values, parsed from a `claude -p` stream-json session,
//! onto the `StreamEvent` values the GUI already knows how to render.
//!
//! This module is pure: no I/O, no process spawning. It only tracks the
//! small bit of state needed to turn one protocol into the other: a turn
//! counter, a map from tool_use id to tool name, and the session id from
//! the init event.

use std::collections::HashMap;

use crate::agent::agent_loop::StreamEvent;

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
            ClaudeEvent::System(_)
            | ClaudeEvent::RateLimitEvent(_)
            | ClaudeEvent::Unknown(_) => Vec::new(),
        }
    }

    fn map_stream_event(&mut self, inner: InnerStreamEvent) -> Vec<StreamEvent> {
        match inner {
            InnerStreamEvent::ContentBlockDelta { index, delta } => match delta {
                ContentDelta::TextDelta { text } => vec![StreamEvent::Text {
                    turn: self.turn,
                    text,
                }],
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::events::parse_line;

    const TEXT_ONLY_FIXTURE: &str =
        include_str!("../../../tests/fixtures/claude_stream_json.jsonl");
    const TOOLS_FIXTURE: &str =
        include_str!("../../../tests/fixtures/claude_stream_json_tools.jsonl");

    fn map_fixture(fixture: &str) -> Vec<StreamEvent> {
        let mut mapper = EventMapper::new();
        let mut out = Vec::new();
        for line in fixture.lines() {
            if let Some(event) = parse_line(line) {
                out.extend(mapper.map(event));
            }
        }
        out
    }

    fn count_deltas(fixture: &str, is_text: bool) -> usize {
        let mut count = 0;
        for line in fixture.lines() {
            if let Some(ClaudeEvent::StreamEvent(envelope)) = parse_line(line)
                && let InnerStreamEvent::ContentBlockDelta { delta, .. } = envelope.event
            {
                match (&delta, is_text) {
                    (ContentDelta::TextDelta { .. }, true) => count += 1,
                    (ContentDelta::ThinkingDelta { .. }, false) => count += 1,
                    _ => {}
                }
            }
        }
        count
    }

    #[test]
    fn text_only_fixture_yields_reasoning_text_and_one_turn_end() {
        let events = map_fixture(TEXT_ONLY_FIXTURE);

        let reasoning_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Reasoning { .. }))
            .count();
        let text_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Text { .. }))
            .count();
        let turn_end_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::TurnEnd { .. }))
            .count();
        let tool_call_start_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::ToolCallStart { .. }))
            .count();

        assert!(reasoning_count >= 1, "expected at least one Reasoning event");
        assert!(text_count >= 1, "expected at least one Text event");
        assert_eq!(turn_end_count, 1, "expected exactly one TurnEnd");
        assert_eq!(tool_call_start_count, 0, "expected zero ToolCallStart");

        let combined: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(!combined.is_empty(), "expected non-empty concatenated text");
    }

    #[test]
    fn tools_fixture_yields_one_tool_call_start_and_end() {
        let events = map_fixture(TOOLS_FIXTURE);

        let start_index = events
            .iter()
            .position(|e| matches!(e, StreamEvent::ToolCallStart { tool, .. } if tool == "Read"));
        let end_index = events
            .iter()
            .position(|e| matches!(e, StreamEvent::ToolCallEnd { tool, .. } if tool == "Read"));

        let start_index = start_index.expect("expected a ToolCallStart with tool Read");
        let end_index = end_index.expect("expected a ToolCallEnd with tool Read");
        assert!(start_index < end_index, "ToolCallStart should precede ToolCallEnd");

        let start_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::ToolCallStart { .. }))
            .count();
        let end_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::ToolCallEnd { .. }))
            .count();
        assert_eq!(start_count, 1, "expected exactly one ToolCallStart");
        assert_eq!(end_count, 1, "expected exactly one ToolCallEnd");

        match &events[end_index] {
            StreamEvent::ToolCallEnd { output, is_error, .. } => {
                assert!(!is_error, "expected is_error to be false");
                assert!(
                    output.contains("[package]"),
                    "expected tool output to contain [package], got: {output}"
                );
            }
            other => panic!("expected ToolCallEnd, got {other:?}"),
        }

        let turn_end_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::TurnEnd { .. }))
            .count();
        assert_eq!(turn_end_count, 1, "expected exactly one TurnEnd");
    }

    #[test]
    fn tools_fixture_tool_call_start_carries_real_arguments() {
        let events = map_fixture(TOOLS_FIXTURE);

        let start = events
            .iter()
            .find(|e| matches!(e, StreamEvent::ToolCallStart { tool, .. } if tool == "Read"))
            .expect("expected a ToolCallStart with tool Read");

        match start {
            StreamEvent::ToolCallStart { args, .. } => {
                let parsed: serde_json::Value =
                    serde_json::from_str(args).unwrap_or_else(|e| {
                        panic!("expected args to parse as JSON, got {args:?}: {e}")
                    });
                let file_path = parsed
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("expected a file_path key in args, got {args:?}"));
                assert!(
                    file_path.ends_with("Cargo.toml"),
                    "expected file_path to end with Cargo.toml, got: {file_path}"
                );
            }
            other => panic!("expected ToolCallStart, got {other:?}"),
        }
    }

    #[test]
    fn tools_fixture_turn_end_carries_nonzero_cache_miss_tokens() {
        let events = map_fixture(TOOLS_FIXTURE);
        let turn_end = events
            .iter()
            .find(|e| matches!(e, StreamEvent::TurnEnd { .. }))
            .expect("expected a TurnEnd event");
        match turn_end {
            StreamEvent::TurnEnd {
                prompt_cache_miss_tokens,
                ..
            } => {
                assert!(
                    *prompt_cache_miss_tokens > 0,
                    "expected nonzero prompt_cache_miss_tokens"
                );
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
    }

    #[test]
    fn session_id_is_captured_from_init_event_for_both_fixtures() {
        for fixture in [TEXT_ONLY_FIXTURE, TOOLS_FIXTURE] {
            let mut mapper = EventMapper::new();
            for line in fixture.lines() {
                if let Some(event) = parse_line(line) {
                    mapper.map(event);
                }
            }
            assert!(
                mapper.session_id().is_some(),
                "expected a session id to be captured"
            );
        }
    }

    #[test]
    fn unannounced_tool_use_id_maps_to_unknown_tool() {
        let mut mapper = EventMapper::new();
        let line = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"never_seen","type":"tool_result","content":"hi"}]}}"#;
        let event = parse_line(line).expect("line should parse");
        let events = mapper.map(event);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolCallEnd { tool, .. } => assert_eq!(tool, "unknown"),
            other => panic!("expected ToolCallEnd, got {other:?}"),
        }
    }

    #[test]
    fn no_fixture_duplicates_replies() {
        for fixture in [TEXT_ONLY_FIXTURE, TOOLS_FIXTURE] {
            let events = map_fixture(fixture);
            let text_event_count = events
                .iter()
                .filter(|e| matches!(e, StreamEvent::Text { .. }))
                .count();
            let text_delta_count = count_deltas(fixture, true);
            assert_eq!(
                text_event_count, text_delta_count,
                "Text event count should equal text_delta count"
            );
        }
    }
}
