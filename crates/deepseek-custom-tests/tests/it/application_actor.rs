use deepseek_custom::application::actor::{AppEvent, ApplicationActor, Replay};
use deepseek_custom::application::dto::{
    AppCommand, AppCommandRequest, AppCommandResult, AppRevision, AppSnapshot, NoticeLevel,
    OperationKind, OperationPhase, OperationState, SessionSummary, TranscriptBlock,
    TranscriptContent, VisibleSettings, Workspace,
};
use deepseek_custom::config::settings::Settings;

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
