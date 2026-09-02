use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use deepseek_custom::agent::events::AgentCommand;
use deepseek_custom::application::actor::{ApplicationActor, ChatLifecycle};
use deepseek_custom::application::dto::{
    AppCommand, AppCommandRequest, AppCommandResult, AppSnapshot, SessionSummary, VisibleSettings,
};
use deepseek_custom::application::services::DomainCommandPort;
use deepseek_custom::application::session::ApplicationSession;
use deepseek_custom::application::session_state::{SessionOrigin, SessionState};
use deepseek_custom::config::settings::Settings;
use deepseek_custom::controlled_development::{
    ControlledDevelopmentPhase, ControlledDevelopmentState, WorkCard,
};
use deepseek_custom::session::SessionStore;
use serde_json::json;
use tokio::sync::mpsc;

// covers: deepseek-custom/controlled-development-mode :: Controlled Development has an explicit lifecycle :: Mode is off
#[test]
fn disabled_controlled_development_keeps_the_existing_chat_lifecycle() {
    let state = ControlledDevelopmentState::default();
    assert!(!state.is_enabled());
    assert_eq!(state.phase(), ControlledDevelopmentPhase::Off);
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        json!({
            "enabled": false,
            "phase": "off"
        })
    );

    let phases = [
        ControlledDevelopmentPhase::Off,
        ControlledDevelopmentPhase::Planning,
        ControlledDevelopmentPhase::AwaitingApproval,
        ControlledDevelopmentPhase::Executing,
        ControlledDevelopmentPhase::Completed,
        ControlledDevelopmentPhase::Blocked,
        ControlledDevelopmentPhase::Interrupted,
    ];
    for phase in phases {
        let encoded = serde_json::to_string(&phase).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlledDevelopmentPhase>(&encoded).unwrap(),
            phase
        );
    }

    let root = super::scratch_dir("controlled-development", "mode-off");
    let origin = SessionOrigin {
        backend: "stub".into(),
        model: "test".into(),
    };
    let session = ApplicationSession::new(SessionState::new(
        SessionStore::for_project(&root),
        origin.clone(),
    ));
    let (tx, mut commands) = mpsc::unbounded_channel();
    let snapshot = AppSnapshot::initial(
        VisibleSettings::from_settings(&Settings::default(), None, None),
        SessionSummary {
            id: "session-1".into(),
            title: "New conversation".into(),
            backend: "stub".into(),
            model: "test".into(),
        },
    );
    let mut actor = ApplicationActor::new(snapshot, 8).with_chat_lifecycle(ChatLifecycle::new(
        session,
        DomainCommandPort::new(tx),
        Arc::new(AtomicBool::new(false)),
        origin,
    ));

    let result = actor.submit(AppCommandRequest {
        revision: actor.snapshot().revision,
        command: AppCommand::SendMessage {
            text: "normal chat request".into(),
            attachment_id: None,
        },
    });

    assert!(matches!(result, AppCommandResult::Applied { .. }));
    assert!(matches!(
        commands.try_recv(),
        Ok(AgentCommand::UserTurn { text, image: None }) if text == "normal chat request"
    ));
    std::fs::remove_dir_all(root).unwrap();
}

// covers: deepseek-custom/controlled-development-mode :: Controlled Development has an explicit lifecycle :: Mode starts planning
#[test]
fn starting_a_packet_enters_planning_and_clears_earlier_approval() {
    let mut state: ControlledDevelopmentState = serde_json::from_value(json!({
        "enabled": true,
        "phase": "completed",
        "packet_id": "packet-old",
        "approved_card_id": "card-old"
    }))
    .unwrap();
    assert_eq!(state.approved_card_id(), Some("card-old"));

    state.begin_packet("packet-new").unwrap();

    assert!(state.is_enabled());
    assert_eq!(state.phase(), ControlledDevelopmentPhase::Planning);
    assert_eq!(state.packet_id(), Some("packet-new"));
    assert_eq!(state.approved_card_id(), None);
}

// covers: deepseek-custom/controlled-development-mode :: A Work Card is strict and bounded :: Valid Work Card awaits approval
#[test]
fn valid_work_card_is_stored_and_awaits_approval() {
    let card_json = json!({
        "id": "packet-1",
        "outcome": "The Controlled Development state accepts one validated Work Card.",
        "proof_commands": [
            "cargo test -p deepseek-custom-tests --test it controlled_development"
        ],
        "production_paths": [
            "crates/deepseek-custom/src/controlled_development/work_card.rs"
        ],
        "supporting_paths": [
            "crates/deepseek-custom-tests/tests/it/controlled_development.rs"
        ],
        "excluded": ["settings.json", "Procedure implementation"],
        "complexity_exceptions": []
    });
    let card: WorkCard = serde_json::from_value(card_json.clone()).unwrap();
    card.validate().unwrap();

    let mut with_unknown = card_json;
    with_unknown["notes"] = json!("must be rejected");
    assert!(serde_json::from_value::<WorkCard>(with_unknown).is_err());

    let mut state = ControlledDevelopmentState::default();
    state.set_enabled(true);
    state.begin_packet("packet-1").unwrap();
    state.accept_work_card(card.clone()).unwrap();

    assert_eq!(state.phase(), ControlledDevelopmentPhase::AwaitingApproval);
    assert_eq!(state.work_card(), Some(&card));
    assert_eq!(state.approved_card_id(), None);
}
