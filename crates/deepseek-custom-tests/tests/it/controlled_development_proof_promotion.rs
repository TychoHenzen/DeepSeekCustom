use std::path::{Path, PathBuf};

use deepseek_custom::controlled_development::{
    ControlledBackendSelection, ControlledDevelopmentCommand, ControlledDevelopmentCoordinator,
    ControlledDevelopmentEffect, ControlledDevelopmentPhase,
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
            } if card_id == "packet-1" && targets.len() == 1
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
        assert_eq!(promotion.final_fingerprints.len(), 1);
        assert_eq!(promotion.final_fingerprints[0].path, "approved.txt");
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

fn executing_coordinator(
    root: &Path,
    proof_commands: &[String],
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
                "production_paths": ["approved.txt"],
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
