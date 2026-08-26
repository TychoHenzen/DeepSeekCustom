use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use deepseek_custom::config::settings::{BackendConfig, ProcedureSettings, Settings};
use deepseek_custom::procedure::{
    CandidateEligibility, LocalCandidateGenerationEvidence, LocalCandidateVerification,
    LocalCandidateVerificationOutcome, LocalCandidateVerificationRun, LocalPatchCandidate,
    LocalPatchCandidateGeneration, LocalPatchCandidateGenerator, LocalPatchCandidateResolution,
    LocalPatchCandidateVerifier, LocalPatchDraftDispatch, LocalPatchDraftError,
    LocalizationAgreementError, LocalizationAgreementOutcome, LocalizationAgreementResolver,
    LocalizationDispatch, LocalizationDispatchError, LocalizationEnvelope,
    LocalizationEscalationTrigger, LocalizationSample, LocalizationSampleOutcome,
    LocalizationSampler, LocalizationTarget, NormalizedLocalizationTarget,
    NormalizedLocalizationTargets, OpenSpecInput, PatchCandidate, ProcedureAttemptDisposition,
    ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureScratchpad, ProcedureStage, ProcedureTask, ProcedureTerminalDisposition,
    RepositoryIndexEntry, SamplingInputGate, SamplingInputRequest, VerifierReport,
    begin_existing_bounded_repair, decode_patch_envelope, select_localization_agreement,
    select_passing_local_candidate, sha256_json,
};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "dsc-procedure-sampling-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_fake_openspec(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = root.join("fake-openspec.cmd");
        std::fs::write(&path, "@echo off\r\necho %*\r\nexit /b 0\r\n").unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fake-openspec");
        std::fs::write(&path, "#!/bin/sh\nprintf '%s\\n' \"$*\"\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn write_fixture(root: &Path) -> PathBuf {
    let command = write_fake_openspec(root);
    let change = root.join("openspec/changes/fixture-change");
    let spec = change.join("specs/sample/capability");
    std::fs::create_dir_all(&spec).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn target_symbol() {}\n").unwrap();
    std::fs::write(
        change.join("proposal.md"),
        "## Why\n\nSample localization.\n\n## What Changes\n\n- Find one target.\n",
    )
    .unwrap();
    std::fs::write(
        change.join("tasks.md"),
        "- [ ] 1.1 Find the target\n  <!-- covers: sample/capability :: Find target :: Target found -->\n",
    )
    .unwrap();
    std::fs::write(
        spec.join("spec.md"),
        "## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Find target\nThe system SHALL find a target.\n\n#### Scenario: Target found\n- **WHEN** localization runs\n- **THEN** it reports one target\n",
    )
    .unwrap();
    command
}

fn approved_sampling_input(
    root: &Path,
    command: &Path,
) -> deepseek_custom::procedure::ValidatedSamplingInput {
    let input = OpenSpecInput::with_command(root, command.display().to_string());
    let validated = input
        .validate_and_select_task("fixture-change", "1.1")
        .unwrap();
    let report = ProcedureRun {
        id: ProcedureRunId::new(),
        change_id: "fixture-change".to_string(),
        selected_task: ProcedureTask {
            id: "1.1".to_string(),
            text: validated.contract.task.text.clone(),
            covers: validated.contract.task.covers.clone(),
        },
        spec_fingerprint: Some(sha256_json(&validated.contract).unwrap()),
        repository_fingerprint: Some("sha256:repository-fixture".to_string()),
        validation: Some(validated.validation),
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![deepseek_custom::procedure::LocalizationAttempt {
            number: 1,
            backend: "fixture-localizer".to_string(),
            model: "fixture-model".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![target(
                "src/lib.rs",
                Some("target_symbol"),
                "The fixture source owns the target.",
            )],
            validation_error: None,
        }],
        review_disposition: ProcedureReviewDisposition::Pending,
        terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
    };
    let reports = ProcedureReportStore::for_project(root);
    reports.save(&report).unwrap();
    reports.approve(&report.id).unwrap();
    SamplingInputGate::new(
        OpenSpecInput::with_command(root, command.display().to_string()),
        root.to_path_buf(),
        reports,
    )
    .load(&SamplingInputRequest {
        baseline_localization_run_id: report.id,
        change_id: "fixture-change".to_string(),
        task_id: "1.1".to_string(),
    })
    .unwrap()
}

fn index() -> Vec<RepositoryIndexEntry> {
    vec![RepositoryIndexEntry {
        path: "src/lib.rs".to_string(),
        symbols: vec!["target_symbol".to_string()],
    }]
}

fn sampling_settings(
    count: u8,
) -> deepseek_custom::config::settings::ValidatedProcedureSamplingSettings {
    sampling_settings_with_quorum(count, 2)
}

fn sampling_settings_with_quorum(
    count: u8,
    quorum: u8,
) -> deepseek_custom::config::settings::ValidatedProcedureSamplingSettings {
    Settings {
        procedure: Some(ProcedureSettings {
            localization_sample_count: count,
            localization_agreement_quorum: quorum,
            ..ProcedureSettings::default()
        }),
        ..Settings::default()
    }
    .validated_procedure_sampling_settings()
    .unwrap()
}

fn candidate_settings(
    count: u8,
) -> deepseek_custom::config::settings::ValidatedProcedureSamplingSettings {
    Settings {
        procedure: Some(ProcedureSettings {
            local_patch_candidate_count: count,
            ..ProcedureSettings::default()
        }),
        ..Settings::default()
    }
    .validated_procedure_sampling_settings()
    .unwrap()
}

fn target(path: &str, symbol: Option<&str>, evidence: &str) -> LocalizationTarget {
    LocalizationTarget {
        path: path.to_string(),
        symbol: symbol.map(str::to_string),
        evidence: evidence.to_string(),
    }
}

#[test]
fn accepted_localization_targets_normalize_to_sorted_deduplicated_identities() {
    let first = NormalizedLocalizationTargets::from_accepted(&[
        target("src/z.rs", Some("later"), "first explanation"),
        target("src/a.rs", Some("symbol"), "one explanation"),
        target("src/a.rs", None, "path-level explanation"),
        target("src/a.rs", Some("symbol"), "a different explanation"),
        target("src/z.rs", Some("later"), "duplicate explanation"),
    ]);
    let second = NormalizedLocalizationTargets::from_accepted(&[
        target("src/a.rs", None, "different path-level explanation"),
        target("src/z.rs", Some("later"), "different explanation"),
        target("src/a.rs", Some("symbol"), "another explanation"),
    ]);

    assert_eq!(first, second);
    assert_eq!(
        first.targets(),
        [
            NormalizedLocalizationTarget {
                path: "src/a.rs".to_string(),
                symbol: None,
            },
            NormalizedLocalizationTarget {
                path: "src/a.rs".to_string(),
                symbol: Some("symbol".to_string()),
            },
            NormalizedLocalizationTarget {
                path: "src/z.rs".to_string(),
                symbol: Some("later".to_string()),
            },
        ]
    );
}

fn accepted_sample(number: u8, targets: Vec<LocalizationTarget>) -> LocalizationSample {
    LocalizationSample {
        number,
        outcome: LocalizationSampleOutcome::Accepted {
            normalized_targets: NormalizedLocalizationTargets::from_accepted(&targets),
            targets,
        },
    }
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Localization agreement controls escalation :: Local samples reach quorum
#[test]
fn largest_normalized_quorum_group_wins_with_first_sample_tie_breaking() {
    let alpha = vec![
        target("src/lib.rs", Some("target_symbol"), "alpha evidence"),
        target("src/lib.rs", None, "alpha path evidence"),
    ];
    let alpha_reordered_with_duplicate = vec![
        target("src/lib.rs", None, "different alpha path evidence"),
        target(
            "src/lib.rs",
            Some("target_symbol"),
            "different alpha evidence",
        ),
        target(
            "src/lib.rs",
            Some("target_symbol"),
            "duplicate alpha evidence",
        ),
    ];
    let beta = vec![target("src/lib.rs", None, "beta evidence")];
    let samples = vec![
        accepted_sample(4, beta.clone()),
        accepted_sample(3, alpha_reordered_with_duplicate),
        accepted_sample(1, beta.clone()),
        accepted_sample(2, alpha.clone()),
    ];

    let agreement = select_localization_agreement(&samples, 2).unwrap();

    assert_eq!(agreement.targets, beta);
    assert_eq!(agreement.sample_numbers, [1, 4]);
    assert_eq!(
        agreement.normalized_targets,
        NormalizedLocalizationTargets::from_accepted(&agreement.targets)
    );
}

#[derive(Clone)]
struct CountingDispatcher {
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct ScriptedDispatcher {
    calls: Arc<AtomicUsize>,
    responses: Arc<Mutex<VecDeque<Result<LocalizationEnvelope, LocalizationDispatchError>>>>,
}

impl ScriptedDispatcher {
    fn new(
        responses: impl IntoIterator<Item = Result<LocalizationEnvelope, LocalizationDispatchError>>,
    ) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
        }
    }
}

#[async_trait]
impl LocalizationDispatch for ScriptedDispatcher {
    fn backend_name(&self) -> &str {
        "scripted-localizer"
    }

    fn model(&self) -> &str {
        "scripted-model"
    }

    async fn dispatch_prompt(
        &self,
        _prompt: String,
        _repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.responses.lock().unwrap().pop_front().unwrap()
    }
}

fn envelope(
    targets: Vec<LocalizationTarget>,
) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
    Ok(LocalizationEnvelope { targets })
}

async fn disagreement_runs_one_validated_frontier_localization_case()
-> (usize, usize, LocalizationAgreementOutcome) {
    let root = temp_dir("frontier-disagreement");
    let command = write_fixture(&root);
    let repository_index = vec![
        RepositoryIndexEntry {
            path: "src/lib.rs".to_string(),
            symbols: vec!["target_symbol".to_string()],
        },
        RepositoryIndexEntry {
            path: "src/one.rs".to_string(),
            symbols: Vec::new(),
        },
        RepositoryIndexEntry {
            path: "src/two.rs".to_string(),
            symbols: Vec::new(),
        },
    ];
    let local = ScriptedDispatcher::new([
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "first")]),
        envelope(vec![target("src/one.rs", None, "second")]),
        envelope(vec![target("src/two.rs", None, "third")]),
    ]);
    let frontier = ScriptedDispatcher::new([envelope(vec![target(
        "src/lib.rs",
        Some("target_symbol"),
        "frontier result",
    )])]);
    let resolver = LocalizationAgreementResolver::new(
        LocalizationSampler::new(
            local.clone(),
            sampling_settings(3),
            Arc::new(AtomicBool::new(false)),
        ),
        frontier.clone(),
    );

    let run = resolver
        .resolve(&approved_sampling_input(&root, &command), &repository_index)
        .await
        .unwrap();

    let local_calls = local.calls.load(Ordering::SeqCst);
    let frontier_calls = frontier.calls.load(Ordering::SeqCst);
    std::fs::remove_dir_all(root).ok();
    (local_calls, frontier_calls, run.outcome)
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Localization agreement controls escalation :: Local samples disagree
#[test]
fn disagreement_runs_one_validated_frontier_localization() {
    let (local_calls, frontier_calls, outcome) =
        run_async_test(disagreement_runs_one_validated_frontier_localization_case());

    assert_eq!(local_calls, 3);
    assert_eq!(frontier_calls, 1);
    assert!(matches!(
        outcome,
        LocalizationAgreementOutcome::Frontier {
            escalation_trigger: LocalizationEscalationTrigger::LocalDisagreement,
            ..
        }
    ));
}

#[tokio::test]
async fn invalid_samples_do_not_count_and_exact_quorum_stays_local() {
    let root = temp_dir("invalid-and-quorum");
    let command = write_fixture(&root);
    let local = ScriptedDispatcher::new([
        envelope(vec![target("outside.rs", None, "not indexed")]),
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "second")]),
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "third")]),
    ]);
    let frontier = ScriptedDispatcher::new(std::iter::empty());
    let resolver = LocalizationAgreementResolver::new(
        LocalizationSampler::new(
            local.clone(),
            sampling_settings_with_quorum(3, 2),
            Arc::new(AtomicBool::new(false)),
        ),
        frontier.clone(),
    );

    let run = resolver
        .resolve(&approved_sampling_input(&root, &command), &index())
        .await
        .unwrap();

    assert_eq!(local.calls.load(Ordering::SeqCst), 3);
    assert_eq!(frontier.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        run.outcome,
        LocalizationAgreementOutcome::Local { ref agreement }
            if agreement.sample_numbers == [2, 3]
    ));
    assert!(matches!(
        run.sampling_run.samples()[0].outcome,
        LocalizationSampleOutcome::Rejected { .. }
    ));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn frontier_localization_failure_preserves_the_disagreement_trigger() {
    let root = temp_dir("frontier-failure");
    let command = write_fixture(&root);
    let local = ScriptedDispatcher::new([
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "first")]),
        envelope(vec![target("src/lib.rs", None, "second")]),
        envelope(Vec::new()),
    ]);
    let frontier = ScriptedDispatcher::new([Err(LocalizationDispatchError::Request {
        backend: "frontier".to_string(),
        reason: "fixture failure".to_string(),
    })]);
    let resolver = LocalizationAgreementResolver::new(
        LocalizationSampler::new(
            local,
            sampling_settings(3),
            Arc::new(AtomicBool::new(false)),
        ),
        frontier.clone(),
    );

    let error = resolver
        .resolve(&approved_sampling_input(&root, &command), &index())
        .await
        .unwrap_err();

    assert_eq!(frontier.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        error,
        LocalizationAgreementError::FrontierDispatch {
            trigger: LocalizationEscalationTrigger::LocalDisagreement,
            ..
        }
    ));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn all_samples_are_required_when_the_quorum_equals_the_sample_count() {
    let root = temp_dir("quorum-boundary");
    let command = write_fixture(&root);
    let local = ScriptedDispatcher::new([
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "first")]),
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "second")]),
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "third")]),
    ]);
    let frontier = ScriptedDispatcher::new(std::iter::empty());
    let resolver = LocalizationAgreementResolver::new(
        LocalizationSampler::new(
            local,
            sampling_settings_with_quorum(3, 3),
            Arc::new(AtomicBool::new(false)),
        ),
        frontier.clone(),
    );

    let run = resolver
        .resolve(&approved_sampling_input(&root, &command), &index())
        .await
        .unwrap();

    assert_eq!(frontier.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        run.outcome,
        LocalizationAgreementOutcome::Local { ref agreement }
            if agreement.sample_numbers == [1, 2, 3]
    ));
    std::fs::remove_dir_all(root).ok();
}

impl CountingDispatcher {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn update_maximum(&self, observed: usize) {
        let mut maximum = self.max_active.load(Ordering::SeqCst);
        while observed > maximum {
            match self.max_active.compare_exchange(
                maximum,
                observed,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(current) => maximum = current,
            }
        }
    }
}

#[async_trait]
impl LocalizationDispatch for CountingDispatcher {
    fn backend_name(&self) -> &str {
        "counting-localizer"
    }

    fn model(&self) -> &str {
        "counting-model"
    }

    async fn dispatch_prompt(
        &self,
        _prompt: String,
        _repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.update_maximum(active);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(LocalizationEnvelope {
            targets: vec![target(
                "src/lib.rs",
                Some("target_symbol"),
                "The indexed source defines the target.",
            )],
        })
    }
}

struct SamplingCountObservation {
    requested: u8,
    dispatches: usize,
    samples: usize,
    all_accepted: bool,
    max_concurrent_dispatches: usize,
}

async fn configured_localization_sample_count_is_attempted_within_the_fixed_cap_case()
-> Vec<SamplingCountObservation> {
    let mut observations = Vec::new();
    for count in 3..=5 {
        let root = temp_dir(&format!("count-{count}"));
        let command = write_fixture(&root);
        let dispatcher = CountingDispatcher::new();
        let sampler = LocalizationSampler::new(
            dispatcher.clone(),
            sampling_settings(count),
            Arc::new(AtomicBool::new(false)),
        );

        let run = sampler
            .run(&approved_sampling_input(&root, &command), &index())
            .await
            .unwrap();

        observations.push(SamplingCountObservation {
            requested: count,
            dispatches: dispatcher.calls.load(Ordering::SeqCst),
            samples: run.samples().len(),
            all_accepted: run
                .samples()
                .iter()
                .all(|sample| matches!(sample.outcome, LocalizationSampleOutcome::Accepted { .. })),
            max_concurrent_dispatches: dispatcher.max_active.load(Ordering::SeqCst),
        });
        std::fs::remove_dir_all(root).ok();
    }
    observations
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Localization uses bounded agreement sampling :: Sample settings are inside bounds
#[test]
fn configured_localization_sample_count_is_attempted_within_the_fixed_cap() {
    let observations = run_async_test(
        configured_localization_sample_count_is_attempted_within_the_fixed_cap_case(),
    );

    assert_eq!(observations.len(), 3);
    for observation in observations {
        assert_eq!(observation.dispatches, usize::from(observation.requested));
        assert_eq!(observation.samples, usize::from(observation.requested));
        assert!(observation.all_accepted);
        assert!(observation.max_concurrent_dispatches <= 5);
    }
}

fn run_async_test<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[derive(Clone)]
struct InterruptingDispatcher {
    calls: Arc<AtomicUsize>,
    interrupt: Arc<AtomicBool>,
}

#[async_trait]
impl LocalizationDispatch for InterruptingDispatcher {
    fn backend_name(&self) -> &str {
        "interrupting-localizer"
    }

    fn model(&self) -> &str {
        "interrupting-model"
    }

    async fn dispatch_prompt(
        &self,
        _prompt: String,
        _repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.interrupt.store(true, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[tokio::test]
async fn shared_interrupt_stops_pending_localization_samples() {
    let root = temp_dir("interrupt");
    let command = write_fixture(&root);
    let interrupt = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let sampler = LocalizationSampler::new(
        InterruptingDispatcher {
            calls: Arc::clone(&calls),
            interrupt: Arc::clone(&interrupt),
        },
        sampling_settings(5),
        interrupt,
    );

    let run = sampler
        .run(&approved_sampling_input(&root, &command), &index())
        .await
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(run.interrupted());
    assert!(
        run.samples()
            .iter()
            .all(|sample| matches!(sample.outcome, LocalizationSampleOutcome::Interrupted))
    );
    std::fs::remove_dir_all(root).ok();
}

fn patch_candidate(replacement: &str) -> PatchCandidate {
    let diff = format!(
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn target_symbol() {{}}\n+pub fn {replacement}() {{}}\n"
    );
    let envelope = serde_json::json!({
        "targets": ["src/lib.rs"],
        "rationale": "Apply one bounded mechanical rename.",
        "route": {
            "automatic_tier": "local",
            "effective_tier": "local",
            "signals": [],
            "selected_override": "automatic",
            "overridden": false
        },
        "unified_diff": diff,
    });
    decode_patch_envelope(&envelope.to_string()).unwrap()
}

struct ScriptedPatchDispatcher {
    responses: Mutex<VecDeque<Result<PatchCandidate, LocalPatchDraftError>>>,
    prompts: Mutex<Vec<String>>,
}

impl ScriptedPatchDispatcher {
    fn new(responses: Vec<Result<PatchCandidate, LocalPatchDraftError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            prompts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl LocalPatchDraftDispatch for ScriptedPatchDispatcher {
    fn backend_name(&self) -> &str {
        "scripted-local-patch"
    }

    fn model(&self) -> &str {
        "scripted-patch-model"
    }

    async fn draft(&self, prompt: String) -> Result<PatchCandidate, LocalPatchDraftError> {
        self.prompts.lock().unwrap().push(prompt);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(LocalPatchDraftError::MissingFinalContent))
    }
}

fn write_candidate_verifier(root: &Path, workspaces: &Path) -> String {
    #[cfg(windows)]
    {
        let script = root.join("candidate-verifier.ps1");
        let quoted = workspaces.display().to_string().replace('\'', "''");
        std::fs::write(
            &script,
            format!(
                "$workspaces = '{quoted}'\nAdd-Content -LiteralPath $workspaces -Value (Get-Location).Path\nif (Test-Path -LiteralPath 'candidate-poison.txt') {{ Write-Error 'prior candidate leaked'; exit 31 }}\n$content = Get-Content -Raw -LiteralPath 'src/lib.rs'\nif ($content -notmatch 'candidate-(one|two|three)') {{ Write-Error 'candidate edit missing'; exit 32 }}\nSet-Content -LiteralPath 'candidate-poison.txt' -Value 'workspace-local mutation'\n"
            ),
        )
        .unwrap();
        format!(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
            script.display()
        )
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let script = root.join("candidate-verifier.sh");
        let quoted = workspaces.display().to_string().replace('\'', "'\\''");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$PWD\" >> '{quoted}'\nif [ -f candidate-poison.txt ]; then echo 'prior candidate leaked' >&2; exit 31; fi\ngrep -Eq 'candidate-(one|two|three)' src/lib.rs || {{ echo 'candidate edit missing' >&2; exit 32; }}\nprintf workspace-local-mutation > candidate-poison.txt\n"
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        format!("\"{}\"", script.display())
    }
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Local mechanical edits use bounded best-of-N :: Local candidates are generated
#[test]
fn bounded_local_candidates_keep_indexed_generation_evidence_and_isolated_verifier_results() {
    let root = temp_dir("local-candidates");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn target_symbol() {}\n").unwrap();
    let workspaces = root.join("verifier-workspaces.txt");
    let dispatcher = ScriptedPatchDispatcher::new(vec![
        Ok(patch_candidate("candidate-one")),
        Ok(patch_candidate("candidate-two")),
        Ok(patch_candidate("candidate-three")),
    ]);
    let interrupt = Arc::new(AtomicBool::new(false));
    let generation = run_async_test(
        LocalPatchCandidateGenerator::new(
            &dispatcher,
            candidate_settings(3),
            Arc::clone(&interrupt),
        )
        .generate("Apply the selected mechanical edit."),
    );

    assert_eq!(generation.candidates.len(), 3);
    assert!(
        generation
            .candidates
            .iter()
            .all(|candidate| matches!(candidate, LocalPatchCandidateGeneration::Completed(_)))
    );
    assert_eq!(
        generation
            .candidates
            .iter()
            .map(|candidate| candidate.evidence().index)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let prompts = dispatcher.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 3);
    for (index, prompt) in prompts.iter().enumerate() {
        assert!(prompt.contains(&format!("local-patch-candidate-{}-of-3", index + 1)));
    }

    let verifier = LocalPatchCandidateVerifier::new(
        root.clone(),
        vec!["src/lib.rs".to_string()],
        vec![write_candidate_verifier(&root, &workspaces)],
        interrupt,
    );
    let verification = run_async_test(verifier.verify(&generation));

    assert_eq!(verification.candidates.len(), 3);
    assert!(verification.candidates.iter().all(|candidate| {
        candidate.changed_line_count == 2
            && matches!(
                candidate.outcome,
                LocalCandidateVerificationOutcome::Verified { ref report }
                    if report.eligibility.eligible
            )
    }));
    let paths = std::fs::read_to_string(&workspaces).unwrap();
    let paths = paths.lines().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(paths.len(), 3);
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "pub fn target_symbol() {}\n"
    );
    assert!(!root.join("candidate-poison.txt").exists());
    std::fs::remove_dir_all(root).ok();
}

fn candidate_verification(
    index: u8,
    changed_line_count: usize,
    eligible: bool,
) -> LocalCandidateVerification {
    LocalCandidateVerification {
        candidate: LocalPatchCandidate {
            evidence: LocalCandidateGenerationEvidence {
                index,
                diversity_hint: format!("candidate-{index}"),
                backend: "fixture-local".to_string(),
                model: "fixture-model".to_string(),
            },
            patch: patch_candidate(&format!("candidate-{index}")),
        },
        changed_line_count,
        outcome: LocalCandidateVerificationOutcome::Verified {
            report: VerifierReport {
                patch_gates: Vec::new(),
                gates: Vec::new(),
                stopped_after_failure: false,
                first_failed_gate: None,
                eligibility: if eligible {
                    CandidateEligibility::eligible()
                } else {
                    CandidateEligibility::ineligible(
                        deepseek_custom::procedure::CandidateIneligibility::VerifierCommandFailed {
                            index: 0,
                            command: "fixture verifier".to_string(),
                            disposition:
                                deepseek_custom::procedure::VerifierCommandDisposition::Failed,
                        },
                    )
                },
                terminal_disposition: None,
            },
        },
    }
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Passing candidates are selected deterministically :: Two candidates pass
#[test]
fn passing_candidate_with_fewer_changed_lines_is_selected() {
    let verification = LocalCandidateVerificationRun {
        candidates: vec![
            candidate_verification(1, 8, true),
            candidate_verification(2, 4, true),
            candidate_verification(3, 1, false),
        ],
    };

    let resolution = select_passing_local_candidate(&verification);

    assert!(matches!(
        resolution,
        LocalPatchCandidateResolution::Selected(candidate)
            if candidate.candidate.evidence.index == 2 && candidate.changed_line_count == 4
    ));
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Passing candidates are selected deterministically :: Passing patches have equal size
#[test]
fn equal_size_passing_candidates_use_the_lowest_generation_index() {
    let verification = LocalCandidateVerificationRun {
        candidates: vec![
            candidate_verification(4, 6, true),
            candidate_verification(2, 6, true),
            candidate_verification(3, 6, true),
        ],
    };

    let resolution = select_passing_local_candidate(&verification);

    assert!(matches!(
        resolution,
        LocalPatchCandidateResolution::Selected(candidate)
            if candidate.candidate.evidence.index == 2 && candidate.changed_line_count == 6
    ));
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Local mechanical edits use bounded best-of-N :: No local candidate passes
#[test]
fn failed_candidates_enter_the_existing_bounded_repair_policy_without_budget_changes() {
    let verification = LocalCandidateVerificationRun {
        candidates: vec![
            candidate_verification(1, 2, false),
            candidate_verification(2, 4, false),
            candidate_verification(3, 6, false),
        ],
    };
    let policy = Settings {
        procedure: Some(ProcedureSettings {
            structural_retries: 1,
            local_verifier_attempts: 2,
            frontier_attempts: 1,
            frontier_patch_backend: Some("fixture-frontier".to_string()),
            ..ProcedureSettings::default()
        }),
        backends: Some(HashMap::from([(
            "fixture-frontier".to_string(),
            BackendConfig::CodexCli {
                model: "fixture-frontier-model".to_string(),
                sandbox: Some("workspace-write".to_string()),
                env: None,
                models: None,
            },
        )])),
        ..Settings::default()
    }
    .validated_procedure_repair_policy()
    .unwrap();

    let resolution = select_passing_local_candidate(&verification);
    let began_with = begin_existing_bounded_repair(&resolution, || policy.clone());

    assert_eq!(
        resolution,
        LocalPatchCandidateResolution::BeginExistingBoundedRepair
    );
    assert_eq!(began_with, Some(policy));
}
