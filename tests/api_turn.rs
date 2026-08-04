//! First integration test target for this crate. Exercises a full agent
//! turn against a canned SSE stream served by a local wiremock server,
//! rather than any real DeepSeek or Ollama endpoint.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use DeepSeekCustom::agent::agent_loop::{AgentConfig, AgentLoop, StreamEvent};
use DeepSeekCustom::api::client::{ApiClient, Provider};
use DeepSeekCustom::tools::ToolRegistry;
use DeepSeekCustom::tools::read::ReadTool;

use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

/// A canned SSE stream modeled on the real DeepSeek shape: several chunks
/// carrying `delta.content` fragments, a final chunk carrying `usage`, then
/// `data: [DONE]`.
const SSE_BODY: &str = concat!(
    "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"content\":\"Hello, \"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"content\":\"world!\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},",
    "\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":120,",
    "\"completion_tokens\":45,\"total_tokens\":165,",
    "\"prompt_cache_hit_tokens\":100,\"prompt_cache_miss_tokens\":20,",
    "\"prompt_cache_write_tokens\":120}}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn full_turn_against_mock_server_collects_text_and_turn_end() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(SSE_BODY, "text/event-stream"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        Some(mock_server.uri()),
        Some("deepseek-v4-flash".into()),
    );

    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent.set_event_sender(tx);

    let result = agent.run("hello").await;
    assert!(result.is_ok(), "agent run should succeed: {:?}", result.err());

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello, world!");

    let last = events.last().expect("expected at least one event");
    match last {
        StreamEvent::TurnEnd {
            prompt_cache_hit_tokens,
            prompt_cache_miss_tokens,
            ..
        } => {
            assert_eq!(*prompt_cache_hit_tokens, 100);
            assert_eq!(*prompt_cache_miss_tokens, 20);
        }
        other => panic!("expected the last event to be TurnEnd, got {other:?}"),
    }

    // mock_server's `expect(1)` on the mounted Mock is checked when
    // `mock_server` drops, so nothing else to assert here for that
    // guarantee. Explicitly verifying request receipt makes intent clear.
    mock_server.verify().await;
}

/// A canned SSE stream carrying one tool call, modeled on the real DeepSeek
/// shape. The first chunk carries `index`, `id`, and `function.name`. The
/// two chunks after it carry only `function.arguments` fragments, which
/// `merge_tool_call` must accumulate into `{"file_path":"Cargo.toml"}`.
const TOOL_CALL_SSE_BODY: &str = concat!(
    "data: {\"id\":\"t2\",\"object\":\"chat.completion.chunk\",\"created\":2,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"tool_calls\":[{\"index\":0,\"id\":\"call_abc123\",\"type\":\"function\",",
    "\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]},",
    "\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"t2\",\"object\":\"chat.completion.chunk\",\"created\":2,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"tool_calls\":[{\"index\":0,\"function\":",
    "{\"arguments\":\"{\\\"file_path\\\":\"}}]},",
    "\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"t2\",\"object\":\"chat.completion.chunk\",\"created\":2,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"tool_calls\":[{\"index\":0,\"function\":",
    "{\"arguments\":\"\\\"Cargo.toml\\\"}\"}}]},",
    "\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"t2\",\"object\":\"chat.completion.chunk\",\"created\":2,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},",
    "\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// The second-round SSE stream: plain text plus a final `usage` block, once
/// the tool result has been fed back into history.
const FOLLOWUP_TEXT_SSE_BODY: &str = concat!(
    "data: {\"id\":\"t3\",\"object\":\"chat.completion.chunk\",\"created\":3,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"content\":\"Done reading.\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"t3\",\"object\":\"chat.completion.chunk\",\"created\":3,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},",
    "\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":140,",
    "\"completion_tokens\":10,\"total_tokens\":150,",
    "\"prompt_cache_hit_tokens\":110,\"prompt_cache_miss_tokens\":30,",
    "\"prompt_cache_write_tokens\":140}}\n\n",
    "data: [DONE]\n\n",
);

/// Build an `AgentLoop` with an empty tool registry, pointed at
/// `mock_server`, and a fresh event channel wired in. Shared setup for the
/// tests below that don't need the tool-call round trip.
fn new_agent_against(
    mock_server: &MockServer,
) -> (AgentLoop, tokio::sync::mpsc::UnboundedReceiver<StreamEvent>) {
    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        Some(mock_server.uri()),
        Some("deepseek-v4-flash".into()),
    );

    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    agent.set_event_sender(tx);
    (agent, rx)
}

/// Matches an outgoing chat-completions request by whether its `messages`
/// array already carries a `Role::Tool` message. Distinguishes the first
/// request (no tool result yet) from the second (tool result fed back),
/// deterministically, without relying on mock registration or reuse order.
struct HasToolMessage(bool);

impl Match for HasToolMessage {
    fn matches(&self, request: &Request) -> bool {
        let body: serde_json::Value = match request.body_json() {
            Ok(v) => v,
            Err(_) => return false,
        };
        let has_tool = body["messages"]
            .as_array()
            .map(|msgs| msgs.iter().any(|m| m["role"] == "tool"))
            .unwrap_or(false);
        has_tool == self.0
    }
}

#[tokio::test]
async fn tool_call_round_trip_feeds_result_back_and_reaches_turn_end() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(HasToolMessage(false))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(TOOL_CALL_SSE_BODY, "text/event-stream"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(HasToolMessage(true))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(FOLLOWUP_TEXT_SSE_BODY, "text/event-stream"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        Some(mock_server.uri()),
        Some("deepseek-v4-flash".into()),
    );

    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ReadTool::new(std::env::current_dir().unwrap())));

    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent.set_event_sender(tx);

    let result = agent.run("please read Cargo.toml").await;
    assert!(result.is_ok(), "agent run should succeed: {:?}", result.err());

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    let start_idx = events
        .iter()
        .position(|e| matches!(e, StreamEvent::ToolCallStart { .. }))
        .expect("expected a ToolCallStart event");
    match &events[start_idx] {
        StreamEvent::ToolCallStart { tool, args, .. } => {
            assert_eq!(tool, "read");
            let parsed_args: serde_json::Value = serde_json::from_str(args).unwrap_or_else(|e| {
                panic!("tool call args should be fully-accumulated valid JSON, got {args:?}: {e}")
            });
            assert_eq!(parsed_args["file_path"], "Cargo.toml");
        }
        other => panic!("expected ToolCallStart, got {other:?}"),
    }

    let end_idx = events
        .iter()
        .position(|e| matches!(e, StreamEvent::ToolCallEnd { .. }))
        .expect("expected a ToolCallEnd event");
    assert!(
        end_idx > start_idx,
        "ToolCallEnd should be emitted after ToolCallStart"
    );

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Done reading.");

    let last = events.last().expect("expected at least one event");
    assert!(
        matches!(last, StreamEvent::TurnEnd { .. }),
        "expected the last event to be TurnEnd, got {last:?}"
    );

    let received = mock_server
        .received_requests()
        .await
        .expect("request recording should be enabled by default");
    assert_eq!(
        received.len(),
        2,
        "expected exactly two requests to the mock server"
    );

    let second_body: serde_json::Value = received[1]
        .body_json()
        .expect("second request body should be valid JSON");
    let messages = second_body["messages"]
        .as_array()
        .expect("messages should be an array");
    let tool_message = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("second request should carry the tool result fed back into history");
    assert_eq!(tool_message["tool_call_id"], "call_abc123");
    assert!(
        tool_message["content"]
            .as_str()
            .unwrap_or("")
            .contains("edition"),
        "tool result content should contain Cargo.toml's own content"
    );

    mock_server.verify().await;
}

/// A stream carrying one malformed `data:` line, whose payload is not valid
/// JSON, between two otherwise-valid content chunks. `chat_stream` in
/// `src/api/client.rs` logs a `warn` and skips a chunk it can't parse as
/// `StreamChunk`, rather than returning an error or stopping the stream. So
/// the turn should still complete, with the broken chunk's text simply
/// absent from the result.
const BROKEN_CHUNK_SSE_BODY: &str = concat!(
    "data: {\"id\":\"t4\",\"object\":\"chat.completion.chunk\",\"created\":4,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"content\":\"Hello, \"},\"finish_reason\":null}]}\n\n",
    "data: {this is not valid json}\n\n",
    "data: {\"id\":\"t4\",\"object\":\"chat.completion.chunk\",\"created\":4,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"content\":\"world!\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"t4\",\"object\":\"chat.completion.chunk\",\"created\":4,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},",
    "\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// A stream that carries a valid text chunk and a valid `finish_reason: stop`
/// chunk, but whose connection closes without ever sending
/// `data: [DONE]\n\n`. `chat_stream`'s read loop only returns early on
/// `[DONE]`. When the byte stream itself ends, the
/// `while let Some(chunk_result) = stream.next().await` loop just falls
/// through. The spawned task returns, and dropping `tx` closes the mpsc
/// channel. The agent's `rx.recv().await` then sees `None` and the turn
/// proceeds to completion normally, so this should not hang.
const NO_DONE_SENTINEL_SSE_BODY: &str = concat!(
    "data: {\"id\":\"t5\",\"object\":\"chat.completion.chunk\",\"created\":5,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"content\":\"no sentinel here\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"t5\",\"object\":\"chat.completion.chunk\",\"created\":5,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},",
    "\"finish_reason\":\"stop\"}]}\n\n",
);

#[tokio::test]
async fn retries_after_a_500_and_completes_the_turn() {
    let mock_server = MockServer::start().await;

    // Same path/method, same default priority: insertion order decides
    // which one a request hits first. `up_to_n_times(1)` retires this mock
    // after the first request, so the retry falls through to the mock
    // mounted after it.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(SSE_BODY, "text/event-stream"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (mut agent, mut rx) = new_agent_against(&mock_server);

    let result = agent.run("hello").await;
    assert!(result.is_ok(), "agent run should succeed: {:?}", result.err());

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello, world!");

    let last = events.last().expect("expected at least one event");
    assert!(
        matches!(last, StreamEvent::TurnEnd { .. }),
        "expected the last event to be TurnEnd, got {last:?}"
    );

    let received = mock_server
        .received_requests()
        .await
        .expect("request recording should be enabled by default");
    assert_eq!(
        received.len(),
        2,
        "expected exactly two requests: the 500 and the retry"
    );

    mock_server.verify().await;
}

#[tokio::test]
async fn a_malformed_chunk_is_skipped_and_the_turn_still_completes() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(BROKEN_CHUNK_SSE_BODY, "text/event-stream"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (mut agent, mut rx) = new_agent_against(&mock_server);

    let result = agent.run("hello").await;
    assert!(result.is_ok(), "agent run should succeed: {:?}", result.err());

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    // The broken chunk contributes no text. The two valid chunks around it
    // still arrive and concatenate normally.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello, world!");

    let last = events.last().expect("expected at least one event");
    assert!(
        matches!(last, StreamEvent::TurnEnd { .. }),
        "expected the last event to be TurnEnd, got {last:?}"
    );

    mock_server.verify().await;
}

#[tokio::test]
async fn a_stream_missing_the_done_sentinel_still_terminates_the_turn() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(NO_DONE_SENTINEL_SSE_BODY, "text/event-stream"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (mut agent, mut rx) = new_agent_against(&mock_server);

    // A hang here means the agent loop never noticed the stream ended, so
    // fail the test instead of blocking the suite forever.
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), agent.run("hello"))
        .await
        .expect("agent.run should not hang when the [DONE] sentinel is missing");
    assert!(result.is_ok(), "agent run should succeed: {:?}", result.err());

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "no sentinel here");

    let last = events.last().expect("expected at least one event");
    assert!(
        matches!(last, StreamEvent::TurnEnd { .. }),
        "expected the last event to be TurnEnd, got {last:?}"
    );

    mock_server.verify().await;
}
