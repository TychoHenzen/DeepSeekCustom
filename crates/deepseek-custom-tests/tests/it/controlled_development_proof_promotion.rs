use std::path::{Path, PathBuf};

use deepseek_custom::controlled_development::{
    ControlledBackendSelection, ControlledDevelopmentCommand, ControlledDevelopmentCoordinator,
    ControlledDevelopmentEffect, ControlledDevelopmentPhase,
    ControlledDevelopmentProjectStateInput, ControlledDevelopmentSystemMapComponent,
    MAX_PROJECT_STATE_NONBLANK_LINES, PROJECT_STATE_PATH, WorkCard, build_project_state,
};
use deepseek_custom::procedure::{VerifierCommandDisposition, VerifierGateDisposition};
use serde_json::json;

// covers: deepseek-custom/controlled-development-mode :: Every proof command must pass in isolation :: Failing proof command blocks promotion
#[test]
fn failing_proof_records_evidence_stops_later_commands_and_never_promotes() {
    run_async(async {
        let root = fixture_root("proof-failure");
        std::fs::write(root.join("approved.txt"), "real before\n").unwrap();
        write_proof_scripts(&root, true);
        let commands = proof_commands();
        let (mut coordinator, execution_root) = executing_coordinator(&root, &commands);
        std::fs::write(execution_root.join("approved.txt"), "isolated after\n").unwrap();

        let proof_effect = coordinator
            .handle(ControlledDevelopmentCommand::ValidateIsolatedChanges {
                card_id: "packet-1".into(),
            })
            .unwrap()
            .unwrap();
        assert!(matches!(
            proof_effect,
            ControlledDevelopmentEffect::RunProofCommands { ref card_id, .. }
                if card_id == "packet-1"
        ));

        let run = proof_effect.run_proof_commands().await.unwrap();
        assert_eq!(run.commands.len(), 1);
        assert_eq!(
            run.commands[0].disposition,
            VerifierCommandDisposition::Failed
        );
        assert_eq!(run.commands[0].exit_code, Some(7));
        assert!(
            run.commands[0]
                .combined_output
                .text
                .contains(&execution_root.display().to_string())
        );
        assert_eq!(
            std::fs::read_to_string(execution_root.join("proof-order.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "first\n"
        );

        let next = coordinator
            .handle(ControlledDevelopmentCommand::ProofsFinished {
                card_id: "packet-1".into(),
                run: Box::new(run),
            })
            .unwrap();

        assert!(next.is_none());
        assert_eq!(
            coordinator.state().phase(),
            ControlledDevelopmentPhase::Blocked
        );
        assert_eq!(coordinator.state().approved_card_id(), None);
        assert_eq!(coordinator.proof_evidence().len(), 2);
        assert_eq!(
            coordinator.proof_evidence()[0].disposition,
            VerifierGateDisposition::Failed
        );
        assert_eq!(
            coordinator.proof_evidence()[1].disposition,
            VerifierGateDisposition::NotRun { blocked_by: 0 }
        );
        assert_eq!(
            std::fs::read_to_string(root.join("approved.txt")).unwrap(),
            "real before\n"
        );
        assert!(coordinator.promotion_evidence().is_none());
        assert!(coordinator.has_retained_workspace());
        assert!(
            coordinator
                .diagnostic_diff()
                .unwrap()
                .contains("approved.txt")
        );
        coordinator
            .handle(ControlledDevelopmentCommand::DiscardRetainedEvidence)
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    });
}

// covers: deepseek-custom/controlled-development-mode :: Every proof command must pass in isolation :: Passing packet promotes only approved paths
#[test]
fn passing_proofs_promote_only_validated_paths_as_one_transaction() {
    run_async(async {
        let root = fixture_root("proof-promotion");
        std::fs::write(root.join("approved.txt"), "real before\n").unwrap();
        std::fs::write(root.join("unapproved.txt"), "user bytes\n").unwrap();
        write_proof_scripts(&root, false);
        let commands = proof_commands();
        let (mut coordinator, execution_root) = executing_coordinator(&root, &commands);
        std::fs::write(execution_root.join("approved.txt"), "isolated after\n").unwrap();

        let proof_effect = coordinator
            .handle(ControlledDevelopmentCommand::ValidateIsolatedChanges {
                card_id: "packet-1".into(),
            })
            .unwrap()
            .unwrap();
        let run = proof_effect.run_proof_commands().await.unwrap();
        assert!(run.all_commands_succeeded());
        assert_eq!(
            std::fs::read_to_string(execution_root.join("proof-order.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "first\nsecond\n"
        );

        let promotion_effect = coordinator
            .handle(ControlledDevelopmentCommand::ProofsFinished {
                card_id: "packet-1".into(),
                run: Box::new(run),
            })
            .unwrap()
            .unwrap();
        assert!(matches!(
            promotion_effect,
            ControlledDevelopmentEffect::PromoteValidatedChanges {
                ref card_id,
                ref targets,
                ..
            } if card_id == "packet-1" && targets.len() == 2
        ));
        let result = promotion_effect.promote_validated_changes().unwrap();
        coordinator
            .handle(ControlledDevelopmentCommand::PromotionFinished {
                card_id: "packet-1".into(),
                result: result.map(Box::new).map_err(Box::new),
            })
            .unwrap();

        assert_eq!(
            coordinator.state().phase(),
            ControlledDevelopmentPhase::Completed
        );
        assert_eq!(coordinator.state().approved_card_id(), None);
        assert_eq!(coordinator.proof_evidence().len(), 2);
        assert!(coordinator.proof_evidence().iter().all(|evidence| {
            evidence.disposition == VerifierGateDisposition::Passed
                && evidence
                    .result
                    .as_ref()
                    .is_some_and(|result| result.success)
        }));
        let promotion = coordinator.promotion_evidence().unwrap();
        assert_eq!(promotion.final_fingerprints.len(), 2);
        assert!(
            promotion
                .final_fingerprints
                .iter()
                .any(|fingerprint| fingerprint.path == "approved.txt")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("approved.txt")).unwrap(),
            "isolated after\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("unapproved.txt")).unwrap(),
            "user bytes\n"
        );
        assert!(!root.join("proof-order.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    });
}

// covers: deepseek-custom/controlled-development-mode :: Successful promotion updates the project snapshot :: PROJECT_STATE remains within 40 nonblank lines
#[test]
fn project_state_builder_is_bounded_and_promotion_installs_it_in_the_packet_transaction() {
    let preview = build_project_state(&ControlledDevelopmentProjectStateInput {
        outcome: "One bounded outcome".into(),
        system_map: (0..12)
            .map(|index| ControlledDevelopmentSystemMapComponent {
                name: format!("component-{index}"),
                responsibility: "production".into(),
            })
            .collect(),
        work_card: WorkCard {
            id: "preview-card".into(),
            outcome: "One bounded outcome".into(),
            proof_commands: vec!["cargo check --workspace".into()],
            production_paths: vec!["approved.txt".into()],
            supporting_paths: Vec::new(),
            excluded: vec!["settings.json".into()],
            complexity_exceptions: Vec::new(),
        },
        changed_paths: vec!["approved.txt".into()],
        proof_evidence: Vec::new(),
        blocker: Some("a blocker with\nembedded whitespace".into()),
    });
    assert_eq!(
        preview
            .lines()
            .filter(|line| line.starts_with("- production: component-"))
            .count(),
        10
    );
    assert!(
        preview
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
            <= MAX_PROJECT_STATE_NONBLANK_LINES
    );

    run_async(async {
        let root = fixture_root("project-state-promotion");
        std::fs::write(root.join("approved.txt"), "real before\n").unwrap();
        std::fs::write(root.join(PROJECT_STATE_PATH), "stale snapshot\n").unwrap();
        write_proof_scripts(&root, false);
        let commands = proof_commands();
        let (mut coordinator, execution_root) = executing_coordinator(&root, &commands);
        std::fs::write(execution_root.join("approved.txt"), "isolated after\n").unwrap();

        let proof_effect = coordinator
            .handle(ControlledDevelopmentCommand::ValidateIsolatedChanges {
                card_id: "packet-1".into(),
            })
            .unwrap()
            .unwrap();
        let run = proof_effect.run_proof_commands().await.unwrap();
        let promotion_effect = coordinator
            .handle(ControlledDevelopmentCommand::ProofsFinished {
                card_id: "packet-1".into(),
                run: Box::new(run),
            })
            .unwrap()
            .unwrap();
        assert!(matches!(
            &promotion_effect,
            ControlledDevelopmentEffect::PromoteValidatedChanges { targets, .. }
                if targets.len() == 2
                    && targets.iter().flat_map(|target| target.paths()).any(|path| path == PROJECT_STATE_PATH)
        ));

        let result = promotion_effect.promote_validated_changes().unwrap();
        coordinator
            .handle(ControlledDevelopmentCommand::PromotionFinished {
                card_id: "packet-1".into(),
                result: result.map(Box::new).map_err(Box::new),
            })
            .unwrap();

        let installed = std::fs::read_to_string(root.join(PROJECT_STATE_PATH)).unwrap();
        let allowed_headings = [
            "## Current outcome",
            "## System map",
            "## Last completed Work Card",
            "## Exact changed paths",
            "## Last proof commands and results",
            "## Known blocker",
        ];
        assert!(
            installed
                .lines()
                .filter(|line| line.starts_with("## "))
                .all(|line| allowed_headings.contains(&line))
        );
        assert!(
            installed
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
                <= MAX_PROJECT_STATE_NONBLANK_LINES
        );
        assert!(installed.contains("\"PROJECT_STATE.md\""));
        assert!(installed.contains("\"approved.txt\""));
        assert!(!installed.contains("## Known blocker"));
        assert_eq!(
            std::fs::read_to_string(root.join("approved.txt")).unwrap(),
            "isolated after\n"
        );
        assert_eq!(
            coordinator.state().phase(),
            ControlledDevelopmentPhase::Completed
        );
        assert!(
            coordinator
                .handle_control_input(
                    deepseek_custom::controlled_development::ControlledDevelopmentControlInput::Diff
                )
                .unwrap()
                .contains("approved.txt")
        );
        std::fs::remove_dir_all(root).unwrap();
    });
}

// covers: deepseek-custom/controlled-development-mode :: Promotion rejects overlapping concurrent edits :: Overlapping real-workspace changes block promotion without data loss
#[test]
fn overlapping_real_workspace_change_blocks_every_packet_target_without_data_loss() {
    run_async(async {
        let root = fixture_root("promotion-overlap");
        std::fs::write(root.join("first.txt"), "first real before\n").unwrap();
        std::fs::write(root.join("second.txt"), "second real before\n").unwrap();
        std::fs::write(root.join("unrelated.txt"), "unrelated before\n").unwrap();
        write_proof_scripts(&root, false);
        let commands = proof_commands();
        let (mut coordinator, execution_root) =
            executing_coordinator_with_paths(&root, &commands, &["first.txt", "second.txt"]);
        std::fs::write(execution_root.join("first.txt"), "first isolated after\n").unwrap();
        std::fs::write(execution_root.join("second.txt"), "second isolated after\n").unwrap();

        let proof_effect = coordinator
            .handle(ControlledDevelopmentCommand::ValidateIsolatedChanges {
                card_id: "packet-1".into(),
            })
            .unwrap()
            .unwrap();
        let run = proof_effect.run_proof_commands().await.unwrap();
        assert!(run.all_commands_succeeded());
        let promotion_effect = coordinator
            .handle(ControlledDevelopmentCommand::ProofsFinished {
                card_id: "packet-1".into(),
                run: Box::new(run),
            })
            .unwrap()
            .unwrap();
        assert!(matches!(
            promotion_effect,
            ControlledDevelopmentEffect::PromoteValidatedChanges { ref targets, .. }
                if targets.len() == 3
        ));

        std::fs::write(root.join("first.txt"), "concurrent user bytes\n").unwrap();
        std::fs::write(root.join("unrelated.txt"), "unrelated concurrent bytes\n").unwrap();
        let result = promotion_effect.promote_validated_changes().unwrap();
        assert!(result.is_err());
        coordinator
            .handle(ControlledDevelopmentCommand::PromotionFinished {
                card_id: "packet-1".into(),
                result: result.map(Box::new).map_err(Box::new),
            })
            .unwrap();

        assert_eq!(
            coordinator.state().phase(),
            ControlledDevelopmentPhase::Blocked
        );
        assert_eq!(
            std::fs::read_to_string(root.join("first.txt")).unwrap(),
            "concurrent user bytes\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("second.txt")).unwrap(),
            "second real before\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("unrelated.txt")).unwrap(),
            "unrelated concurrent bytes\n"
        );
        assert!(coordinator.has_retained_workspace());
        coordinator
            .handle(ControlledDevelopmentCommand::DiscardRetainedEvidence)
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    });
}

fn executing_coordinator(
    root: &Path,
    proof_commands: &[String],
) -> (ControlledDevelopmentCoordinator, PathBuf) {
    executing_coordinator_with_paths(root, proof_commands, &["approved.txt"])
}

fn executing_coordinator_with_paths(
    root: &Path,
    proof_commands: &[String],
    production_paths: &[&str],
) -> (ControlledDevelopmentCoordinator, PathBuf) {
    let mut coordinator = ControlledDevelopmentCoordinator::default();
    coordinator
        .handle(ControlledDevelopmentCommand::SetEnabled { enabled: true })
        .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::Plan {
            packet_id: "packet-1".into(),
            original_request: "Change the approved file".into(),
            selection: ControlledBackendSelection::new("controlled-stub", Some("model".into())),
            workspace_root: root.to_path_buf(),
        })
        .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::PlanningFinished {
            packet_id: "packet-1".into(),
            final_response: json!({
                "id": "packet-1",
                "outcome": "The approved file contains the requested bytes.",
                "proof_commands": proof_commands,
                "production_paths": production_paths,
                "supporting_paths": proof_script_paths(),
                "excluded": ["unapproved.txt"],
                "complexity_exceptions": []
            })
            .to_string(),
        })
        .unwrap();
    coordinator
        .handle(ControlledDevelopmentCommand::Approve {
            card_id: "packet-1".into(),
        })
        .unwrap();
    let pair =
        deepseek_custom::procedure::DisposableDraftWorkspace::create_current_state_pair(root)
            .unwrap();
    let execution_root = pair.execution_path().to_path_buf();
    coordinator
        .handle(ControlledDevelopmentCommand::ExecutionWorkspaceReady {
            card_id: "packet-1".into(),
            workspace: Box::new(pair),
        })
        .unwrap();
    (coordinator, execution_root)
}

fn fixture_root(tag: &str) -> PathBuf {
    let root = super::scratch_dir("controlled-development-proof", tag);
    std::fs::create_dir_all(root.join("tests")).unwrap();
    root
}

fn proof_script_paths() -> Vec<String> {
    if cfg!(windows) {
        vec![
            "tests/proof-first.ps1".into(),
            "tests/proof-second.ps1".into(),
        ]
    } else {
        vec![
            "tests/proof-first.sh".into(),
            "tests/proof-second.sh".into(),
        ]
    }
}

fn proof_commands() -> Vec<String> {
    let first = if cfg!(windows) {
        "powershell -NoProfile -File tests/proof-first.ps1"
    } else {
        "sh tests/proof-first.sh"
    };
    let second = if cfg!(windows) {
        "powershell -NoProfile -File tests/proof-second.ps1"
    } else {
        "sh tests/proof-second.sh"
    };
    vec![first.into(), second.into()]
}

fn write_proof_scripts(root: &Path, first_fails: bool) {
    if cfg!(windows) {
        let exit = if first_fails { "exit 7\r\n" } else { "" };
        std::fs::write(
            root.join("tests/proof-first.ps1"),
            format!(
                "Write-Output (Get-Location).Path\r\nSet-Content -LiteralPath proof-order.txt -Value 'first'\r\n{exit}"
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("tests/proof-second.ps1"),
            "Add-Content -LiteralPath proof-order.txt -Value 'second'\r\n",
        )
        .unwrap();
    } else {
        let exit = if first_fails { "exit 7\n" } else { "" };
        std::fs::write(
            root.join("tests/proof-first.sh"),
            format!("#!/bin/sh\npwd\nprintf 'first\\n' > proof-order.txt\n{exit}"),
        )
        .unwrap();
        std::fs::write(
            root.join("tests/proof-second.sh"),
            "#!/bin/sh\nprintf 'second\\n' >> proof-order.txt\n",
        )
        .unwrap();
    }
}

fn run_async(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}
