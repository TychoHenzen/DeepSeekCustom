//! First integration test target for this crate. Exercises a full agent
//! turn against a canned SSE stream served by a local wiremock server,
//! rather than any real DeepSeek or Ollama endpoint.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::agent_loop::{AgentConfig, AgentLoop, RoutedEvent, StreamEvent};
use deepseek_custom::api::client::{ApiClient, Provider};
use deepseek_custom::api::types::ImageAttachment;
use deepseek_custom::effort::Effort;
use deepseek_custom::tools::ToolRegistry;
use deepseek_custom::tools::read::ReadTool;
use deepseek_custom::tools::read_image::ReadImageTool;

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
    assert!(
        result.is_ok(),
        "agent run should succeed: {:?}",
        result.err()
    );

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
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
) -> (AgentLoop, tokio::sync::mpsc::UnboundedReceiver<RoutedEvent>) {
    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        Some(mock_server.uri()),
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
    );

    let tools = ToolRegistry::new();
    tools.register(Arc::new(ReadTool::new(Arc::new(Mutex::new(
        std::env::current_dir().unwrap(),
    )))));

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
    assert!(
        result.is_ok(),
        "agent run should succeed: {:?}",
        result.err()
    );

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
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
    assert!(
        result.is_ok(),
        "agent run should succeed: {:?}",
        result.err()
    );

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
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
    assert!(
        result.is_ok(),
        "agent run should succeed: {:?}",
        result.err()
    );

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
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
    assert!(
        result.is_ok(),
        "agent run should succeed: {:?}",
        result.err()
    );

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
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

/// Runs one full turn against a fresh wiremock server for `provider` at the
/// given `effort` level, and returns the outgoing request body the server
/// recorded. `SSE_BODY`'s shape is generic OpenAI-compatible chunk data, so
/// it works as the canned response for either provider.
async fn request_body_for_effort(provider: Provider, effort: Effort) -> serde_json::Value {
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

    let client = ApiClient::new(provider, "sk-test".into(), Some(mock_server.uri()));

    let tools = ToolRegistry::new();
    let config = AgentConfig {
        effort,
        ..AgentConfig::default()
    };
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        config,
        Arc::new(AtomicBool::new(false)),
    );

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent.set_event_sender(tx);

    let result = agent.run("hello").await;
    assert!(
        result.is_ok(),
        "agent run should succeed: {:?}",
        result.err()
    );

    let received = mock_server
        .received_requests()
        .await
        .expect("request recording should be enabled by default");
    assert_eq!(received.len(), 1, "expected exactly one request");

    received[0]
        .body_json()
        .expect("request body should be valid JSON")
}

/// Proves every `Effort` level actually reaches the wire for the DeepSeek
/// provider, per the mapping `Effort::deepseek_thinking_mode` documents.
/// This mapping is deliberately not one to one: `Low`, `Medium`, and `High`
/// all send `"thinking"`, since DeepSeek only offers three modes. What this
/// closes is that `Max` reaches `"thinking_max"` and `None` reaches
/// `"non-thinking"`, the two values the old boolean toggle could never
/// reach at all.
#[tokio::test]
async fn deepseek_sends_the_documented_thinking_mode_per_effort_level_on_the_wire() {
    for (effort, expected_thinking_mode) in [
        (Effort::None, "non-thinking"),
        (Effort::Low, "thinking"),
        (Effort::Medium, "thinking"),
        (Effort::High, "thinking"),
        (Effort::Max, "thinking_max"),
    ] {
        let body = request_body_for_effort(Provider::DeepSeek, effort).await;
        assert_eq!(
            body["thinking_mode"], expected_thinking_mode,
            "effort {effort:?} should send thinking_mode {expected_thinking_mode:?}"
        );
        assert!(
            body.get("reasoning_effort").is_none() || body["reasoning_effort"].is_null(),
            "DeepSeek should never send reasoning_effort on the wire, got {:?}",
            body.get("reasoning_effort")
        );
    }
}

/// Proves every `Effort` level reaches the wire as a distinct
/// `reasoning_effort` value for the Ollama provider, per
/// `Effort::ollama_reasoning_effort`'s one-to-one mapping. Unlike DeepSeek,
/// none of Ollama's five levels collapse into another.
#[tokio::test]
async fn ollama_sends_a_distinct_reasoning_effort_per_effort_level_on_the_wire() {
    let mut seen = std::collections::HashSet::new();

    for (effort, expected_reasoning_effort) in [
        (Effort::None, "none"),
        (Effort::Low, "low"),
        (Effort::Medium, "medium"),
        (Effort::High, "high"),
        (Effort::Max, "max"),
    ] {
        let body = request_body_for_effort(Provider::Ollama, effort).await;
        assert_eq!(
            body["reasoning_effort"], expected_reasoning_effort,
            "effort {effort:?} should send reasoning_effort {expected_reasoning_effort:?}"
        );
        assert!(
            body.get("thinking_mode").is_none() || body["thinking_mode"].is_null(),
            "Ollama should never send thinking_mode on the wire, got {:?}",
            body.get("thinking_mode")
        );
        assert!(
            seen.insert(expected_reasoning_effort),
            "reasoning_effort {expected_reasoning_effort:?} was not distinct across levels"
        );
    }
}

/// The 68-byte grayscale test PNG used throughout
/// `docs/notes/image-support.md`, so these tests exercise the same bytes
/// the real per-backend probes did.
const TEST_IMAGE_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

fn test_image() -> ImageAttachment {
    ImageAttachment {
        data: TEST_IMAGE_BASE64.to_string(),
        media_type: "image/png".to_string(),
    }
}

/// Runs one full turn with `image` attached against a fresh wiremock server
/// for `provider`, and returns the outgoing request body plus every event
/// the turn emitted.
async fn run_turn_with_image(
    provider: Provider,
    image: Option<&ImageAttachment>,
) -> (serde_json::Value, Vec<StreamEvent>) {
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

    let client = ApiClient::new(provider, "sk-test".into(), Some(mock_server.uri()));

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

    let result = agent.run_with_image("look at this", image).await;
    assert!(
        result.is_ok(),
        "agent run should succeed: {:?}",
        result.err()
    );

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
    }

    let received = mock_server
        .received_requests()
        .await
        .expect("request recording should be enabled by default");
    assert_eq!(received.len(), 1, "expected exactly one request");
    let body = received[0]
        .body_json()
        .expect("request body should be valid JSON");

    (body, events)
}

/// Ollama accepts the OpenAI `image_url` content part shape, confirmed
/// against a real local instance in `docs/notes/image-support.md`. The
/// outgoing request must carry that exact shape: a `text` part followed by
/// an `image_url` part with a `data:` URL.
#[tokio::test]
async fn ollama_sends_the_image_as_an_openai_image_url_part() {
    let image = test_image();
    let (body, events) = run_turn_with_image(Provider::Ollama, Some(&image)).await;

    let content = &body["messages"][1]["content"];
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "look at this");
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(
        content[1]["image_url"]["url"],
        format!("data:image/png;base64,{TEST_IMAGE_BASE64}")
    );

    // Ollama can take the image, so no unsupported-backend notice fires.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error { .. })),
        "expected no Error/notice event, got {events:?}"
    );
}

/// DeepSeek has no image support at all, confirmed against the live API in
/// `docs/notes/image-support.md`: a hard 400 naming `image_url` as an
/// unknown content-part variant. The mapping must drop the image before the
/// request is built, rather than send it and let the 400 happen, and must
/// post a transcript notice naming the backend.
#[tokio::test]
async fn deepseek_drops_the_image_and_posts_a_notice_naming_the_backend() {
    let image = test_image();
    let (body, events) = run_turn_with_image(Provider::DeepSeek, Some(&image)).await;

    // The outgoing content is plain text: no image part was ever built,
    // let alone sent.
    assert_eq!(body["messages"][1]["content"], "look at this");

    let notice = events
        .iter()
        .find_map(|e| match e {
            StreamEvent::Error { message } => Some(message.clone()),
            _ => None,
        })
        .expect("expected a notice (Error event) for the dropped image");
    assert!(
        notice.contains("DeepSeek"),
        "notice should name the backend: {notice}"
    );

    // The notice must fire before the request goes out, not after some 400
    // comes back: the mock only ever answers 200, so the turn completing
    // successfully alongside the notice proves the notice did not come from
    // a failed request.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello, world!");
}

/// A turn with no image attached must be byte-for-byte unaffected: plain
/// text content, and no notice, on either provider.
#[tokio::test]
async fn a_turn_with_no_image_is_unaffected_on_either_provider() {
    for provider in [Provider::DeepSeek, Provider::Ollama] {
        let (body, events) = run_turn_with_image(provider, None).await;

        assert_eq!(
            body["messages"][1]["content"], "look at this",
            "provider {provider:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, StreamEvent::Error { .. })),
            "provider {provider:?}: expected no notice, got {events:?}"
        );
    }
}

// ── Mechanism: a tool-returned image reaching the next request ──
//
// `ToolOutput::image` cannot travel inside the `Role::Tool` result message
// itself (see that field's doc comment in `src/tools/mod.rs`). These tests
// prove what `AgentLoop::run_turn` actually does instead: push a synthetic
// `Role::User` message right after the tool result, mapped through the same
// `build_user_content` a pasted or dropped image already goes through.

/// A canned SSE stream carrying one `read_image` tool call, modeled on the
/// real DeepSeek tool-call shape `TOOL_CALL_SSE_BODY` above already uses.
const READ_IMAGE_TOOL_CALL_SSE_BODY: &str = concat!(
    "data: {\"id\":\"t6\",\"object\":\"chat.completion.chunk\",\"created\":6,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":",
    "{\"tool_calls\":[{\"index\":0,\"id\":\"call_img1\",\"type\":\"function\",",
    "\"function\":{\"name\":\"read_image\",",
    "\"arguments\":\"{\\\"file_path\\\":\\\"pixel.png\\\"}\"}}]},",
    "\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"t6\",\"object\":\"chat.completion.chunk\",\"created\":6,",
    "\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},",
    "\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// Sets up a temp directory with the same 68-byte grayscale test PNG, an
/// `AgentLoop` with a `ReadImageTool` registered against it, and a mock
/// server that answers the first request with a `read_image` tool call and
/// the second with plain text. Returns the second request's body (the one
/// built after the tool result and any synthetic image message were pushed)
/// plus every event the turn emitted.
async fn run_tool_returned_image_turn(provider: Provider) -> (serde_json::Value, Vec<StreamEvent>) {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(HasToolMessage(false))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(READ_IMAGE_TOOL_CALL_SSE_BODY, "text/event-stream"),
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

    let client = ApiClient::new(provider, "sk-test".into(), Some(mock_server.uri()));

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dsc-api-turn-read-image-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let image_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        TEST_IMAGE_BASE64,
    )
    .unwrap();
    std::fs::write(dir.join("pixel.png"), &image_bytes).unwrap();

    let tools = ToolRegistry::new();
    tools.register(Arc::new(ReadImageTool::new(Arc::new(Mutex::new(
        dir.clone(),
    )))));

    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent.set_event_sender(tx);

    let result = agent.run("please look at pixel.png").await;
    assert!(
        result.is_ok(),
        "agent run should succeed: {:?}",
        result.err()
    );

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
    }

    let received = mock_server
        .received_requests()
        .await
        .expect("request recording should be enabled by default");
    assert_eq!(received.len(), 2, "expected exactly two requests");
    let second_body: serde_json::Value = received[1]
        .body_json()
        .expect("second request body should be valid JSON");

    let _ = std::fs::remove_dir_all(&dir);
    (second_body, events)
}

/// Ollama can take the image, so the synthetic follow-up message must carry
/// it as an `image_url` content part, with the exact base64 payload the
/// tool read off disk.
#[tokio::test]
async fn ollama_carries_a_tool_returned_image_into_the_next_request() {
    let (second_body, events) = run_tool_returned_image_turn(Provider::Ollama).await;

    let messages = second_body["messages"]
        .as_array()
        .expect("messages should be an array");
    let image_message = messages
        .iter()
        .find(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_array()
                    .map(|parts| parts.iter().any(|p| p["type"] == "image_url"))
                    .unwrap_or(false)
        })
        .expect("expected a user message carrying the tool-returned image");
    let content = image_message["content"].as_array().unwrap();
    let image_part = content
        .iter()
        .find(|p| p["type"] == "image_url")
        .expect("image_url part present");
    assert_eq!(
        image_part["image_url"]["url"],
        format!("data:image/png;base64,{TEST_IMAGE_BASE64}")
    );

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error { .. })),
        "Ollama can take the image, expected no notice, got {events:?}"
    );
}

/// DeepSeek cannot take an image at all. The synthetic follow-up message
/// must drop it exactly the way a pasted image already does, and post the
/// same transcript notice.
#[tokio::test]
async fn deepseek_drops_a_tool_returned_image_and_posts_a_notice() {
    let (second_body, events) = run_tool_returned_image_turn(Provider::DeepSeek).await;

    let messages = second_body["messages"]
        .as_array()
        .expect("messages should be an array");
    let has_image_part = messages.iter().any(|m| {
        m["content"]
            .as_array()
            .map(|parts| parts.iter().any(|p| p["type"] == "image_url"))
            .unwrap_or(false)
    });
    assert!(
        !has_image_part,
        "DeepSeek must never receive an image_url part, got {second_body:?}"
    );

    let notice = events
        .iter()
        .find_map(|e| match e {
            StreamEvent::Error { message } => Some(message.clone()),
            _ => None,
        })
        .expect("expected a notice (Error event) for the dropped tool-returned image");
    assert!(
        notice.contains("DeepSeek"),
        "notice should name the backend: {notice}"
    );
}
