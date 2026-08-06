//! Unit tests for `deepseek_custom::backend::claude_cli::events` (`src/backend/claude_cli/events.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::backend::claude_cli::events::{ClaudeEvent, SystemEvent, parse_line};
use deepseek_custom::backend::claude_cli::stream::{ContentBlock, ContentDelta, InnerStreamEvent};

const TEXT_ONLY_FIXTURE: &str = include_str!("fixtures/claude_stream_json.jsonl");
const TOOLS_FIXTURE: &str = include_str!("fixtures/claude_stream_json_tools.jsonl");

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
