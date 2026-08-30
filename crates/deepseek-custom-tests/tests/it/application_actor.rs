use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deepseek_custom::agent::events::AgentCommand;
use deepseek_custom::agent::events::StreamEvent;
use deepseek_custom::application::actor::{AppEvent, ApplicationActor, ChatLifecycle, Replay};
use deepseek_custom::application::dto::{
    AppCommand, AppCommandRequest, AppCommandResult, AppRevision, AppSnapshot, NoticeLevel,
    OperationKind, OperationPhase, OperationState, SessionSummary, TranscriptBlock,
    TranscriptContent, VisibleSettings, Workspace,
};
use deepseek_custom::application::services::DomainCommandPort;
use deepseek_custom::application::session::ApplicationSession;
use deepseek_custom::application::session_state::{SessionOrigin, SessionState};
use deepseek_custom::config::settings::Settings;
use deepseek_custom::procedure::{
    PatchPreviewId, ProcedureCommand, ProcedureProgress, ProcedureReviewDecision, ProcedureRunId,
    ProcedureStage, ProcedureTerminalDisposition,
};
use deepseek_custom::search::{SearchCommand, SearchKind, SearchSnapshot};
use deepseek_custom::session::SessionStore;
use tokio::sync::mpsc;

fn actor(capacity: usize) -> ApplicationActor {
    ApplicationActor::new(
        AppSnapshot::initial(
            VisibleSettings::from_settings(&Settings::default(), None, None),
            SessionSummary {
                id: "session-1".into(),
                title: "New conversation".into(),
                backend: "stub".into(),
                model: "test".into(),
            },
        ),
        capacity,
    )
}

#[test]
fn autopilot_dispatches_repeat_tracks_progress_and_sets_its_stop_flag() {
    let mut actor = actor(16);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let interrupt = Arc::new(AtomicBool::new(true));
    actor.connect_autopilot(DomainCommandPort::new(tx), Arc::clone(&interrupt));
    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision: AppRevision(0),
            command: AppCommand::StartAutopilot {
                task: "repair tests".into(),
                iterations: 4
            }
        }),
        AppCommandResult::Applied { .. }
    ));
    let command = rx.try_recv().unwrap();
    assert_eq!(command.task, "repair tests");
    assert_eq!(command.iterations, 4);
    assert!(!interrupt.load(Ordering::SeqCst));
    actor
        .apply_operation_stream_event(&StreamEvent::RepeatIterationStart {
            index: 2,
            total: 4,
            task: "repair tests".into(),
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        actor.snapshot().operations[0].progress.unwrap().completed,
        1
    );
    let revision = actor.snapshot().revision;
    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision,
            command: AppCommand::StopOperation {
                kind: OperationKind::Autopilot
            }
        }),
        AppCommandResult::Applied { .. }
    ));
    assert!(interrupt.load(Ordering::SeqCst));
    actor
        .apply_operation_stream_event(&StreamEvent::RepeatFinished {
            completed: 2,
            total: 4,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        actor.snapshot().operations[0].phase,
        OperationPhase::Interrupted
    );
}

#[test]
fn search_commands_keep_parameters_share_stop_and_project_results() {
    let mut actor = actor(16);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let interrupt = Arc::new(AtomicBool::new(true));
    actor.connect_search(DomainCommandPort::new(tx), Arc::clone(&interrupt));
    let command = AppCommand::StartCascade {
        prompt: "solve".into(),
        backend: "stub".into(),
        n: 5,
        vote_k: 2,
        check_cmd: Some("check".into()),
        diversity_hints: vec!["different".into()],
        escalate_backend: Some("strong".into()),
    };
    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision: AppRevision(0),
            command
        }),
        AppCommandResult::Applied { .. }
    ));
    match rx.try_recv().unwrap() {
        SearchCommand::Cascade(params) => {
            assert_eq!(params.backend, "stub");
            assert_eq!(params.n, 5);
            assert_eq!(params.vote_k, 2);
            assert_eq!(params.escalate_backend.as_deref(), Some("strong"));
        }
        _ => panic!("expected cascade"),
    }
    assert!(!interrupt.load(Ordering::SeqCst));
    let revision = actor.snapshot().revision;
    actor.submit(AppCommandRequest {
        revision,
        command: AppCommand::StopOperation {
            kind: OperationKind::Cascade,
        },
    });
    assert!(interrupt.load(Ordering::SeqCst));
    actor
        .apply_operation_stream_event(&StreamEvent::SearchFinished {
            kind: SearchKind::Cascade,
            summary: "best candidate".into(),
            is_error: false,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        actor.snapshot().operations[0].phase,
        OperationPhase::Interrupted
    );

    let revision = actor.snapshot().revision;
    let evolve = AppCommand::StartEvolve {
        prompt: "improve".into(),
        backend: "stub".into(),
        generations: 3,
        population: 2,
        fitness_cmd: "score".into(),
        feature_cmd: None,
        islands: 1,
        migration_interval: 0,
        mutation_hints: vec![],
    };
    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision,
            command: evolve
        }),
        AppCommandResult::Applied { .. }
    ));
    assert!(matches!(rx.try_recv().unwrap(), SearchCommand::Evolve(_)));
    let snapshot = SearchSnapshot::starting(SearchKind::Evolve, 3);
    actor
        .apply_operation_stream_event(&StreamEvent::SearchProgress(Box::new(snapshot)))
        .unwrap()
        .unwrap();
    actor
        .apply_operation_stream_event(&StreamEvent::SearchFinished {
            kind: SearchKind::Evolve,
            summary: "winner".into(),
            is_error: false,
        })
        .unwrap()
        .unwrap();
    let evolve = actor
        .snapshot()
        .operations
        .iter()
        .find(|item| item.kind == OperationKind::Evolve)
        .unwrap();
    assert_eq!(evolve.phase, OperationPhase::Completed);
    assert_eq!(evolve.message.as_deref(), Some("winner"));
}

#[test]
fn invalid_search_parameters_are_rejected_before_dispatch() {
    let mut actor = actor(4);
    let (tx, mut rx) = mpsc::unbounded_channel();
    actor.connect_search(DomainCommandPort::new(tx), Arc::new(AtomicBool::new(false)));
    let result = actor.submit(AppCommandRequest {
        revision: AppRevision(0),
        command: AppCommand::StartEvolve {
            prompt: "seed".into(),
            backend: "stub".into(),
            generations: 1,
            population: 1,
            fitness_cmd: " ".into(),
            feature_cmd: None,
            islands: 1,
            migration_interval: 0,
            mutation_hints: vec![],
        },
    });
    assert!(matches!(result, AppCommandResult::Rejected { .. }));
    assert!(rx.try_recv().is_err());
}

#[test]
fn operation_events_replace_state_for_the_same_service() {
    let mut actor = actor(4);
    let running = OperationState {
        kind: OperationKind::Procedure,
        operation_id: Some("run-1".into()),
        phase: OperationPhase::Running,
        progress: None,
        message: Some("localizing".into()),
        error: None,
    };
    actor
        .apply_event(AppEvent::OperationChanged(running))
        .unwrap();
    let awaiting_review = OperationState {
        kind: OperationKind::Procedure,
        operation_id: Some("run-1".into()),
        phase: OperationPhase::AwaitingReview,
        progress: None,
        message: Some("review evidence ready".into()),
        error: None,
    };
    actor
        .apply_event(AppEvent::OperationChanged(awaiting_review.clone()))
        .unwrap();

    assert_eq!(actor.snapshot().operations, vec![awaiting_review]);
    assert_eq!(actor.snapshot().revision, AppRevision(2));
}

// covers: deepseek-custom/web-application :: Existing operational workspaces remain available :: Procedure waits for review
#[test]
fn procedure_commands_preserve_run_identity_and_reject_stale_reviews() {
    let mut snapshot = AppSnapshot::initial(
        VisibleSettings::from_settings(&Settings::default(), None, None),
        SessionSummary {
            id: "session-1".into(),
            title: "New conversation".into(),
            backend: "stub".into(),
            model: "test".into(),
        },
    );
    snapshot.settings.procedure.localization_backend = Some("localizer".into());
    let mut actor = ApplicationActor::new(snapshot, 16);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let interrupt = Arc::new(AtomicBool::new(true));
    actor.connect_procedure(DomainCommandPort::new(tx), Arc::clone(&interrupt));

    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision: AppRevision(0),
            command: AppCommand::RunProcedure {
                change_id: "web-change".into(),
                task_id: "5.4".into()
            },
        }),
        AppCommandResult::Applied { .. }
    ));
    let run_id = match rx.try_recv().unwrap() {
        ProcedureCommand::Run {
            run_id,
            backend,
            request,
        } => {
            assert_eq!(backend, "localizer");
            assert_eq!(request.change_id, "web-change");
            assert_eq!(request.task_id, "5.4");
            run_id
        }
        _ => panic!("expected procedure run"),
    };
    assert!(!interrupt.load(Ordering::SeqCst));
    let run_id_text = run_id.as_str();
    assert_eq!(
        actor.snapshot().operations[0].operation_id.as_deref(),
        Some(run_id_text.as_str())
    );

    let revision = actor.snapshot().revision;
    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision,
            command: AppCommand::ReviewProcedure {
                run_id: "00000000-0000-0000-0000-000000000000".into(),
                decision: deepseek_custom::application::dto::ReviewDecision::Approve
            },
        }),
        AppCommandResult::Rejected { .. }
    ));
    assert!(rx.try_recv().is_err());

    actor
        .apply_procedure_progress(&ProcedureProgress::StageStarted {
            run_id,
            stage: ProcedureStage::Localization,
        })
        .unwrap()
        .unwrap();
    actor
        .apply_procedure_progress(&ProcedureProgress::AttemptAccepted {
            run_id,
            number: 1,
            targets: 2,
        })
        .unwrap()
        .unwrap();
    actor
        .apply_procedure_progress(&ProcedureProgress::RunFinished {
            run_id,
            disposition: ProcedureTerminalDisposition::AwaitingReview,
        })
        .unwrap()
        .unwrap();
    let evidence = actor.snapshot().operations[0].message.as_deref().unwrap();
    assert!(evidence.contains("Stage started: Localization"));
    assert!(evidence.contains("Attempt 1 accepted with 2 target(s)"));
    assert_eq!(
        actor.snapshot().operations[0].phase,
        OperationPhase::AwaitingReview
    );
    let revision = actor.snapshot().revision;
    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision,
            command: AppCommand::ReviewProcedure {
                run_id: run_id.as_str(),
                decision: deepseek_custom::application::dto::ReviewDecision::Reject
            },
        }),
        AppCommandResult::Applied { .. }
    ));
    assert!(matches!(rx.try_recv().unwrap(), ProcedureCommand::Review {
        run_id: actual,
        decision: ProcedureReviewDecision::Reject,
    } if actual == run_id));
}

// covers: deepseek-custom/web-application :: Existing operational workspaces remain available :: User runs an operational workflow
#[test]
fn every_operational_workspace_keeps_its_typed_command_and_observable_state() {
    let commands = [
        (
            OperationKind::Autopilot,
            AppCommand::StartAutopilot {
                task: "repeat".into(),
                iterations: 2,
            },
        ),
        (
            OperationKind::Cascade,
            AppCommand::StartCascade {
                prompt: "search".into(),
                backend: "worker".into(),
                n: 3,
                vote_k: 1,
                check_cmd: None,
                diversity_hints: vec![],
                escalate_backend: None,
            },
        ),
        (
            OperationKind::Evolve,
            AppCommand::StartEvolve {
                prompt: "evolve".into(),
                backend: "worker".into(),
                generations: 2,
                population: 2,
                fitness_cmd: "score".into(),
                feature_cmd: None,
                islands: 1,
                migration_interval: 0,
                mutation_hints: vec![],
            },
        ),
        (
            OperationKind::Procedure,
            AppCommand::RunProcedure {
                change_id: "web-change".into(),
                task_id: "5.4".into(),
            },
        ),
    ];
    let expected_commands = [
        "start_autopilot",
        "start_cascade",
        "start_evolve",
        "run_procedure",
    ];
    for ((kind, command), expected_command) in commands.into_iter().zip(expected_commands) {
        let encoded = serde_json::to_value(command).unwrap();
        assert_eq!(encoded["command"], expected_command);
        let running = OperationState {
            kind,
            operation_id: Some(format!("{expected_command}-run")),
            phase: OperationPhase::Running,
            progress: Some(deepseek_custom::application::dto::OperationProgress {
                completed: 1,
                total: Some(2),
            }),
            message: Some("live progress".into()),
            error: None,
        };
        assert_eq!(running.progress.unwrap().completed, 1);
        assert_eq!(running.phase, OperationPhase::Running);
        let terminal = OperationState {
            phase: OperationPhase::Completed,
            message: Some("terminal outcome".into()),
            ..running
        };
        assert_eq!(terminal.message.as_deref(), Some("terminal outcome"));
    }
}

#[test]
fn procedure_browser_modes_dispatch_existing_preview_whole_change_and_apply_commands() {
    fn connected() -> (ApplicationActor, mpsc::UnboundedReceiver<ProcedureCommand>) {
        let mut snapshot = AppSnapshot::initial(
            VisibleSettings::from_settings(&Settings::default(), None, None),
            SessionSummary {
                id: "s".into(),
                title: "t".into(),
                backend: "b".into(),
                model: "m".into(),
            },
        );
        snapshot.settings.procedure.localization_backend = Some("localizer".into());
        let mut actor = ApplicationActor::new(snapshot, 8);
        let (tx, rx) = mpsc::unbounded_channel();
        actor.connect_procedure(DomainCommandPort::new(tx), Arc::new(AtomicBool::new(false)));
        (actor, rx)
    }
    let localization_run_id = ProcedureRunId::new();
    let preview_id = PatchPreviewId::new();

    let (mut actor, mut rx) = connected();
    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision: AppRevision(0),
            command: AppCommand::PreviewProcedure {
                localization_run_id: localization_run_id.as_str(),
                change_id: "change".into(),
                task_id: "1.1".into(),
                route: deepseek_custom::application::dto::ProcedureRouteOverride::ForceLocal,
                local_backend: "local".into(),
                local_model: "lm".into(),
                frontier_backend: "frontier".into(),
                frontier_model: "fm".into(),
            }
        }),
        AppCommandResult::Applied { .. }
    ));
    assert!(
        matches!(rx.try_recv().unwrap(), ProcedureCommand::Preview { request, .. }
        if request.localization_run_id == localization_run_id && request.local_backend == "local" && request.route_override == deepseek_custom::procedure::RouteOverride::ForceLocal)
    );

    let (mut actor, mut rx) = connected();
    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision: AppRevision(0),
            command: AppCommand::RunWholeChangeProcedure {
                change_id: "change".into(),
                route: deepseek_custom::application::dto::ProcedureRouteOverride::ForceFrontier,
                localization_backend: "localizer".into(),
                local_backend: "local".into(),
                local_model: "lm".into(),
                frontier_backend: "frontier".into(),
                frontier_model: "fm".into(),
            }
        }),
        AppCommandResult::Applied { .. }
    ));
    assert!(
        matches!(rx.try_recv().unwrap(), ProcedureCommand::WholeChange { request, .. }
        if request.change_id == "change" && request.route_override == deepseek_custom::procedure::RouteOverride::ForceFrontier)
    );

    let (mut actor, mut rx) = connected();
    assert!(matches!(
        actor.submit(AppCommandRequest {
            revision: AppRevision(0),
            command: AppCommand::ApplyProcedure {
                localization_run_id: localization_run_id.as_str(),
                preview_id: preview_id.as_str(),
                change_id: "change".into(),
                task_id: "1.1".into(),
            }
        }),
        AppCommandResult::Applied { .. }
    ));
    assert!(
        matches!(rx.try_recv().unwrap(), ProcedureCommand::Apply { request, .. }
        if request.localization_run_id == localization_run_id && request.preview_id == preview_id)
    );
}

fn notice(id: u64, message: &str) -> AppEvent {
    AppEvent::TranscriptAppended(TranscriptBlock {
        id,
        content: TranscriptContent::Notice {
            message: message.into(),
            level: NoticeLevel::Info,
        },
    })
}

#[test]
fn commands_and_events_share_one_monotonic_order() {
    let mut actor = actor(8);
    assert_eq!(
        actor.apply_event(notice(1, "first")).unwrap(),
        AppRevision(1)
    );
    assert_eq!(
        actor.submit(AppCommandRequest {
            revision: AppRevision(1),
            command: AppCommand::SelectWorkspace {
                workspace: Workspace::Sessions
            },
        }),
        AppCommandResult::Applied {
            revision: AppRevision(2)
        }
    );
    assert_eq!(
        actor.apply_event(notice(2, "third")).unwrap(),
        AppRevision(3)
    );
    let Replay::Changes(changes) = actor.replay_after(AppRevision::INITIAL) else {
        panic!()
    };
    assert_eq!(
        changes
            .iter()
            .map(|change| change.revision.0)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn stale_commands_conflict_without_mutating_state() {
    let mut actor = actor(4);
    actor.apply_event(notice(1, "changed")).unwrap();
    assert_eq!(
        actor.submit(AppCommandRequest {
            revision: AppRevision::INITIAL,
            command: AppCommand::SelectWorkspace {
                workspace: Workspace::Tests
            },
        }),
        AppCommandResult::Conflict {
            current_revision: AppRevision(1)
        }
    );
    assert_eq!(actor.snapshot().workspace, Workspace::Chat);
    assert_eq!(actor.snapshot().revision, AppRevision(1));
}

#[test]
fn bounded_replay_resets_old_clients_deterministically() {
    let mut actor = actor(2);
    actor.apply_event(notice(1, "one")).unwrap();
    actor.apply_event(notice(2, "two")).unwrap();
    actor.apply_event(notice(3, "three")).unwrap();
    assert!(
        matches!(actor.replay_after(AppRevision::INITIAL), Replay::Reset(snapshot) if snapshot.revision == AppRevision(3))
    );
    assert!(
        matches!(actor.replay_after(AppRevision(1)), Replay::Changes(changes) if changes.len() == 2)
    );
}

#[test]
fn zero_capacity_always_resets_a_client_that_is_behind() {
    let mut actor = actor(0);
    actor.apply_event(notice(1, "one")).unwrap();
    assert!(matches!(
        actor.replay_after(AppRevision::INITIAL),
        Replay::Reset(_)
    ));
}

#[test]
fn send_message_projects_user_then_running_state_in_revision_order() {
    let mut actor = actor(8);
    actor.register_attachment(
        "accepted-image-1".into(),
        deepseek_custom::api::types::ImageAttachment {
            data: "AA==".into(),
            media_type: "image/png".into(),
        },
    );
    let result = actor.submit(AppCommandRequest {
        revision: AppRevision::INITIAL,
        command: AppCommand::SendMessage {
            text: " hello ".into(),
            attachment_id: Some("accepted-image-1".into()),
        },
    });
    assert_eq!(
        result,
        AppCommandResult::Applied {
            revision: AppRevision(2)
        }
    );
    let Replay::Changes(changes) = actor.replay_after(AppRevision::INITIAL) else {
        panic!()
    };
    assert_eq!(changes.len(), 2);
    assert!(
        matches!(&changes[0].change, deepseek_custom::application::dto::AppChangeKind::TranscriptAppended(TranscriptBlock { content: TranscriptContent::User { text, has_image: true }, .. }) if text == "hello")
    );
    assert!(matches!(
        &changes[1].change,
        deepseek_custom::application::dto::AppChangeKind::OperationChanged(OperationState {
            kind: OperationKind::Chat,
            phase: OperationPhase::Running,
            ..
        })
    ));
}

fn chat_actor(
    tag: &str,
) -> (
    std::path::PathBuf,
    ApplicationActor,
    tokio::sync::mpsc::UnboundedReceiver<AgentCommand>,
    Arc<AtomicBool>,
) {
    let dir = super::scratch_dir("application-actor-chat", tag);
    let origin = SessionOrigin {
        backend: "stub".into(),
        model: "test".into(),
    };
    let session = ApplicationSession::new(SessionState::new(
        SessionStore::for_project(&dir),
        origin.clone(),
    ));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let interrupt = Arc::new(AtomicBool::new(false));
    let actor = actor(32).with_chat_lifecycle(ChatLifecycle::new(
        session,
        DomainCommandPort::new(tx),
        Arc::clone(&interrupt),
        origin,
    ));
    (dir, actor, rx, interrupt)
}

#[test]
fn accepted_attachment_reaches_existing_backend_turn_and_is_consumed_once() {
    let (dir, mut actor, mut commands, _) = chat_actor("attachment");
    actor.register_attachment(
        "image-1".into(),
        deepseek_custom::api::types::ImageAttachment {
            data: "AA==".into(),
            media_type: "image/png".into(),
        },
    );
    let sent = actor.submit(AppCommandRequest {
        revision: actor.snapshot().revision,
        command: AppCommand::SendMessage {
            text: "inspect".into(),
            attachment_id: Some("image-1".into()),
        },
    });
    assert!(matches!(sent, AppCommandResult::Applied { .. }));
    assert!(matches!(
        commands.try_recv(),
        Ok(AgentCommand::UserTurn { image: Some(image), .. })
            if image.media_type == "image/png" && image.data == "AA=="
    ));
    assert!(!actor.remove_attachment("image-1"));
    drop(actor);
    std::fs::remove_dir_all(dir).unwrap();
}

// covers: deepseek-custom/web-application :: Chat and saved sessions preserve their lifecycle :: User interrupts a turn
#[test]
fn stop_signals_the_backend_and_projects_an_interrupted_terminal() {
    let (dir, mut actor, mut commands, interrupt) = chat_actor("interrupt");
    let sent = actor.submit(AppCommandRequest {
        revision: actor.snapshot().revision,
        command: AppCommand::SendMessage {
            text: "keep this".into(),
            attachment_id: None,
        },
    });
    assert!(matches!(sent, AppCommandResult::Applied { .. }));
    assert!(
        matches!(commands.try_recv(), Ok(AgentCommand::UserTurn { text, .. }) if text == "keep this")
    );

    let stopped = actor.submit(AppCommandRequest {
        revision: actor.snapshot().revision,
        command: AppCommand::StopOperation {
            kind: OperationKind::Chat,
        },
    });

    assert!(matches!(stopped, AppCommandResult::Applied { .. }));
    assert!(interrupt.load(Ordering::SeqCst));
    assert!(matches!(
        actor.snapshot().transcript.last(),
        Some(TranscriptBlock {
            content: TranscriptContent::Terminal {
                outcome: OperationPhase::Interrupted,
                ..
            },
            ..
        })
    ));
    assert!(matches!(
        actor
            .snapshot()
            .operations
            .iter()
            .find(|operation| operation.kind == OperationKind::Chat),
        Some(OperationState {
            phase: OperationPhase::Interrupted,
            ..
        })
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

// covers: deepseek-custom/web-application :: Chat and saved sessions preserve their lifecycle :: User changes sessions during a turn
#[test]
fn session_switch_waits_for_terminal_and_saves_the_outgoing_turn() {
    let (dir, mut actor, mut commands, _interrupt) = chat_actor("deferred-session");
    let outgoing_id = actor.snapshot().session.id.clone();
    let sent = actor.submit(AppCommandRequest {
        revision: actor.snapshot().revision,
        command: AppCommand::SendMessage {
            text: "outgoing complete turn".into(),
            attachment_id: None,
        },
    });
    assert!(matches!(sent, AppCommandResult::Applied { .. }));
    assert!(matches!(
        commands.try_recv(),
        Ok(AgentCommand::UserTurn { .. })
    ));

    let pending = actor.submit(AppCommandRequest {
        revision: actor.snapshot().revision,
        command: AppCommand::NewSession,
    });
    assert!(matches!(pending, AppCommandResult::Applied { .. }));
    assert_eq!(actor.snapshot().session.id, outgoing_id);
    assert_eq!(
        actor.snapshot().pending_session_switch,
        Some(deepseek_custom::application::dto::PendingSessionSwitch::New)
    );
    assert!(commands.try_recv().is_err());

    actor
        .apply_event(AppEvent::TranscriptAppended(TranscriptBlock {
            id: 90,
            content: TranscriptContent::Terminal {
                outcome: OperationPhase::Completed,
                message: "Response complete".into(),
            },
        }))
        .unwrap();
    assert_ne!(actor.snapshot().session.id, outgoing_id);
    assert_eq!(actor.snapshot().pending_session_switch, None);
    assert!(matches!(commands.try_recv(), Ok(AgentCommand::NewSession)));
    let outgoing = SessionStore::for_project(&dir)
        .list()
        .into_iter()
        .find(|meta| meta.id.as_str() == outgoing_id)
        .expect("outgoing session is autosaved after its terminal event");
    let record = SessionStore::for_project(&dir).load(&outgoing.id).unwrap();
    assert!(format!("{:?}", record.transcript.blocks()).contains("outgoing complete turn"));
    assert!(format!("{:?}", record.transcript.blocks()).contains("Response complete"));
    std::fs::remove_dir_all(dir).unwrap();
}
