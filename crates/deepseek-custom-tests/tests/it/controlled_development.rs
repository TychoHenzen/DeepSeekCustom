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
    ControlledDevelopmentPhase, ControlledDevelopmentState, MAX_COMPLEXITY_EXCEPTIONS,
    MAX_EXCLUSIONS, MAX_PROOF_COMMAND_CHARS, MAX_SUPPORTING_PATHS, MAX_WORK_CARD_ID_CHARS,
    MAX_WORK_CARD_ITEM_CHARS, MAX_WORK_CARD_OUTCOME_CHARS, MAX_WORK_CARD_PATH_CHARS, WorkCard,
    work_card_json_schema,
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

// covers: deepseek-custom/controlled-development-mode :: A Work Card is strict and bounded :: Valid Work Card awaits approval
#[test]
fn complete_structured_planning_response_matches_schema_and_awaits_approval() {
    let schema = work_card_json_schema();
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["proof_commands"]["maxItems"], 3);
    assert_eq!(schema["properties"]["production_paths"]["maxItems"], 3);
    assert_eq!(schema["required"].as_array().unwrap().len(), 7);

    let complete_response = json!({
        "id": "packet-schema-1",
        "outcome": "The planning profile produces one complete bounded Work Card.",
        "proof_commands": ["cargo check --workspace"],
        "production_paths": ["crates/deepseek-custom/src/backend/factory.rs"],
        "supporting_paths": ["crates/deepseek-custom-tests/tests/it/backend_factory.rs"],
        "excluded": ["settings.json"],
        "complexity_exceptions": []
    })
    .to_string();
    let mut state = ControlledDevelopmentState::default();
    state.set_enabled(true);
    state.begin_packet("packet-schema-1").unwrap();

    state.accept_planning_result(&complete_response).unwrap();

    assert_eq!(state.phase(), ControlledDevelopmentPhase::AwaitingApproval);
    assert_eq!(state.work_card().unwrap().id, "packet-schema-1");
    assert!(state.structural_errors().is_empty());
}

// covers: deepseek-custom/controlled-development-mode :: A Work Card is strict and bounded :: Malformed Work Card is rejected
#[test]
fn malformed_work_cards_block_with_structural_errors_without_prose_recovery() {
    let valid = json!({
        "id": "packet-1",
        "outcome": "The packet creates one observable file.",
        "proof_commands": ["cargo check --workspace"],
        "production_paths": ["crates/deepseek-custom/src/controlled_development/state.rs"],
        "supporting_paths": ["crates/deepseek-custom-tests/tests/it/controlled_development.rs"],
        "excluded": ["settings.json"],
        "complexity_exceptions": []
    });

    let mut cases = Vec::new();
    for field in [
        "id",
        "outcome",
        "proof_commands",
        "production_paths",
        "supporting_paths",
        "excluded",
        "complexity_exceptions",
    ] {
        let mut case = valid.clone();
        case.as_object_mut().unwrap().remove(field);
        cases.push(("missing field", case.to_string()));
    }
    let mut unknown_field = valid.clone();
    unknown_field["notes"] = json!("not allowed");
    cases.push(("unknown field", unknown_field.to_string()));
    for (field, invalid_value) in [
        ("id", json!(1)),
        ("outcome", json!(["result"])),
        ("proof_commands", json!("cargo check --workspace")),
        ("production_paths", json!("src/lib.rs")),
        ("supporting_paths", json!("tests/it.rs")),
        ("excluded", json!("settings.json")),
        ("complexity_exceptions", json!("none")),
    ] {
        let mut case = valid.clone();
        case[field] = invalid_value;
        cases.push(("invalid value type", case.to_string()));
    }

    for (name, path) in [
        ("empty path", ""),
        ("absolute path", "C:/outside.rs"),
        ("rooted path", "/outside.rs"),
        ("traversal", "../outside.rs"),
        ("current component", "src/./file.rs"),
        ("empty component", "src//file.rs"),
        ("backslash", "src\\file.rs"),
        ("glob", "src/*.rs"),
    ] {
        let mut case = valid.clone();
        case["production_paths"] = json!([path]);
        cases.push((name, case.to_string()));
    }
    let mut oversized_path = valid.clone();
    oversized_path["production_paths"] =
        json!([format!("src/{}.rs", "a".repeat(MAX_WORK_CARD_PATH_CHARS))]);
    cases.push(("oversized path", oversized_path.to_string()));
    let mut unsupported_supporting_path = valid.clone();
    unsupported_supporting_path["supporting_paths"] = json!(["src/helper.rs"]);
    cases.push((
        "non-supporting supporting path",
        unsupported_supporting_path.to_string(),
    ));
    let mut duplicate_path = valid.clone();
    duplicate_path["production_paths"] = json!(["src/lib.rs", "SRC/LIB.RS"]);
    cases.push(("duplicate path", duplicate_path.to_string()));
    let mut overlapping_paths = valid.clone();
    overlapping_paths["production_paths"] = json!(["docs/result.md"]);
    overlapping_paths["supporting_paths"] = json!(["DOCS/RESULT.MD"]);
    cases.push(("overlapping path lists", overlapping_paths.to_string()));

    for (name, outcome) in [
        ("empty outcome", ""),
        ("whitespace outcome", "   "),
        ("multiline outcome", "one\ntwo"),
        ("punctuation-only outcome", "---"),
    ] {
        let mut case = valid.clone();
        case["outcome"] = json!(outcome);
        cases.push((name, case.to_string()));
    }
    let mut oversized_id = valid.clone();
    oversized_id["id"] = json!("a".repeat(MAX_WORK_CARD_ID_CHARS + 1));
    cases.push(("oversized id", oversized_id.to_string()));
    let mut oversized_outcome = valid.clone();
    oversized_outcome["outcome"] = json!("a".repeat(MAX_WORK_CARD_OUTCOME_CHARS + 1));
    cases.push(("oversized outcome", oversized_outcome.to_string()));

    let mut no_proofs = valid.clone();
    no_proofs["proof_commands"] = json!([]);
    cases.push(("zero proof commands", no_proofs.to_string()));
    let mut too_many_proofs = valid.clone();
    too_many_proofs["proof_commands"] = json!(["one", "two", "three", "four"]);
    cases.push((
        "more than three proof commands",
        too_many_proofs.to_string(),
    ));
    let mut too_many_production_paths = valid.clone();
    too_many_production_paths["production_paths"] =
        json!(["src/one.rs", "src/two.rs", "src/three.rs", "src/four.rs"]);
    cases.push((
        "more than three production paths",
        too_many_production_paths.to_string(),
    ));
    for (name, proof) in [
        ("empty proof command", String::new()),
        (
            "oversized proof command",
            "a".repeat(MAX_PROOF_COMMAND_CHARS + 1),
        ),
        ("null proof command", "cargo\0check".into()),
    ] {
        let mut case = valid.clone();
        case["proof_commands"] = json!([proof]);
        cases.push((name, case.to_string()));
    }
    let mut too_many_supporting_paths = valid.clone();
    too_many_supporting_paths["supporting_paths"] = json!(
        (0..=MAX_SUPPORTING_PATHS)
            .map(|index| format!("tests/case-{index}.rs"))
            .collect::<Vec<_>>()
    );
    cases.push((
        "too many supporting paths",
        too_many_supporting_paths.to_string(),
    ));
    for (name, field, value) in [
        ("empty exclusion", "excluded", json!([])),
        (
            "too many exclusions",
            "excluded",
            json!(
                (0..=MAX_EXCLUSIONS)
                    .map(|index| format!("excluded-{index}"))
                    .collect::<Vec<_>>()
            ),
        ),
        (
            "oversized exclusion",
            "excluded",
            json!(["a".repeat(MAX_WORK_CARD_ITEM_CHARS + 1)]),
        ),
        (
            "too many complexity exceptions",
            "complexity_exceptions",
            json!(
                (0..=MAX_COMPLEXITY_EXCEPTIONS)
                    .map(|index| format!("exception-{index}"))
                    .collect::<Vec<_>>()
            ),
        ),
        (
            "oversized complexity exception",
            "complexity_exceptions",
            json!(["a".repeat(MAX_WORK_CARD_ITEM_CHARS + 1)]),
        ),
    ] {
        let mut case = valid.clone();
        case[field] = value;
        cases.push((name, case.to_string()));
    }
    cases.push((
        "JSON embedded in prose",
        format!("The card follows:\n```json\n{valid}\n```"),
    ));

    for (name, response) in cases {
        let mut state = ControlledDevelopmentState::default();
        state.set_enabled(true);
        state.begin_packet("packet-1").unwrap();

        let error = state.accept_planning_result(&response).unwrap_err();

        assert_eq!(state.phase(), ControlledDevelopmentPhase::Blocked, "{name}");
        assert!(state.work_card().is_none(), "{name}");
        assert!(state.approved_card_id().is_none(), "{name}");
        assert!(!state.structural_errors().is_empty(), "{name}: {error}");
    }
}
