//! Unit tests for `deepseek_custom::agent::agent_loop`, moved out of the
//! production module as part of the two-crate workspace split.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::agent_loop::{
    AgentConfig, AgentLoop, DEFAULT_CONTEXT_BUDGET, build_user_content, context_low_water,
};
use deepseek_custom::agent::events::{StreamEvent, SubagentId};
use deepseek_custom::agent::repeat::run_repeat;
use deepseek_custom::api::client::{ApiClient, Provider};
use deepseek_custom::api::types::{Content, ContentPart, ImageAttachment, Message};
use deepseek_custom::backend::Backend;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::backend::stub::StubBackend;
use deepseek_custom::effort::Effort;
use deepseek_custom::error::Result;
use deepseek_custom::tools::reset::ResetTool;
use deepseek_custom::tools::{Tool, ToolOutput, ToolRegistry};

use tokio::sync::mpsc;

struct EchoTool;
#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echoes input"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn execute(&self, _input: serde_json::Value) -> Result<ToolOutput> {
        Ok(ToolOutput {
            content: "echoed".into(),
            is_error: false,
            image: None,
        })
    }
}

#[test]
fn agent_config_defaults() {
    let cfg = AgentConfig::default();
    assert_eq!(cfg.max_turns, 100);
    assert_eq!(cfg.model, "deepseek-v4-flash");
    assert_eq!(cfg.effort, Effort::None);
}

#[test]
fn agent_loop_creates_with_history() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        client,
        tools,
        "test prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(agent.history().len(), 0);
}

#[test]
fn session_reset_clears_history() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let mut agent = AgentLoop::new(
        client,
        tools,
        "initial".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    // Push a user message then reset
    agent.history_mut().push(Message::user("hello".into()));
    assert_eq!(agent.history().len(), 1);

    agent.reset("fresh prompt".into(), "restart".into());
    // After reset: fresh system + 1 user message
    assert_eq!(agent.history().len(), 1);
}

#[test]
fn interrupt_flag_is_the_injected_arc_not_a_fresh_one() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let gui_flag = Arc::new(AtomicBool::new(false));
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig::default(),
        Arc::clone(&gui_flag),
    );

    // Set the flag through the handle the GUI would hold, never
    // through the agent's own getter, then check the agent's flag
    // reads true. That only happens if both point at the same
    // underlying `AtomicBool`.
    gui_flag.store(true, Ordering::SeqCst);

    assert!(agent.interrupt_flag().load(Ordering::SeqCst));
}

#[test]
fn voice_mode_flag_defaults_to_false() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(!agent.voice_mode_flag().load(Ordering::SeqCst));
}

#[test]
fn voice_mode_flag_handle_observes_writes() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    let flag = agent.voice_mode_flag();
    flag.store(true, Ordering::SeqCst);
    assert!(agent.voice_mode_flag().load(Ordering::SeqCst));
}

#[test]
fn effort_flag_defaults_to_none() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(Effort::load(&agent.effort_flag()), Effort::None);
}

#[test]
fn sync_dynamic_config_reads_the_effort_flag() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    Effort::Max.store(&agent.effort_flag());

    agent.sync_dynamic_config_for_test();

    assert_eq!(agent.config().effort, Effort::Max);
}

#[test]
fn sync_dynamic_config_sets_voice_suffix_when_flag_true() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    agent.voice_mode_flag().store(true, Ordering::SeqCst);

    agent.sync_dynamic_config_for_test();

    let api = agent.history().to_api_messages();
    assert!(
        api[0]
            .content
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .contains("## Voice reply mode")
    );
}

#[test]
fn sync_dynamic_config_clears_voice_suffix_when_flag_false() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    agent.voice_mode_flag().store(true, Ordering::SeqCst);
    agent.sync_dynamic_config_for_test();

    agent.voice_mode_flag().store(false, Ordering::SeqCst);
    agent.sync_dynamic_config_for_test();

    let api = agent.history().to_api_messages();
    assert!(
        !api[0]
            .content
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .contains("## Voice reply mode")
    );
}

#[test]
fn sync_dynamic_config_reports_working_dir_when_set() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    let working_dir = Arc::new(Mutex::new(PathBuf::from("C:\\proj")));
    agent.set_working_dir(Arc::clone(&working_dir));

    agent.sync_dynamic_config_for_test();

    let api = agent.history().to_api_messages();
    assert!(
        api[0]
            .content
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .contains("Working directory: C:\\proj.")
    );
}

#[test]
fn sync_dynamic_config_omits_working_dir_when_never_set() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    agent.sync_dynamic_config_for_test();

    let api = agent.history().to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("sys prompt")
    );
}

#[test]
fn changing_shared_working_dir_between_turns_changes_next_syncs_prompt() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    let working_dir = Arc::new(Mutex::new(PathBuf::from("C:\\proj")));
    agent.set_working_dir(Arc::clone(&working_dir));

    agent.sync_dynamic_config_for_test();
    let first = agent.history().to_api_messages();
    assert!(
        first[0]
            .content
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .contains("C:\\proj")
    );

    // Change the shared value the way a future Cd tool or GUI control
    // would, with no restart and no re-registering of the agent.
    *working_dir.lock().unwrap() = PathBuf::from("C:\\other");
    agent.sync_dynamic_config_for_test();
    let second = agent.history().to_api_messages();
    assert!(
        second[0]
            .content
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .contains("C:\\other")
    );
    assert!(
        !second[0]
            .content
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .contains("C:\\proj")
    );
}

#[test]
fn clear_history_then_sync_still_reports_the_current_directory() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    let working_dir = Arc::new(Mutex::new(PathBuf::from("C:\\proj")));
    agent.set_working_dir(Arc::clone(&working_dir));
    agent.sync_dynamic_config_for_test();
    agent.history_mut().push(Message::user("hello".into()));

    agent.clear_history();
    // Rebuilt history has dropped the working directory line, exactly
    // like it drops the voice suffix, until the next sync restores it.
    assert_eq!(agent.history().len(), 0);

    agent.sync_dynamic_config_for_test();
    let api = agent.history().to_api_messages();
    assert!(
        api[0]
            .content
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .contains("C:\\proj")
    );
}

#[test]
fn working_dir_and_voice_suffix_both_survive_together() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    let working_dir = Arc::new(Mutex::new(PathBuf::from("C:\\proj")));
    agent.set_working_dir(working_dir);
    agent.voice_mode_flag().store(true, Ordering::SeqCst);

    agent.sync_dynamic_config_for_test();

    let api = agent.history().to_api_messages();
    let content = api[0].content.as_ref().and_then(Content::as_text).unwrap();
    assert!(content.contains("Working directory: C:\\proj."));
    assert!(content.contains("## Voice reply mode"));
}

#[test]
fn context_low_water_is_a_third_of_budget() {
    assert_eq!(context_low_water(100_000), 33_333);
    assert_eq!(context_low_water(300), 100);
}

#[test]
fn context_budget_flag_defaults_to_100000() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(
        agent.context_budget_flag().load(Ordering::SeqCst),
        DEFAULT_CONTEXT_BUDGET
    );
}

#[test]
fn context_budget_flag_handle_observes_writes() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    let flag = agent.context_budget_flag();
    flag.store(42_000, Ordering::SeqCst);
    assert_eq!(agent.context_budget_flag().load(Ordering::SeqCst), 42_000);
}

#[test]
fn apply_prune_is_noop_under_budget() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    agent.history_mut().push(Message::user("hello".into()));
    let before = agent.history().estimated_tokens();

    let report = agent.apply_prune_for_test(None);

    assert_eq!(report.tokens_before, before);
    assert_eq!(report.tokens_after, before);
    assert_eq!(agent.history().estimated_tokens(), before);
    assert_eq!(agent.history().len(), 1);
}

#[test]
fn apply_prune_reduces_oversized_history_to_low_water() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    // Push far more content than a small test budget allows. The last
    // two groups are pinned and never touched by any tier, so keep
    // them short. A large pinned tail would set a floor above the
    // low water mark, and no amount of pruning could reach it.
    for i in 0..18 {
        agent
            .history_mut()
            .push(Message::user(format!("question {i} {}", "x".repeat(200))));
        agent.history_mut().push(Message::assistant(format!(
            "answer {i} {}",
            "x".repeat(200)
        )));
    }
    agent.history_mut().push(Message::user("hi".into()));
    agent.history_mut().push(Message::assistant("ok".into()));
    agent.history_mut().push(Message::user("bye".into()));
    agent.history_mut().push(Message::assistant("ok".into()));

    agent.context_budget_flag().store(200, Ordering::SeqCst);

    let report = agent.apply_prune_for_test(None);

    assert!(agent.history().estimated_tokens() <= context_low_water(200));
    assert_eq!(report.tokens_after, agent.history().estimated_tokens());
    assert!(report.tokens_after < report.tokens_before);
    assert!(
        report.tool_bodies_elided > 0 || report.groups_collapsed > 0 || report.groups_dropped > 0
    );
}

#[test]
fn apply_prune_with_none_scores_does_not_panic() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    for i in 0..10 {
        agent.history_mut().push(Message::user(format!("q{i}")));
        agent
            .history_mut()
            .push(Message::assistant(format!("a{i}")));
    }
    agent.context_budget_flag().store(1, Ordering::SeqCst);

    let report = agent.apply_prune_for_test(None);
    assert!(report.tokens_after <= report.tokens_before);
}

#[test]
fn rebuild_system_prompt_clears_messages() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let mut agent = AgentLoop::new(
        client,
        tools,
        "initial".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    agent.history_mut().push(Message::user("hi".into()));

    agent.rebuild_system_prompt(Some("memory"), Some("skills"));
    assert_eq!(agent.history().len(), 0);
}

#[tokio::test]
async fn execute_tool_returns_output() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    let output = agent.execute_tool_for_test("echo", "{}").await;
    assert!(!output.is_error);
    assert_eq!(output.content, "echoed");
}

#[tokio::test]
async fn execute_unknown_tool_returns_error() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    let output = agent.execute_tool_for_test("nonexistent", "{}").await;
    assert!(output.is_error);
    assert!(output.content.contains("Unknown tool"));
}

/// A `write` call cut off by the output cap arrives as JSON that simply
/// stops. The old message was serde's own text alone, and a real run shows
/// what that costs: the model read "EOF while parsing a string" as "the
/// payload is too long for the tool", tried three smaller writes, and then
/// went looking for a way to build the file through `bash`. The message
/// has to name the cap.
#[tokio::test]
async fn truncated_tool_arguments_report_the_output_cap() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig {
            max_tokens: 4096,
            ..Default::default()
        },
        Arc::new(AtomicBool::new(false)),
    );

    let output = agent
        .execute_tool_for_test("echo", "{\"content\": \"fn main() { unfinis")
        .await;

    assert!(output.is_error);
    assert!(output.content.contains("4096"), "{}", output.content);
    assert!(output.content.contains("edit"), "{}", output.content);
}

/// Arguments that are malformed rather than cut off keep the plain parse
/// error. Blaming the cap here would send the model chasing a limit it
/// never reached.
#[tokio::test]
async fn malformed_tool_arguments_do_not_blame_the_output_cap() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = AgentLoop::new(
        client,
        tools,
        "test".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    let output = agent.execute_tool_for_test("echo", "{\"a\" 1}").await;

    assert!(output.is_error);
    assert!(
        output.content.contains("Invalid input"),
        "{}",
        output.content
    );
    assert!(!output.content.contains("output cap"), "{}", output.content);
}

fn sample_image() -> ImageAttachment {
    ImageAttachment {
        data: "AAA".into(),
        media_type: "image/png".into(),
    }
}

#[test]
fn build_user_content_with_no_image_is_plain_text_on_either_provider() {
    for provider in [Provider::DeepSeek, Provider::Ollama] {
        let built = build_user_content(provider, "hello", None);
        assert_eq!(built.content, Content::text("hello"));
        assert!(built.notice.is_none(), "provider {provider:?}");
    }
}

#[test]
fn build_user_content_drops_the_image_on_deepseek_and_names_it_in_the_notice() {
    let image = sample_image();
    let built = build_user_content(Provider::DeepSeek, "look at this", Some(&image));

    assert_eq!(built.content, Content::text("look at this"));
    let notice = built
        .notice
        .expect("expected a notice for a DeepSeek image attachment");
    assert!(
        notice.contains("DeepSeek"),
        "notice should name the backend: {notice}"
    );
}

#[test]
fn build_user_content_maps_the_image_to_an_image_url_part_on_ollama() {
    let image = sample_image();
    let built = build_user_content(Provider::Ollama, "look at this", Some(&image));

    assert!(built.notice.is_none());
    assert_eq!(
        built.content,
        Content::Parts(vec![
            ContentPart::Text {
                text: "look at this".into(),
            },
            ContentPart::ImageUrl {
                url: "data:image/png;base64,AAA".into(),
            },
        ])
    );
}

/// Integration test: spawn a mock HTTP server returning SSE with reasoning_content,
/// run the full agent loop, and verify Reasoning events are emitted.
#[tokio::test]
async fn effort_above_none_emits_reasoning_events() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    // Bind to port 0 so the OS assigns a free port
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();

    // SSE response with reasoning_content, then text, then stop + [DONE]
    let response_body = concat!(
        "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":",
        "{\"content\":null,\"reasoning_content\":\"I should think about this\"},",
        "\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":",
        "{\"content\":\"The answer is 42\"},",
        "\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    // Spawn mock server thread
    let response = response_body.to_string();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        // Read the HTTP request (header part)
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        // Write HTTP response
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
            response.len(),
            response,
        );
        let _ = stream.write_all(http_response.as_bytes());
        let _ = stream.flush();
        // Keep connection alive briefly so client can read
        thread::sleep(std::time::Duration::from_millis(200));
    });

    // Create client pointing at mock server
    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        Some(format!("http://127.0.0.1:{port}")),
    );

    let tools = ToolRegistry::new();
    let config = AgentConfig {
        effort: Effort::High,
        ..AgentConfig::default()
    };

    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        config,
        Arc::new(AtomicBool::new(false)),
    );

    // Capture events via channel
    let (tx, mut rx) = mpsc::unbounded_channel();
    agent.set_event_sender(tx);

    // Run the agent
    let result = agent.run("hello").await;
    assert!(
        result.is_ok(),
        "agent run should succeed: {:?}",
        result.err()
    );
    let responses = result.unwrap();
    assert!(!responses.is_empty(), "should have response text");

    // Collect all events
    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
    }

    // Verify Reasoning events were emitted
    let reasoning_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::Reasoning { .. }))
        .collect();
    assert!(
        !reasoning_events.is_empty(),
        "expected at least one Reasoning event, got events: {:?}",
        events
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Verify reasoning content is correct
    let reasoning: String = reasoning_events
        .iter()
        .filter_map(|e| {
            if let StreamEvent::Reasoning { text, .. } = e {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(reasoning, "I should think about this");

    // Verify Text events were also emitted
    let text_count = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::Text { .. }))
        .count();
    assert!(text_count > 0, "expected at least one Text event");
}

/// Build a stub subagent session ready to register into a
/// `SubagentRegistry`. Its own script never matters here: these tests
/// only check whether the session is still registered afterward, not
/// what it would have answered.
fn stub_session() -> Backend {
    Backend::Stub(Box::new(StubBackend::new(
        Vec::new(),
        "stub-model".to_string(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    )))
}

/// A minimal mock server that answers one turn with plain text and no
/// tool calls, so `run` completes on its first attempt with no retry
/// delay. Returns the port it bound to.
fn spawn_text_only_mock_server() -> u16 {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let response_body = concat!(
        "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":",
        "{\"content\":\"hi\"},",
        "\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
            response_body.len(),
            response_body,
        );
        let _ = stream.write_all(http_response.as_bytes());
        let _ = stream.flush();
        thread::sleep(std::time::Duration::from_millis(200));
    });

    port
}

/// The lifetime rule from the roadmap's Phase 3: a subagent session
/// registered against the agent's registry does not survive the turn
/// that opened it. `run` wraps `run_turn` specifically to guarantee
/// this regardless of how the turn finishes.
#[tokio::test]
async fn no_session_survives_a_parent_turn_end() {
    let port = spawn_text_only_mock_server();
    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        Some(format!("http://127.0.0.1:{port}")),
    );
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    let registry = Arc::new(SubagentRegistry::new());
    agent.set_subagent_registry(Arc::clone(&registry));
    let id = SubagentId::next();
    registry.register(id, stub_session()).await;
    assert!(
        registry.contains(id).await,
        "session should be live before the turn runs"
    );

    let result = agent.run("hello").await;

    assert!(result.is_ok(), "turn should succeed: {:?}", result.err());
    assert!(
        !registry.contains(id).await,
        "session should not survive the turn that opened it"
    );
    assert_eq!(registry.len().await, 0);
}

/// `Reset` closes every session, per the roadmap's Phase 3 lifetime
/// rule, even though `ResetTool` itself never touches the registry: the
/// close happens in `execute_tool`'s `SessionReset` branch.
#[tokio::test]
async fn no_session_survives_a_reset() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(ResetTool));
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    let registry = Arc::new(SubagentRegistry::new());
    agent.set_subagent_registry(Arc::clone(&registry));
    let id = SubagentId::next();
    registry.register(id, stub_session()).await;

    let output = agent
        .execute_tool_for_test("reset", "{\"prompt\":\"start fresh\"}")
        .await;

    assert!(!output.is_error);
    assert!(
        !registry.contains(id).await,
        "session should not survive a reset"
    );
    assert_eq!(registry.len().await, 0);
}

#[test]
fn clear_history_drops_messages_keeps_system_prompt() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    agent.history_mut().push(Message::user("hello".into()));
    assert_eq!(agent.history().len(), 1);

    agent.clear_history();

    assert_eq!(agent.history().len(), 0);
    let api = agent.history().to_api_messages();
    assert_eq!(api.len(), 1);
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("sys prompt")
    );
}

#[test]
fn restore_history_replaces_messages_and_keeps_system_prompt() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    agent.history_mut().push(Message::user("stale".into()));

    agent.restore_history(vec![
        Message::user("saved one".into()),
        Message::assistant("saved reply".into()),
    ]);

    assert_eq!(agent.history().len(), 2);
    let api = agent.history().to_api_messages();
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("sys prompt")
    );
}

#[test]
fn clear_history_after_voice_suffix_still_produces_working_system_message() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys prompt".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    agent.voice_mode_flag().store(true, Ordering::SeqCst);
    agent.sync_dynamic_config_for_test();
    agent.history_mut().push(Message::user("hello".into()));

    agent.clear_history();

    assert_eq!(agent.history().len(), 0);
    let api = agent.history().to_api_messages();
    assert_eq!(api.len(), 1);
    assert_eq!(
        api[0].content.as_ref().and_then(Content::as_text),
        Some("sys prompt")
    );

    // sync_dynamic_config still works after clear_history and restores
    // the voice suffix on the next turn.
    agent.sync_dynamic_config_for_test();
    let api = agent.history().to_api_messages();
    assert!(
        api[0]
            .content
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .contains("## Voice reply mode")
    );
}

#[tokio::test]
async fn run_repeat_zero_iterations_emits_only_repeat_finished() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    agent.set_event_sender(tx);

    run_repeat(&mut agent, "do the thing", 0, &std::env::temp_dir()).await;

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
    }
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::RepeatFinished { completed, total } => {
            assert_eq!(*completed, 0);
            assert_eq!(*total, 0);
        }
        other => panic!("expected RepeatFinished, got {other:?}"),
    }
}

#[tokio::test]
async fn run_repeat_stops_immediately_when_interrupt_flag_already_set() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    agent.set_event_sender(tx);
    agent.repeat_interrupt_flag().store(true, Ordering::SeqCst);

    run_repeat(&mut agent, "do the thing", 3, &std::env::temp_dir()).await;

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
    }
    // No iteration should have started.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, StreamEvent::RepeatIterationStart { .. }))
    );
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::RepeatFinished { completed, total } => {
            assert_eq!(*completed, 0);
            assert_eq!(*total, 3);
        }
        other => panic!("expected RepeatFinished, got {other:?}"),
    }
}

/// Integration test: spawn a mock HTTP server that serves two
/// connections, each a minimal text-only SSE response, and run
/// `run_repeat` for two iterations against it.
#[tokio::test]
async fn run_repeat_two_iterations_against_mock_server() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();

    fn response_for(text: &str) -> String {
        format!(
            concat!(
                "data: {{\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
                "\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":",
                "{{\"content\":\"{}\"}},",
                "\"finish_reason\":null}}]}}\n\n",
                "data: {{\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
                "\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":{{}},",
                "\"finish_reason\":\"stop\"}}]}}\n\n",
                "data: [DONE]\n\n",
            ),
            text
        )
    }

    thread::spawn(move || {
        for i in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = response_for(&format!("answer {i}"));
            let http_response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body,
            );
            let _ = stream.write_all(http_response.as_bytes());
            let _ = stream.flush();
            thread::sleep(std::time::Duration::from_millis(100));
        }
    });

    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        Some(format!("http://127.0.0.1:{port}")),
    );

    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    agent.set_event_sender(tx);

    run_repeat(&mut agent, "do the task", 2, &std::env::temp_dir()).await;

    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev.event);
    }

    let starts: Vec<(u32, u32)> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::RepeatIterationStart { index, total, .. } => Some((*index, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec![(1, 2), (2, 2)]);

    let finished = events
        .iter()
        .find(|e| matches!(e, StreamEvent::RepeatFinished { .. }))
        .expect("expected a RepeatFinished event");
    match finished {
        StreamEvent::RepeatFinished { completed, total } => {
            assert_eq!(*completed, 2);
            assert_eq!(*total, 2);
        }
        _ => unreachable!(),
    }

    // History at the end should hold only the last iteration's
    // messages: the user message plus the assistant reply.
    assert_eq!(agent.history().len(), 2);
    let api = agent.history().to_api_messages();
    let last_assistant = api
        .iter()
        .rev()
        .find(|m| m.role == deepseek_custom::api::types::Role::Assistant)
        .expect("expected an assistant message");
    assert_eq!(
        last_assistant.content.as_ref().and_then(Content::as_text),
        Some("answer 1")
    );
}
