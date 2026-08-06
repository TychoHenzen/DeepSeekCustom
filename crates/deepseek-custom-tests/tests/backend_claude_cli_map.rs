//! Unit tests for `deepseek_custom::backend::claude_cli::map` (`src/backend/claude_cli/map.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::agent::agent_loop::StreamEvent;
use deepseek_custom::backend::claude_cli::events::{ClaudeEvent, parse_line};
use deepseek_custom::backend::claude_cli::map::EventMapper;
use deepseek_custom::backend::claude_cli::stream::{ContentDelta, InnerStreamEvent};

const TEXT_ONLY_FIXTURE: &str = include_str!("fixtures/claude_stream_json.jsonl");
const TOOLS_FIXTURE: &str = include_str!("fixtures/claude_stream_json_tools.jsonl");

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
