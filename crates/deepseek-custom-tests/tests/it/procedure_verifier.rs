use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use deepseek_custom::procedure::{
    BoundedVerifierOutput, CandidateIneligibility, GitApplyPhase, GitApplyResult,
    VERIFIER_OUTPUT_EDGE_BYTES, VerifierCommandDisposition, VerifierCommandResult,
    VerifierCommandRunner, evaluate_candidate_eligibility,
};

fn temp_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "dsc verifier runner {tag} {}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_order_script(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = root.join("fake verifier.cmd");
        std::fs::write(
            &path,
            "@echo off\r\nif \"%~1\"==\"write\" echo %~1>>\"%~2\"\r\nif \"%~1\"==\"fail\" exit /b 7\r\necho %~1>>\"%~2\"\r\n",
        )
        .unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fake verifier.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\nif [ \"$1\" = \"write\" ]; then printf '%s\\n' \"$1\" >> \"$2\"; fi\nif [ \"$1\" = \"fail\" ]; then exit 7; fi\nprintf '%s\\n' \"$1\" >> \"$2\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn command_line(script: &Path, action: &str, marker: &Path) -> String {
    format!(
        "\"{}\" {} \"{}\"",
        script.display(),
        action,
        marker.display()
    )
}

fn write_descendant_fixture(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = root.join("descendant verifier.cmd");
        std::fs::write(
            &path,
            "@echo off\r\nif \"%~1\"==\"spawn\" (\r\n  start \"\" /b powershell -NoProfile -Command \"Start-Sleep -Seconds 2; Set-Content -LiteralPath '%~3' -Value descendant-ran\"\r\n  echo started>\"%~2\"\r\n  timeout /t 2 /nobreak >nul\r\n)\r\n",
        )
        .unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("descendant verifier.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\nif [ \"$1\" = spawn ]; then (sleep 2; printf descendant-ran > \"$3\") & printf started > \"$2\"; sleep 2; fi\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn run_async(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

fn patch_result(phase: GitApplyPhase, success: bool) -> GitApplyResult {
    GitApplyResult {
        phase,
        success,
        status_code: Some(if success { 0 } else { 1 }),
        stdout: String::new(),
        stderr: String::new(),
    }
}

fn command_result(
    command: &str,
    disposition: VerifierCommandDisposition,
    success: bool,
) -> VerifierCommandResult {
    let output = BoundedVerifierOutput {
        text: String::new(),
        first_edge: String::new(),
        last_edge: String::new(),
        truncated: false,
        bytes_seen: 0,
    };
    VerifierCommandResult {
        command: command.to_string(),
        disposition,
        success,
        exit_code: Some(if success { 0 } else { 1 }),
        stdout: output.clone(),
        stderr: output.clone(),
        combined_output: output,
        duration: std::time::Duration::from_millis(1),
        error: None,
    }
}

#[test]
fn commands_run_in_order_inside_the_verification_workspace() {
    run_async(async {
        let root = temp_dir("ordered");
        let script = write_order_script(&root);
        let marker = root.join("marker file.txt");
        let commands = vec![
            command_line(&script, "first", &marker),
            command_line(&script, "second", &marker),
        ];

        let run = VerifierCommandRunner::new().run(&root, &commands).await;

        assert_eq!(run.commands.len(), 2);
        assert!(run.commands.iter().all(|result| result.success));
        assert!(
            run.commands
                .iter()
                .all(|result| result.exit_code == Some(0))
        );
        assert!(
            run.commands
                .iter()
                .all(|result| result.duration > std::time::Duration::ZERO)
        );
        assert!(!run.stopped_after_failure);
        assert_eq!(
            std::fs::read_to_string(&marker)
                .unwrap()
                .replace("\r\n", "\n"),
            "first\nsecond\n"
        );
        std::fs::remove_dir_all(root).ok();
    });
}

#[test]
fn failed_command_has_exit_evidence_and_later_commands_do_not_run() {
    run_async(async {
        let root = temp_dir("failure-order");
        let script = write_order_script(&root);
        let marker = root.join("marker file.txt");
        let commands = vec![
            command_line(&script, "fail", &marker),
            command_line(&script, "later", &marker),
        ];

        let run = VerifierCommandRunner::new().run(&root, &commands).await;

        assert_eq!(run.commands.len(), 1);
        assert!(run.stopped_after_failure);
        assert_eq!(run.first_failed_gate, Some(0));
        assert_eq!(run.gate_results.len(), 2);
        assert_eq!(
            run.commands[0].disposition,
            VerifierCommandDisposition::Failed
        );
        assert_eq!(
            run.gate_results[1].disposition,
            deepseek_custom::procedure::VerifierGateDisposition::NotRun { blocked_by: 0 }
        );
        assert!(run.gate_results[1].result.is_none());
        assert!(!run.commands[0].success);
        assert_eq!(run.commands[0].exit_code, Some(7));
        assert!(!marker.exists());
        std::fs::remove_dir_all(root).ok();
    });
}

#[test]
fn output_keeps_both_edges_and_reports_truncation() {
    run_async(async {
        let root = temp_dir("bounded-output");
        let command = if cfg!(windows) {
            format!(
                "powershell -NoProfile -Command \"Write-Output ('a' * {}); [Console]::Error.WriteLine('b' * {}); exit 0\"",
                VERIFIER_OUTPUT_EDGE_BYTES, VERIFIER_OUTPUT_EDGE_BYTES
            )
        } else {
            "sh -c \"yes A | head -c 9000; yes B | head -c 9000 >&2\"".to_string()
        };
        let run = VerifierCommandRunner::new().run(&root, &[command]).await;
        let result = &run.commands[0];

        assert!(result.success);
        assert!(
            result.stdout.truncated || result.stderr.truncated || result.combined_output.truncated
        );
        assert!(result.combined_output.text.contains("output truncated"));
        assert!(result.combined_output.text.len() <= VERIFIER_OUTPUT_EDGE_BYTES * 2 + 64);
        assert!(result.combined_output.bytes_seen > (VERIFIER_OUTPUT_EDGE_BYTES * 2) as u64);
        assert!(result.stdout.first_edge.starts_with('a'));
        assert!(result.stdout.last_edge.starts_with('a'));
        assert!(result.stderr.first_edge.starts_with('b'));
        assert!(result.stderr.last_edge.starts_with('b'));
        assert_eq!(
            result.combined_output.first_edge.chars().count(),
            VERIFIER_OUTPUT_EDGE_BYTES
        );
        assert_eq!(
            result.combined_output.last_edge.chars().count(),
            VERIFIER_OUTPUT_EDGE_BYTES
        );
        std::fs::remove_dir_all(root).ok();
    });
}

#[test]
fn missing_command_is_a_typed_failed_result() {
    run_async(async {
        let root = temp_dir("missing-command");
        let run = VerifierCommandRunner::new()
            .run(
                &root,
                &["command-that-does-not-exist-for-verifier".to_string()],
            )
            .await;

        assert_eq!(run.commands.len(), 1);
        assert_eq!(
            run.commands[0].disposition,
            VerifierCommandDisposition::SpawnFailed
        );
        assert!(!run.commands[0].success);
        assert!(run.commands[0].error.is_some());
        std::fs::remove_dir_all(root).ok();
    });
}

#[test]
fn interrupt_before_the_sequence_stays_ineligible_without_spawning() {
    run_async(async {
        let root = temp_dir("pre-interrupt");
        let interrupt = Arc::new(AtomicBool::new(true));
        let run = VerifierCommandRunner::with_interrupt(interrupt)
            .run(&root, &["echo never-runs".to_string()])
            .await;

        assert_eq!(
            run.commands[0].disposition,
            VerifierCommandDisposition::Interrupted
        );
        assert!(!run.commands[0].success);
        std::fs::remove_dir_all(root).ok();
    });
}

#[test]
fn interrupting_an_active_gate_kills_descendants_and_returns_ineligible() {
    run_async(async {
        let root = temp_dir("active interrupt");
        let fixture = write_descendant_fixture(&root);
        let started = root.join("descendant-started.txt");
        let descendant_marker = root.join("descendant-ran.txt");
        let command = if cfg!(windows) {
            command_line(&fixture, "spawn", &started)
                + &format!(" \"{}\"", descendant_marker.display())
        } else {
            format!(
                "sh \"{}\" spawn \"{}\" \"{}\"",
                fixture.display(),
                started.display(),
                descendant_marker.display()
            )
        };
        let interrupt = Arc::new(AtomicBool::new(false));
        let runner = VerifierCommandRunner::with_interrupt(Arc::clone(&interrupt));
        {
            let commands = [command];
            let run_future = runner.run(&root, &commands);
            tokio::pin!(run_future);

            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                tokio::select! {
                    run = &mut run_future => panic!("gate ended before the interrupt fixture started: {run:?}"),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {
                        if started.exists() {
                            break;
                        }
                        assert!(Instant::now() < deadline, "interrupt fixture did not start");
                    }
                }
            }

            interrupt.store(true, std::sync::atomic::Ordering::SeqCst);
            let run = tokio::time::timeout(Duration::from_secs(5), run_future)
                .await
                .expect("an interrupted verifier must finish before the fixture timeout");

            assert_eq!(run.commands.len(), 1);
            assert_eq!(
                run.commands[0].disposition,
                VerifierCommandDisposition::Interrupted
            );
            assert!(!run.commands[0].success);
            assert!(
                !evaluate_candidate_eligibility(
                    &patch_result(GitApplyPhase::Check, true),
                    &patch_result(GitApplyPhase::Apply, true),
                    &run.commands,
                )
                .eligible
            );
        }

        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!descendant_marker.exists());
        std::fs::remove_dir_all(root).ok();
    });
}

#[test]
fn candidate_is_eligible_only_when_patch_and_every_command_succeed() {
    let check = patch_result(GitApplyPhase::Check, true);
    let apply = patch_result(GitApplyPhase::Apply, true);
    let commands = vec![
        command_result("format", VerifierCommandDisposition::Passed, true),
        command_result("test", VerifierCommandDisposition::Passed, true),
    ];

    let outcome = evaluate_candidate_eligibility(&check, &apply, &commands);

    assert!(outcome.eligible);
    assert_eq!(outcome.reason, None);
}

#[test]
fn missing_commands_make_a_candidate_ineligible() {
    let check = patch_result(GitApplyPhase::Check, true);
    let apply = patch_result(GitApplyPhase::Apply, true);

    let outcome = evaluate_candidate_eligibility(&check, &apply, &[]);

    assert_eq!(
        outcome.reason,
        Some(CandidateIneligibility::NoVerifierCommands)
    );
    assert!(!outcome.eligible);
}

#[test]
fn patch_check_and_apply_failures_make_a_candidate_ineligible() {
    let apply = patch_result(GitApplyPhase::Apply, true);
    let commands = [command_result(
        "test",
        VerifierCommandDisposition::Passed,
        true,
    )];
    let check_failure = evaluate_candidate_eligibility(
        &patch_result(GitApplyPhase::Check, false),
        &apply,
        &commands,
    );
    assert_eq!(
        check_failure.reason,
        Some(CandidateIneligibility::PatchCheckFailed)
    );
    assert!(!check_failure.eligible);

    let apply_failure = evaluate_candidate_eligibility(
        &patch_result(GitApplyPhase::Check, true),
        &patch_result(GitApplyPhase::Apply, false),
        &commands,
    );
    assert_eq!(
        apply_failure.reason,
        Some(CandidateIneligibility::PatchApplyFailed)
    );
    assert!(!apply_failure.eligible);
}

#[test]
fn failing_commands_make_a_candidate_ineligible_with_their_reason() {
    let check = patch_result(GitApplyPhase::Check, true);
    let apply = patch_result(GitApplyPhase::Apply, true);
    for (disposition, command) in [
        (VerifierCommandDisposition::Failed, "failed"),
        (VerifierCommandDisposition::SpawnFailed, "missing"),
        (VerifierCommandDisposition::Interrupted, "interrupted"),
    ] {
        let outcome = evaluate_candidate_eligibility(
            &check,
            &apply,
            &[command_result(command, disposition, false)],
        );

        assert_eq!(
            outcome.reason,
            Some(CandidateIneligibility::VerifierCommandFailed {
                index: 0,
                command: command.to_string(),
                disposition,
            })
        );
        assert!(!outcome.eligible);
    }
}
