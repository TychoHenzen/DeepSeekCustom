use deepseek_custom::agent::events::{
    RouteHop, RoutedEvent, StreamEvent, SubagentId, SubagentMeta,
};
use deepseek_custom::application::dto::{PendingSessionSwitch, TranscriptContent};
use deepseek_custom::application::session::{ApplicationSession, PendingSwitch};
use deepseek_custom::application::session_state::{SessionOrigin, SessionState};
use deepseek_custom::application::transcript::BlockKind;
use deepseek_custom::session::SessionStore;

fn application(tag: &str) -> (std::path::PathBuf, ApplicationSession) {
    let dir = super::scratch_dir("application-session", tag);
    let state = SessionState::new(
        SessionStore::for_project(&dir),
        SessionOrigin {
            backend: "stub".into(),
            model: "test".into(),
        },
    );
    (dir, ApplicationSession::new(state))
}

#[test]
fn main_and_routed_events_project_in_order_without_mixing_subagent_text() {
    let (dir, mut application) = application("routing");
    application
        .transcript
        .apply_routed_event(RoutedEvent::own(StreamEvent::Text {
            turn: 1,
            text: "main".into(),
        }));
    application.transcript.apply_routed_event(RoutedEvent {
        route: vec![RouteHop {
            id: SubagentId::next(),
            meta: SubagentMeta {
                backend: "stub".into(),
                model: "child".into(),
                depth: 1,
            },
            session_turns: 1,
            session_turn_cap: 4,
            send_message_calls: 0,
            send_message_call_cap: 4,
        }],
        event: StreamEvent::Text {
            turn: 1,
            text: "child".into(),
        },
    });

    let projection = application.transcript_projection();
    assert!(
        matches!(&projection[0].content, TranscriptContent::Assistant { spans } if format!("{spans:?}").contains("main"))
    );
    assert!(
        matches!(&projection[1].content, TranscriptContent::Subagent { blocks, .. } if format!("{blocks:?}").contains("child"))
    );
    assert!(!format!("{:?}", projection[0]).contains("child"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn deferred_switch_and_current_session_are_projected_from_actor_owned_state() {
    let (dir, mut application) = application("session");
    let original_id = application.session_summary().id;
    application.pending_switch = Some(PendingSwitch::New);
    assert_eq!(
        application.pending_session_switch(),
        Some(PendingSessionSwitch::New)
    );
    application.transcript.push(BlockKind::User {
        text: "saved title".into(),
    });
    application.sessions.autosave(
        &mut application.transcript,
        SessionOrigin {
            backend: "stub".into(),
            model: "test".into(),
        },
    );
    assert_eq!(application.session_summary().id, original_id);
    assert_eq!(application.session_summary().title, "saved title");
    std::fs::remove_dir_all(dir).unwrap();
}
