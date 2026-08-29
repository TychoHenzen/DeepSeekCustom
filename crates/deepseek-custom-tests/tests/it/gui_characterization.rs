//! Cross-cutting characterization tests for the native GUI boundary.
//!
//! These tests intentionally drive the existing external test seams before
//! application-state ownership moves into the actor.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize};

use deepseek_custom::agent::events::{
    AgentCommand, RouteHop, RoutedEvent, StreamEvent, SubagentId, SubagentMeta,
};
use deepseek_custom::config::settings::Settings;
use deepseek_custom::effort::Effort;
use deepseek_custom::gui::agent_handles::AgentHandles;
use deepseek_custom::gui::procedure_tab::{ProcedureTab, ProcedureViewState};
use deepseek_custom::gui::settings_panel::{
    apply_context_budget, apply_effort, apply_plain_language, apply_show_raw_output,
    apply_target_grade, apply_working_dir,
};
use deepseek_custom::gui::transcript::{BlockKind, Span};
use deepseek_custom::gui::{DeepSeekGui, PendingSwitch};
use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, ProcedureAttemptDisposition, ProcedureCommand,
    ProcedureProgress, ProcedureReportStore, ProcedureReviewDecision, ProcedureReviewDisposition,
    ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage, ProcedureTask,
    ProcedureTerminalDisposition,
};
use tokio::sync::mpsc;

fn scratch_dir(tag: &str) -> PathBuf {
    super::scratch_dir("dsc-gui-characterization", tag)
}

fn make_gui(tag: &str) -> (DeepSeekGui, mpsc::UnboundedReceiver<AgentCommand>, PathBuf) {
    let root = scratch_dir(tag);
    let (_tx_events, rx_events) = mpsc::unbounded_channel();
    let (tx_input, rx_input) = mpsc::unbounded_channel();
    let gui = DeepSeekGui::new(
        rx_events,
        tx_input,
        AgentHandles {
            interrupt: Arc::new(AtomicBool::new(false)),
            effort: Arc::new(AtomicU8::new(0)),
            voice_mode: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(100_000)),
            model: Arc::new(Mutex::new("test-model".to_string())),
            working_dir: Arc::new(Mutex::new(root.clone())),
            cascade_total: Arc::new(AtomicUsize::new(0)),
            cascade_escalated: Arc::new(AtomicUsize::new(0)),
            style_plain_language: Arc::new(AtomicBool::new(false)),
            style_target_grade: Arc::new(AtomicU8::new(8)),
        },
        Settings::default(),
        root.clone(),
    );
    (gui, rx_input, root)
}

fn test_route_hop(id: SubagentId) -> RouteHop {
    RouteHop {
        id,
        meta: SubagentMeta {
            backend: "test-backend".to_string(),
            model: "test-model".to_string(),
            depth: 1,
        },
        session_turns: 1,
        session_turn_cap: 20,
        send_message_calls: 0,
        send_message_call_cap: 10,
    }
}

#[test]
fn characterization_projects_ordered_chat_events_into_transcript_blocks() {
    let (mut gui, mut rx_input, _root) = make_gui("transcript");

    gui.send_input_for_test("explain the result");
    assert!(matches!(
        rx_input.try_recv().unwrap(),
        AgentCommand::UserTurn { text, image: None } if text == "explain the result"
    ));
    gui.handle_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: "I will inspect the input. ".to_string(),
    });
    gui.handle_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: "Then I will answer.".to_string(),
    });
    gui.handle_stream_event(StreamEvent::ToolCallStart {
        turn: 1,
        tool: "Read".to_string(),
        args: "result.txt".to_string(),
    });
    gui.handle_stream_event(StreamEvent::ToolCallEnd {
        turn: 1,
        tool: "Read".to_string(),
        output: "42".to_string(),
        is_error: false,
    });
    gui.handle_stream_event(StreamEvent::Text {
        turn: 1,
        text: "The answer is **42**.".to_string(),
    });
    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".to_string(),
        total_tokens: 42,
        prompt_cache_hit_tokens: 3,
        prompt_cache_miss_tokens: 2,
    });

    let blocks = gui.transcript_for_test().blocks();
    assert_eq!(blocks.len(), 4);
    assert_eq!(
        blocks[0].kind,
        BlockKind::User {
            text: "explain the result".to_string()
        }
    );
    assert_eq!(
        blocks[1].kind,
        BlockKind::Assistant {
            spans: vec![Span::Reasoning(
                "I will inspect the input. Then I will answer.".to_string(),
            )]
        }
    );
    assert_eq!(
        blocks[2].kind,
        BlockKind::ToolCall {
            tool: "Read".to_string(),
            args: "result.txt".to_string(),
            output: Some("42".to_string()),
            is_error: false,
        }
    );
    assert_eq!(
        blocks[3].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("The answer is **42**.".to_string())]
        }
    );
    assert_eq!(gui.token_count_for_test(), "42");
    assert_eq!(gui.session_status_for_test(), "Ready");
}

#[test]
fn characterization_excludes_routed_subagent_terminal_side_effects_from_main_operation() {
    let (mut gui, mut rx_input, _root) = make_gui("active-operation");
    gui.send_input_for_test("keep running");
    let _ = rx_input.try_recv().unwrap();
    let subagent_id = SubagentId::next();

    gui.handle_routed_event_for_test(RoutedEvent {
        route: vec![test_route_hop(subagent_id)],
        event: StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".to_string(),
            total_tokens: 999,
            prompt_cache_hit_tokens: 50,
            prompt_cache_miss_tokens: 25,
        },
    });

    assert!(gui.turn_active_for_test());
    assert_eq!(gui.token_count_for_test(), "0");
    assert_eq!(gui.total_cache_hit_tokens_for_test(), 0);
    assert_eq!(gui.total_cache_miss_tokens_for_test(), 0);
    assert!(gui.sessions_for_test().saved().is_empty());

    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".to_string(),
        total_tokens: 12,
        prompt_cache_hit_tokens: 4,
        prompt_cache_miss_tokens: 1,
    });
    assert!(!gui.turn_active_for_test());
    assert_eq!(gui.token_count_for_test(), "12");
    assert_eq!(gui.total_cache_hit_tokens_for_test(), 4);
    assert_eq!(gui.total_cache_miss_tokens_for_test(), 1);
}

#[test]
fn characterization_settings_effects_persist_through_the_existing_schema() {
    let (mut gui, _rx_input, root) = make_gui("settings");
    let selected_dir = root.join("working");
    std::fs::create_dir_all(&selected_dir).unwrap();
    let selected_dir_text = selected_dir.display().to_string();

    apply_effort(gui.settings_mut_for_test(), Effort::High);
    apply_context_budget(gui.settings_mut_for_test(), 150_000);
    apply_show_raw_output(gui.settings_mut_for_test(), true);
    apply_plain_language(gui.settings_mut_for_test(), true);
    apply_target_grade(gui.settings_mut_for_test(), 12.0);
    apply_working_dir(gui.settings_mut_for_test(), &selected_dir_text);
    gui.persist_settings_for_test();

    let saved = Settings::load(&root).unwrap();
    assert_eq!(saved.effort(), Effort::High);
    assert_eq!(saved.context_budget(), 150_000);
    assert!(saved.show_raw_output());
    assert!(saved.style_plain_language_enabled());
    assert_eq!(saved.style_target_grade(), 12.0);
    assert_eq!(saved.working_dir(), Some(selected_dir_text));
}

#[test]
fn characterization_session_switch_waits_for_terminal_turn_and_saves_outgoing_state() {
    let (mut gui, mut rx_input, _root) = make_gui("pending-switch");
    let old_id = gui.sessions_for_test().current_id();
    gui.send_input_for_test("outgoing turn");
    let _ = rx_input.try_recv().unwrap();

    gui.start_new_session_for_test();
    assert_eq!(gui.pending_switch_for_test(), Some(PendingSwitch::New));
    assert_eq!(gui.sessions_for_test().current_id(), old_id);
    assert!(gui.transcript_for_test().blocks().iter().any(|block| {
        matches!(
            &block.kind,
            BlockKind::Notice { text, .. } if text.contains("Session switch waiting")
        )
    }));
    assert!(rx_input.try_recv().is_err());

    gui.handle_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".to_string(),
        total_tokens: 7,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });

    assert_eq!(gui.pending_switch_for_test(), None);
    assert_ne!(gui.sessions_for_test().current_id(), old_id);
    assert!(gui.transcript_for_test().blocks().is_empty());
    assert!(matches!(
        rx_input.try_recv().unwrap(),
        AgentCommand::NewSession
    ));
    assert!(gui.sessions_for_test().store().load(&old_id).is_ok());
}
fn pending_review_run(run_id: ProcedureRunId) -> ProcedureRun {
    ProcedureRun {
        id: run_id,
        change_id: "characterization-change".to_string(),
        selected_task: ProcedureTask {
            id: "1.1".to_string(),
            text: "Characterize review".to_string(),
            covers: None,
        },
        spec_fingerprint: Some("spec".to_string()),
        repository_fingerprint: Some("repository".to_string()),
        validation: None,
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "test-backend".to_string(),
            model: "test-model".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![LocalizationTarget {
                path: "src/example.rs".to_string(),
                symbol: Some("example".to_string()),
                evidence: "characterization target".to_string(),
            }],
            validation_error: None,
        }],
        review_disposition: ProcedureReviewDisposition::Pending,
        terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
    }
}

#[test]
fn characterization_procedure_review_exposes_pending_evidence_and_run_scoped_decision() {
    let root = scratch_dir("procedure-review");
    let settings = Settings::default();
    let run_id = ProcedureRunId::new();
    let reports = ProcedureReportStore::for_project(&root);
    reports.save(&pending_review_run(run_id)).unwrap();

    let mut tab = ProcedureTab::new(&settings, &root);
    let (tx_command, mut rx_command) = mpsc::unbounded_channel();
    let (_tx_progress, rx_progress) = mpsc::unbounded_channel();
    tab.attach(tx_command, rx_progress, Arc::new(AtomicBool::new(false)));
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "characterization-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });

    assert_eq!(tab.view_state(), ProcedureViewState::AwaitingReview);
    assert!(tab.review_actions_available_for_test());
    tab.approve_for_test();
    assert!(matches!(
        rx_command.try_recv().unwrap(),
        ProcedureCommand::Review {
            run_id: received_id,
            decision: ProcedureReviewDecision::Approve,
        } if received_id == run_id
    ));

    reports.approve(&run_id).unwrap();
    tab.handle_progress(ProcedureProgress::ReviewSucceeded {
        run_id,
        disposition: ProcedureReviewDisposition::Approved,
    });
    assert_eq!(tab.view_state(), ProcedureViewState::Approved);
    assert_eq!(
        tab.latest_run().unwrap().review_disposition,
        ProcedureReviewDisposition::Approved
    );
}
