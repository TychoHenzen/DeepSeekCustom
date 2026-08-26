use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use deepseek_custom::config::settings::{ProcedureSettings, Settings};
use deepseek_custom::procedure::{
    LocalizationDispatch, LocalizationDispatchError, LocalizationEnvelope,
    LocalizationSampleOutcome, LocalizationSampler, LocalizationTarget,
    NormalizedLocalizationTarget, NormalizedLocalizationTargets, OpenSpecInput,
    ProcedureAttemptDisposition, ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun,
    ProcedureRunId, ProcedureScratchpad, ProcedureStage, ProcedureTask,
    ProcedureTerminalDisposition, RepositoryIndexEntry, SamplingInputGate, SamplingInputRequest,
    sha256_json,
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
    Settings {
        procedure: Some(ProcedureSettings {
            localization_sample_count: count,
            localization_agreement_quorum: 2,
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

#[derive(Clone)]
struct CountingDispatcher {
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
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

async fn configured_localization_sample_count_is_attempted_within_the_fixed_cap_case(
) -> Vec<SamplingCountObservation> {
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
    let observations =
        run_async_test(configured_localization_sample_count_is_attempted_within_the_fixed_cap_case());

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
