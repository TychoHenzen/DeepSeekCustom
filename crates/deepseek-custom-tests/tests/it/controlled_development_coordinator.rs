use deepseek_custom::controlled_development::{
    ControlledBackendSelection, ControlledDevelopmentCommand, ControlledDevelopmentCoordinator,
    ControlledDevelopmentEffect, ControlledDevelopmentPhase, ControlledDevelopmentTransitionError,
};
use serde_json::json;

// covers: deepseek-custom/controlled-development-mode :: Approval belongs to one card :: Approval applies to one card only
#[test]
fn approval_cannot_authorize_a_later_packet_after_any_terminal_or_replacement_path() {
    let root = super::scratch_dir("controlled-development", "approval-terminal");

    for terminal in ["completed", "failed", "interrupted"] {
        let mut coordinator = awaiting_coordinator(&root, "card-one");
        let effect = coordinator
            .handle(ControlledDevelopmentCommand::Approve {
                card_id: "card-one".into(),
            })
            .unwrap();
        assert!(matches!(
            effect,
            Some(ControlledDevelopmentEffect::CreateExecutionWorkspace { ref card_id, .. })
                if card_id == "card-one"
        ));
        assert_eq!(coordinator.state().approved_card_id(), Some("card-one"));

        match terminal {
            "completed" => coordinator
                .handle(ControlledDevelopmentCommand::Complete {
                    card_id: "card-one".into(),
                    summary: "completed".into(),
                })
                .unwrap(),
            "failed" => coordinator
                .handle(ControlledDevelopmentCommand::Fail {
                    packet_id: "card-one".into(),
                    blocker: "proof failed".into(),
                    summary: "blocked".into(),
                })
                .unwrap(),
            "interrupted" => coordinator
                .handle(ControlledDevelopmentCommand::Interrupt {
                    packet_id: "card-one".into(),
                    summary: "interrupted".into(),
                })
                .unwrap(),
            _ => unreachable!(),
        };

        assert_eq!(coordinator.state().approved_card_id(), None, "{terminal}");
        let terminal_phase = coordinator.state().phase();
        assert!(matches!(
            coordinator.handle(ControlledDevelopmentCommand::Complete {
                card_id: "card-one".into(),
                summary: "late completion".into(),
            }),
            Err(ControlledDevelopmentTransitionError::NotExecuting)
        ));
        assert!(matches!(
            coordinator.handle(ControlledDevelopmentCommand::Fail {
                packet_id: "card-one".into(),
                blocker: "late failure".into(),
                summary: "late failure".into(),
            }),
            Err(ControlledDevelopmentTransitionError::NotActivePacket)
        ));
        assert_eq!(coordinator.state().phase(), terminal_phase);

        begin_and_finish_planning(&mut coordinator, &root, "card-two");
        assert_eq!(
            coordinator.state().phase(),
            ControlledDevelopmentPhase::AwaitingApproval
        );
        assert_eq!(coordinator.state().approved_card_id(), None);
        assert!(matches!(
            coordinator.handle(ControlledDevelopmentCommand::Approve {
                card_id: "card-one".into(),
            }),
            Err(ControlledDevelopmentTransitionError::CardIdMismatch)
        ));
    }

    let mut replaced = awaiting_coordinator(&root, "replaced-card");
    replaced
        .handle(ControlledDevelopmentCommand::Approve {
            card_id: "replaced-card".into(),
        })
        .unwrap();
    begin_and_finish_planning(&mut replaced, &root, "replacement-card");
    assert_eq!(replaced.state().approved_card_id(), None);
    assert!(matches!(
        replaced.handle(ControlledDevelopmentCommand::Approve {
            card_id: "replaced-card".into(),
        }),
        Err(ControlledDevelopmentTransitionError::CardIdMismatch)
    ));

    std::fs::remove_dir_all(root).unwrap();
}

// covers: deepseek-custom/controlled-development-mode :: Approval belongs to one card :: User rejects a card
#[test]
fn current_card_rejection_is_terminal_and_produces_no_execution_effect() {
    let root = super::scratch_dir("controlled-development", "reject-card");
    let mut coordinator = awaiting_coordinator(&root, "rejectable-card");

    assert!(matches!(
        coordinator.handle(ControlledDevelopmentCommand::Reject {
            card_id: "stale-card".into(),
        }),
        Err(ControlledDevelopmentTransitionError::CardIdMismatch)
    ));
    assert_eq!(
        coordinator.state().phase(),
        ControlledDevelopmentPhase::AwaitingApproval
    );
    assert!(!coordinator.has_execution_workspace());

    let effect = coordinator
        .handle(ControlledDevelopmentCommand::Reject {
            card_id: "rejectable-card".into(),
        })
        .unwrap();

    assert!(effect.is_none());
    assert_eq!(
        coordinator.state().phase(),
        ControlledDevelopmentPhase::Blocked
    );
    assert_eq!(coordinator.state().approved_card_id(), None);
    assert_eq!(coordinator.blocker(), Some("Work Card rejected by user"));
    assert!(!coordinator.has_execution_workspace());
    assert!(matches!(
        coordinator.handle(ControlledDevelopmentCommand::Approve {
            card_id: "rejectable-card".into(),
        }),
        Err(ControlledDevelopmentTransitionError::NotAwaitingApproval)
    ));

    std::fs::remove_dir_all(root).unwrap();
}

fn awaiting_coordinator(root: &std::path::Path, card_id: &str) -> ControlledDevelopmentCoordinator {
    let mut coordinator = ControlledDevelopmentCoordinator::default();
    coordinator
        .handle(ControlledDevelopmentCommand::SetEnabled { enabled: true })
        .unwrap();
    begin_and_finish_planning(&mut coordinator, root, card_id);
    coordinator
}

fn begin_and_finish_planning(
    coordinator: &mut ControlledDevelopmentCoordinator,
    root: &std::path::Path,
    card_id: &str,
) {
    coordinator
        .handle(ControlledDevelopmentCommand::Plan {
            packet_id: card_id.into(),
            original_request: format!("Implement {card_id}"),
            selection: ControlledBackendSelection::new("controlled-stub", Some("model".into())),
            workspace_root: root.to_path_buf(),
        })
        .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::PlanningFinished {
            packet_id: card_id.into(),
            final_response: valid_card_json(card_id),
        })
        .unwrap();
}

fn valid_card_json(card_id: &str) -> String {
    json!({
        "id": card_id,
        "outcome": "The packet produces one observable result.",
        "proof_commands": ["cargo check --workspace"],
        "production_paths": ["crates/deepseek-custom/src/controlled_development/coordinator.rs"],
        "supporting_paths": ["crates/deepseek-custom-tests/tests/it/controlled_development.rs"],
        "excluded": ["settings.json"],
        "complexity_exceptions": []
    })
    .to_string()
}
