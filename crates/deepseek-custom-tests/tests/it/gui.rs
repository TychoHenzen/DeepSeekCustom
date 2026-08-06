//! Unit tests for `deepseek_custom::gui` (`src/gui/mod.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use deepseek_custom::agent::agent_loop::{
    AgentCommand, RouteHop, RoutedEvent, StreamEvent, SubagentId, SubagentMeta,
};
use deepseek_custom::api::types::{Content, ImageAttachment, Message, Role};
use deepseek_custom::config::settings::{
    ApiProvider, BackendConfig, Settings, TriggerMode, VoiceConfig,
};
use deepseek_custom::effort::Effort;
use deepseek_custom::gui::agent_handles::AgentHandles;
use deepseek_custom::gui::attachment::decode_image_bytes;
use deepseek_custom::gui::autopilot_tab::{
    AutopilotProgress, apply_autopilot_iterations, apply_autopilot_task,
};
use deepseek_custom::gui::backend_picker::apply_default_backend;
use deepseek_custom::gui::settings_panel::{
    apply_context_budget, apply_effort, apply_show_raw_output, apply_working_dir,
};
use deepseek_custom::gui::transcript::{
    Block, BlockKind, Severity, Span, SubagentState, Transcript,
};
use deepseek_custom::gui::voice_ui::{
    apply_stt_enabled, apply_trigger_mode, apply_tts_enabled, apply_tts_speed, apply_tts_voice,
    apply_voice_enabled, apply_wake_phrase, voice_mode_flag_for_tts,
};
use deepseek_custom::gui::{
    ActiveTab, BLOCK_GAP, DeepSeekGui, IMAGE_LABEL_COLOR, TOOL_ERROR_COLOR, TURN_GAP, block_color,
    format_elapsed_ms, gap_before, raw_block_text, raw_span_text, role_label, severity_color,
    subagent_elapsed_ms, subagent_header_summary, subagent_state_color, tool_color,
    tool_output_color, tool_summary, truncate_args,
};
use deepseek_custom::voice::service::{VoiceCommand, VoiceEvent};

use eframe::egui::Color32;
use tokio::sync::mpsc;

fn make_gui() -> DeepSeekGui {
    make_gui_with_settings(&Settings::default())
}

/// Create a uniquely named directory under the system temp dir, so a
/// test that saves never touches the repository's real settings.json.
fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-gui-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build a GUI from a specific settings value, so a test can check how
/// the panel controls get seeded.
fn make_gui_with_settings(settings: &Settings) -> DeepSeekGui {
    make_gui_in(settings, unique_temp_dir("seed"))
}

/// Build a GUI over a specific project root, for tests that save.
fn make_gui_in(settings: &Settings, project_root: PathBuf) -> DeepSeekGui {
    let (_tx_events, rx_events) = mpsc::unbounded_channel();
    let (tx_input, _rx_input) = mpsc::unbounded_channel();
    // Mirrors main.rs: the effort flag is seeded from settings before
    // the GUI is constructed, so `new` can read the starting level back
    // off the flag the same way it does for `context_budget_flag`.
    let effort_flag = Arc::new(AtomicU8::new(0));
    settings.effort().store(&effort_flag);
    DeepSeekGui::new(
        rx_events,
        tx_input,
        AgentHandles {
            interrupt: Arc::new(AtomicBool::new(false)),
            effort: effort_flag,
            voice_mode: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(100_000)),
            model: Arc::new(Mutex::new("deepseek-v4-flash".into())),
            working_dir: Arc::new(Mutex::new(project_root.clone())),
        },
        settings.clone(),
        project_root,
    )
}

/// A settings value with every panel control set away from its default.
fn settings_with_voice(tts_voice: &str) -> Settings {
    Settings {
        show_raw_output: Some(true),
        voice: Some(VoiceConfig {
            enabled: true,
            stt_enabled: true,
            tts_enabled: true,
            stt_model_path: None,
            tts_model_path: None,
            tts_voices_path: None,
            trigger_mode: TriggerMode::WakeWord,
            wake_phrase: Some("hey computer".to_string()),
            tts_voice: Some(tts_voice.to_string()),
            tts_speed: Some(1.4),
        }),
        ..Settings::default()
    }
}

/// A settings value with two backends, "alpha" and "beta", and the
/// given `default_backend`.
fn settings_with_backends(default_backend: Option<&str>) -> Settings {
    let mut backends = HashMap::new();
    backends.insert(
        "alpha".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "alpha-model".to_string(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    backends.insert(
        "beta".to_string(),
        BackendConfig::ClaudeCli {
            model: "beta-model".to_string(),
            permission_mode: None,
            env: None,
            models: None,
        },
    );
    Settings {
        backends: Some(backends),
        default_backend: default_backend.map(|s| s.to_string()),
        ..Settings::default()
    }
}

#[tokio::test]
async fn model_options_always_contain_the_backends_declared_model_after_a_fetch() {
    let mut gui = make_gui_with_settings(&settings_with_backends(Some("alpha")));
    let (backend_name, models) = gui
        .backends_mut_for_test()
        .recv_fetched_list()
        .await
        .expect("the startup fetch must report a result");
    assert_eq!(backend_name, "alpha");
    gui.backends_mut_for_test()
        .apply_fetched_list(&backend_name, models);
    let current = gui.backends_mut_for_test().model().to_string();
    assert!(
        gui.backends_mut_for_test()
            .model_options()
            .contains(&current)
    );
}

/// The spans of the only block in the transcript, which must be an
/// `Assistant` block. Panics otherwise, so a test that expects
/// assistant output fails loudly on any other block kind.
fn only_assistant_spans(gui: &DeepSeekGui) -> Vec<Span> {
    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(blocks.len(), 1, "expected exactly one block");
    let BlockKind::Assistant { spans } = &blocks[0].kind else {
        panic!("expected an Assistant block, got {:?}", blocks[0].kind);
    };
    spans.clone()
}

#[test]
fn reasoning_event_adds_payload_line() {
    let mut gui = make_gui();
    gui.handle_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: "Let me think about this...".into(),
    });
    assert_eq!(
        only_assistant_spans(&gui),
        vec![Span::Reasoning("Let me think about this...".into())]
    );
}

#[test]
fn reasoning_event_appends_to_last_reasoning_line() {
    let mut gui = make_gui();
    gui.handle_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: "First".into(),
    });
    gui.handle_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: "Second".into(),
    });
    assert_eq!(
        only_assistant_spans(&gui),
        vec![Span::Reasoning("FirstSecond".into())]
    );
}

fn tool_block(args: &str, output: Option<&str>, is_error: bool) -> Block {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::ToolCall {
        tool: "Bash".into(),
        args: args.into(),
        output: output.map(str::to_string),
        is_error,
    });
    transcript.find(id).unwrap().clone()
}

#[test]
fn role_label_names_each_kind() {
    assert_eq!(role_label(&BlockKind::User { text: "hi".into() }), "You");
    assert_eq!(
        role_label(&BlockKind::Assistant { spans: Vec::new() }),
        "Assistant"
    );
    assert_eq!(role_label(&tool_block("ls", None, false).kind), "Tool");
    assert_eq!(
        role_label(&BlockKind::Notice {
            text: "reset".into(),
            severity: Severity::Info,
        }),
        "Notice"
    );
    assert_eq!(
        role_label(&BlockKind::Image {
            image: test_image_attachment(),
        }),
        "Image"
    );
}

#[test]
fn severity_color_is_distinct_per_severity() {
    let colors = [
        severity_color(Severity::Error),
        severity_color(Severity::Warning),
        severity_color(Severity::Info),
        severity_color(Severity::Debug),
    ];
    for (index, color) in colors.iter().enumerate() {
        for other in &colors[index + 1..] {
            assert_ne!(color, other, "each severity needs its own colour");
        }
    }
}

#[test]
fn truncate_args_leaves_short_arguments_alone() {
    assert_eq!(truncate_args("ls -la", 60), "ls -la");
}

#[test]
fn truncate_args_flattens_newlines_onto_one_line() {
    assert_eq!(truncate_args("echo one\necho two", 60), "echo one echo two");
}

#[test]
fn truncate_args_cuts_long_arguments_and_marks_the_cut() {
    let truncated = truncate_args(&"x".repeat(100), 10);
    assert_eq!(truncated, format!("{}...", "x".repeat(10)));
}

#[test]
fn truncate_args_counts_characters_not_bytes() {
    // A cut by byte index would panic here, since each character is
    // three bytes wide.
    let truncated = truncate_args(&"\u{4f60}".repeat(10), 4);
    assert_eq!(truncated, format!("{}...", "\u{4f60}".repeat(4)));
}

#[test]
fn a_collapsed_tool_summary_names_the_tool_and_shows_arguments() {
    let summary = tool_summary(true, "Bash", "ls -la", false);
    assert!(summary.contains("Bash"), "summary must name the tool");
    assert!(summary.contains("ls -la"), "summary must show arguments");
}

#[test]
fn an_errored_tool_summary_says_so_in_both_states() {
    for collapsed in [true, false] {
        let summary = tool_summary(collapsed, "Bash", "boom", true);
        assert!(
            summary.contains("[error]"),
            "an error must be visible without expanding the call"
        );
    }
    assert!(!tool_summary(true, "Bash", "ok", false).contains("[error]"));
}

#[test]
fn an_errored_tool_call_is_red_in_both_states() {
    assert_eq!(tool_color(true), TOOL_ERROR_COLOR);
    assert_eq!(tool_output_color(true), TOOL_ERROR_COLOR);
    assert_ne!(tool_color(false), TOOL_ERROR_COLOR);
    assert_ne!(tool_output_color(false), TOOL_ERROR_COLOR);
}

#[test]
fn a_user_block_after_the_first_opens_a_turn_with_a_wider_gap() {
    let user = BlockKind::User {
        text: "hello".into(),
    };
    assert_eq!(gap_before(1, &user), TURN_GAP);
    assert_eq!(gap_before(3, &user), TURN_GAP);
}

#[test]
fn the_first_block_and_non_user_blocks_get_the_plain_gap() {
    let user = BlockKind::User {
        text: "hello".into(),
    };
    assert_eq!(gap_before(0, &user), BLOCK_GAP);
    assert_eq!(
        gap_before(2, &BlockKind::Assistant { spans: Vec::new() }),
        BLOCK_GAP
    );
    assert!(TURN_GAP > BLOCK_GAP, "a turn boundary must read as wider");
}

#[test]
fn raw_text_marks_reasoning_apart_from_reply_text() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::Assistant {
        spans: vec![Span::Reasoning("thought".into()), Span::Text("said".into())],
    });
    let block = transcript.find(id).unwrap();
    assert_eq!(
        raw_block_text(block),
        "Assistant:\n[reasoning] thought\nsaid"
    );
}

#[test]
fn raw_text_for_a_running_tool_call_says_it_is_running() {
    let block = tool_block("ls", None, false);
    assert!(raw_block_text(&block).contains("(running)"));
    let done = tool_block("ls", Some("file1"), false);
    assert!(raw_block_text(&done).contains("file1"));
}

#[test]
fn raw_text_for_a_tool_call_carries_its_arguments_and_output() {
    let block = tool_block("ls -la", Some("file1\nfile2"), false);
    let text = raw_block_text(&block);
    assert!(
        text.contains("ls -la"),
        "arguments must be reachable: {text}"
    );
    assert!(
        text.contains("file1\nfile2"),
        "output must be reachable: {text}"
    );
}

#[test]
fn raw_text_for_a_user_block_carries_the_you_role_label_and_exact_text() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::User {
        text: "hello there".into(),
    });
    let block = transcript.find(id).unwrap();
    assert_eq!(raw_block_text(block), "You: hello there");
}

#[test]
fn raw_text_for_a_notice_block_carries_its_text() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::Notice {
        text: "Session reset".into(),
        severity: Severity::Info,
    });
    let block = transcript.find(id).unwrap();
    assert_eq!(raw_block_text(block), "Session reset");
}

#[test]
fn raw_text_for_a_subagent_block_carries_its_header_and_nested_content() {
    let mut transcript = Transcript::new();
    let subagent_id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![RouteHop {
            id: subagent_id,
            meta: SubagentMeta {
                backend: "ollama".into(),
                model: "test-model".into(),
                depth: 1,
            },
            session_turns: 1,
            session_turn_cap: 20,
            send_message_calls: 0,
            send_message_call_cap: 10,
        }],
        event: StreamEvent::Text {
            turn: 1,
            text: "hi from a subagent".into(),
        },
    });
    let block = &transcript.blocks()[0];
    let text = raw_block_text(block);
    assert!(text.contains("ollama"), "must name the backend: {text}");
    assert!(text.contains("test-model"), "must name the model: {text}");
    assert!(text.contains("RUNNING"), "must show the state: {text}");
    assert!(
        text.contains("hi from a subagent"),
        "must carry the nested content: {text}"
    );
    assert!(
        text.contains("turns 1/20"),
        "must show the turn count and cap: {text}"
    );
    assert!(
        text.contains("sends 0/10"),
        "must show the call count and cap: {text}"
    );
}

#[test]
fn raw_span_text_marks_reasoning_and_leaves_reply_text_plain() {
    assert_eq!(raw_span_text(&Span::Text("hello".into())), "hello");
    assert_eq!(
        raw_span_text(&Span::Reasoning("thinking".into())),
        "[reasoning] thinking"
    );
}

#[test]
fn block_color_follows_kind_and_severity() {
    assert_eq!(
        block_color(&BlockKind::Assistant { spans: Vec::new() }),
        Color32::WHITE
    );
    assert_eq!(
        block_color(&tool_block("ls", None, true).kind),
        TOOL_ERROR_COLOR
    );
    assert_eq!(
        block_color(&BlockKind::Notice {
            text: "boom".into(),
            severity: Severity::Error,
        }),
        severity_color(Severity::Error)
    );
    assert_eq!(
        block_color(&BlockKind::Image {
            image: test_image_attachment(),
        }),
        IMAGE_LABEL_COLOR
    );
}

/// A tiny, real base64 payload for image-block tests: the 1x1
/// transparent PNG egui's own examples use. Its bytes are not
/// inspected, so any well-formed base64 string would do, but a real
/// PNG keeps the fixture honest about what this block actually holds.
fn test_image_attachment() -> ImageAttachment {
    ImageAttachment {
        data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".into(),
        media_type: "image/png".into(),
    }
}

#[test]
fn decode_image_bytes_accepts_well_formed_base64() {
    let image = test_image_attachment();
    let bytes = decode_image_bytes(&image).expect("valid base64 must decode");
    // PNG's fixed 8-byte magic number.
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']
    );
}

#[test]
fn decode_image_bytes_rejects_malformed_base64() {
    let image = ImageAttachment {
        data: "not valid base64 !!!".into(),
        media_type: "image/png".into(),
    };
    assert!(decode_image_bytes(&image).is_none());
}

#[test]
fn raw_block_text_names_the_image_media_type() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::Image {
        image: test_image_attachment(),
    });
    let block = transcript.find(id).unwrap();
    let text = raw_block_text(block);
    assert!(
        text.contains("image/png"),
        "must name the media type: {text}"
    );
}

#[test]
fn submit_current_input_attaches_the_pending_image_and_clears_the_strip() {
    let mut gui = make_gui();
    gui.set_attachment_for_test(test_image_attachment());
    gui.set_input_buffer_for_test("look at this");

    gui.submit_current_input_for_test();

    assert!(
        gui.attachment_for_test().is_empty(),
        "the strip must clear on send"
    );
    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(blocks.len(), 2, "a User block and an Image block");
    assert!(matches!(blocks[0].kind, BlockKind::User { .. }));
    assert!(matches!(blocks[1].kind, BlockKind::Image { .. }));
}

/// The image belongs on `AgentCommand::UserTurn` itself, not on a side
/// channel delivered separately. This is the fix for P6S05: a shared
/// `Arc<Mutex<Option<ImageAttachment>>>` used to carry the image next
/// to the command, with no guarantee the two paired up. Proves text
/// and image now arrive on the very same command.
#[test]
fn submit_current_input_sends_text_and_image_on_the_same_command() {
    let mut gui = make_gui();
    let (tx_input, mut rx_input) = mpsc::unbounded_channel();
    gui.set_tx_input_for_test(tx_input);
    gui.set_attachment_for_test(test_image_attachment());
    gui.set_input_buffer_for_test("look at this");

    gui.submit_current_input_for_test();

    match rx_input.try_recv().unwrap() {
        AgentCommand::UserTurn { text, image } => {
            assert_eq!(text, "look at this");
            assert_eq!(image, Some(test_image_attachment()));
        }
        other => panic!("expected UserTurn, got {other:?}"),
    }
}

/// Two turns sent back to back must each carry their own attachment,
/// never the other's. This is exactly the mis-pairing a shared side
/// channel allowed: a second send could overwrite the first turn's
/// image before the agent task read it out.
#[test]
fn two_turns_in_a_row_each_carry_their_own_attachment() {
    let mut gui = make_gui();
    let (tx_input, mut rx_input) = mpsc::unbounded_channel();
    gui.set_tx_input_for_test(tx_input);

    gui.set_attachment_for_test(test_image_attachment());
    gui.set_input_buffer_for_test("first turn");
    gui.submit_current_input_for_test();

    gui.set_input_buffer_for_test("second turn");
    gui.submit_current_input_for_test();

    match rx_input.try_recv().unwrap() {
        AgentCommand::UserTurn { text, image } => {
            assert_eq!(text, "first turn");
            assert_eq!(image, Some(test_image_attachment()));
        }
        other => panic!("expected UserTurn, got {other:?}"),
    }
    match rx_input.try_recv().unwrap() {
        AgentCommand::UserTurn { text, image } => {
            assert_eq!(text, "second turn");
            assert_eq!(
                image, None,
                "the second turn must not inherit the first's image"
            );
        }
        other => panic!("expected UserTurn, got {other:?}"),
    }
}

#[test]
fn submit_current_input_sends_an_image_with_no_text() {
    let mut gui = make_gui();
    gui.set_attachment_for_test(test_image_attachment());
    assert!(gui.input_buffer_for_test().is_empty());

    gui.submit_current_input_for_test();

    assert_eq!(
        gui.session_status_for_test(),
        "Running...",
        "an image-only turn must still send"
    );
    assert!(gui.attachment_for_test().is_empty());
}

#[test]
fn subagent_header_summary_names_backend_model_depth_and_elapsed() {
    let header = subagent_header_summary(
        "ollama",
        "test-model",
        2,
        SubagentState::Running,
        1500,
        1,
        20,
        0,
        10,
    );
    assert!(header.contains("ollama"), "must name the backend: {header}");
    assert!(
        header.contains("test-model"),
        "must name the model: {header}"
    );
    assert!(header.contains('2'), "must name the depth: {header}");
    assert!(header.contains("1.5s"), "must show elapsed time: {header}");
}

/// The header must name both runaway-cost counts against their caps,
/// and it must do so while the session is still comfortably under
/// both: the roadmap calls for the counts to be visible the whole
/// time a session is open, not only once a cap trips.
#[test]
fn subagent_header_summary_names_both_counts_against_their_caps() {
    let header = subagent_header_summary(
        "ollama",
        "test-model",
        1,
        SubagentState::Running,
        0,
        3,
        20,
        2,
        10,
    );
    assert!(
        header.contains("3/20"),
        "must show turns against its cap: {header}"
    );
    assert!(
        header.contains("2/10"),
        "must show sends against its cap: {header}"
    );
}

#[test]
fn subagent_header_summary_carries_a_distinct_badge_for_every_state() {
    let states = [
        SubagentState::Running,
        SubagentState::Done,
        SubagentState::Failed,
        SubagentState::Interrupted,
    ];
    let headers: Vec<String> = states
        .iter()
        .map(|state| subagent_header_summary("ollama", "m", 1, *state, 0, 1, 20, 0, 10))
        .collect();
    for (index, header) in headers.iter().enumerate() {
        for other in &headers[index + 1..] {
            assert_ne!(header, other, "each state needs its own header text");
        }
    }
    assert!(headers[0].contains("RUNNING"));
    assert!(headers[1].contains("DONE"));
    assert!(headers[2].contains("FAILED"));
    assert!(headers[3].contains("INTERRUPTED"));
}

#[test]
fn subagent_state_color_is_distinct_per_state() {
    let colors = [
        subagent_state_color(SubagentState::Running),
        subagent_state_color(SubagentState::Done),
        subagent_state_color(SubagentState::Failed),
        subagent_state_color(SubagentState::Interrupted),
    ];
    for (index, color) in colors.iter().enumerate() {
        for other in &colors[index + 1..] {
            assert_ne!(color, other, "each state needs its own colour");
        }
    }
}

#[test]
fn subagent_elapsed_ms_uses_the_stored_value_once_terminal() {
    assert_eq!(subagent_elapsed_ms(None, 4200), 4200);
}

#[test]
fn subagent_elapsed_ms_computes_a_live_value_while_running() {
    let start = Instant::now() - Duration::from_millis(50);
    let live = subagent_elapsed_ms(Some(start), 0);
    assert!(
        live >= 50,
        "a running block's elapsed time must grow from `started_at`, got {live}"
    );
}

#[test]
fn format_elapsed_ms_shows_one_decimal_of_seconds() {
    assert_eq!(format_elapsed_ms(1500), "1.5s");
    assert_eq!(format_elapsed_ms(0), "0.0s");
}

#[test]
fn text_event_creates_white_payload_lines() {
    let mut gui = make_gui();
    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "Hello world".into(),
    });
    // The colour this used to assert on now lives in the block kind:
    // a `Text` span inside an `Assistant` block is what the renderer
    // draws white and passes to the markdown viewer.
    assert_eq!(
        only_assistant_spans(&gui),
        vec![Span::Text("Hello world".into())]
    );
}

#[test]
fn user_input_is_a_user_block_not_an_assistant_one() {
    let mut gui = make_gui();
    gui.set_input_buffer_for_test("pick a number between 1 and 100");
    gui.submit_current_input_for_test();
    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(blocks.len(), 1, "exactly one block must be appended");
    assert_eq!(
        blocks[0].kind,
        BlockKind::User {
            text: "pick a number between 1 and 100".into(),
        },
        "user input must be a User block, never assistant output"
    );

    // A second submission must not merge into the first: unlike an
    // Assistant block's spans, a User block never coalesces with an
    // earlier one.
    gui.set_input_buffer_for_test("actually pick a letter instead");
    gui.submit_current_input_for_test();
    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(
        blocks.len(),
        2,
        "a second submission must append a new block, not merge"
    );
    assert_eq!(
        blocks[0].kind,
        BlockKind::User {
            text: "pick a number between 1 and 100".into(),
        },
        "the first block's text must stay untouched by the second submission"
    );
    assert_eq!(
        blocks[1].kind,
        BlockKind::User {
            text: "actually pick a letter instead".into(),
        }
    );
    assert_ne!(blocks[0].id, blocks[1].id);
}

#[test]
fn model_output_is_an_assistant_text_span_for_markdown_rendering() {
    let mut gui = make_gui();
    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "**bold** ".into(),
    });
    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "and *italic*".into(),
    });
    // Two deltas of the same kind must coalesce into exactly one Text
    // span with the concatenated content, not one span per delta, and
    // must land in exactly one Assistant block (checked inside
    // only_assistant_spans).
    assert_eq!(
        only_assistant_spans(&gui),
        vec![Span::Text("**bold** and *italic*".into())],
        "model output must coalesce into a single Text span, the kind the renderer treats as markdown"
    );
}

/// Simulates a full thinking-enabled interaction: user input ->
/// reasoning -> a tool call -> more text -> turn end. Proves the block
/// kinds now carry what the line colours used to: the user's message
/// is a `User` block, reasoning and reply text stay separate spans of
/// one `Assistant` block each, and a tool call in between splits the
/// reply into two `Assistant` blocks around one filled `ToolCall`
/// block, exactly as the scripted events imply.
#[test]
fn full_thinking_interaction_produces_correct_block_kinds() {
    let mut gui = make_gui();

    // 1. User presses Enter.
    gui.set_input_buffer_for_test("pick a number between 1 and 100 but don't tell me");
    gui.submit_current_input_for_test();

    // 2. Reasoning chunk arrives from agent (thinking enabled)
    gui.handle_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: "The user wants me to pick a secret number.".into(),
    });

    // 3. More reasoning (coalesces into the same span)
    gui.handle_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: " I'll pick 42.".into(),
    });

    // 4. The agent calls a tool before replying.
    gui.handle_stream_event(StreamEvent::ToolCallStart {
        turn: 1,
        tool: "Bash".into(),
        args: "echo 42".into(),
    });
    gui.handle_stream_event(StreamEvent::ToolCallEnd {
        turn: 1,
        tool: "Bash".into(),
        output: "42".into(),
        is_error: false,
    });

    // 5. Model text response (markdown), after the tool result.
    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "I've picked a number between 1 and 100.".into(),
    });

    // 6. Turn end
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 150,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });

    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(
        blocks.len(),
        4,
        "expected 4 blocks: the user message, the reasoning reply, \
             the tool call, and the post-tool reply"
    );
    assert_eq!(
        blocks[0].kind,
        BlockKind::User {
            text: "pick a number between 1 and 100 but don't tell me".into(),
        },
        "user input must be a User block, never assistant output"
    );
    assert_eq!(
        blocks[1].kind,
        BlockKind::Assistant {
            spans: vec![Span::Reasoning(
                "The user wants me to pick a secret number. I'll pick 42.".into()
            )],
        },
        "reasoning before the tool call must be its own Assistant block"
    );
    assert_eq!(
        blocks[2].kind,
        BlockKind::ToolCall {
            tool: "Bash".into(),
            args: "echo 42".into(),
            output: Some("42".into()),
            is_error: false,
        },
        "the tool call block must carry the filled-in output the scripted end event sent"
    );
    assert_eq!(
        blocks[3].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("I've picked a number between 1 and 100.".into())],
        },
        "reply text after a tool result must start a new Assistant block, not join the one before the call"
    );
}

/// Pins the ordering rule on its own, isolated from reasoning: text
/// that arrives after a tool call's result must start a brand new
/// Assistant block rather than being appended to the block that was
/// open before the call. A refactor that made the tool call transparent
/// to span coalescing would merge "before" and "after" into one span
/// of one block; this test fails the moment that happens.
#[test]
fn text_after_a_tool_call_starts_a_new_assistant_block() {
    let mut gui = make_gui();
    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "before".into(),
    });
    gui.handle_stream_event(StreamEvent::ToolCallStart {
        turn: 1,
        tool: "Bash".into(),
        args: "ls".into(),
    });
    gui.handle_stream_event(StreamEvent::ToolCallEnd {
        turn: 1,
        tool: "Bash".into(),
        output: "file1".into(),
        is_error: false,
    });
    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "after".into(),
    });

    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(
        blocks.len(),
        3,
        "expected the before-text block, the tool call, and a new after-text block"
    );
    assert_eq!(
        blocks[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("before".into())],
        }
    );
    assert!(
        matches!(blocks[1].kind, BlockKind::ToolCall { .. }),
        "the middle block must be the tool call"
    );
    assert_eq!(
        blocks[2].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("after".into())],
        },
        "the after-text must be its own span in its own block, not merged with \"before\""
    );
    assert_ne!(
        blocks[0].id, blocks[2].id,
        "the two Assistant blocks around the tool call must be distinct blocks"
    );
}

#[test]
fn turn_end_accumulates_cache_tokens() {
    let mut gui = make_gui();
    assert_eq!(gui.total_cache_hit_tokens_for_test(), 0);
    assert_eq!(gui.total_cache_miss_tokens_for_test(), 0);

    // First turn: 100 cache hit, 20 miss
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 150,
        prompt_cache_hit_tokens: 100,
        prompt_cache_miss_tokens: 20,
    });
    assert_eq!(gui.total_cache_hit_tokens_for_test(), 100);
    assert_eq!(gui.total_cache_miss_tokens_for_test(), 20);

    // Second turn: 80 cache hit, 30 miss
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 2,
        finish_reason: "stop".into(),
        total_tokens: 200,
        prompt_cache_hit_tokens: 80,
        prompt_cache_miss_tokens: 30,
    });
    assert_eq!(gui.total_cache_hit_tokens_for_test(), 180);
    assert_eq!(gui.total_cache_miss_tokens_for_test(), 50);

    // Third turn: no cache (new conversation or first turn)
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 3,
        finish_reason: "stop".into(),
        total_tokens: 100,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });
    assert_eq!(gui.total_cache_hit_tokens_for_test(), 180);
    assert_eq!(gui.total_cache_miss_tokens_for_test(), 50);
}

#[test]
fn with_voice_attaches_both_channels() {
    let gui = make_gui();
    let (_tx_voice_events, rx_voice) = mpsc::unbounded_channel::<VoiceEvent>();
    let (tx_voice_cmd, _rx_voice_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
    let gui = gui.with_voice(rx_voice, tx_voice_cmd);
    assert!(gui.voice_for_test().is_attached());
}

#[test]
fn new_seeds_every_panel_control_from_settings() {
    let gui = make_gui_with_settings(&settings_with_voice("am_michael"));
    assert!(gui.show_raw_output_for_test());
    // The voice controls seed inside `VoiceUi::new`, covered by
    // `voice_ui::tests::new_seeds_every_control_from_settings`.
}

#[test]
fn transcript_event_submits_through_the_enter_path() {
    let mut gui = make_gui();
    gui.handle_voice_event_for_test(VoiceEvent::Transcript("turn on the lights".into()));
    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0].kind,
        BlockKind::User {
            text: "turn on the lights".into(),
        }
    );
    assert!(
        gui.input_buffer_for_test().is_empty(),
        "buffer clears on submit, same as Enter"
    );
    assert_eq!(gui.session_status_for_test(), "Running...");
}

#[test]
fn transcript_event_forwards_text_to_the_agent_channel() {
    let (_tx_events, rx_events) = mpsc::unbounded_channel();
    let (tx_input, mut rx_input) = mpsc::unbounded_channel();
    let mut gui = DeepSeekGui::new(
        rx_events,
        tx_input,
        AgentHandles {
            interrupt: Arc::new(AtomicBool::new(false)),
            effort: Arc::new(AtomicU8::new(0)),
            voice_mode: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(100_000)),
            model: Arc::new(Mutex::new("deepseek-v4-flash".into())),
            working_dir: Arc::new(Mutex::new(PathBuf::from("."))),
        },
        Settings::default(),
        unique_temp_dir("ctor"),
    );
    gui.handle_voice_event_for_test(VoiceEvent::Transcript("hello".into()));
    match rx_input.try_recv().unwrap() {
        AgentCommand::UserTurn { text, .. } => assert_eq!(text, "hello"),
        other => panic!("expected UserTurn, got {other:?}"),
    }
}

#[test]
fn empty_transcript_is_not_submitted() {
    let mut gui = make_gui();
    gui.handle_voice_event_for_test(VoiceEvent::Transcript("".into()));
    assert!(gui.transcript_for_test().blocks().is_empty());
    assert_eq!(gui.session_status_for_test(), "Ready");
}

#[test]
fn whitespace_only_transcript_is_not_submitted() {
    let mut gui = make_gui();
    gui.handle_voice_event_for_test(VoiceEvent::Transcript("   ".into()));
    assert!(gui.transcript_for_test().blocks().is_empty());
    assert_eq!(gui.session_status_for_test(), "Ready");
}

#[test]
fn voice_mode_flag_starts_matching_initial_tts_state() {
    let gui = make_gui();
    assert!(!gui.voice_for_test().tts_enabled_for_test());
    assert!(!gui.handles_for_test().voice_mode.load(Ordering::SeqCst));
}

#[test]
fn voice_mode_flag_tracks_tts_toggling_on_then_off() {
    let mut gui = make_gui();

    gui.voice_mut_for_test().set_tts_enabled_for_test(true);
    gui.handles_for_test().voice_mode.store(
        voice_mode_flag_for_tts(gui.voice_for_test().tts_enabled_for_test()),
        Ordering::SeqCst,
    );
    assert!(gui.handles_for_test().voice_mode.load(Ordering::SeqCst));

    gui.voice_mut_for_test().set_tts_enabled_for_test(false);
    gui.handles_for_test().voice_mode.store(
        voice_mode_flag_for_tts(gui.voice_for_test().tts_enabled_for_test()),
        Ordering::SeqCst,
    );
    assert!(!gui.handles_for_test().voice_mode.load(Ordering::SeqCst));
}

/// A GUI with the voice command channel attached, plus the receiving
/// end of that channel so a test can see what got sent.
fn make_gui_with_voice() -> (DeepSeekGui, mpsc::UnboundedReceiver<VoiceCommand>) {
    let gui = make_gui();
    let (_tx_voice_events, rx_voice) = mpsc::unbounded_channel::<VoiceEvent>();
    let (tx_voice_cmd, rx_voice_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
    let gui = gui.with_voice(rx_voice, tx_voice_cmd);
    (gui, rx_voice_cmd)
}

#[test]
fn turn_end_speaks_the_accumulated_reply_when_tts_is_enabled() {
    let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
    gui.voice_mut_for_test().set_tts_enabled_for_test(true);

    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "Run `cargo test` to check it.".into(),
    });
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 10,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });

    assert_eq!(
        rx_voice_cmd.try_recv().unwrap(),
        VoiceCommand::Speak("Run to check it.".into())
    );
    assert!(gui.voice_for_test().reply_buffer_for_test().is_empty());
}

#[test]
fn turn_end_sends_nothing_when_tts_is_disabled() {
    let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
    assert!(
        !gui.voice_for_test().tts_enabled_for_test(),
        "tts is off by default"
    );

    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "Hello there.".into(),
    });
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 10,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });

    assert!(rx_voice_cmd.try_recv().is_err());
}

#[test]
fn turn_end_sends_exactly_one_speak_for_multiple_text_chunks() {
    let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
    gui.voice_mut_for_test().set_tts_enabled_for_test(true);

    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "Hello".into(),
    });
    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: " there.".into(),
    });
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 10,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });

    assert_eq!(
        rx_voice_cmd.try_recv().unwrap(),
        VoiceCommand::Speak("Hello there.".into())
    );
    assert!(
        rx_voice_cmd.try_recv().is_err(),
        "exactly one Speak per turn, not one per chunk"
    );
}

#[test]
fn reasoning_and_tool_output_are_never_spoken() {
    let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
    gui.voice_mut_for_test().set_tts_enabled_for_test(true);

    gui.handle_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: "thinking about it".into(),
    });
    gui.handle_stream_event(StreamEvent::ToolCallStart {
        turn: 1,
        tool: "Bash".into(),
        args: "{}".into(),
    });
    gui.handle_stream_event(StreamEvent::ToolCallEnd {
        turn: 1,
        tool: "Bash".into(),
        output: "some tool output".into(),
        is_error: false,
    });
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 10,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });

    assert!(
        rx_voice_cmd.try_recv().is_err(),
        "no model reply text was seen, so nothing should be spoken"
    );
}

#[test]
fn interrupted_clears_the_pending_reply_so_it_is_never_spoken_later() {
    let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
    gui.voice_mut_for_test().set_tts_enabled_for_test(true);

    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "partial reply".into(),
    });
    gui.handle_stream_event(StreamEvent::Interrupted {
        message: "Interrupted by user (Escape)".into(),
    });
    assert!(gui.voice_for_test().reply_buffer_for_test().is_empty());

    // The next turn must not inherit the interrupted turn's text.
    gui.handle_stream_event(StreamEvent::Text {
        turn: 2,
        text: "fresh reply".into(),
    });
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 2,
        finish_reason: "stop".into(),
        total_tokens: 10,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });

    assert_eq!(
        rx_voice_cmd.try_recv().unwrap(),
        VoiceCommand::Speak("fresh reply".into())
    );
}

#[test]
fn new_gui_seeds_context_budget_from_the_flag() {
    let (_tx_events, rx_events) = mpsc::unbounded_channel();
    let (tx_input, _rx_input) = mpsc::unbounded_channel();
    let gui = DeepSeekGui::new(
        rx_events,
        tx_input,
        AgentHandles {
            interrupt: Arc::new(AtomicBool::new(false)),
            effort: Arc::new(AtomicU8::new(0)),
            voice_mode: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(64_000)),
            model: Arc::new(Mutex::new("deepseek-v4-flash".into())),
            working_dir: Arc::new(Mutex::new(PathBuf::from("."))),
        },
        Settings::default(),
        unique_temp_dir("ctor"),
    );
    assert_eq!(gui.context_budget_for_test(), 64_000);
}

#[test]
fn context_budget_flag_write_is_observable_through_a_second_handle() {
    let mut gui = make_gui();
    let observer = Arc::clone(&gui.handles_for_test().context_budget);
    gui.set_context_budget_for_test(80_000);
    gui.handles_for_test()
        .context_budget
        .store(80_000, Ordering::SeqCst);
    assert_eq!(observer.load(Ordering::SeqCst), 80_000);
}

#[test]
fn context_budget_caption_value_is_a_third_of_the_budget() {
    let mut gui = make_gui();
    gui.set_context_budget_for_test(90_000);
    assert_eq!(gui.context_budget_for_test() / 3, 30_000);
}

#[test]
fn new_gui_seeds_effort_from_the_flag() {
    let (tx_events, rx_events) = mpsc::unbounded_channel();
    let (tx_input, _rx_input) = mpsc::unbounded_channel();
    let effort_flag = Arc::new(AtomicU8::new(0));
    Effort::High.store(&effort_flag);
    let gui = DeepSeekGui::new(
        rx_events,
        tx_input,
        AgentHandles {
            interrupt: Arc::new(AtomicBool::new(false)),
            effort: effort_flag,
            voice_mode: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(64_000)),
            model: Arc::new(Mutex::new("deepseek-v4-flash".into())),
            working_dir: Arc::new(Mutex::new(PathBuf::from("."))),
        },
        Settings::default(),
        unique_temp_dir("effort-ctor"),
    );
    let _ = tx_events;
    assert_eq!(gui.effort_for_test(), Effort::High);
}

#[test]
fn new_gui_seeds_effort_from_settings_via_the_test_helper() {
    let settings = Settings {
        effort: Some(Effort::Max),
        ..Settings::default()
    };
    let gui = make_gui_with_settings(&settings);
    assert_eq!(gui.effort_for_test(), Effort::Max);
}

#[test]
fn effort_flag_write_is_observable_through_a_second_handle() {
    let mut gui = make_gui();
    let observer = Arc::clone(&gui.handles_for_test().effort);
    gui.set_effort_for_test(Effort::Medium);
    gui.effort_for_test().store(&gui.handles_for_test().effort);
    assert_eq!(Effort::load(&observer), Effort::Medium);
}

#[test]
fn apply_effort_round_trips_through_settings() {
    let mut settings = Settings::default();
    assert_eq!(settings.effort(), Effort::None);
    apply_effort(&mut settings, Effort::Low);
    assert_eq!(settings.effort(), Effort::Low);
}

#[test]
fn apply_effort_reaches_every_level() {
    let mut settings = Settings::default();
    for level in [
        Effort::None,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Max,
    ] {
        apply_effort(&mut settings, level);
        assert_eq!(settings.effort(), level);
        assert_eq!(settings.effort().to_u8(), level.to_u8());
    }
}

#[test]
fn apply_effort_persists_to_disk() {
    let project_root = unique_temp_dir("effort-persist");
    let mut gui = make_gui_in(&Settings::default(), project_root.clone());
    apply_effort(gui.settings_mut_for_test(), Effort::High);
    gui.persist_settings_for_test();
    let reloaded = Settings::load(&project_root).unwrap();
    assert_eq!(reloaded.effort(), Effort::High);
}

/// Apply a change to a GUI's settings, persist it, and read the file
/// back through the real load path. This is the seam every panel
/// control goes through.
fn round_trip<F: FnOnce(&mut Settings)>(change: F) -> Settings {
    let dir = unique_temp_dir("persist");
    let gui = make_gui_in(&Settings::default(), dir.clone());
    let mut gui = gui;
    change(gui.settings_mut_for_test());
    gui.persist_settings_for_test();
    assert!(dir.join("settings.json").exists(), "the file must be there");
    let loaded = Settings::load(&dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    loaded
}

#[test]
fn default_backend_change_survives_a_save_and_a_load() {
    let loaded = round_trip(|s| apply_default_backend(s, "claude"));
    assert_eq!(loaded.default_backend(), Some("claude"));
}

#[test]
fn show_raw_output_change_survives_a_save_and_a_load() {
    let loaded = round_trip(|s| apply_show_raw_output(s, true));
    assert!(loaded.show_raw_output());
}

#[test]
fn context_budget_change_survives_a_save_and_a_load() {
    let loaded = round_trip(|s| apply_context_budget(s, 150_000));
    assert_eq!(loaded.context_budget(), 150_000);
}

#[test]
fn new_gui_seeds_working_dir_buffer_from_the_flag() {
    let (_tx_events, rx_events) = mpsc::unbounded_channel();
    let (tx_input, _rx_input) = mpsc::unbounded_channel();
    let seeded_dir = unique_temp_dir("seed-workdir");
    let gui = DeepSeekGui::new(
        rx_events,
        tx_input,
        AgentHandles {
            interrupt: Arc::new(AtomicBool::new(false)),
            effort: Arc::new(AtomicU8::new(0)),
            voice_mode: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(64_000)),
            model: Arc::new(Mutex::new("deepseek-v4-flash".into())),
            working_dir: Arc::new(Mutex::new(seeded_dir.clone())),
        },
        Settings::default(),
        unique_temp_dir("ctor"),
    );
    assert_eq!(
        gui.working_dir_buffer_for_test(),
        seeded_dir.display().to_string()
    );
}

#[test]
fn working_dir_change_survives_a_save_and_a_load() {
    let dir = unique_temp_dir("workdir-persist");
    let loaded = round_trip(|s| apply_working_dir(s, &dir.display().to_string()));
    assert_eq!(loaded.working_dir(), Some(dir.display().to_string()));
}

#[test]
fn commit_working_dir_change_writes_a_valid_directory_to_the_shared_flag() {
    let mut gui = make_gui();
    let dir = unique_temp_dir("commit-valid");
    gui.set_working_dir_buffer_for_test(&dir.display().to_string());

    gui.commit_working_dir_change_for_test();

    assert_eq!(*gui.handles_for_test().working_dir.lock().unwrap(), dir);
    assert_eq!(
        gui.settings_mut_for_test().working_dir(),
        Some(dir.display().to_string())
    );
}

#[test]
fn commit_working_dir_change_rejects_a_path_that_is_not_a_directory() {
    let mut gui = make_gui();
    let original = gui.handles_for_test().working_dir.lock().unwrap().clone();
    gui.set_working_dir_buffer_for_test("Z:/definitely/does/not/exist/anywhere");

    gui.commit_working_dir_change_for_test();

    assert_eq!(
        *gui.handles_for_test().working_dir.lock().unwrap(),
        original
    );
    assert!(gui.settings_mut_for_test().working_dir().is_none());
}

#[test]
fn voice_change_creates_the_block_when_it_is_missing() {
    let mut settings = Settings::default();
    assert!(settings.voice.is_none(), "no voice block to start");
    apply_stt_enabled(&mut settings, true);
    assert!(settings.voice.is_some(), "the block gets created");
    assert!(settings.voice.as_ref().unwrap().stt_enabled);
}

#[test]
fn every_voice_control_change_survives_a_save_and_a_load() {
    let loaded = round_trip(|s| {
        apply_voice_enabled(s, true);
        apply_stt_enabled(s, true);
        apply_tts_enabled(s, true);
        apply_trigger_mode(s, TriggerMode::WakeWord);
        apply_wake_phrase(s, "hey computer");
        apply_tts_voice(s, "am_michael");
        apply_tts_speed(s, 1.4);
    });
    assert!(loaded.voice_enabled());
    assert!(loaded.voice_stt_enabled());
    assert!(loaded.voice_tts_enabled());
    assert_eq!(loaded.voice_trigger_mode(), TriggerMode::WakeWord);
    assert_eq!(loaded.voice_wake_phrase(), "hey computer");
    assert_eq!(loaded.voice_tts_voice(), "am_michael");
    assert_eq!(loaded.voice_tts_speed(), 1.4);
}

#[test]
fn persisting_to_an_unwritable_root_logs_instead_of_panicking() {
    let gui = make_gui_in(
        &Settings::default(),
        PathBuf::from("/nonexistent/dsc/path/xyz"),
    );
    // Must not panic. The failure is logged and the session goes on.
    gui.persist_settings_for_test();
}

#[test]
fn active_tab_defaults_to_chat() {
    let gui = make_gui();
    assert_eq!(gui.active_tab_for_test(), ActiveTab::Chat);
}

#[test]
fn autopilot_task_change_survives_a_save_and_a_load() {
    let loaded = round_trip(|s| apply_autopilot_task(s, "fix the build"));
    assert_eq!(loaded.autopilot_task().as_deref(), Some("fix the build"));
}

#[test]
fn autopilot_iterations_change_survives_a_save_and_a_load() {
    let loaded = round_trip(|s| apply_autopilot_iterations(s, 12));
    assert_eq!(loaded.autopilot_iterations(), 12);
}

#[test]
fn autopilot_progress_updates_from_iteration_start_then_finished() {
    let mut gui = make_gui();
    assert_eq!(gui.autopilot_for_test().progress(), AutopilotProgress::Idle);

    gui.handle_stream_event(StreamEvent::RepeatIterationStart {
        index: 2,
        total: 5,
        task: "run the plan".into(),
    });
    assert_eq!(
        gui.autopilot_for_test().progress(),
        AutopilotProgress::Running { index: 2, total: 5 }
    );

    gui.handle_stream_event(StreamEvent::RepeatFinished {
        completed: 5,
        total: 5,
    });
    assert_eq!(
        gui.autopilot_for_test().progress(),
        AutopilotProgress::Finished {
            completed: 5,
            total: 5
        }
    );
}

#[test]
fn resolve_autopilot_policy_path_defaults_under_project_root() {
    let root = PathBuf::from("/project");
    let store = deepseek_custom::autopilot::policy::PolicyStore::new(root.clone(), None);
    let path = store.resolved_policy_path();
    assert_eq!(path, root.join("autopilot-policy.md"));
}

#[test]
fn resolve_autopilot_policy_path_resolves_relative_override_against_root() {
    let root = PathBuf::from("/project");
    let store = deepseek_custom::autopilot::policy::PolicyStore::new(
        root.clone(),
        Some("custom-policy.md".into()),
    );
    let path = store.resolved_policy_path();
    assert_eq!(path, root.join("custom-policy.md"));
}

#[test]
fn session_reset_clears_the_pending_reply() {
    let mut gui = make_gui();
    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "partial reply".into(),
    });
    gui.handle_stream_event(StreamEvent::SessionReset);
    assert!(gui.voice_for_test().reply_buffer_for_test().is_empty());
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: Some(Content::text(text)),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }
}

/// Drive one full turn through a GUI: a user block in the transcript,
/// a `ConversationSnapshot` carrying `messages`, then `TurnEnd`, which
/// triggers the autosave.
fn run_one_turn(gui: &mut DeepSeekGui, user_text: &str) {
    gui.transcript_mut_for_test().push(BlockKind::User {
        text: user_text.into(),
    });
    gui.handle_stream_event(StreamEvent::ConversationSnapshot {
        messages: vec![user_message(user_text)],
        claude_session_id: None,
    });
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 10,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });
}

#[test]
fn turn_end_writes_a_session_file_the_store_can_load_back() {
    let mut gui = make_gui();
    run_one_turn(&mut gui, "fix the parser bug");

    let loaded = gui
        .sessions_for_test()
        .store()
        .load(&gui.sessions_for_test().current_id())
        .expect("expected the autosaved session to load back");
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.meta.title, "fix the parser bug");
}

#[test]
fn new_session_saves_outgoing_then_leaves_an_empty_transcript_and_a_different_id() {
    let mut gui = make_gui();
    run_one_turn(&mut gui, "add a new endpoint");
    let old_id = gui.sessions_for_test().current_id();

    gui.start_new_session_for_test();

    assert!(gui.sessions_for_test().store().load(&old_id).is_ok());
    assert!(gui.transcript_for_test().blocks().is_empty());
    assert_ne!(gui.sessions_for_test().current_id(), old_id);
    assert!(gui.sessions_for_test().messages().is_empty());
}

#[test]
fn new_session_from_empty_conversation_writes_nothing_to_disk() {
    let mut gui = make_gui();
    let old_id = gui.sessions_for_test().current_id();

    gui.start_new_session_for_test();

    assert!(gui.sessions_for_test().store().load(&old_id).is_err());
    assert!(gui.sessions_for_test().saved().is_empty());
}

#[test]
fn load_session_saves_outgoing_then_installs_the_loaded_transcript_and_id() {
    let mut gui = make_gui();
    run_one_turn(&mut gui, "first conversation");
    let first_id = gui.sessions_for_test().current_id();

    gui.start_new_session_for_test();
    run_one_turn(&mut gui, "second conversation");
    let second_id = gui.sessions_for_test().current_id();

    gui.load_session_for_test(first_id);

    assert_eq!(gui.sessions_for_test().current_id(), first_id);
    assert_eq!(gui.transcript_for_test().blocks().len(), 1);
    let saved_second = gui
        .sessions_for_test()
        .store()
        .load(&second_id)
        .expect("expected the outgoing second conversation to be saved");
    assert_eq!(saved_second.meta.title, "second conversation");
}

#[test]
fn session_reset_saves_the_outgoing_conversation_rather_than_discarding_it() {
    let mut gui = make_gui();
    run_one_turn(&mut gui, "reset me please");
    let old_id = gui.sessions_for_test().current_id();

    gui.handle_stream_event(StreamEvent::SessionReset);

    let loaded = gui
        .sessions_for_test()
        .store()
        .load(&old_id)
        .expect("expected the pre-reset conversation to have been saved");
    assert_eq!(loaded.meta.title, "reset me please");
    assert_ne!(gui.sessions_for_test().current_id(), old_id);
}

#[test]
fn a_save_failure_does_not_panic_and_does_not_take_down_the_session() {
    // Point the session store at a path that cannot be a directory: a
    // regular file sits where the sessions directory would need to go,
    // so `save`'s `create_dir_all` fails every time.
    let root = unique_temp_dir("save-failure");
    std::fs::write(root.join(".deepseek"), "not a directory").unwrap();
    let mut gui = make_gui_in(&Settings::default(), root);

    run_one_turn(&mut gui, "this save will fail");

    assert_eq!(gui.session_status_for_test(), "Ready");
}

#[test]
fn title_is_derived_on_first_turn_and_not_rederived_once_set() {
    let mut gui = make_gui();
    run_one_turn(&mut gui, "the original title");
    assert_eq!(
        gui.sessions_for_test().title_for_test(),
        "the original title"
    );

    gui.handle_stream_event(StreamEvent::ConversationSnapshot {
        messages: vec![
            user_message("the original title"),
            user_message("a later message that should not overwrite the title"),
        ],
        claude_session_id: None,
    });
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 2,
        finish_reason: "stop".into(),
        total_tokens: 20,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });

    assert_eq!(
        gui.sessions_for_test().title_for_test(),
        "the original title"
    );
}

#[test]
fn switching_to_sessions_tab_does_not_disturb_the_transcript() {
    let mut gui = make_gui();
    run_one_turn(&mut gui, "keep this transcript intact");
    let block_count_before = gui.transcript_for_test().blocks().len();

    gui.set_active_tab_for_test(ActiveTab::Sessions);

    assert_eq!(gui.active_tab_for_test(), ActiveTab::Sessions);
    assert_eq!(gui.transcript_for_test().blocks().len(), block_count_before);
}

#[test]
fn new_chat_action_leaves_an_empty_transcript_and_returns_to_chat_tab() {
    let mut gui = make_gui();
    run_one_turn(&mut gui, "an old conversation");
    gui.set_active_tab_for_test(ActiveTab::Sessions);

    // This mirrors exactly what the Sessions tab's "New Chat" button
    // does: start a fresh session, then switch the view back to Chat.
    gui.start_new_session_for_test();
    gui.set_active_tab_for_test(ActiveTab::Chat);

    assert!(gui.transcript_for_test().blocks().is_empty());
    assert_eq!(gui.active_tab_for_test(), ActiveTab::Chat);
}

#[test]
fn deleting_a_session_removes_it_from_the_list() {
    let mut gui = make_gui();
    run_one_turn(&mut gui, "a session to delete");
    let id = gui.sessions_for_test().current_id();
    assert!(
        gui.sessions_for_test()
            .saved()
            .iter()
            .any(|meta| meta.id == id)
    );

    gui.delete_saved_session_for_test(id);

    assert!(
        !gui.sessions_for_test()
            .saved()
            .iter()
            .any(|meta| meta.id == id)
    );
}

// ── Routed events (P2S03) ──

fn test_route_hop(id: SubagentId) -> RouteHop {
    RouteHop {
        id,
        meta: SubagentMeta {
            backend: "ollama".into(),
            model: "test-model".into(),
            depth: 1,
        },
        session_turns: 1,
        session_turn_cap: 20,
        send_message_calls: 0,
        send_message_call_cap: 10,
    }
}

/// A routed event with a non-empty route lands inside its own nested
/// `Subagent` block, not as a top-level block in the main transcript.
#[test]
fn a_routed_event_lands_in_a_nested_subagent_block_not_the_top_level_transcript() {
    let mut gui = make_gui();
    let subagent_id = SubagentId::next();

    gui.handle_routed_event_for_test(RoutedEvent {
        route: vec![test_route_hop(subagent_id)],
        event: StreamEvent::Text {
            turn: 1,
            text: "hi from a subagent".into(),
        },
    });

    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(blocks.len(), 1);
    let BlockKind::Subagent { transcript, .. } = &blocks[0].kind else {
        panic!("expected a Subagent block, got {:?}", blocks[0].kind);
    };
    assert_eq!(
        transcript.blocks()[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("hi from a subagent".into())],
        }
    );
}

/// A subagent's own `TurnEnd`, arriving with a non-empty route, must
/// not trigger any of the main session's `TurnEnd` side effects: no
/// token-count update, no cache-counter update, no session autosave.
#[test]
fn a_subagent_turn_end_does_not_touch_main_session_counters_or_trigger_a_save() {
    let mut gui = make_gui();
    run_one_turn(&mut gui, "the real conversation");
    let token_count_before = gui.token_count_for_test().to_string();
    let hit_before = gui.total_cache_hit_tokens_for_test();
    let miss_before = gui.total_cache_miss_tokens_for_test();
    let status_before = gui.session_status_for_test().to_string();
    let saved_sessions_before = gui.sessions_for_test().saved().len();
    let subagent_id = SubagentId::next();

    gui.handle_routed_event_for_test(RoutedEvent {
        route: vec![test_route_hop(subagent_id)],
        event: StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 9999,
            prompt_cache_hit_tokens: 500,
            prompt_cache_miss_tokens: 500,
        },
    });

    assert_eq!(gui.token_count_for_test(), token_count_before);
    assert_eq!(gui.total_cache_hit_tokens_for_test(), hit_before);
    assert_eq!(gui.total_cache_miss_tokens_for_test(), miss_before);
    assert_eq!(gui.session_status_for_test(), status_before);
    assert_eq!(gui.sessions_for_test().saved().len(), saved_sessions_before);
}

/// A main-session event, empty route, still does everything it always
/// did: `handle_routed_event` with an empty route behaves exactly like
/// the old `handle_stream_event` path, side effects included.
#[test]
fn a_main_session_routed_event_still_applies_its_side_effects() {
    let mut gui = make_gui();
    assert_eq!(gui.total_cache_hit_tokens_for_test(), 0);

    gui.handle_routed_event_for_test(RoutedEvent::own(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 42,
        prompt_cache_hit_tokens: 10,
        prompt_cache_miss_tokens: 5,
    }));

    assert_eq!(gui.token_count_for_test(), "42");
    assert_eq!(gui.total_cache_hit_tokens_for_test(), 10);
    assert_eq!(gui.total_cache_miss_tokens_for_test(), 5);
    // And the transcript still sees it too, exactly as
    // `apply_stream_event` would have handled it directly.
    assert!(gui.transcript_for_test().blocks().is_empty());
}

/// Drive one autopilot iteration boundary the way `run_repeat` does.
fn start_iteration(gui: &mut DeepSeekGui, index: u32, total: u32, task: &str) {
    gui.handle_stream_event(StreamEvent::RepeatIterationStart {
        index,
        total,
        task: task.into(),
    });
}

#[test]
fn each_autopilot_iteration_opens_its_own_session() {
    let mut gui = make_gui();
    start_iteration(&mut gui, 1, 2, "tighten the codebase");
    run_one_turn(&mut gui, "tighten the codebase");
    let first_id = gui.sessions_for_test().current_id();

    start_iteration(&mut gui, 2, 2, "tighten the codebase");
    let second_id = gui.sessions_for_test().current_id();

    assert_ne!(second_id, first_id);
    assert!(gui.sessions_for_test().store().load(&first_id).is_ok());
}

#[test]
fn an_autopilot_iteration_starts_from_an_empty_transcript() {
    let mut gui = make_gui();
    start_iteration(&mut gui, 1, 2, "tighten the codebase");
    run_one_turn(&mut gui, "tighten the codebase");

    start_iteration(&mut gui, 2, 2, "tighten the codebase");

    // Only this iteration's own notice and task block, nothing from the
    // iteration before it.
    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks[1].kind,
        BlockKind::User {
            text: "tighten the codebase".into()
        }
    );
}

#[test]
fn an_autopilot_session_takes_its_title_from_the_task() {
    let mut gui = make_gui();
    start_iteration(&mut gui, 1, 1, "tighten the codebase");
    let id = gui.sessions_for_test().current_id();
    gui.handle_stream_event(StreamEvent::ConversationSnapshot {
        messages: Vec::new(),
        claude_session_id: Some("abc".into()),
    });
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 10,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });

    let loaded = gui
        .sessions_for_test()
        .store()
        .load(&id)
        .expect("expected the iteration's session to load back");
    assert_eq!(loaded.meta.title, "tighten the codebase");
}
