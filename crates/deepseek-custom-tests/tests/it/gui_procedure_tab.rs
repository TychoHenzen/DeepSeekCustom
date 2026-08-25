use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::events::{AgentCommand, StreamEvent};
use deepseek_custom::config::settings::{
    ApiProvider, BackendConfig, ProcedureSettings, RepositoryIndexLimits, Settings,
};
use deepseek_custom::gui::DeepSeekGui;
use deepseek_custom::gui::agent_handles::AgentHandles;
use deepseek_custom::gui::procedure_tab::{
    PatchPreviewStatus, ProcedureApplyStatus, ProcedureStatus, ProcedureTab, ProcedureViewState,
};
use deepseek_custom::procedure::{
    BoundedVerifierOutput, CandidateEligibility, CandidateIneligibility, GitApplyPhase,
    GitApplyResult, LocalizationAttempt, LocalizationTarget, MechanicalVerb, OpenSpecValidation,
    PatchPreview, PatchPreviewStore, ProcedureApplyProgress, ProcedureAttemptDisposition,
    ProcedureCommand, ProcedureProgress, ProcedureReportStore, ProcedureReviewDecision,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition, PromotionBaselineComparison,
    PromotionRecoveryEvidence, PromotionResult, RouteDecision, RouteOverride, RouteSignal,
    RouteTier, StalePromotionPath, VerifierCommandDisposition, VerifierCommandEvidence,
    VerifierGateDisposition, VerifierGateEvidence, VerifierReport, apply_review_decision,
    capture_path_fingerprint,
};
use tokio::sync::mpsc;

const PROCEDURE_VISUAL_VERIFICATION_MANIFEST: &[(&str, &str)] = &[
    (
        "maintained GUI checklist",
        "docs/procedure-localization-verification.md",
    ),
    (
        "running screenshot",
        "docs/evidence/procedure-localization/running.png",
    ),
    (
        "awaiting-review screenshot",
        "docs/evidence/procedure-localization/awaiting-review.png",
    ),
    (
        "approved screenshot",
        "docs/evidence/procedure-localization/approved.png",
    ),
    (
        "rejected screenshot",
        "docs/evidence/procedure-localization/rejected.png",
    ),
    (
        "failed screenshot",
        "docs/evidence/procedure-localization/failed.png",
    ),
    (
        "interrupted screenshot",
        "docs/evidence/procedure-localization/interrupted.png",
    ),
];

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "dsc-gui-procedure-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_change(root: &Path, id: &str, tasks: &str) {
    let change = root.join("openspec/changes").join(id);
    std::fs::create_dir_all(&change).unwrap();
    std::fs::write(change.join("tasks.md"), tasks).unwrap();
}

fn settings() -> Settings {
    let mut backends = HashMap::new();
    backends.insert(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".to_string(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    backends.insert(
        "ollama-b".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen-b".to_string(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    backends.insert(
        "ollama-a".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen-a".to_string(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    backends.insert(
        "claude".to_string(),
        BackendConfig::ClaudeCli {
            model: "opus".to_string(),
            permission_mode: None,
            env: None,
            models: None,
        },
    );
    backends.insert(
        "codex".to_string(),
        BackendConfig::CodexCli {
            model: "gpt-5.6".to_string(),
            sandbox: None,
            env: None,
            models: Some(vec!["gpt-5.6".to_string(), "gpt-5.5".to_string()]),
        },
    );
    Settings {
        backends: Some(backends),
        procedure: Some(ProcedureSettings {
            localization_backend: Some("deepseek".to_string()),
            local_patch_backend: Some("ollama-b".to_string()),
            frontier_patch_backend: Some("claude".to_string()),
            repository_index: RepositoryIndexLimits::default(),
            verifier_commands: Vec::new(),
        }),
        ..Settings::default()
    }
}

fn fixture_root(tag: &str) -> PathBuf {
    let root = temp_dir(tag);
    write_change(
        &root,
        "a-change",
        "## Tasks\n\n- [x] 1.0 Finished\n- [ ] 1.1 First pending\n- [ ] 1.2 Second pending\n",
    );
    write_change(&root, "empty-change", "## Tasks\n\n- [x] 1.0 Finished\n");
    root
}

fn attach_tab(tab: &mut ProcedureTab) -> mpsc::UnboundedReceiver<ProcedureCommand> {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (_progress_tx, progress_rx) = mpsc::unbounded_channel();
    tab.attach(command_tx, progress_rx, Arc::new(AtomicBool::new(false)));
    command_rx
}

fn apply_progress(
    tab: &mut ProcedureTab,
    run_id: ProcedureRunId,
    progress: ProcedureApplyProgress,
) {
    tab.handle_progress(ProcedureProgress::Apply {
        run_id,
        progress: Box::new(progress),
    });
}

fn bounded_output(text: &str) -> BoundedVerifierOutput {
    BoundedVerifierOutput {
        text: text.to_string(),
        first_edge: text.to_string(),
        last_edge: text.to_string(),
        truncated: false,
        bytes_seen: text.len() as u64,
    }
}

fn verifier_command_evidence(
    command: &str,
    disposition: VerifierCommandDisposition,
    output: &str,
) -> VerifierCommandEvidence {
    let success = disposition == VerifierCommandDisposition::Passed;
    let bounded = bounded_output(output);
    VerifierCommandEvidence {
        command: command.to_string(),
        disposition,
        success,
        exit_code: Some(if success { 0 } else { 1 }),
        stdout: bounded.clone(),
        stderr: bounded.clone(),
        combined_output: bounded,
        duration_millis: 12,
        error: (!success).then(|| "command failed".to_string()),
    }
}

fn verifier_gate(
    command: &str,
    gate_disposition: VerifierGateDisposition,
    command_disposition: Option<VerifierCommandDisposition>,
    output: &str,
) -> VerifierGateEvidence {
    VerifierGateEvidence {
        command: command.to_string(),
        disposition: gate_disposition,
        result: command_disposition
            .map(|disposition| verifier_command_evidence(command, disposition, output)),
    }
}

fn git_apply_result(phase: GitApplyPhase, success: bool, output: &str) -> GitApplyResult {
    GitApplyResult {
        phase,
        success,
        status_code: Some(if success { 0 } else { 1 }),
        stdout: output.to_string(),
        stderr: if success {
            String::new()
        } else {
            "patch gate failed".to_string()
        },
    }
}

fn finish_review_command(
    root: &Path,
    tab: &mut ProcedureTab,
    command_rx: &mut mpsc::UnboundedReceiver<ProcedureCommand>,
) -> (ProcedureRunId, ProcedureReviewDecision) {
    let ProcedureCommand::Review { run_id, decision } = command_rx.try_recv().unwrap() else {
        panic!("review control must send a review command")
    };
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    apply_review_decision(
        &ProcedureReportStore::for_project(root),
        run_id,
        decision,
        &progress_tx,
    );
    tab.handle_progress(progress_rx.try_recv().unwrap());
    (run_id, decision)
}

// covers: deepseek-custom/procedure-localization :: Localization is observable and non-mutating :: Procedure review states are visually inspectable
#[test]
fn procedure_visual_verification_manifest_requires_every_state_artifact() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("external test crate must remain under the repository's crates directory")
        .canonicalize()
        .expect("repository root must be readable");
    let mut problems = Vec::new();

    for (description, relative) in PROCEDURE_VISUAL_VERIFICATION_MANIFEST {
        let relative_path = Path::new(relative);
        let escapes_repository = relative_path.is_absolute()
            || relative_path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
        if escapes_repository {
            problems.push(format!(
                "{description}: path must stay inside the repository: {relative}"
            ));
            continue;
        }

        let candidate = repository.join(relative_path);
        match candidate.canonicalize() {
            Ok(actual) if !actual.starts_with(&repository) => problems.push(format!(
                "{description}: resolved path escapes the repository: {relative}"
            )),
            Ok(actual) if !actual.is_file() => {
                problems.push(format!("{description}: is not a file: {relative}"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                problems.push(format!("{description}: missing {relative}"));
            }
            Err(error) => problems.push(format!(
                "{description}: could not inspect {relative}: {error}"
            )),
        }
    }

    assert!(
        problems.is_empty(),
        "procedure visual verification manifest is incomplete:\n- {}",
        problems.join("\n- ")
    );
}

fn gui_with_procedure(
    root: PathBuf,
) -> (
    DeepSeekGui,
    mpsc::UnboundedReceiver<AgentCommand>,
    mpsc::UnboundedReceiver<deepseek_custom::procedure::ProcedureCommand>,
    mpsc::UnboundedSender<ProcedureProgress>,
    Arc<AtomicBool>,
) {
    let (_event_tx, event_rx) = mpsc::unbounded_channel();
    let (agent_tx, agent_rx) = mpsc::unbounded_channel();
    let (procedure_tx, procedure_rx) = mpsc::unbounded_channel();
    let (progress_tx, progress_rx) = mpsc::unbounded_channel();
    let interrupt = Arc::new(AtomicBool::new(false));
    let gui = DeepSeekGui::new(
        event_rx,
        agent_tx,
        AgentHandles {
            interrupt: Arc::new(AtomicBool::new(false)),
            effort: Arc::new(AtomicU8::new(0)),
            voice_mode: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(100_000)),
            model: Arc::new(Mutex::new("deepseek-v4-flash".to_string())),
            working_dir: Arc::new(Mutex::new(root.clone())),
            cascade_total: Arc::new(AtomicUsize::new(0)),
            cascade_escalated: Arc::new(AtomicUsize::new(0)),
            style_plain_language: Arc::new(AtomicBool::new(false)),
            style_target_grade: Arc::new(AtomicU8::new(8)),
        },
        settings(),
        root,
    )
    .with_procedure(procedure_tx, progress_rx, Arc::clone(&interrupt));
    (gui, agent_rx, procedure_rx, progress_tx, interrupt)
}

#[test]
fn tab_lists_pending_tasks_and_only_ollama_backends() {
    let root = fixture_root("selection");

    let tab = ProcedureTab::new(&settings(), &root);

    assert_eq!(tab.backend_names(), ["ollama-a", "ollama-b"]);
    assert_eq!(tab.local_backend_names(), ["ollama-a", "ollama-b"]);
    assert_eq!(tab.frontier_backend_names(), ["claude", "codex"]);
    assert_eq!(tab.changes().len(), 1);
    assert_eq!(tab.selected_change(), "a-change");
    assert_eq!(tab.selected_task(), "1.1");
    assert_eq!(
        tab.changes()[0]
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["1.1", "1.2"]
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn preview_model_choices_keep_every_discovered_model_for_each_selected_backend() {
    let root = fixture_root("preview-model-choices");
    let mut tab = ProcedureTab::new(&settings(), &root);

    tab.send_model_list_for_test(
        "ollama-b",
        vec![
            "qwen-b".to_string(),
            "codestral-local".to_string(),
            "small-general".to_string(),
        ],
    );
    tab.send_model_list_for_test(
        "claude",
        vec![
            "opus".to_string(),
            "sonnet".to_string(),
            "haiku".to_string(),
        ],
    );
    tab.drain_progress();

    assert_eq!(
        tab.local_model_options(),
        ["qwen-b", "codestral-local", "small-general"]
    );
    assert_eq!(tab.frontier_model_options(), ["opus", "sonnet", "haiku"]);
    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/routed-patch-preview :: Preview exposes the route and does not edit :: User inspects a preview
#[test]
fn preview_action_sends_selected_route_and_exposes_complete_evidence() {
    let root = fixture_root("patch-preview-view");
    let store = ProcedureReportStore::for_project(&root);
    let run_id = ProcedureRunId::new();
    store.save(&completed_run(run_id)).unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::Succeeded,
    });
    let mut command_rx = attach_tab(&mut tab);
    tab.set_route_override_for_test(RouteOverride::ForceFrontier);
    tab.start_preview_for_test();

    let ProcedureCommand::Preview {
        preview_id,
        request,
    } = command_rx.try_recv().unwrap()
    else {
        panic!("Preview must send a patch-preview command")
    };
    assert_eq!(request.localization_run_id, run_id);
    assert_eq!(request.change_id, "a-change");
    assert_eq!(request.task_id, "1.1");
    assert_eq!(request.route_override, RouteOverride::ForceFrontier);
    assert_eq!(request.local_backend, "ollama-b");
    assert_eq!(request.local_model, "qwen-b");
    assert_eq!(request.frontier_backend, "claude");
    assert_eq!(request.frontier_model, "opus");
    assert_eq!(tab.preview_status(), &PatchPreviewStatus::Running);

    let complete_diff = "diff --git a/src/procedure.rs b/src/procedure.rs\n--- a/src/procedure.rs\n+++ b/src/procedure.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let preview = PatchPreview {
        id: preview_id,
        localization_run_id: run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
        route: RouteDecision {
            automatic_tier: RouteTier::Local,
            effective_tier: RouteTier::Frontier,
            signals: vec![
                RouteSignal::MechanicalVerb(MechanicalVerb::Rename),
                RouteSignal::TargetCount(1),
            ],
            selected_override: RouteOverride::ForceFrontier,
            overridden: true,
        },
        backend: "claude".to_string(),
        model: "opus".to_string(),
        targets: vec!["src/procedure.rs".to_string()],
        rationale: "Rename the approved localized symbol.".to_string(),
        unified_diff: complete_diff.to_string(),
    };
    let report_path = PatchPreviewStore::for_project(&root)
        .save(&preview)
        .unwrap();
    let persisted: PatchPreview =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(persisted, preview);
    tab.handle_progress(ProcedureProgress::PreviewStarted { preview_id });
    tab.handle_progress(ProcedureProgress::PreviewFinished {
        preview_id,
        preview: Box::new(preview.clone()),
        report_path: report_path.clone(),
    });

    assert_eq!(tab.preview_status(), &PatchPreviewStatus::Finished);
    assert_eq!(tab.latest_preview(), Some(&preview));
    assert_eq!(tab.latest_preview().unwrap().unified_diff, complete_diff);
    assert_eq!(tab.latest_preview().unwrap().route.signals.len(), 2);
    assert_eq!(
        tab.latest_preview_report_path(),
        Some(report_path.as_path())
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn apply_is_disabled_without_verifier_commands_and_explains_missing_configuration() {
    let root = fixture_root("apply-missing-verifiers");
    let run_id = ProcedureRunId::new();
    ProcedureReportStore::for_project(&root)
        .save(&completed_run(run_id))
        .unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::Succeeded,
    });
    let mut command_rx = attach_tab(&mut tab);
    tab.start_preview_for_test();
    let ProcedureCommand::Preview { preview_id, .. } = command_rx.try_recv().unwrap() else {
        panic!("Preview must send a patch-preview command")
    };
    let preview = PatchPreview {
        id: preview_id,
        localization_run_id: run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
        route: RouteDecision {
            automatic_tier: RouteTier::Local,
            effective_tier: RouteTier::Local,
            signals: vec![RouteSignal::TargetCount(1)],
            selected_override: RouteOverride::Automatic,
            overridden: false,
        },
        backend: "ollama-b".to_string(),
        model: "qwen-b".to_string(),
        targets: vec!["src/procedure.rs".to_string()],
        rationale: "Keep the approved localized target.".to_string(),
        unified_diff: String::new(),
    };
    tab.handle_progress(ProcedureProgress::PreviewFinished {
        preview_id,
        preview: Box::new(preview),
        report_path: root.join("preview.json"),
    });

    let settings = settings();
    assert!(!tab.apply_enabled_for_test(&settings));
    assert_eq!(
        tab.apply_missing_configuration_for_test(&settings),
        Some("Apply unavailable: configure at least one command in procedure.verifier_commands.")
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn apply_is_enabled_and_lists_verifier_commands_in_execution_order() {
    let root = fixture_root("apply-verifier-list");
    let mut settings = settings();
    settings.procedure_mut().verifier_commands = vec![
        "cargo fmt --all -- --check".to_string(),
        "cargo check --workspace".to_string(),
        "cargo clippy --workspace -- -D warnings".to_string(),
        "cargo test --workspace -- --test-threads=1".to_string(),
    ];
    let run_id = ProcedureRunId::new();
    ProcedureReportStore::for_project(&root)
        .save(&completed_run(run_id))
        .unwrap();
    let mut tab = ProcedureTab::new(&settings, &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::Succeeded,
    });
    let mut command_rx = attach_tab(&mut tab);
    tab.start_preview_for_test();
    let ProcedureCommand::Preview { preview_id, .. } = command_rx.try_recv().unwrap() else {
        panic!("Preview must send a patch-preview command")
    };
    tab.handle_progress(ProcedureProgress::PreviewFinished {
        preview_id,
        preview: Box::new(PatchPreview {
            id: preview_id,
            localization_run_id: run_id,
            change_id: "a-change".to_string(),
            task_id: "1.1".to_string(),
            route: RouteDecision {
                automatic_tier: RouteTier::Local,
                effective_tier: RouteTier::Local,
                signals: vec![RouteSignal::TargetCount(1)],
                selected_override: RouteOverride::Automatic,
                overridden: false,
            },
            backend: "ollama-b".to_string(),
            model: "qwen-b".to_string(),
            targets: vec!["src/procedure.rs".to_string()],
            rationale: "Keep the approved localized target.".to_string(),
            unified_diff: String::new(),
        }),
        report_path: root.join("preview.json"),
    });

    assert!(tab.apply_enabled_for_test(&settings));
    assert_eq!(
        tab.verifier_command_labels_for_test(&settings),
        vec![
            "1. cargo fmt --all -- --check",
            "2. cargo check --workspace",
            "3. cargo clippy --workspace -- -D warnings",
            "4. cargo test --workspace -- --test-threads=1",
        ]
    );
    assert_eq!(tab.apply_missing_configuration_for_test(&settings), None);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn apply_view_renders_snapshot_patch_gates_and_each_command_output() {
    let root = fixture_root("apply-progress");
    let mut tab = ProcedureTab::new(&settings(), &root);
    let run_id = ProcedureRunId::new();

    apply_progress(&mut tab, run_id, ProcedureApplyProgress::Started);
    assert_eq!(tab.apply_status(), &ProcedureApplyStatus::Snapshotting);
    apply_progress(&mut tab, run_id, ProcedureApplyProgress::SnapshotStarted);
    apply_progress(
        &mut tab,
        run_id,
        ProcedureApplyProgress::SnapshotProgress {
            progress: deepseek_custom::procedure::SnapshotProgress {
                files_copied: 3,
                bytes_copied: 128,
                total_bytes: 256,
            },
        },
    );
    apply_progress(
        &mut tab,
        run_id,
        ProcedureApplyProgress::PatchGateStarted {
            phase: GitApplyPhase::Check,
        },
    );
    apply_progress(
        &mut tab,
        run_id,
        ProcedureApplyProgress::PatchGateCompleted {
            result: git_apply_result(GitApplyPhase::Check, true, "patch check ok"),
        },
    );
    apply_progress(
        &mut tab,
        run_id,
        ProcedureApplyProgress::PatchGateStarted {
            phase: GitApplyPhase::Apply,
        },
    );
    apply_progress(
        &mut tab,
        run_id,
        ProcedureApplyProgress::PatchGateCompleted {
            result: git_apply_result(GitApplyPhase::Apply, true, "patch applied"),
        },
    );

    let gates = vec![
        verifier_gate(
            "cargo fmt --all -- --check",
            VerifierGateDisposition::Passed,
            Some(VerifierCommandDisposition::Passed),
            "format ok",
        ),
        verifier_gate(
            "cargo check --workspace",
            VerifierGateDisposition::Passed,
            Some(VerifierCommandDisposition::Passed),
            "compile ok",
        ),
        verifier_gate(
            "cargo clippy --workspace -- -D warnings",
            VerifierGateDisposition::Failed,
            Some(VerifierCommandDisposition::Failed),
            "lint diagnostic",
        ),
        verifier_gate(
            "cargo test --workspace -- --test-threads=1",
            VerifierGateDisposition::NotRun { blocked_by: 2 },
            None,
            "",
        ),
    ];
    for (index, gate) in gates.iter().enumerate() {
        apply_progress(
            &mut tab,
            run_id,
            ProcedureApplyProgress::VerifierGateStarted {
                index,
                command: gate.command.clone(),
            },
        );
        apply_progress(
            &mut tab,
            run_id,
            ProcedureApplyProgress::VerifierGateCompleted {
                index,
                evidence: Box::new(gate.clone()),
            },
        );
    }
    apply_progress(
        &mut tab,
        run_id,
        ProcedureApplyProgress::VerificationFinished {
            report: VerifierReport {
                gates,
                stopped_after_failure: true,
                first_failed_gate: Some(2),
                eligibility: CandidateEligibility::ineligible(
                    CandidateIneligibility::VerifierCommandFailed {
                        index: 2,
                        command: "cargo clippy --workspace -- -D warnings".to_string(),
                        disposition: VerifierCommandDisposition::Failed,
                    },
                ),
            },
        },
    );

    let lines = tab.apply_render_lines_for_test();
    for expected in [
        "Snapshotting verification workspace: 3 file(s), 128 / 256 bytes",
        "Patch gate git apply --check: passed",
        "Patch gate git apply: passed",
        "Verifier gate 1: cargo fmt --all -- --check (passed)",
        "Verifier gate 2: cargo check --workspace (passed)",
        "Verifier gate 3: cargo clippy --workspace -- -D warnings (failed)",
        "Verifier gate 4: cargo test --workspace -- --test-threads=1 (not run)",
        "Command output: lint diagnostic",
        "Verification failure evidence:",
        "Verification stopped after gate Some(2)",
    ] {
        assert!(
            lines.iter().any(|line| line.contains(expected)),
            "missing `{expected}` in {lines:?}"
        );
    }

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn apply_view_renders_stale_conflict_and_interruption_terminal_state() {
    let root = fixture_root("apply-conflict");
    let mut tab = ProcedureTab::new(&settings(), &root);
    let run_id = ProcedureRunId::new();
    let expected = capture_path_fingerprint(&root, "src/stale.rs").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/stale.rs"), "changed during verification\n").unwrap();
    let actual = capture_path_fingerprint(&root, "src/stale.rs").unwrap();

    apply_progress(&mut tab, run_id, ProcedureApplyProgress::Started);
    apply_progress(
        &mut tab,
        run_id,
        ProcedureApplyProgress::ConflictDetected {
            paths: vec![StalePromotionPath {
                path: "src/stale.rs".to_string(),
                expected,
                actual,
            }],
        },
    );
    assert_eq!(tab.apply_status(), &ProcedureApplyStatus::Conflict);
    let conflict_lines = tab.apply_render_lines_for_test();
    assert!(
        conflict_lines
            .iter()
            .any(|line| line.contains("Stale conflict: src/stale.rs"))
    );

    apply_progress(
        &mut tab,
        run_id,
        ProcedureApplyProgress::Finished {
            disposition: ProcedureTerminalDisposition::Failed {
                reason: "stale baseline".to_string(),
            },
        },
    );
    assert_eq!(tab.apply_status(), &ProcedureApplyStatus::Failed);

    let interrupted_id = ProcedureRunId::new();
    apply_progress(&mut tab, interrupted_id, ProcedureApplyProgress::Started);
    apply_progress(
        &mut tab,
        interrupted_id,
        ProcedureApplyProgress::Finished {
            disposition: ProcedureTerminalDisposition::Interrupted,
        },
    );
    assert_eq!(tab.apply_status(), &ProcedureApplyStatus::Interrupted);
    assert!(
        tab.apply_render_lines_for_test()
            .iter()
            .any(|line| line.contains("Apply terminal disposition: interrupted"))
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn apply_view_renders_promotion_success_and_failure_recovery_evidence() {
    let root = fixture_root("apply-promotion");
    let mut tab = ProcedureTab::new(&settings(), &root);
    let success_id = ProcedureRunId::new();
    apply_progress(&mut tab, success_id, ProcedureApplyProgress::Started);
    apply_progress(
        &mut tab,
        success_id,
        ProcedureApplyProgress::PromotionStarted,
    );
    apply_progress(
        &mut tab,
        success_id,
        ProcedureApplyProgress::PromotionSucceeded {
            result: PromotionResult {
                baseline: PromotionBaselineComparison {
                    checked_paths: vec!["src/a.rs".to_string()],
                    stale_paths: Vec::new(),
                },
                final_fingerprints: Vec::new(),
            },
        },
    );
    apply_progress(
        &mut tab,
        success_id,
        ProcedureApplyProgress::Finished {
            disposition: ProcedureTerminalDisposition::Succeeded,
        },
    );
    assert_eq!(
        tab.apply_status(),
        &ProcedureApplyStatus::Terminal(ProcedureTerminalDisposition::Succeeded)
    );
    let success_lines = tab.apply_render_lines_for_test();
    assert!(
        success_lines
            .iter()
            .any(|line| line.contains("Promotion: succeeded"))
    );
    assert!(
        success_lines
            .iter()
            .any(|line| line.contains("Apply terminal disposition: succeeded"))
    );

    let failure_id = ProcedureRunId::new();
    apply_progress(&mut tab, failure_id, ProcedureApplyProgress::Started);
    apply_progress(
        &mut tab,
        failure_id,
        ProcedureApplyProgress::PromotionStarted,
    );
    apply_progress(
        &mut tab,
        failure_id,
        ProcedureApplyProgress::PromotionFailed {
            message: "could not install src/b.rs".to_string(),
            recovery: Some(PromotionRecoveryEvidence {
                rollback_errors: vec!["restore failed".to_string()],
                recovery_paths: vec![PathBuf::from("src/.b.rs.deepseek-promotion-backup")],
            }),
        },
    );
    apply_progress(
        &mut tab,
        failure_id,
        ProcedureApplyProgress::Finished {
            disposition: ProcedureTerminalDisposition::Failed {
                reason: "promotion failed".to_string(),
            },
        },
    );
    assert_eq!(tab.apply_status(), &ProcedureApplyStatus::Failed);
    let failure_lines = tab.apply_render_lines_for_test();
    assert!(
        failure_lines
            .iter()
            .any(|line| line.contains("Promotion: failed: could not install src/b.rs"))
    );
    assert!(
        failure_lines
            .iter()
            .any(|line| line.contains("Recovery data retained:"))
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn run_sends_one_command_and_disables_a_second_start() {
    let root = fixture_root("command");
    let mut tab = ProcedureTab::new(&settings(), &root);
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();
    let (_progress_tx, progress_rx) = mpsc::unbounded_channel();
    let interrupt = Arc::new(AtomicBool::new(true));
    tab.attach(command_tx, progress_rx, Arc::clone(&interrupt));

    tab.start_for_test();
    tab.start_for_test();

    let ProcedureCommand::Run {
        backend, request, ..
    } = command_rx.try_recv().unwrap()
    else {
        panic!("Run must send a localization command")
    };
    assert_eq!(backend, "ollama-a");
    assert_eq!(request.change_id, "a-change");
    assert_eq!(request.task_id, "1.1");
    assert!(
        command_rx.try_recv().is_err(),
        "second start stays disabled"
    );
    assert!(
        !interrupt.load(Ordering::SeqCst),
        "new run clears stale stop"
    );
    assert!(tab.is_running());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn completed_progress_loads_targets_dispatch_details_and_report_path() {
    let root = fixture_root("completed");
    let mut tab = ProcedureTab::new(&settings(), &root);
    let run_id = ProcedureRunId::new();
    let run = completed_run(run_id);
    ProcedureReportStore::for_project(&root).save(&run).unwrap();

    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::AttemptStarted {
        run_id,
        number: 1,
        backend: "ollama-a".to_string(),
        model: "qwen-a".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::Succeeded,
    });

    assert_eq!(
        tab.status(),
        &ProcedureStatus::Finished(ProcedureTerminalDisposition::Succeeded)
    );
    let loaded = tab.latest_run().unwrap();
    assert_eq!(loaded.attempts.len(), 1);
    assert_eq!(loaded.attempts[0].backend, "ollama-a");
    assert_eq!(loaded.attempts[0].model, "qwen-a");
    assert_eq!(loaded.attempts[0].targets[0].evidence, "owns the runner");
    assert!(
        tab.latest_report_path()
            .unwrap()
            .ends_with(format!("{}.json", run_id.as_str()))
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn procedure_view_exposes_distinct_run_and_review_state_labels() {
    let root = fixture_root("state-labels");
    let mut tab = ProcedureTab::new(&settings(), &root);
    let run_id = ProcedureRunId::new();

    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    assert_eq!(tab.view_state(), ProcedureViewState::Running);
    assert_eq!(tab.view_state().label(), "running");

    let mut pending = completed_run(run_id);
    pending.review_disposition = ProcedureReviewDisposition::Pending;
    pending.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    ProcedureReportStore::for_project(&root)
        .save(&pending)
        .unwrap();
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    assert_eq!(tab.view_state(), ProcedureViewState::AwaitingReview);
    assert_eq!(tab.view_state().label(), "awaiting review");
    assert!(tab.review_actions_available_for_test());
    assert_eq!(
        tab.latest_run().unwrap().attempts[0].targets[0],
        LocalizationTarget {
            path: "src/procedure.rs".to_string(),
            symbol: Some("run".to_string()),
            evidence: "owns the runner".to_string(),
        }
    );

    let mut command_rx = attach_tab(&mut tab);
    tab.approve_for_test();
    finish_review_command(&root, &mut tab, &mut command_rx);
    assert_eq!(tab.view_state(), ProcedureViewState::Approved);
    assert_eq!(tab.view_state().label(), "approved");
    assert!(!tab.review_actions_available_for_test());

    let rejected_id = ProcedureRunId::new();
    let mut rejected = completed_run(rejected_id);
    rejected.review_disposition = ProcedureReviewDisposition::Pending;
    rejected.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    ProcedureReportStore::for_project(&root)
        .save(&rejected)
        .unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: rejected_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: rejected_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    let mut command_rx = attach_tab(&mut tab);
    tab.reject_for_test();
    finish_review_command(&root, &mut tab, &mut command_rx);
    assert_eq!(tab.view_state(), ProcedureViewState::Rejected);
    assert_eq!(tab.view_state().label(), "rejected");
    assert!(!tab.review_actions_available_for_test());

    let failed_id = ProcedureRunId::new();
    let mut failed = completed_run(failed_id);
    failed.review_disposition = ProcedureReviewDisposition::Pending;
    failed.terminal_disposition = Some(ProcedureTerminalDisposition::Failed {
        reason: "exact fixture failure".to_string(),
    });
    ProcedureReportStore::for_project(&root)
        .save(&failed)
        .unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: failed_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: failed_id,
        disposition: failed.terminal_disposition.clone().unwrap(),
    });
    assert_eq!(tab.view_state(), ProcedureViewState::Failed);
    assert_eq!(tab.view_state().label(), "failed");

    let interrupted_id = ProcedureRunId::new();
    let mut interrupted = completed_run(interrupted_id);
    interrupted.review_disposition = ProcedureReviewDisposition::Pending;
    interrupted.terminal_disposition = Some(ProcedureTerminalDisposition::Interrupted);
    ProcedureReportStore::for_project(&root)
        .save(&interrupted)
        .unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: interrupted_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: interrupted_id,
        disposition: ProcedureTerminalDisposition::Interrupted,
    });
    assert_eq!(tab.view_state(), ProcedureViewState::Interrupted);
    assert_eq!(tab.view_state().label(), "interrupted");

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn review_controls_are_visible_only_for_the_displayed_pending_run() {
    let root = fixture_root("review-controls");
    let store = ProcedureReportStore::for_project(&root);
    let visible_id = ProcedureRunId::new();
    let other_id = ProcedureRunId::new();
    let mut visible = completed_run(visible_id);
    visible.review_disposition = ProcedureReviewDisposition::Pending;
    visible.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    let mut other = completed_run(other_id);
    other.review_disposition = ProcedureReviewDisposition::Pending;
    other.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    store.save(&visible).unwrap();
    store.save(&other).unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);

    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: visible_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: visible_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });

    assert!(tab.review_actions_available_for_test());
    assert_eq!(tab.latest_run().unwrap().id, visible_id);
    assert_eq!(
        tab.latest_run().unwrap().attempts[0].targets,
        visible.attempts[0].targets
    );
    let mut command_rx = attach_tab(&mut tab);
    tab.approve_for_test();
    finish_review_command(&root, &mut tab, &mut command_rx);
    assert_eq!(
        tab.latest_run().unwrap().review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert!(!tab.review_actions_available_for_test());
    assert_eq!(
        store.load(&visible_id).unwrap().review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert_eq!(
        store.load(&other_id).unwrap().review_disposition,
        ProcedureReviewDisposition::Pending
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn review_failure_is_visible_and_keeps_the_pending_disposition() {
    let root = fixture_root("review-error");
    let store = ProcedureReportStore::for_project(&root);
    let run_id = ProcedureRunId::new();
    let mut pending = completed_run(run_id);
    pending.review_disposition = ProcedureReviewDisposition::Pending;
    pending.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    store.save(&pending).unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    std::fs::remove_file(store.report_path(&run_id)).unwrap();

    let mut command_rx = attach_tab(&mut tab);
    tab.reject_for_test();
    finish_review_command(&root, &mut tab, &mut command_rx);

    assert_eq!(
        tab.latest_run().unwrap().review_disposition,
        ProcedureReviewDisposition::Pending
    );
    assert_eq!(tab.view_state(), ProcedureViewState::AwaitingReview);
    assert!(tab.review_actions_available_for_test());
    let error = tab.review_error().unwrap();
    assert!(error.contains(&run_id.as_str()));
    assert!(error.contains("could not load procedure run"));

    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/procedure-localization :: Localization is observable and non-mutating :: Completed report is inspectable
#[test]
fn approved_structural_report_round_trip_populates_the_complete_procedure_view() {
    let root = fixture_root("approved-observability");
    let store = ProcedureReportStore::for_project(&root);
    let run_id = ProcedureRunId::new();
    let target = LocalizationTarget {
        path: "src/procedure.rs".to_string(),
        symbol: Some("run".to_string()),
        evidence: "The indexed symbol owns the localization runner.".to_string(),
    };
    let validation = OpenSpecValidation {
        command: vec![
            "openspec".to_string(),
            "validate".to_string(),
            "a-change".to_string(),
            "--strict".to_string(),
        ],
        exit_code: Some(0),
        stdout: "Change 'a-change' is valid".to_string(),
        stderr: String::new(),
    };
    let mut pending = completed_run(run_id);
    pending.validation = Some(validation.clone());
    pending.review_disposition = ProcedureReviewDisposition::Pending;
    pending.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    pending.attempts = vec![
        LocalizationAttempt {
            number: 1,
            backend: "ollama-a".to_string(),
            model: "qwen-a".to_string(),
            disposition: ProcedureAttemptDisposition::Rejected,
            targets: Vec::new(),
            validation_error: Some("first result was not structurally valid".to_string()),
        },
        LocalizationAttempt {
            number: 2,
            backend: "ollama-a".to_string(),
            model: "qwen-a".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![target.clone()],
            validation_error: None,
        },
    ];
    store.save(&pending).unwrap();
    let mut review_tab = ProcedureTab::new(&settings(), &root);
    review_tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    review_tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    assert_eq!(review_tab.view_state(), ProcedureViewState::AwaitingReview);

    let mut command_rx = attach_tab(&mut review_tab);
    review_tab.approve_for_test();
    finish_review_command(&root, &mut review_tab, &mut command_rx);
    let persisted = store.load(&run_id).unwrap();
    let mut reloaded_tab = ProcedureTab::new(&settings(), &root);
    reloaded_tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    reloaded_tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    let visible = reloaded_tab.latest_run().unwrap();

    assert_eq!(reloaded_tab.view_state(), ProcedureViewState::Approved);
    assert_ne!(
        reloaded_tab.view_state(),
        ProcedureViewState::AwaitingReview
    );
    assert_eq!(visible, &persisted);
    assert_eq!(
        visible.review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert_eq!(visible.validation.as_ref(), Some(&validation));
    assert_eq!(visible.attempts.len(), 2);
    assert_eq!(
        visible
            .attempts
            .iter()
            .map(|attempt| attempt.disposition)
            .collect::<Vec<_>>(),
        vec![
            ProcedureAttemptDisposition::Rejected,
            ProcedureAttemptDisposition::Accepted,
        ]
    );
    assert_eq!(visible.attempts[1].backend, "ollama-a");
    assert_eq!(visible.attempts[1].model, "qwen-a");
    assert_eq!(visible.attempts[1].targets, vec![target]);
    assert!(!reloaded_tab.review_actions_available_for_test());

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn procedure_progress_stays_out_of_transcript_and_agent_history_path() {
    let root = fixture_root("event-isolation");
    let (mut gui, mut agent_rx, mut procedure_rx, progress_tx, _interrupt) =
        gui_with_procedure(root.clone());

    gui.procedure_mut_for_test().start_for_test();
    let ProcedureCommand::Run {
        run_id, request, ..
    } = procedure_rx.try_recv().unwrap()
    else {
        panic!("start must send a run command")
    };
    progress_tx
        .send(ProcedureProgress::RunStarted {
            run_id,
            change_id: request.change_id,
            task_id: request.task_id,
        })
        .unwrap();
    gui.drain_procedure_for_test();

    assert!(gui.transcript_for_test().blocks().is_empty());
    assert!(
        agent_rx.try_recv().is_err(),
        "procedure uses no AgentCommand, so MessageHistory cannot receive it"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn stale_run_and_review_events_cannot_change_the_active_or_visible_run() {
    let root = fixture_root("stale-procedure-events");
    let store = ProcedureReportStore::for_project(&root);
    let active_id = ProcedureRunId::new();
    let stale_id = ProcedureRunId::new();
    let mut active = completed_run(active_id);
    active.review_disposition = ProcedureReviewDisposition::Pending;
    active.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    let mut stale = completed_run(stale_id);
    stale.review_disposition = ProcedureReviewDisposition::Pending;
    stale.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    store.save(&active).unwrap();
    store.save(&stale).unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);

    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: active_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::AttemptStarted {
        run_id: stale_id,
        number: 2,
        backend: "stale-backend".to_string(),
        model: "stale-model".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: stale_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });

    assert_eq!(
        tab.status(),
        &ProcedureStatus::Running {
            message: "Validating a-change task 1.1".to_string(),
        }
    );
    assert!(tab.latest_run().is_none());

    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: active_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    store.approve(&stale_id).unwrap();
    tab.handle_progress(ProcedureProgress::ReviewSucceeded {
        run_id: stale_id,
        disposition: ProcedureReviewDisposition::Approved,
    });
    tab.handle_progress(ProcedureProgress::ReviewFailed {
        run_id: stale_id,
        disposition: ProcedureReviewDisposition::Rejected,
        error: "stale review failure".to_string(),
    });

    assert_eq!(tab.latest_run().unwrap().id, active_id);
    assert_eq!(
        tab.latest_run().unwrap().review_disposition,
        ProcedureReviewDisposition::Pending
    );
    assert_eq!(tab.view_state(), ProcedureViewState::AwaitingReview);
    assert!(tab.review_error().is_none());

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn session_reset_and_drop_interrupt_an_owned_procedure() {
    let root = fixture_root("lifecycle-stop");
    let (mut gui, _agent_rx, mut procedure_rx, _progress_tx, interrupt) =
        gui_with_procedure(root.clone());

    gui.procedure_mut_for_test().start_for_test();
    procedure_rx.try_recv().unwrap();
    gui.handle_stream_event(StreamEvent::SessionReset);
    assert!(interrupt.load(Ordering::SeqCst));
    assert!(gui.transcript_for_test().blocks().is_empty());

    interrupt.store(false, Ordering::SeqCst);
    gui.procedure_mut_for_test().start_for_test();
    drop(gui);
    assert!(interrupt.load(Ordering::SeqCst));
    std::fs::remove_dir_all(root).ok();
}

fn completed_run(id: ProcedureRunId) -> ProcedureRun {
    ProcedureRun {
        id,
        change_id: "a-change".to_string(),
        selected_task: ProcedureTask {
            id: "1.1".to_string(),
            text: "First pending".to_string(),
            covers: None,
        },
        spec_fingerprint: Some("spec".to_string()),
        repository_fingerprint: Some("repository".to_string()),
        validation: None,
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "ollama-a".to_string(),
            model: "qwen-a".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![LocalizationTarget {
                path: "src/procedure.rs".to_string(),
                symbol: Some("run".to_string()),
                evidence: "owns the runner".to_string(),
            }],
            validation_error: None,
        }],
        review_disposition: ProcedureReviewDisposition::Approved,
        terminal_disposition: Some(ProcedureTerminalDisposition::Succeeded),
    }
}
