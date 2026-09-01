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
    ProcedureCandidateMetric, ProcedureReportRepository, ProcedureReviewDisposition, ProcedureRun,
    ProcedureRunId, ProcedureRunMetrics, ProcedureScratchpad, ProcedureStage, ProcedureStageTiming,
    ProcedureTask, ProcedureTerminalDisposition, PromotionBaseline, RepositoryIndexEntry,
    RouteOverride, RouteTier, SampledProcedureOutcome, SampledProcedureRequest,
    SampledProcedureRunner, SamplingInputGate, SamplingInputRequest, VerifierReport,
    WholeChangeProcedureOutcome, WholeChangeProcedureRequest, WholeChangeProcedureRunner,
    apply_patch_in_workspace, begin_existing_bounded_repair, decode_patch_envelope,
    model_promotion_targets, promote_verified_workspace, select_localization_agreement,
    select_passing_local_candidate, sha256_json, validate_patch_boundary,
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
    let reports = ProcedureReportRepository::for_project(root);
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
    prompts: Arc<Mutex<Vec<String>>>,
}

impl ScriptedDispatcher {
    fn new(
        responses: impl IntoIterator<Item = Result<LocalizationEnvelope, LocalizationDispatchError>>,
    ) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            prompts: Arc::new(Mutex::new(Vec::new())),
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
        prompt: String,
        _repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(prompt);
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

// covers: deepseek-custom/routing-sampling-and-metrics :: The completed procedure remains bounded end to end :: Local end-to-end success
#[test]
fn approved_local_quorum_and_verified_candidate_promote_without_frontier_dispatch() {
    let root = temp_dir("completed-local-success");
    let command = write_fixture(&root);
    let reports = ProcedureReportRepository::for_project(&root);
    let input = approved_sampling_input(&root, &command);
    let interrupt = Arc::new(AtomicBool::new(false));
    let local = ScriptedDispatcher::new([
        envelope(vec![target(
            "src/lib.rs",
            Some("target_symbol"),
            "sample one",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("target_symbol"),
            "sample two",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("target_symbol"),
            "sample three",
        )]),
    ]);
    let frontier = ScriptedDispatcher::new([]);
    let resolver = LocalizationAgreementResolver::new(
        LocalizationSampler::new(local.clone(), sampling_settings(3), Arc::clone(&interrupt)),
        frontier.clone(),
    );
    let agreement = run_async_test(resolver.resolve(&input, &index())).unwrap();
    let LocalizationAgreementOutcome::Local { agreement } = agreement.outcome else {
        panic!("matching local samples must not dispatch frontier localization");
    };
    assert_eq!(agreement.sample_numbers, [1, 2, 3]);
    assert_eq!(local.calls.load(Ordering::SeqCst), 3);
    assert_eq!(frontier.calls.load(Ordering::SeqCst), 0);

    let patch_dispatcher = ScriptedPatchDispatcher::new(vec![
        Ok(patch_candidate("candidate-one")),
        Ok(patch_candidate("candidate-two")),
        Ok(patch_candidate("candidate-three")),
    ]);
    let generation = run_async_test(
        LocalPatchCandidateGenerator::new(
            &patch_dispatcher,
            candidate_settings(3),
            Arc::clone(&interrupt),
        )
        .generate("Apply the selected mechanical edit."),
    );
    let verifier_command = write_candidate_verifier(&root, &root.join("candidate-workspaces.txt"));
    let verification = run_async_test(
        LocalPatchCandidateVerifier::new(
            root.clone(),
            vec!["src/lib.rs".to_string()],
            vec![verifier_command],
            Arc::clone(&interrupt),
        )
        .verify(&generation),
    );
    let LocalPatchCandidateResolution::Selected(selected) =
        select_passing_local_candidate(&verification)
    else {
        panic!("one verified local candidate must be selected");
    };
    assert_eq!(selected.candidate.evidence.index, 1);

    let boundary = validate_patch_boundary(
        selected.candidate.patch.clone(),
        &["src/lib.rs".to_string()],
    )
    .unwrap();
    let promotion_targets = model_promotion_targets(&boundary).unwrap();
    let baseline = PromotionBaseline::capture(&root, &promotion_targets).unwrap();
    let applied = apply_patch_in_workspace(&root, boundary).unwrap();
    let promotion =
        promote_verified_workspace(&root, applied.path(), &baseline, &promotion_targets).unwrap();
    applied.close().unwrap();
    assert!(promotion.baseline.can_promote());
    assert!(
        std::fs::read_to_string(root.join("src/lib.rs"))
            .unwrap()
            .contains("candidate-one")
    );

    let mut metrics = ProcedureRunMetrics::from_terminal_run(&input.report).unwrap();
    metrics.stage_timings.extend([
        ProcedureStageTiming {
            stage: "agreement_sampling".to_string(),
            duration_ms: 1,
        },
        ProcedureStageTiming {
            stage: "candidate_verification".to_string(),
            duration_ms: 1,
        },
        ProcedureStageTiming {
            stage: "promotion".to_string(),
            duration_ms: 1,
        },
    ]);
    metrics.route.selected_tier = Some(RouteTier::Local);
    metrics.route.local_mechanical_success = Some(true);
    metrics.candidates = verification
        .candidates
        .iter()
        .map(|candidate| ProcedureCandidateMetric {
            index: candidate.candidate.evidence.index,
            changed_line_count: Some(candidate.changed_line_count),
            verifier_passed: Some(matches!(
                candidate.outcome,
                LocalCandidateVerificationOutcome::Verified { ref report } if report.eligibility.eligible
            )),
        })
        .collect();
    reports.replace_metrics(&input.report.id, &metrics).unwrap();
    let stored = reports.load_with_fingerprints(&input.report.id).unwrap();
    let recorded = stored.metrics.expect("completed local run records metrics");
    assert_eq!(recorded.route.selected_tier, Some(RouteTier::Local));
    assert_eq!(recorded.route.local_mechanical_success, Some(true));
    assert!(recorded.route.escalation_triggers.is_empty());
    assert_eq!(recorded.candidates.len(), 3);
    assert!(
        recorded
            .stage_timings
            .iter()
            .any(|timing| timing.stage == "agreement_sampling")
    );
    assert!(
        recorded
            .stage_timings
            .iter()
            .any(|timing| timing.stage == "candidate_verification")
    );
    assert!(
        recorded
            .stage_timings
            .iter()
            .any(|timing| timing.stage == "promotion")
    );

    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/routing-sampling-and-metrics :: The completed procedure remains bounded end to end :: Local end-to-end success
#[test]
fn sampled_procedure_runner_composes_the_approved_input_and_local_promotion_path() {
    let root = temp_dir("sampled-runner-local-success");
    let command = write_fixture(&root);
    let input = approved_sampling_input(&root, &command);
    let reports = ProcedureReportRepository::for_project(&root);
    let interrupt = Arc::new(AtomicBool::new(false));
    let local = ScriptedDispatcher::new([
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "first")]),
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "second")]),
        envelope(vec![target("src/lib.rs", Some("target_symbol"), "third")]),
    ]);
    let frontier = ScriptedDispatcher::new(std::iter::empty());
    let sampling = sampling_settings(3);
    let resolver = LocalizationAgreementResolver::new(
        LocalizationSampler::new(local.clone(), sampling.clone(), Arc::clone(&interrupt)),
        frontier.clone(),
    );
    let runner = SampledProcedureRunner::new(
        SamplingInputGate::new(
            OpenSpecInput::with_command(&root, command.display().to_string()),
            root.clone(),
            reports.clone(),
        ),
        root.clone(),
        ProcedureSettings::default().repository_index,
        resolver,
        sampling,
        reports.clone(),
        Arc::clone(&interrupt),
    );
    let patch = ScriptedPatchDispatcher::new(vec![
        Ok(patch_candidate("candidate-one")),
        Ok(patch_candidate("candidate-two")),
        Ok(patch_candidate("candidate-three")),
    ]);
    let verifier = write_candidate_verifier(&root, &root.join("sampled-runner-workspaces.txt"));

    let outcome = run_async_test(runner.run(
        SampledProcedureRequest {
            baseline_localization_run_id: input.report.id,
            change_id: "fixture-change".to_string(),
            task_id: "1.1".to_string(),
            route_override: deepseek_custom::procedure::RouteOverride::ForceLocal,
        },
        &patch,
        &[verifier],
        None,
    ))
    .unwrap();

    assert_eq!(
        outcome,
        SampledProcedureOutcome::Promoted { candidate_index: 1 }
    );
    assert_eq!(local.calls.load(Ordering::SeqCst), 3);
    assert_eq!(frontier.calls.load(Ordering::SeqCst), 0);
    assert!(
        std::fs::read_to_string(root.join("src/lib.rs"))
            .unwrap()
            .contains("candidate-one")
    );
    let metrics = reports
        .load_with_fingerprints(&input.report.id)
        .unwrap()
        .metrics
        .unwrap();
    assert_eq!(metrics.route.selected_tier, Some(RouteTier::Local));
    assert_eq!(metrics.route.local_mechanical_success, Some(true));
    assert_eq!(metrics.candidates.len(), 3);
    assert!(
        metrics
            .stage_timings
            .iter()
            .any(|timing| timing.stage == "promotion")
    );
    std::fs::remove_dir_all(root).ok();
}

fn write_whole_change_fixture(root: &Path) -> PathBuf {
    let command = write_fake_openspec(root);
    let change = root.join("openspec/changes/fixture-change");
    let spec = change.join("specs/sample/capability");
    std::fs::create_dir_all(&spec).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn first() {}\n").unwrap();
    std::fs::write(
        change.join("proposal.md"),
        "## Why\n\nRun the whole change.\n\n## What Changes\n\n- Rename two functions.\n",
    )
    .unwrap();
    std::fs::write(
        change.join("tasks.md"),
        "- [ ] 1.1 Rename first\n  <!-- covers: sample/capability :: Rename first :: First renamed -->\n- [ ] 1.2 Rename second\n  <!-- covers: sample/capability :: Rename second :: Second renamed -->\n",
    )
    .unwrap();
    std::fs::write(
        spec.join("spec.md"),
        "## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Rename first\nThe system SHALL rename the first function.\n\n#### Scenario: First renamed\n- **WHEN** the first task runs\n- **THEN** it promotes its verified patch\n\n### Requirement: Rename second\nThe system SHALL rename the second function.\n\n#### Scenario: Second renamed\n- **WHEN** the next task runs\n- **THEN** it uses the changed repository\n",
    )
    .unwrap();
    command
}

fn patch_between(previous: &str, replacement: &str) -> PatchCandidate {
    let diff = format!(
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn {previous}() {{}}\n+pub fn {replacement}() {{}}\n"
    );
    decode_patch_envelope(
        &serde_json::json!({
            "targets": ["src/lib.rs"],
            "rationale": "Apply one verified rename.",
            "route": {
                "automatic_tier": "local",
                "effective_tier": "local",
                "signals": [],
                "selected_override": "automatic",
                "overridden": false
            },
            "unified_diff": diff,
        })
        .to_string(),
    )
    .unwrap()
}

fn passing_verifier_command() -> String {
    #[cfg(windows)]
    {
        "powershell.exe -NoProfile -Command \"exit 0\"".to_string()
    }
    #[cfg(not(windows))]
    {
        "true".to_string()
    }
}

fn whole_change_reports(root: &Path) -> Vec<serde_json::Value> {
    let reports = root.join(".deepseek/procedure-runs");
    let mut values = std::fs::read_dir(reports)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        })
        .collect::<Vec<serde_json::Value>>();
    values.sort_by_key(|value| value["selected_task"]["id"].to_string());
    values
}

// covers: deepseek-custom/routing-sampling-and-metrics :: The completed procedure remains bounded end to end :: Whole change runs sequentially
#[test]
fn whole_change_runner_approves_and_promotes_each_task_in_fresh_order() {
    let root = temp_dir("whole-change-success");
    let command = write_whole_change_fixture(&root);
    let interrupt = Arc::new(AtomicBool::new(false));
    let local = Arc::new(ScriptedDispatcher::new([
        envelope(vec![target("src/lib.rs", Some("first"), "localize first")]),
        envelope(vec![target(
            "src/lib.rs",
            Some("first"),
            "sample first one",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("first"),
            "sample first two",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("first"),
            "sample first three",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("second"),
            "localize second",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("second"),
            "sample second one",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("second"),
            "sample second two",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("second"),
            "sample second three",
        )]),
    ]));
    let frontier = Arc::new(ScriptedDispatcher::new(std::iter::empty()));
    let reports = ProcedureReportRepository::for_project(&root);
    let runner = WholeChangeProcedureRunner::new(
        OpenSpecInput::with_command(&root, command.display().to_string()),
        root.clone(),
        ProcedureSettings::default().repository_index,
        Arc::clone(&local),
        frontier,
        sampling_settings(3),
        reports,
        Arc::clone(&interrupt),
    );
    let patch = ScriptedPatchDispatcher::new(vec![
        Ok(patch_between("first", "second")),
        Ok(patch_between("first", "second")),
        Ok(patch_between("first", "second")),
        Ok(patch_between("second", "third")),
        Ok(patch_between("second", "third")),
        Ok(patch_between("second", "third")),
    ]);

    let outcome = run_async_test(runner.run(
        WholeChangeProcedureRequest {
            change_id: "fixture-change".to_string(),
            route_override: RouteOverride::ForceLocal,
        },
        &patch,
        &[passing_verifier_command()],
        None,
    ))
    .unwrap();

    assert_eq!(
        outcome,
        WholeChangeProcedureOutcome::Completed {
            task_ids: vec!["1.1".to_string(), "1.2".to_string()],
        }
    );
    assert_eq!(local.calls.load(Ordering::SeqCst), 8);
    assert!(
        local
            .prompts
            .lock()
            .unwrap()
            .iter()
            .any(|prompt| prompt.contains("second")),
        "the second task must be localized against the repository after the first promotion"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "pub fn third() {}\n"
    );
    let reports = whole_change_reports(&root);
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0]["selected_task"]["id"], "1.1");
    assert_eq!(reports[1]["selected_task"]["id"], "1.2");
    assert!(reports.iter().all(|report| {
        report["review_disposition"] == "approved"
            && report["terminal_disposition"]["status"] == "awaiting_review"
    }));
    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/routing-sampling-and-metrics :: The completed procedure remains bounded end to end :: Whole change stops at first failed task
#[test]
fn whole_change_runner_does_not_attempt_later_tasks_after_a_failed_task() {
    let root = temp_dir("whole-change-failure");
    let command = write_whole_change_fixture(&root);
    let interrupt = Arc::new(AtomicBool::new(false));
    let local = Arc::new(ScriptedDispatcher::new([
        envelope(vec![target("src/lib.rs", Some("first"), "localize first")]),
        envelope(vec![target(
            "src/lib.rs",
            Some("first"),
            "sample first one",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("first"),
            "sample first two",
        )]),
        envelope(vec![target(
            "src/lib.rs",
            Some("first"),
            "sample first three",
        )]),
    ]));
    let reports = ProcedureReportRepository::for_project(&root);
    let runner = WholeChangeProcedureRunner::new(
        OpenSpecInput::with_command(&root, command.display().to_string()),
        root.clone(),
        ProcedureSettings::default().repository_index,
        Arc::clone(&local),
        Arc::new(ScriptedDispatcher::new(std::iter::empty())),
        sampling_settings(3),
        reports,
        interrupt,
    );
    let patch = ScriptedPatchDispatcher::new(vec![
        Err(LocalPatchDraftError::MissingFinalContent),
        Err(LocalPatchDraftError::MissingFinalContent),
        Err(LocalPatchDraftError::MissingFinalContent),
    ]);

    let outcome = run_async_test(runner.run(
        WholeChangeProcedureRequest {
            change_id: "fixture-change".to_string(),
            route_override: RouteOverride::ForceLocal,
        },
        &patch,
        &[passing_verifier_command()],
        None,
    ))
    .unwrap();

    assert!(matches!(
        outcome,
        WholeChangeProcedureOutcome::Failed { ref task_id, .. } if task_id == "1.1"
    ));
    assert_eq!(local.calls.load(Ordering::SeqCst), 4);
    assert_eq!(whole_change_reports(&root).len(), 1);
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "pub fn first() {}\n"
    );
    std::fs::remove_dir_all(root).ok();
}
