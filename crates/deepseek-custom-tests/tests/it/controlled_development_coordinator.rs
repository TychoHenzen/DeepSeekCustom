use deepseek_custom::controlled_development::{
    ControlledBackendSelection, ControlledChangeGateError, ControlledDevelopmentCommand,
    ControlledDevelopmentCoordinator, ControlledDevelopmentEffect, ControlledDevelopmentPhase,
    ControlledDevelopmentTransitionError, DependencyFileKind, authorize_changed_paths,
    classify_dependency_file,
};
use serde_json::json;

use deepseek_custom::procedure::DisposableDraftWorkspace;

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

// covers: deepseek-custom/controlled-development-mode :: Changed paths must match the approved card :: Changed path outside approved lists blocks promotion
#[test]
fn changed_path_outside_approved_lists_blocks_before_any_promotion_effect() {
    let root = super::scratch_dir("controlled-development", "unauthorized-change");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/approved.rs"), "before\n").unwrap();
    let mut coordinator =
        awaiting_coordinator_with_paths(&root, "unauthorized-card", &["src/approved.rs"], &[]);
    coordinator
        .handle(ControlledDevelopmentCommand::Approve {
            card_id: "unauthorized-card".into(),
        })
        .unwrap();
    let pair = DisposableDraftWorkspace::create_current_state_pair(&root).unwrap();
    std::fs::write(pair.execution_path().join("src/approved.rs"), "after\n").unwrap();
    std::fs::write(pair.execution_path().join("src/unapproved.rs"), "new\n").unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::ExecutionWorkspaceReady {
            card_id: "unauthorized-card".into(),
            workspace: Box::new(pair),
        })
        .unwrap();

    let effect = coordinator
        .handle(ControlledDevelopmentCommand::ValidateIsolatedChanges {
            card_id: "unauthorized-card".into(),
        })
        .unwrap();

    assert!(effect.is_none());
    assert_eq!(
        coordinator.state().phase(),
        ControlledDevelopmentPhase::Blocked
    );
    assert_eq!(coordinator.state().approved_card_id(), None);
    assert!(coordinator.blocker().unwrap().contains("src/unapproved.rs"));
    assert_eq!(
        coordinator.changed_paths(),
        ["src/approved.rs", "src/unapproved.rs"]
    );
    assert_eq!(
        std::fs::read(root.join("src/approved.rs")).unwrap(),
        b"before\n"
    );
    assert!(!root.join("src/unapproved.rs").exists());
    assert!(matches!(
        coordinator.handle(ControlledDevelopmentCommand::Complete {
            card_id: "unauthorized-card".into(),
            summary: "must not complete".into(),
        }),
        Err(ControlledDevelopmentTransitionError::NotExecuting)
    ));

    std::fs::remove_dir_all(root).unwrap();
}

// covers: deepseek-custom/controlled-development-mode :: Changed paths must match the approved card :: More than three production paths blocks promotion
#[test]
fn raw_change_gate_blocks_fourth_production_endpoint_but_not_supporting_paths() {
    let production_paths = (1..=4)
        .map(|index| format!("src/production-{index}.rs"))
        .collect::<Vec<_>>();
    let supporting_paths = (1..=12)
        .map(|index| format!("tests/supporting-{index}.rs"))
        .collect::<Vec<_>>();
    let all_paths = production_paths
        .iter()
        .chain(&supporting_paths)
        .cloned()
        .collect::<Vec<_>>();

    let error =
        authorize_changed_paths(all_paths, &production_paths, &supporting_paths).unwrap_err();

    assert_eq!(
        error,
        ControlledChangeGateError::TooManyProductionPaths {
            maximum: 3,
            changed_paths: production_paths,
        }
    );
    assert!(authorize_changed_paths(supporting_paths.clone(), &[], &supporting_paths,).is_ok());
}

// covers: deepseek-custom/controlled-development-mode :: Complexity exceptions gate dependency files :: Unapproved manifest or lockfile change blocks promotion
#[test]
fn dependency_file_classifier_and_named_exception_gate_fail_closed() {
    assert_eq!(
        classify_dependency_file("Cargo.toml"),
        Some(DependencyFileKind::CargoManifest)
    );
    assert_eq!(
        classify_dependency_file("crates/member/Cargo.toml"),
        Some(DependencyFileKind::CargoManifest)
    );
    assert_eq!(
        classify_dependency_file("Cargo.lock"),
        Some(DependencyFileKind::CargoLock)
    );
    assert_eq!(
        classify_dependency_file("web/package.json"),
        Some(DependencyFileKind::WebPackageManifest)
    );
    assert_eq!(
        classify_dependency_file("web/package-lock.json"),
        Some(DependencyFileKind::WebPackageLock)
    );
    for unrelated in [
        "nested/Cargo.lock",
        "package.json",
        "web/other/package.json",
        "web/package-lock.yaml",
    ] {
        assert_eq!(classify_dependency_file(unrelated), None, "{unrelated}");
    }

    let root = super::scratch_dir("controlled-development", "dependency-gate");
    std::fs::create_dir_all(root.join("web")).unwrap();
    std::fs::write(
        root.join("web/package.json"),
        r#"{"dependencies":{"react":"1"}}"#,
    )
    .unwrap();
    let mut coordinator = awaiting_coordinator_with_card(
        &root,
        "dependency-card",
        &["web/package.json"],
        &[],
        &["Add unrelated tokio dependency"],
    );
    coordinator
        .handle(ControlledDevelopmentCommand::Approve {
            card_id: "dependency-card".into(),
        })
        .unwrap();
    let pair = DisposableDraftWorkspace::create_current_state_pair(&root).unwrap();
    std::fs::write(
        pair.execution_path().join("web/package.json"),
        r#"{"dependencies":{"react":"1","zod":"4"}}"#,
    )
    .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::ExecutionWorkspaceReady {
            card_id: "dependency-card".into(),
            workspace: Box::new(pair),
        })
        .unwrap();

    let effect = coordinator
        .handle(ControlledDevelopmentCommand::ValidateIsolatedChanges {
            card_id: "dependency-card".into(),
        })
        .unwrap();

    assert!(effect.is_none());
    assert_eq!(
        coordinator.state().phase(),
        ControlledDevelopmentPhase::Blocked
    );
    assert!(coordinator.blocker().unwrap().contains("zod"));
    assert_eq!(
        std::fs::read_to_string(root.join("web/package.json")).unwrap(),
        r#"{"dependencies":{"react":"1"}}"#
    );
    std::fs::remove_dir_all(root).unwrap();

    let allowed_root = super::scratch_dir("controlled-development", "dependency-allowed");
    std::fs::create_dir_all(allowed_root.join("web")).unwrap();
    std::fs::write(
        allowed_root.join("web/package.json"),
        r#"{"dependencies":{"react":"1"}}"#,
    )
    .unwrap();
    let mut allowed = awaiting_coordinator_with_card(
        &allowed_root,
        "allowed-dependency-card",
        &["web/package.json"],
        &[],
        &["Add zod dependency"],
    );
    allowed
        .handle(ControlledDevelopmentCommand::Approve {
            card_id: "allowed-dependency-card".into(),
        })
        .unwrap();
    let allowed_pair = DisposableDraftWorkspace::create_current_state_pair(&allowed_root).unwrap();
    std::fs::write(
        allowed_pair.execution_path().join("web/package.json"),
        r#"{"dependencies":{"react":"1","zod":"4"}}"#,
    )
    .unwrap();
    allowed
        .handle(ControlledDevelopmentCommand::ExecutionWorkspaceReady {
            card_id: "allowed-dependency-card".into(),
            workspace: Box::new(allowed_pair),
        })
        .unwrap();

    assert!(matches!(
        allowed
            .handle(ControlledDevelopmentCommand::ValidateIsolatedChanges {
                card_id: "allowed-dependency-card".into(),
            })
            .unwrap(),
        Some(ControlledDevelopmentEffect::RunProofCommands { card_id, .. })
            if card_id == "allowed-dependency-card"
    ));
    assert_eq!(
        allowed.state().phase(),
        ControlledDevelopmentPhase::Executing
    );
    assert_eq!(allowed.changed_paths(), ["web/package.json"]);
    assert_eq!(allowed.dependency_matches().len(), 1);
    assert_eq!(allowed.dependency_matches()[0].dependency_name, "zod");
    assert_eq!(
        allowed.dependency_matches()[0].complexity_exception,
        "Add zod dependency"
    );
    assert_eq!(
        std::fs::read_to_string(allowed_root.join("web/package.json")).unwrap(),
        r#"{"dependencies":{"react":"1"}}"#
    );
    std::fs::remove_dir_all(allowed_root).unwrap();
}

fn awaiting_coordinator(root: &std::path::Path, card_id: &str) -> ControlledDevelopmentCoordinator {
    let mut coordinator = ControlledDevelopmentCoordinator::default();
    coordinator
        .handle(ControlledDevelopmentCommand::SetEnabled { enabled: true })
        .unwrap();
    begin_and_finish_planning(&mut coordinator, root, card_id);
    coordinator
}

fn awaiting_coordinator_with_paths(
    root: &std::path::Path,
    card_id: &str,
    production_paths: &[&str],
    supporting_paths: &[&str],
) -> ControlledDevelopmentCoordinator {
    awaiting_coordinator_with_card(root, card_id, production_paths, supporting_paths, &[])
}

fn awaiting_coordinator_with_card(
    root: &std::path::Path,
    card_id: &str,
    production_paths: &[&str],
    supporting_paths: &[&str],
    complexity_exceptions: &[&str],
) -> ControlledDevelopmentCoordinator {
    let mut coordinator = ControlledDevelopmentCoordinator::default();
    coordinator
        .handle(ControlledDevelopmentCommand::SetEnabled { enabled: true })
        .unwrap();
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
            final_response: json!({
                "id": card_id,
                "outcome": "The packet produces one observable result.",
                "proof_commands": ["cargo check --workspace"],
                "production_paths": production_paths,
                "supporting_paths": supporting_paths,
                "excluded": ["settings.json"],
                "complexity_exceptions": complexity_exceptions
            })
            .to_string(),
        })
        .unwrap();
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
