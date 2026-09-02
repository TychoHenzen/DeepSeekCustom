use std::sync::Arc;

use deepseek_custom::agent::events::{RoutedEvent, StreamEvent};
use deepseek_custom::backend::Backend;
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::controlled_development::{
    ControlledBackendSelection, ControlledDevelopmentCommand, ControlledDevelopmentCoordinator,
    ControlledDevelopmentEffect,
};
use deepseek_custom::procedure::{
    DisposableDraftWorkspace, VerifierGateDisposition, VerifierGateEvidence,
};
use serde_json::json;
use tokio::sync::mpsc;

#[test]
fn coordinator_projects_typed_evidence_and_builds_fresh_controlled_backends() {
    let root = super::scratch_dir("controlled-development", "coordinator-effects");
    std::fs::write(root.join("source.txt"), b"current workspace bytes\n").unwrap();
    let normal_root = super::scratch_dir("controlled-development", "normal-backend");
    let factory = Arc::new(
        BackendFactory::new(Settings::default(), normal_root.clone())
            .with_stub("controlled-stub", vec![StubTurn::Text("unused".into())]),
    );
    let mut coordinator = ControlledDevelopmentCoordinator::default();
    coordinator
        .handle(ControlledDevelopmentCommand::SetEnabled { enabled: true })
        .unwrap();

    let planning = coordinator
        .handle(ControlledDevelopmentCommand::Plan {
            packet_id: "evidence-card".into(),
            original_request: "Implement the bounded packet".into(),
            selection: ControlledBackendSelection::new(
                "controlled-stub",
                Some("selected-model".into()),
            ),
            workspace_root: root.clone(),
        })
        .unwrap()
        .unwrap();
    assert!(matches!(
        &planning,
        ControlledDevelopmentEffect::DispatchPlanning {
            packet_id,
            original_request,
            selection,
            planning_root,
        } if packet_id == "evidence-card"
            && original_request == "Implement the bounded packet"
            && selection.backend == "controlled-stub"
            && selection.model.as_deref() == Some("selected-model")
            && planning_root == &root
    ));
    let (planning_tx, _planning_rx) = mpsc::unbounded_channel();
    assert!(matches!(
        planning.build_fresh_backend(&factory, planning_tx).unwrap(),
        Some(Backend::Stub(_))
    ));
    assert_eq!(factory.working_dir_snapshot_for_test(), normal_root);

    coordinator
        .handle(ControlledDevelopmentCommand::RecordRawEvent {
            packet_id: "evidence-card".into(),
            event: RoutedEvent::own(StreamEvent::Info {
                message: "planning detail".into(),
            }),
        })
        .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::PlanningFinished {
            packet_id: "evidence-card".into(),
            final_response: valid_card_json("evidence-card"),
        })
        .unwrap();
    let create_workspace = coordinator
        .handle(ControlledDevelopmentCommand::Approve {
            card_id: "evidence-card".into(),
        })
        .unwrap();
    assert!(matches!(
        create_workspace,
        Some(ControlledDevelopmentEffect::CreateExecutionWorkspace {
            ref card_id,
            ref project_root,
        }) if card_id == "evidence-card" && project_root == &root
    ));

    let pair = DisposableDraftWorkspace::create_current_state_pair(&root).unwrap();
    let execution = coordinator
        .handle(ControlledDevelopmentCommand::ExecutionWorkspaceReady {
            card_id: "evidence-card".into(),
            workspace: Box::new(pair),
        })
        .unwrap()
        .unwrap();
    assert!(coordinator.has_execution_workspace());
    assert_eq!(
        coordinator.execution_workspace_path(),
        match &execution {
            ControlledDevelopmentEffect::DispatchExecution { execution_root, .. } => {
                Some(execution_root.as_path())
            }
            _ => None,
        }
    );
    let (execution_tx, _execution_rx) = mpsc::unbounded_channel();
    assert!(matches!(
        execution
            .build_fresh_backend(&factory, execution_tx)
            .unwrap(),
        Some(Backend::Stub(_))
    ));
    assert_eq!(factory.working_dir_snapshot_for_test(), normal_root);

    coordinator
        .handle(ControlledDevelopmentCommand::RecordChangedPaths {
            card_id: "evidence-card".into(),
            paths: vec!["source.txt".into()],
        })
        .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::RecordProofEvidence {
            card_id: "evidence-card".into(),
            evidence: Box::new(VerifierGateEvidence {
                command: "cargo check --workspace".into(),
                disposition: VerifierGateDisposition::Passed,
                result: None,
            }),
        })
        .unwrap();
    assert_eq!(coordinator.raw_events().len(), 1);
    assert_eq!(coordinator.changed_paths(), ["source.txt"]);
    assert_eq!(coordinator.proof_evidence().len(), 1);

    coordinator
        .handle(ControlledDevelopmentCommand::Complete {
            card_id: "evidence-card".into(),
            summary: "packet completed".into(),
        })
        .unwrap();
    assert!(!coordinator.has_execution_workspace());
    assert_eq!(coordinator.compact_summary(), Some("packet completed"));
    assert_eq!(coordinator.state().approved_card_id(), None);

    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(normal_root).unwrap();
}

fn valid_card_json(card_id: &str) -> String {
    json!({
        "id": card_id,
        "outcome": "The packet produces one observable result.",
        "proof_commands": ["cargo check --workspace"],
        "production_paths": ["crates/deepseek-custom/src/controlled_development/coordinator.rs"],
        "supporting_paths": ["crates/deepseek-custom-tests/tests/it/controlled_development_service.rs"],
        "excluded": ["settings.json"],
        "complexity_exceptions": []
    })
    .to_string()
}
