use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deepseek_custom::agent::events::AgentCommand;
use deepseek_custom::application::actor::{AppEvent, ApplicationActor, ChatLifecycle, Replay};
use deepseek_custom::application::dto::{
    AppCommand, AppCommandRequest, AppCommandResult, AppRevision, AppSnapshot, NoticeLevel,
    OperationKind, OperationPhase, OperationState, SessionSummary, TranscriptBlock,
    TranscriptContent, VisibleSettings, Workspace,
};
use deepseek_custom::application::services::DomainCommandPort;
use deepseek_custom::application::session::ApplicationSession;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::gui::session_state::{SessionOrigin, SessionState};
use deepseek_custom::session::SessionStore;

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
