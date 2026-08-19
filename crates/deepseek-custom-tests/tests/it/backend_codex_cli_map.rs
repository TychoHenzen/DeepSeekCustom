//! Contract tests for mapping `codex exec --json` events to `StreamEvent`.

use deepseek_custom::agent::events::StreamEvent;
use deepseek_custom::backend::codex_cli::events::parse_event;
use deepseek_custom::backend::codex_cli::map::EventMapper;

fn map_line(mapper: &mut EventMapper, line: &str) -> Vec<StreamEvent> {
    let event = parse_event(line).expect("event should parse");
    mapper.map(event)
}

#[test]
fn thread_and_turn_started_emit_nothing() {
    let mut mapper = EventMapper::new();

    assert!(
        map_line(
            &mut mapper,
            r#"{"type":"thread.started","thread_id":"thread-42"}"#
        )
        .is_empty()
    );
    assert!(map_line(&mut mapper, r#"{"type":"turn.started"}"#).is_empty());
}

#[test]
fn agent_message_snapshots_emit_only_new_text() {
    let mut mapper = EventMapper::new();

    let started = map_line(
        &mut mapper,
        r#"{"type":"item.started","item":{"id":"message-1","type":"agent_message","text":"Hello"}}"#,
    );
    let updated = map_line(
        &mut mapper,
        r#"{"type":"item.updated","item":{"id":"message-1","type":"agent_message","text":"Hello world"}}"#,
    );
    let completed = map_line(
        &mut mapper,
        r#"{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Hello world!"}}"#,
    );

    assert!(matches!(started.as_slice(), [StreamEvent::Text { turn: 0, text }] if text == "Hello"));
    assert!(
        matches!(updated.as_slice(), [StreamEvent::Text { turn: 0, text }] if text == " world")
    );
    assert!(matches!(completed.as_slice(), [StreamEvent::Text { turn: 0, text }] if text == "!"));
}

#[test]
fn reasoning_snapshots_emit_only_new_reasoning() {
    let mut mapper = EventMapper::new();

    let started = map_line(
        &mut mapper,
        r#"{"type":"item.started","item":{"id":"reasoning-1","type":"reasoning","text":"Inspect"}}"#,
    );
    let updated = map_line(
        &mut mapper,
        r#"{"type":"item.updated","item":{"id":"reasoning-1","type":"reasoning","text":"Inspect parser"}}"#,
    );
    let completed = map_line(
        &mut mapper,
        r#"{"type":"item.completed","item":{"id":"reasoning-1","type":"reasoning","text":"Inspect parser"}}"#,
    );

    assert!(
        matches!(started.as_slice(), [StreamEvent::Reasoning { turn: 0, text }] if text == "Inspect")
    );
    assert!(
        matches!(updated.as_slice(), [StreamEvent::Reasoning { turn: 0, text }] if text == " parser")
    );
    assert!(
        completed.is_empty(),
        "the repeated completed snapshot must not duplicate reasoning"
    );
}

#[test]
fn command_execution_maps_to_start_and_failed_end() {
    let mut mapper = EventMapper::new();
    let started = map_line(
        &mut mapper,
        r#"{"type":"item.started","item":{"id":"command-1","type":"command_execution","command":"cargo test"}}"#,
    );
    let completed = map_line(
        &mut mapper,
        r#"{"type":"item.completed","item":{"id":"command-1","type":"command_execution","aggregated_output":"test failed\n","exit_code":101,"status":"failed"}}"#,
    );

    assert!(
        matches!(started.as_slice(), [StreamEvent::ToolCallStart { turn: 0, tool, args }] if tool == "command_execution" && args == r#"{"command":"cargo test"}"#)
    );
    assert!(
        matches!(completed.as_slice(), [StreamEvent::ToolCallEnd { turn: 0, tool, output, is_error: true }] if tool == "command_execution" && output == "test failed\n")
    );
}

#[test]
fn file_change_maps_to_start_and_successful_end() {
    let mut mapper = EventMapper::new();
    let started = map_line(
        &mut mapper,
        r#"{"type":"item.started","item":{"id":"change-1","type":"file_change","changes":[{"path":"src/main.rs","kind":"update"}]}}"#,
    );
    let completed = map_line(
        &mut mapper,
        r#"{"type":"item.completed","item":{"id":"change-1","type":"file_change","changes":[{"path":"src/main.rs","kind":"update"}],"status":"completed"}}"#,
    );

    assert!(
        matches!(started.as_slice(), [StreamEvent::ToolCallStart { turn: 0, tool, args }] if tool == "file_change" && args == r#"{"changes":[{"kind":"update","path":"src/main.rs"}]}"#)
    );
    assert!(
        matches!(completed.as_slice(), [StreamEvent::ToolCallEnd { turn: 0, tool, output, is_error: false }] if tool == "file_change" && output == r#"[{"kind":"update","path":"src/main.rs"}]"#)
    );
}

#[test]
fn mcp_tool_call_maps_to_start_and_error_end() {
    let mut mapper = EventMapper::new();
    let started = map_line(
        &mut mapper,
        r#"{"type":"item.started","item":{"id":"mcp-1","type":"mcp_tool_call","server":"files","tool":"read","arguments":{"path":"a.rs"}}}"#,
    );
    let completed = map_line(
        &mut mapper,
        r#"{"type":"item.completed","item":{"id":"mcp-1","type":"mcp_tool_call","server":"files","tool":"read","arguments":{"path":"a.rs"},"error":{"message":"denied"},"status":"failed"}}"#,
    );

    assert!(
        matches!(started.as_slice(), [StreamEvent::ToolCallStart { turn: 0, tool, args }] if tool == "mcp_tool_call" && args == r#"{"arguments":{"path":"a.rs"},"server":"files","tool":"read"}"#)
    );
    assert!(
        matches!(completed.as_slice(), [StreamEvent::ToolCallEnd { turn: 0, tool, output, is_error: true }] if tool == "mcp_tool_call" && output == r#"{"message":"denied"}"#)
    );
}

#[test]
fn tool_updates_and_unknown_item_kinds_emit_nothing() {
    let mut mapper = EventMapper::new();

    assert!(map_line(&mut mapper, r#"{"type":"item.updated","item":{"id":"command-1","type":"command_execution","aggregated_output":"running"}}"#).is_empty());
    assert!(
        map_line(
            &mut mapper,
            r#"{"type":"item.started","item":{"id":"future-1","type":"future_item"}}"#
        )
        .is_empty()
    );
    assert!(
        map_line(
            &mut mapper,
            r#"{"type":"item.updated","item":{"id":"future-1","type":"future_item"}}"#
        )
        .is_empty()
    );
    assert!(
        map_line(
            &mut mapper,
            r#"{"type":"item.completed","item":{"id":"future-1","type":"future_item"}}"#
        )
        .is_empty()
    );
}

#[test]
fn turn_completed_maps_exact_usage_and_advances_the_turn() {
    let mut mapper = EventMapper::new();
    let completed = map_line(
        &mut mapper,
        r#"{"type":"turn.completed","usage":{"input_tokens":120,"cached_input_tokens":80,"output_tokens":15}}"#,
    );
    let next_text = map_line(
        &mut mapper,
        r#"{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"next"}}"#,
    );

    assert!(
        matches!(completed.as_slice(), [StreamEvent::TurnEnd { turn: 0, finish_reason, total_tokens: 135, prompt_cache_hit_tokens: 80, prompt_cache_miss_tokens: 40 }] if finish_reason == "end_turn")
    );
    assert!(
        matches!(next_text.as_slice(), [StreamEvent::Text { turn: 1, text }] if text == "next")
    );
}

#[test]
fn turn_failed_maps_error_message_and_advances_the_turn() {
    let mut mapper = EventMapper::new();
    let failed = map_line(
        &mut mapper,
        r#"{"type":"turn.failed","error":{"message":"model unavailable","code":"overloaded"}}"#,
    );
    let next_text = map_line(
        &mut mapper,
        r#"{"type":"item.started","item":{"id":"message-1","type":"agent_message","text":"retry"}}"#,
    );

    assert!(
        matches!(failed.as_slice(), [StreamEvent::Error { message }] if message == "model unavailable")
    );
    assert!(
        matches!(next_text.as_slice(), [StreamEvent::Text { turn: 1, text }] if text == "retry")
    );
}
