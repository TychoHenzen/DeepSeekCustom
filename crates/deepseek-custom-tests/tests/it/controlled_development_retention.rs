use std::path::{Path, PathBuf};

use deepseek_custom::agent::events::{RoutedEvent, StreamEvent};
use deepseek_custom::controlled_development::{
    ControlledBackendSelection, ControlledDevelopmentCommand, ControlledDevelopmentCoordinator,
    ControlledDevelopmentPhase,
};
use deepseek_custom::procedure::DisposableDraftWorkspace;
use serde_json::json;

// covers: deepseek-custom/controlled-development-mode :: Failed packets retain diagnostic evidence :: Blocked packet remains inspectable
#[test]
fn blocked_packet_retains_diff_until_replacement_or_explicit_discard() {
    let root = fixture_root();
    let mut coordinator = awaiting_coordinator(&root, "retained-packet");
    attach_changed_workspace(&mut coordinator, &root, "retained-packet");
    coordinator
        .handle(ControlledDevelopmentCommand::RecordRawEvent {
            packet_id: "retained-packet".into(),
            event: RoutedEvent::own(StreamEvent::Info {
                message: "complete backend diagnostic".into(),
            }),
        })
        .unwrap();

    let effect = coordinator
        .handle(ControlledDevelopmentCommand::ValidateIsolatedChanges {
            card_id: "retained-packet".into(),
        })
        .unwrap();

    assert!(effect.is_none());
    assert_eq!(
        coordinator.state().phase(),
        ControlledDevelopmentPhase::Blocked
    );
    assert!(!coordinator.has_execution_workspace());
    assert!(coordinator.has_retained_workspace());
    assert_eq!(coordinator.retained_packet_id(), Some("retained-packet"));
    assert_eq!(coordinator.raw_events().len(), 1);
    let first_diff = coordinator.diagnostic_diff().unwrap();
    assert!(first_diff.contains("approved.txt"), "{first_diff}");
    assert!(first_diff.contains("unexpected.txt"), "{first_diff}");
    assert!(
        first_diff.contains("isolated approved bytes"),
        "{first_diff}"
    );
    let (first_baseline, first_execution) = retained_paths(&coordinator);
    assert!(first_baseline.is_dir());
    assert!(first_execution.is_dir());
    assert_eq!(
        std::fs::read_to_string(root.join("approved.txt")).unwrap(),
        "real approved bytes\n"
    );
    assert!(!root.join("unexpected.txt").exists());

    coordinator
        .handle(ControlledDevelopmentCommand::Plan {
            packet_id: "replacement-packet".into(),
            original_request: "Replace the retained packet".into(),
            selection: selection(),
            workspace_root: root.clone(),
        })
        .unwrap();
    assert!(!first_baseline.exists());
    assert!(!first_execution.exists());
    assert!(!coordinator.has_retained_workspace());
    assert!(coordinator.diagnostic_diff().is_none());
    assert!(coordinator.raw_events().is_empty());

    finish_planning(&mut coordinator, "replacement-packet");
    attach_changed_workspace(&mut coordinator, &root, "replacement-packet");
    coordinator
        .handle(ControlledDevelopmentCommand::RecordRawEvent {
            packet_id: "replacement-packet".into(),
            event: RoutedEvent::own(StreamEvent::Info {
                message: "replacement diagnostic".into(),
            }),
        })
        .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::ValidateIsolatedChanges {
            card_id: "replacement-packet".into(),
        })
        .unwrap();
    let (second_baseline, second_execution) = retained_paths(&coordinator);

    coordinator
        .handle(ControlledDevelopmentCommand::DiscardRetainedEvidence)
        .unwrap();

    assert!(!second_baseline.exists());
    assert!(!second_execution.exists());
    assert!(!coordinator.has_retained_workspace());
    assert!(coordinator.diagnostic_diff().is_none());
    assert!(coordinator.raw_events().is_empty());
    assert_eq!(
        std::fs::read_to_string(root.join("approved.txt")).unwrap(),
        "real approved bytes\n"
    );
    assert!(!root.join("unexpected.txt").exists());
    std::fs::remove_dir_all(root).unwrap();
}

fn awaiting_coordinator(root: &Path, packet_id: &str) -> ControlledDevelopmentCoordinator {
    let mut coordinator = ControlledDevelopmentCoordinator::default();
    coordinator
        .handle(ControlledDevelopmentCommand::SetEnabled { enabled: true })
        .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::Plan {
            packet_id: packet_id.into(),
            original_request: "Change one approved path".into(),
            selection: selection(),
            workspace_root: root.to_path_buf(),
        })
        .unwrap();
    finish_planning(&mut coordinator, packet_id);
    coordinator
}

fn finish_planning(coordinator: &mut ControlledDevelopmentCoordinator, packet_id: &str) {
    coordinator
        .handle(ControlledDevelopmentCommand::PlanningFinished {
            packet_id: packet_id.into(),
            final_response: json!({
                "id": packet_id,
                "outcome": "The approved file contains the requested bytes.",
                "proof_commands": ["cargo check --workspace"],
                "production_paths": ["approved.txt"],
                "supporting_paths": [],
                "excluded": ["unexpected.txt"],
                "complexity_exceptions": []
            })
            .to_string(),
        })
        .unwrap();
}

fn attach_changed_workspace(
    coordinator: &mut ControlledDevelopmentCoordinator,
    root: &Path,
    packet_id: &str,
) {
    coordinator
        .handle(ControlledDevelopmentCommand::Approve {
            card_id: packet_id.into(),
        })
        .unwrap();
    let pair = DisposableDraftWorkspace::create_current_state_pair(root).unwrap();
    std::fs::write(
        pair.execution_path().join("approved.txt"),
        "isolated approved bytes\n",
    )
    .unwrap();
    std::fs::write(
        pair.execution_path().join("unexpected.txt"),
        "isolated unexpected bytes\n",
    )
    .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::ExecutionWorkspaceReady {
            card_id: packet_id.into(),
            workspace: Box::new(pair),
        })
        .unwrap();
}

fn retained_paths(coordinator: &ControlledDevelopmentCoordinator) -> (PathBuf, PathBuf) {
    let (baseline, execution) = coordinator.retained_workspace_paths().unwrap();
    (baseline.to_path_buf(), execution.to_path_buf())
}

fn selection() -> ControlledBackendSelection {
    ControlledBackendSelection::new("controlled-stub", Some("model".into()))
}

fn fixture_root() -> PathBuf {
    let root = super::scratch_dir("controlled-development", "retained-evidence");
    std::fs::write(root.join("approved.txt"), "real approved bytes\n").unwrap();
    root
}
