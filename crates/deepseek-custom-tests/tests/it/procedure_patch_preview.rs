use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::config::settings::{ApiProvider, BackendConfig, Settings};
use deepseek_custom::effort::Effort;
use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, MechanicalVerb, OpenSpecInput, PatchPreviewInputGate,
    PatchPreviewRequest, PatchPreviewRunner, ProcedureAttemptDisposition, ProcedureReportStore,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTerminalDisposition, RouteOverride, RouteSignal, RouteTier, capture_path_fingerprint,
    sha256_json,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHANGE_ID: &str = "preview-fixture";
const TASK_ID: &str = "1.1";
const TARGET: &str = "src/lib.rs";

fn temp_dir(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("dsc patch preview {tag} {}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_fake_openspec(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = root.join("fake-openspec.cmd");
        std::fs::write(&path, "@echo off\r\nexit /b 0\r\n").unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fake-openspec");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn write_fixture(root: &Path, task_text: &str, requirement_text: &str) -> PathBuf {
    let command = write_fake_openspec(root);
    let change = root.join("openspec/changes").join(CHANGE_ID);
    let spec = change.join("specs/sample/capability");
    std::fs::create_dir_all(&spec).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join(TARGET), "pub fn target_symbol() {}\n").unwrap();
    std::fs::write(
        change.join("proposal.md"),
        "## Why\n\nPrepare one preview.\n\n## What Changes\n\n- Rename one selected item.\n",
    )
    .unwrap();
    std::fs::write(
        change.join("tasks.md"),
        format!(
            "- [ ] {TASK_ID} {task_text}\n  <!-- covers: sample/capability :: Preview requirement :: Preview scenario -->\n"
        ),
    )
    .unwrap();
    std::fs::write(
        spec.join("spec.md"),
        format!(
            "## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Preview requirement\nThe system SHALL {requirement_text}.\n\n#### Scenario: Preview scenario\n- **WHEN** preview starts\n- **THEN** produce a complete diff\n"
        ),
    )
    .unwrap();
    command
}

fn approved_report(root: &Path, command: &Path) -> ProcedureRun {
    let input = OpenSpecInput::with_command(root, command.display().to_string());
    let validated = input.validate_and_select_task(CHANGE_ID, TASK_ID).unwrap();
    let report = ProcedureRun {
        id: ProcedureRunId::new(),
        change_id: CHANGE_ID.to_string(),
        selected_task: validated.contract.task.clone(),
        spec_fingerprint: Some(sha256_json(&validated.contract).unwrap()),
        repository_fingerprint: Some("sha256:preview-fixture".to_string()),
        validation: Some(validated.validation),
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "fixture-localizer".to_string(),
            model: "fixture-model".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![LocalizationTarget {
                path: TARGET.to_string(),
                symbol: Some("target_symbol".to_string()),
                evidence: "The selected function owns the requested edit.".to_string(),
            }],
            validation_error: None,
        }],
        review_disposition: ProcedureReviewDisposition::Approved,
        terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
    };
    ProcedureReportStore::for_project(root)
        .save(&report)
        .unwrap();
    report
}

fn source_hashes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn collect(root: &Path, directory: &Path, hashes: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if relative == ".deepseek" || relative.starts_with(".deepseek/") {
                continue;
            }
            if path.is_dir() {
                collect(root, &path, hashes);
            } else if path.is_file() {
                hashes.insert(relative, std::fs::read(path).unwrap());
            }
        }
    }

    let mut hashes = BTreeMap::new();
    collect(root, root, &mut hashes);
    hashes
}

fn envelope(route: serde_json::Value, replacement: &str, rationale: &str) -> String {
    serde_json::json!({
        "route": route,
        "targets": [TARGET],
        "rationale": rationale,
        "unified_diff": format!(
            "diff --git a/{TARGET} b/{TARGET}\n--- a/{TARGET}\n+++ b/{TARGET}\n@@ -1 +1 @@\n-pub fn target_symbol() {{}}\n+pub fn {replacement}() {{}}\n"
        )
    })
    .to_string()
}

fn chat_response(content: String, model: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "preview-1",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
    })
}

fn runner(root: &Path, command: &Path, settings: Settings) -> PatchPreviewRunner {
    let reports = ProcedureReportStore::for_project(root);
    PatchPreviewRunner::new(
        PatchPreviewInputGate::new(
            OpenSpecInput::with_command(root, command.display().to_string()),
            root.to_path_buf(),
            reports,
        ),
        root.to_path_buf(),
        Arc::new(BackendFactory::new(settings, root.to_path_buf())),
        Arc::new(AtomicBool::new(false)),
        Effort::None,
        4096,
    )
}

fn request(run_id: ProcedureRunId, local_model: &str) -> PatchPreviewRequest {
    PatchPreviewRequest {
        localization_run_id: run_id,
        change_id: CHANGE_ID.to_string(),
        task_id: TASK_ID.to_string(),
        route_override: RouteOverride::Automatic,
        local_backend: "local-ollama".to_string(),
        local_model: local_model.to_string(),
        frontier_backend: "frontier-claude".to_string(),
        frontier_model: "frontier-test".to_string(),
    }
}

// Task 5.3: complete production pipeline with a controlled local backend.
#[tokio::test]
async fn local_mechanical_preview_runs_end_to_end_without_changing_source_hashes() {
    let root = temp_dir("local mechanical");
    let command = write_fixture(&root, "Rename target_symbol", "rename one function");
    let report = approved_report(&root, &command);
    let before = source_hashes(&root);
    let server = MockServer::start().await;
    let model = "qwen-local-test";
    let response = envelope(
        serde_json::json!({
            "automatic_tier": "local",
            "effective_tier": "local",
            "selected_override": "automatic",
            "overridden": false,
            "signals": [
                {"kind": "mechanical_verb", "value": "rename"},
                {"kind": "target_count", "value": 1}
            ]
        }),
        "renamed_symbol",
        "Rename the one localized function.",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(response, model)))
        .expect(1)
        .mount(&server)
        .await;
    let settings = Settings {
        backends: Some(HashMap::from([(
            "local-ollama".to_string(),
            BackendConfig::Api {
                provider: ApiProvider::Ollama,
                model: model.to_string(),
                base_url: Some(server.uri()),
                api_key: None,
                models: None,
            },
        )])),
        ..Settings::default()
    };

    let (preview, path) = runner(&root, &command, settings)
        .run(
            deepseek_custom::procedure::PatchPreviewId::new(),
            request(report.id, model),
        )
        .await
        .unwrap();

    assert_eq!(preview.route.automatic_tier, RouteTier::Local);
    assert_eq!(preview.route.effective_tier, RouteTier::Local);
    assert_eq!(
        preview.route.signals,
        [
            RouteSignal::MechanicalVerb(MechanicalVerb::Rename),
            RouteSignal::TargetCount(1)
        ]
    );
    assert_eq!(preview.backend, "local-ollama");
    assert_eq!(preview.model, model);
    assert_eq!(preview.targets, [TARGET]);
    assert_eq!(preview.rationale, "Rename the one localized function.");
    assert!(preview.unified_diff.contains("+pub fn renamed_symbol() {}"));
    assert!(path.is_file());
    assert_eq!(source_hashes(&root), before);
    assert_eq!(
        std::fs::read_to_string(root.join(TARGET)).unwrap(),
        "pub fn target_symbol() {}\n"
    );
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = requests[0].body_json().unwrap();
    let prompt = body["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains("--- BEGIN src/lib.rs ---"));
    assert!(prompt.contains("pub fn target_symbol() {}"));
    assert!(prompt.contains("must end with a newline"));
    server.verify().await;
    std::fs::remove_dir_all(root).ok();
}

// Task 5.3: complete production pipeline with a controlled frontier CLI backend.
#[tokio::test(flavor = "current_thread")]
async fn frontier_architectural_preview_runs_end_to_end_without_changing_source_hashes() {
    let root = temp_dir("frontier architecture");
    let command = write_fixture(
        &root,
        "Revise the architecture entry",
        "revise the architecture entry",
    );
    let report = approved_report(&root, &command);
    let before = source_hashes(&root);
    let response = envelope(
        serde_json::json!({
            "automatic_tier": "frontier",
            "effective_tier": "frontier",
            "selected_override": "automatic",
            "overridden": false,
            "signals": [
                {"kind": "mechanical_verb", "value": "rename"},
                {"kind": "target_count", "value": 1},
                {"kind": "architecture"}
            ]
        }),
        "architecture_entry",
        "Revise the localized architecture entry.",
    );
    let settings = Settings {
        backends: Some(HashMap::from([(
            "frontier-claude".to_string(),
            BackendConfig::ClaudeCli {
                model: "frontier-test".to_string(),
                permission_mode: None,
                env: Some(HashMap::from([
                    (
                        "CLAUDE_CLI_PATH".to_string(),
                        env!("CARGO_BIN_EXE_fake_claude").to_string(),
                    ),
                    ("FAKE_CLI_RESPONSE".to_string(), response),
                    (
                        "FAKE_CLI_SIDE_EFFECT_PATH".to_string(),
                        "src/fake-side-effect.txt".to_string(),
                    ),
                ])),
                models: None,
            },
        )])),
        ..Settings::default()
    };

    let (preview, path) = runner(&root, &command, settings)
        .run(
            deepseek_custom::procedure::PatchPreviewId::new(),
            request(report.id, "unused-local-model"),
        )
        .await
        .unwrap();

    assert_eq!(preview.route.automatic_tier, RouteTier::Frontier);
    assert_eq!(preview.route.effective_tier, RouteTier::Frontier);
    assert!(preview.route.signals.contains(&RouteSignal::Architecture));
    assert_eq!(preview.backend, "frontier-claude");
    assert_eq!(preview.model, "frontier-test");
    assert_eq!(preview.targets, [TARGET]);
    assert_eq!(
        preview.rationale,
        "Revise the localized architecture entry."
    );
    assert!(
        preview
            .unified_diff
            .contains("+pub fn architecture_entry() {}")
    );
    assert!(path.is_file());
    assert_eq!(source_hashes(&root), before);
    assert!(!root.join("src/fake-side-effect.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.join(TARGET)).unwrap(),
        "pub fn target_symbol() {}\n"
    );
    std::fs::remove_dir_all(root).ok();
}

fn configured_backend(
    settings: &Settings,
    configured: Option<&str>,
    predicate: impl Fn(&BackendConfig) -> bool,
) -> (String, BackendConfig) {
    let backends = settings.backends.as_ref().expect("configured backends");
    if let Some(name) = configured
        && let Some(backend) = backends.get(name)
        && predicate(backend)
    {
        return (name.to_string(), backend.clone());
    }
    let mut matches = backends
        .iter()
        .filter(|(_, backend)| predicate(backend))
        .map(|(name, backend)| (name.clone(), backend.clone()))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    matches
        .into_iter()
        .next()
        .expect("matching configured backend")
}

fn sanitized_error(error: &str, fixture_root: &Path) -> String {
    error.replace(&fixture_root.display().to_string(), "<fixture>")
}

async fn run_real_case(
    settings: Settings,
    backend: &str,
    model: &str,
    task_text: &str,
    requirement_text: &str,
    expected_route: RouteTier,
) -> serde_json::Value {
    let root = temp_dir(&format!("real {expected_route}"));
    let command = write_fixture(&root, task_text, requirement_text);
    let report = approved_report(&root, &command);
    let before = capture_path_fingerprint(&root, TARGET).unwrap();
    let request = PatchPreviewRequest {
        localization_run_id: report.id,
        change_id: CHANGE_ID.to_string(),
        task_id: TASK_ID.to_string(),
        route_override: RouteOverride::Automatic,
        local_backend: if expected_route == RouteTier::Local {
            backend.to_string()
        } else {
            "unused-local".to_string()
        },
        local_model: if expected_route == RouteTier::Local {
            model.to_string()
        } else {
            "unused-local-model".to_string()
        },
        frontier_backend: if expected_route == RouteTier::Frontier {
            backend.to_string()
        } else {
            "unused-frontier".to_string()
        },
        frontier_model: if expected_route == RouteTier::Frontier {
            model.to_string()
        } else {
            "unused-frontier-model".to_string()
        },
    };
    let timeout = if expected_route == RouteTier::Local {
        std::time::Duration::from_secs(120)
    } else {
        std::time::Duration::from_secs(180)
    };
    let result = tokio::time::timeout(
        timeout,
        runner(&root, &command, settings)
            .run(deepseek_custom::procedure::PatchPreviewId::new(), request),
    )
    .await;
    let after = capture_path_fingerprint(&root, TARGET).unwrap();
    let unchanged = before.content_sha256 == after.content_sha256;
    let evidence = match result {
        Ok(Ok((preview, _))) => serde_json::json!({
            "outcome": if unchanged { "valid_preview" } else { "workspace_changed" },
            "backend": backend,
            "model": model,
            "automatic_route": preview.route.automatic_tier,
            "effective_route": preview.route.effective_tier,
            "signals": preview.route.signals,
            "targets": preview.targets,
            "before_source_sha256": before.content_sha256,
            "after_source_sha256": after.content_sha256,
            "source_unchanged": unchanged,
            "parser_failure": serde_json::Value::Null,
            "error": serde_json::Value::Null
        }),
        Ok(Err(error)) => {
            let error = sanitized_error(&error.to_string(), &root);
            let parser_failure =
                (error.contains("decod") || error.contains("envelope") || error.contains("parse"))
                    .then_some(error.clone());
            serde_json::json!({
                "outcome": "failed",
                "backend": backend,
                "model": model,
                "automatic_route": serde_json::Value::Null,
                "effective_route": serde_json::Value::Null,
                "signals": [],
                "targets": [TARGET],
                "before_source_sha256": before.content_sha256,
                "after_source_sha256": after.content_sha256,
                "source_unchanged": unchanged,
                "parser_failure": parser_failure,
                "error": error
            })
        }
        Err(_) => serde_json::json!({
            "outcome": "timed_out",
            "backend": backend,
            "model": model,
            "automatic_route": serde_json::Value::Null,
            "effective_route": serde_json::Value::Null,
            "signals": [],
            "targets": [TARGET],
            "before_source_sha256": before.content_sha256,
            "after_source_sha256": after.content_sha256,
            "source_unchanged": unchanged,
            "parser_failure": serde_json::Value::Null,
            "error": format!("preview exceeded {} seconds", timeout.as_secs())
        }),
    };
    std::fs::remove_dir_all(root).ok();
    evidence
}

// Task 5.4 practical verification. This stays out of normal workspace tests
// because it invokes the user's configured Ollama and frontier CLI backends.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires configured Ollama and frontier CLI backends"]
async fn practical_real_preview_cases_record_sanitized_evidence() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let settings = Settings::load(&repo_root).unwrap();
    let procedure = settings.procedure.as_ref();
    let (local_backend, _) = configured_backend(
        &settings,
        procedure.and_then(|value| value.local_patch_backend.as_deref()),
        |backend| {
            matches!(
                backend,
                BackendConfig::Api {
                    provider: ApiProvider::Ollama,
                    ..
                }
            )
        },
    );
    let (frontier_backend, frontier_config) = configured_backend(
        &settings,
        procedure.and_then(|value| value.frontier_patch_backend.as_deref()),
        |backend| matches!(backend, BackendConfig::CodexCli { .. }),
    );
    let local_model = "qwen2.5-coder:7b-instruct-q4_K_M";
    let frontier_model = frontier_config.model().to_string();
    let selected_case = std::env::var("DEEPSEEK_PRACTICAL_CASE").ok();
    let evidence_path = repo_root
        .join("openspec/changes/add-routed-patch-preview/evidence/practical-preview-cases.json");
    let existing_case = |name: &str| {
        let document: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&evidence_path).expect("existing practical evidence"),
        )
        .unwrap();
        document["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .map(|case| case["evidence"].clone())
            .expect("named existing practical case")
    };

    let local = if selected_case.as_deref() == Some("frontier") {
        existing_case("local_mechanical")
    } else {
        run_real_case(
            settings.clone(),
            &local_backend,
            local_model,
            "Rename target_symbol to renamed_symbol",
            "rename one function",
            RouteTier::Local,
        )
        .await
    };
    let inherited_path = std::env::var_os("PATH").expect("process PATH");
    let node = std::env::split_paths(&inherited_path)
        .map(|directory| directory.join("node.exe"))
        .find(|path| path.is_file())
        .expect("installed node.exe");
    let codex_script = std::env::split_paths(&inherited_path)
        .map(|directory| directory.join("node_modules/@openai/codex/bin/codex.js"))
        .find(|path| path.is_file())
        .expect("installed npm Codex CLI script");
    let codex_wrapper =
        std::env::temp_dir().join(format!("dsc-codex-wrapper-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&codex_wrapper).unwrap();
    std::fs::write(
        codex_wrapper.join("codex.cmd"),
        format!(
            "@echo off\r\n\"{}\" \"{}\" %*\r\n",
            node.display(),
            codex_script.display()
        ),
    )
    .unwrap();
    let native_first_path = std::env::join_paths(
        std::iter::once(codex_wrapper.clone()).chain(std::env::split_paths(&inherited_path)),
    )
    .unwrap();
    // SAFETY: this ignored practical test is the only selected test in its
    // process. The original PATH is restored immediately after the call.
    unsafe { std::env::set_var("PATH", native_first_path) };
    let frontier = if selected_case.as_deref() == Some("local") {
        existing_case("frontier_architectural")
    } else {
        run_real_case(
            settings,
            &frontier_backend,
            &frontier_model,
            "Revise the architecture entry",
            "revise the architecture entry",
            RouteTier::Frontier,
        )
        .await
    };
    // SAFETY: see the single-test process note above.
    unsafe { std::env::set_var("PATH", inherited_path) };
    std::fs::remove_dir_all(codex_wrapper).ok();
    let evidence = serde_json::json!({
        "schema_version": 1,
        "bounded_repair_history": [
            {
                "case": "local_mechanical",
                "stage": "shared_decoder",
                "diagnostic": "patch envelope unified_diff is invalid: first line must start with `diff --git `"
            },
            {
                "case": "local_mechanical",
                "stage": "shared_decoder",
                "diagnostic": "patch envelope unified_diff is invalid: line 5 has an invalid hunk prefix"
            },
            {
                "case": "local_mechanical",
                "stage": "git_apply_check",
                "diagnostic": "patch hunks do not apply to the current source snapshot: error: corrupt patch at line 6"
            },
            {
                "case": "frontier_architectural",
                "stage": "codex_dispatch",
                "diagnostic": "disposable snapshot required --skip-git-repo-check"
            }
        ],
        "cases": [
            {"name": "local_mechanical", "evidence": local},
            {"name": "frontier_architectural", "evidence": frontier}
        ]
    });
    std::fs::create_dir_all(evidence_path.parent().unwrap()).unwrap();
    std::fs::write(
        &evidence_path,
        format!("{}\n", serde_json::to_string_pretty(&evidence).unwrap()),
    )
    .unwrap();

    for case in evidence["cases"].as_array().unwrap() {
        assert_eq!(case["evidence"]["outcome"], "valid_preview", "{case}");
        assert_eq!(case["evidence"]["source_unchanged"], true, "{case}");
    }
}
