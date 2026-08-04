//! Line protocol for `claude -p --output-format stream-json`.
//!
//! Every event is one JSON object on one line, discriminated by a top-level
//! `"type"` field, and for `"system"` further by a `"subtype"` field. This
//! module only parses the protocol. It has no dependency on the agent, GUI,
//! or process modules.

use serde::Deserialize;

use super::stream::{StreamEventEnvelope, Usage};

/// One parsed line of the stream-json protocol.
#[derive(Debug, Clone)]
pub enum ClaudeEvent {
    System(SystemEvent),
    RateLimitEvent(RateLimitEventData),
    StreamEvent(StreamEventEnvelope),
    Assistant(AssistantEventData),
    User(UserEventData),
    Result(ResultEventData),
    /// A `"type"` value this parser does not recognize. Carries the raw
    /// type string so a newly added event kind is ignored, not fatal.
    Unknown(String),
}

/// Parse one line of the stream-json protocol.
///
/// Returns `None` for a blank line, a line that is not JSON, or a line that
/// does not start with `{` after trimming. Never panics, never returns an
/// error: a malformed line is skipped so one bad line cannot kill a live
/// session.
pub fn parse_line(line: &str) -> Option<ClaudeEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let type_str = value.get("type")?.as_str()?;

    match type_str {
        "system" => parse_system(&value).map(ClaudeEvent::System),
        "rate_limit_event" => serde_json::from_value(value)
            .ok()
            .map(ClaudeEvent::RateLimitEvent),
        "stream_event" => serde_json::from_value(value)
            .ok()
            .map(ClaudeEvent::StreamEvent),
        "assistant" => serde_json::from_value(value)
            .ok()
            .map(ClaudeEvent::Assistant),
        "user" => serde_json::from_value(value).ok().map(ClaudeEvent::User),
        "result" => serde_json::from_value(value)
            .ok()
            .map(ClaudeEvent::Result),
        other => Some(ClaudeEvent::Unknown(other.to_string())),
    }
}

fn parse_system(value: &serde_json::Value) -> Option<SystemEvent> {
    let subtype = value.get("subtype")?.as_str()?;
    match subtype {
        "init" => serde_json::from_value(value.clone()).ok().map(SystemEvent::Init),
        "status" => serde_json::from_value(value.clone())
            .ok()
            .map(SystemEvent::Status),
        "thinking_tokens" => serde_json::from_value(value.clone())
            .ok()
            .map(SystemEvent::ThinkingTokens),
        "hook_started" => serde_json::from_value(value.clone())
            .ok()
            .map(SystemEvent::HookStarted),
        "hook_response" => serde_json::from_value(value.clone())
            .ok()
            .map(SystemEvent::HookResponse),
        other => Some(SystemEvent::Unknown(other.to_string())),
    }
}

/// A `"type":"system"` event, dispatched further by its `"subtype"` field.
#[derive(Debug, Clone)]
pub enum SystemEvent {
    Init(SystemInit),
    Status(SystemStatus),
    ThinkingTokens(SystemThinkingTokens),
    HookStarted(SystemHookStarted),
    HookResponse(SystemHookResponse),
    /// A `"subtype"` value this parser does not recognize.
    Unknown(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemInit {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemStatus {
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemThinkingTokens {
    #[serde(default)]
    pub estimated_tokens: i64,
    #[serde(default)]
    pub estimated_tokens_delta: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemHookStarted {
    #[serde(default)]
    pub hook_name: Option<String>,
    #[serde(default)]
    pub hook_event: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemHookResponse {
    #[serde(default)]
    pub hook_name: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitEventData {
    #[serde(default)]
    pub rate_limit_info: Option<serde_json::Value>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantEventData {
    pub message: AssistantMessage,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub content: Vec<super::stream::ContentBlock>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserEventData {
    pub message: UserMessage,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserMessage {
    #[serde(default)]
    pub content: Vec<ToolResultBlock>,
}

/// A `tool_result` content block. Its `content` field may be a plain string
/// or an array of blocks, so it is kept as a raw `serde_json::Value`. Use
/// [`ToolResultBlock::content_as_string`] to render it.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    #[serde(default)]
    pub content: serde_json::Value,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResultBlock {
    /// Render `content` to a plain string, whether it arrived as a string
    /// or as an array of blocks.
    pub fn content_as_string(&self) -> String {
        match &self.content {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultEventData {
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub duration_api_ms: Option<u64>,
    #[serde(default)]
    pub num_turns: Option<u64>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub result: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::super::stream::{ContentBlock, ContentDelta, InnerStreamEvent};
    use super::*;

    const TEXT_ONLY_FIXTURE: &str =
        include_str!("../../../tests/fixtures/claude_stream_json.jsonl");
    const TOOLS_FIXTURE: &str =
        include_str!("../../../tests/fixtures/claude_stream_json_tools.jsonl");

    #[test]
    fn every_line_of_text_only_fixture_parses() {
        let mut skipped = 0;
        for line in TEXT_ONLY_FIXTURE.lines() {
            if parse_line(line).is_none() {
                skipped += 1;
            }
        }
        assert_eq!(skipped, 0, "expected every fixture line to parse");
    }

    #[test]
    fn every_line_of_tools_fixture_parses() {
        let mut skipped = 0;
        for line in TOOLS_FIXTURE.lines() {
            if parse_line(line).is_none() {
                skipped += 1;
            }
        }
        assert_eq!(skipped, 0, "expected every fixture line to parse");
    }

    #[test]
    fn text_only_fixture_has_text_and_thinking_deltas() {
        let mut has_text_delta = false;
        let mut has_thinking_delta = false;
        for line in TEXT_ONLY_FIXTURE.lines() {
            if let Some(ClaudeEvent::StreamEvent(envelope)) = parse_line(line)
                && let InnerStreamEvent::ContentBlockDelta { delta, .. } = envelope.event
            {
                match delta {
                    ContentDelta::TextDelta { .. } => has_text_delta = true,
                    ContentDelta::ThinkingDelta { .. } => has_thinking_delta = true,
                    _ => {}
                }
            }
        }
        assert!(has_text_delta, "expected at least one text_delta");
        assert!(has_thinking_delta, "expected at least one thinking_delta");
    }

    #[test]
    fn tools_fixture_has_one_read_tool_use_start_and_matching_result() {
        let mut tool_use_id = None;
        let mut tool_use_starts = 0;
        for line in TOOLS_FIXTURE.lines() {
            if let Some(ClaudeEvent::StreamEvent(envelope)) = parse_line(line)
                && let InnerStreamEvent::ContentBlockStart { content_block, .. } = envelope.event
                && let ContentBlock::ToolUse { id, name, .. } = content_block
            {
                assert_eq!(name, "Read");
                tool_use_id = Some(id);
                tool_use_starts += 1;
            }
        }
        assert_eq!(tool_use_starts, 1, "expected exactly one tool_use start");
        let expected_id = tool_use_id.expect("expected a tool_use content_block_start");

        let mut tool_results = 0;
        for line in TOOLS_FIXTURE.lines() {
            if let Some(ClaudeEvent::User(user_event)) = parse_line(line) {
                for block in &user_event.message.content {
                    assert_eq!(block.tool_use_id, expected_id);
                    tool_results += 1;
                }
            }
        }
        assert_eq!(tool_results, 1, "expected exactly one tool_result block");
    }

    #[test]
    fn tools_fixture_has_one_result_with_cache_creation_tokens() {
        let mut results = 0;
        for line in TOOLS_FIXTURE.lines() {
            if let Some(ClaudeEvent::Result(result)) = parse_line(line) {
                results += 1;
                let usage = result.usage.expect("result should carry usage");
                assert!(usage.cache_creation_input_tokens > 0);
            }
        }
        assert_eq!(results, 1, "expected exactly one result event");
    }

    #[test]
    fn blank_line_returns_none() {
        assert!(parse_line("").is_none());
    }

    #[test]
    fn whitespace_only_line_returns_none() {
        assert!(parse_line("   \t  ").is_none());
    }

    #[test]
    fn plain_prose_line_returns_none() {
        assert!(parse_line("this is not json at all").is_none());
    }

    #[test]
    fn truncated_json_fragment_returns_none() {
        assert!(parse_line(r#"{"type":"system","subtype":"#).is_none());
    }

    #[test]
    fn init_event_yields_session_id_amid_unrelated_fields() {
        let line = r#"{"type":"system","subtype":"init","cwd":"C:\\repo","session_id":"c18eb67f-6873-45a4-aa7a-8755cecb4361","tools":[],"mcp_servers":[{"name":"x","status":"pending"}],"model":"claude-haiku-4-5","permissionMode":"bypassPermissions","apiKeySource":"none"}"#;
        let event = parse_line(line).expect("init line should parse");
        match event {
            ClaudeEvent::System(SystemEvent::Init(init)) => {
                assert_eq!(
                    init.session_id.as_deref(),
                    Some("c18eb67f-6873-45a4-aa7a-8755cecb4361")
                );
            }
            other => panic!("expected System(Init), got {other:?}"),
        }
    }

    #[test]
    fn init_event_with_no_session_id_field_yields_none_without_panicking() {
        let line = r#"{"type":"system","subtype":"init","cwd":"C:\\repo","tools":[]}"#;
        let event = parse_line(line).expect("init line should parse");
        match event {
            ClaudeEvent::System(SystemEvent::Init(init)) => {
                assert_eq!(init.session_id, None);
            }
            other => panic!("expected System(Init), got {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_type_becomes_catch_all() {
        let event = parse_line(r#"{"type":"some_future_event_type","foo":"bar"}"#)
            .expect("unknown type should still parse");
        match event {
            ClaudeEvent::Unknown(type_str) => assert_eq!(type_str, "some_future_event_type"),
            other => panic!("expected Unknown variant, got {other:?}"),
        }
    }
}
