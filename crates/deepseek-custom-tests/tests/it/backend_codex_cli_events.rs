//! Contract tests for the `codex exec --json` line protocol parser.

use deepseek_custom::backend::codex_cli::events::{CodexEvent, parse_event};

#[test]
fn thread_started_retains_thread_id() {
    let event = parse_event(r#"{"type":"thread.started","thread_id":"thread-42"}"#)
        .expect("thread.started should parse");

    match event {
        CodexEvent::ThreadStarted(event) => assert_eq!(event.thread_id, "thread-42"),
        other => panic!("expected ThreadStarted, got {other:?}"),
    }
}

#[test]
fn turn_started_parses() {
    let event = parse_event(r#"{"type":"turn.started"}"#).expect("turn.started should parse");
    assert!(matches!(event, CodexEvent::TurnStarted(_)));
}

#[test]
fn item_started_retains_agent_message_fields() {
    let event = parse_event(
        r#"{"type":"item.started","item":{"id":"item-1","type":"agent_message","text":"hello","phase":"commentary"}}"#,
    )
    .expect("item.started should parse");

    match event {
        CodexEvent::ItemStarted(event) => {
            assert_eq!(event.item.id.as_deref(), Some("item-1"));
            assert_eq!(event.item.item_type, "agent_message");
            assert_eq!(event.item.text.as_deref(), Some("hello"));
            assert_eq!(event.item.extra["phase"], "commentary");
        }
        other => panic!("expected ItemStarted, got {other:?}"),
    }
}

#[test]
fn item_updated_retains_reasoning_fields() {
    let event = parse_event(
        r#"{"type":"item.updated","item":{"id":"item-2","type":"reasoning","text":"inspect parser"}}"#,
    )
    .expect("item.updated should parse");

    match event {
        CodexEvent::ItemUpdated(event) => {
            assert_eq!(event.item.id.as_deref(), Some("item-2"));
            assert_eq!(event.item.item_type, "reasoning");
            assert_eq!(event.item.text.as_deref(), Some("inspect parser"));
        }
        other => panic!("expected ItemUpdated, got {other:?}"),
    }
}

#[test]
fn item_completed_retains_command_execution_fields() {
    let event = parse_event(
        r#"{"type":"item.completed","item":{"id":"item-3","type":"command_execution","command":"cargo test","aggregated_output":"ok\n","exit_code":0,"status":"completed"}}"#,
    )
    .expect("command item should parse");

    match event {
        CodexEvent::ItemCompleted(event) => {
            assert_eq!(event.item.item_type, "command_execution");
            assert_eq!(event.item.command.as_deref(), Some("cargo test"));
            assert_eq!(event.item.aggregated_output.as_deref(), Some("ok\n"));
            assert_eq!(event.item.exit_code, Some(0));
            assert_eq!(event.item.status.as_deref(), Some("completed"));
        }
        other => panic!("expected ItemCompleted, got {other:?}"),
    }
}

#[test]
fn item_completed_retains_file_change_fields() {
    let event = parse_event(
        r#"{"type":"item.completed","item":{"type":"file_change","changes":[{"path":"src/main.rs","kind":"update"}],"status":"completed"}}"#,
    )
    .expect("file change item should parse");

    match event {
        CodexEvent::ItemCompleted(event) => {
            assert_eq!(event.item.item_type, "file_change");
            let changes = event.item.changes.expect("changes should be retained");
            assert_eq!(changes[0]["path"], "src/main.rs");
            assert_eq!(changes[0]["kind"], "update");
        }
        other => panic!("expected ItemCompleted, got {other:?}"),
    }
}

#[test]
fn item_completed_retains_mcp_tool_call_fields() {
    let event = parse_event(
        r#"{"type":"item.completed","item":{"id":"item-5","type":"mcp_tool_call","server":"files","tool":"read","arguments":{"path":"a.rs"},"result":{"content":"source"},"error":null,"status":"completed"}}"#,
    )
    .expect("MCP tool item should parse");

    match event {
        CodexEvent::ItemCompleted(event) => {
            assert_eq!(event.item.item_type, "mcp_tool_call");
            assert_eq!(event.item.server.as_deref(), Some("files"));
            assert_eq!(event.item.tool.as_deref(), Some("read"));
            assert_eq!(event.item.arguments.as_ref().unwrap()["path"], "a.rs");
            assert_eq!(event.item.result.as_ref().unwrap()["content"], "source");
            assert!(event.item.error.is_none());
        }
        other => panic!("expected ItemCompleted, got {other:?}"),
    }
}

#[test]
fn turn_completed_retains_usage() {
    let event = parse_event(
        r#"{"type":"turn.completed","usage":{"input_tokens":120,"cached_input_tokens":80,"output_tokens":15}}"#,
    )
    .expect("turn.completed should parse");

    match event {
        CodexEvent::TurnCompleted(event) => {
            let usage = event.usage.expect("usage should be retained");
            assert_eq!(usage.input_tokens, 120);
            assert_eq!(usage.cached_input_tokens, 80);
            assert_eq!(usage.output_tokens, 15);
        }
        other => panic!("expected TurnCompleted, got {other:?}"),
    }
}

#[test]
fn turn_failed_retains_error_fields() {
    let event = parse_event(
        r#"{"type":"turn.failed","error":{"message":"model unavailable","code":"overloaded"}}"#,
    )
    .expect("turn.failed should parse");

    match event {
        CodexEvent::TurnFailed(event) => {
            assert_eq!(event.error.message, "model unavailable");
            assert_eq!(event.error.extra["code"], "overloaded");
        }
        other => panic!("expected TurnFailed, got {other:?}"),
    }
}

#[test]
fn malformed_and_unsupported_lines_return_none() {
    assert!(parse_event(r#"{"type":"turn.started"#).is_none());
    assert!(parse_event(r#"{"type":"future.event"}"#).is_none());
    assert!(parse_event(r#"{"payload":{}}"#).is_none());
    assert!(parse_event(r#"{"type":42}"#).is_none());
}
