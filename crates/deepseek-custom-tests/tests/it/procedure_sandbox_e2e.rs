use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepseek_custom::config::settings::RepositoryIndexLimits;
use deepseek_custom::procedure::{
    ContractSelection, FrontierPatchDraftError, FrontierRepairDispatch, FrontierRepairRequest,
    LocalPatchDraftDispatch, LocalPatchDraftError, LocalizationDispatch, LocalizationDispatchError,
    LocalizationEnvelope, OpenSpecInput, OpenSpecInputError, PatchCandidate, RepositoryIndexEntry,
    ValidatedContractInput, VerifierCommandRunner, VerifierRun, VerifierRunProgress,
    build_repository_index,
};

const CHANGE_ID: &str = "sandbox-change";
const TASK_ID: &str = "1.1";
const TARGET_PATH: &str = "src/lib.rs";
const UNRELATED_PATH: &str = "notes.txt";
const TARGET_SOURCE: &str = "pub fn target_symbol() -> &'static str {\n    \"before\"\n}\n";
const UNRELATED_BYTES: &[u8] = b"unrelated fixture bytes\n";
const COVERS: &str =
    "deepseek-custom/procedure-sandbox-e2e-test :: Apply fixture edit :: Target is updated";

#[derive(Clone, Copy)]
enum RequiredArtifact {
    Proposal,
    Tasks,
    Spec,
}

#[derive(Default)]
struct PreflightActivity {
    localization: AtomicUsize,
    patch: AtomicUsize,
    verifier: AtomicUsize,
    promotion: AtomicUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordedEvent {
    ValidationCompleted,
    LocalizationDispatched,
    PatchDispatched,
    FrontierDispatched,
    VerifierStarted,
    VerifierCompleted,
    PromotionObserved,
}

#[derive(Default)]
struct RecordingCallCounters {
    localization: AtomicUsize,
    patch: AtomicUsize,
    frontier: AtomicUsize,
    verifier: AtomicUsize,
}

#[derive(Clone, Default)]
struct RecordingState {
    events: Arc<Mutex<Vec<RecordedEvent>>>,
    calls: Arc<RecordingCallCounters>,
}

impl RecordingState {
    fn record(&self, event: RecordedEvent) {
        self.events.lock().unwrap().push(event);
    }

    fn events(&self) -> Vec<RecordedEvent> {
        self.events.lock().unwrap().clone()
    }

    fn counts(&self) -> [usize; 4] {
        [
            self.calls.localization.load(Ordering::SeqCst),
            self.calls.patch.load(Ordering::SeqCst),
            self.calls.frontier.load(Ordering::SeqCst),
            self.calls.verifier.load(Ordering::SeqCst),
        ]
    }
}

struct RecordingLocalization {
    state: RecordingState,
    responses: Mutex<VecDeque<Result<LocalizationEnvelope, LocalizationDispatchError>>>,
}

impl RecordingLocalization {
    fn new(
        state: RecordingState,
        responses: impl IntoIterator<Item = Result<LocalizationEnvelope, LocalizationDispatchError>>,
    ) -> Self {
        Self {
            state,
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }
}

#[async_trait]
impl LocalizationDispatch for RecordingLocalization {
    fn backend_name(&self) -> &str {
        "recording-localizer"
    }

    fn model(&self) -> &str {
        "fixture-model"
    }

    async fn dispatch_prompt(
        &self,
        _prompt: String,
        _repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
        self.state.calls.localization.fetch_add(1, Ordering::SeqCst);
        self.state.record(RecordedEvent::LocalizationDispatched);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("localization calls must stay inside the fixture response bound")
    }
}

struct RecordingPatch {
    state: RecordingState,
    responses: Mutex<VecDeque<Result<PatchCandidate, LocalPatchDraftError>>>,
}

impl RecordingPatch {
    fn new(
        state: RecordingState,
        responses: impl IntoIterator<Item = Result<PatchCandidate, LocalPatchDraftError>>,
    ) -> Self {
        Self {
            state,
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }
}

#[async_trait]
impl LocalPatchDraftDispatch for RecordingPatch {
    fn backend_name(&self) -> &str {
        "recording-patch"
    }

    fn model(&self) -> &str {
        "fixture-model"
    }

    async fn draft(&self, _prompt: String) -> Result<PatchCandidate, LocalPatchDraftError> {
        self.state.calls.patch.fetch_add(1, Ordering::SeqCst);
        self.state.record(RecordedEvent::PatchDispatched);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("patch calls must stay inside the fixture response bound")
    }
}

struct RecordingFrontier {
    state: RecordingState,
    responses: Mutex<VecDeque<Result<PatchCandidate, FrontierPatchDraftError>>>,
}

impl RecordingFrontier {
    fn new(
        state: RecordingState,
        responses: impl IntoIterator<Item = Result<PatchCandidate, FrontierPatchDraftError>>,
    ) -> Self {
        Self {
            state,
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }
}

#[async_trait]
impl FrontierRepairDispatch for RecordingFrontier {
    fn model(&self, _backend: &str) -> String {
        "fixture-frontier-model".to_string()
    }

    async fn draft(
        &self,
        _request: &FrontierRepairRequest,
    ) -> Result<PatchCandidate, FrontierPatchDraftError> {
        self.state.calls.frontier.fetch_add(1, Ordering::SeqCst);
        self.state.record(RecordedEvent::FrontierDispatched);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("frontier calls must stay inside the fixture response bound")
    }
}

struct RecordingVerifier {
    state: RecordingState,
}

impl RecordingVerifier {
    fn new(state: RecordingState) -> Self {
        Self { state }
    }

    async fn run(&self, workspace: &Path, commands: &[String]) -> VerifierRun {
        let state = self.state.clone();
        VerifierCommandRunner::new()
            .run_with_progress(workspace, commands, move |progress| match progress {
                VerifierRunProgress::GateStarted { .. } => {
                    state.calls.verifier.fetch_add(1, Ordering::SeqCst);
                    state.record(RecordedEvent::VerifierStarted);
                }
                VerifierRunProgress::GateCompleted { .. } => {
                    state.record(RecordedEvent::VerifierCompleted);
                }
            })
            .await
    }
}

impl PreflightActivity {
    fn record_downstream_execution(&self) {
        self.localization.fetch_add(1, Ordering::SeqCst);
        self.patch.fetch_add(1, Ordering::SeqCst);
        self.verifier.fetch_add(1, Ordering::SeqCst);
        self.promotion.fetch_add(1, Ordering::SeqCst);
    }

    fn counts(&self) -> [usize; 4] {
        [
            self.localization.load(Ordering::SeqCst),
            self.patch.load(Ordering::SeqCst),
            self.verifier.load(Ordering::SeqCst),
            self.promotion.load(Ordering::SeqCst),
        ]
    }
}

/// A disposable project that follows the production OpenSpec filesystem shape.
///
/// Assumption: this module is registered in `tests/it/main.rs` during task 1.1
/// because `autotests = false`; task 5.1 still owns documentation of its command.
struct SandboxFixture {
    root: PathBuf,
    openspec_command: PathBuf,
}

impl SandboxFixture {
    fn new(tag: &str) -> Self {
        let root = super::scratch_dir("procedure-sandbox-e2e", tag);
        let change_dir = root.join("openspec").join("changes").join(CHANGE_ID);
        let spec_dir = change_dir
            .join("specs")
            .join("deepseek-custom")
            .join("procedure-sandbox-e2e-test");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"procedure-sandbox-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(root.join(TARGET_PATH), TARGET_SOURCE).unwrap();
        std::fs::write(root.join(UNRELATED_PATH), UNRELATED_BYTES).unwrap();
        std::fs::write(
            change_dir.join("proposal.md"),
            "## Why\n\nProve the isolated procedure lifecycle.\n\n## What Changes\n\n- Update the fixture target.\n",
        )
        .unwrap();
        std::fs::write(
            change_dir.join("tasks.md"),
            format!("- [ ] {TASK_ID} Update the fixture target\n  <!-- covers: {COVERS} -->\n"),
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("spec.md"),
            "## Purpose\n\nDefine the fixture edit.\n\n## ADDED Requirements\n\n### Requirement: Apply fixture edit\nThe procedure SHALL update the fixture target.\n\n#### Scenario: Target is updated\n- **WHEN** the fixture procedure runs\n- **THEN** only the target is updated\n",
        )
        .unwrap();
        let openspec_command = write_strict_openspec_fixture(&root);
        Self {
            root,
            openspec_command,
        }
    }

    fn input(&self) -> OpenSpecInput {
        OpenSpecInput::with_command(&self.root, self.openspec_command.display().to_string())
    }

    fn preflight(&self) -> ValidatedContractInput {
        self.input()
            .validate_and_select_task(CHANGE_ID, TASK_ID)
            .unwrap()
    }

    fn run_after_preflight<T>(
        &self,
        after_preflight: impl FnOnce(ValidatedContractInput) -> T,
    ) -> Result<T, OpenSpecInputError> {
        let validated = self.input().validate_and_select_task(CHANGE_ID, TASK_ID)?;
        Ok(after_preflight(validated))
    }

    fn index(&self) -> Vec<RepositoryIndexEntry> {
        build_repository_index(&self.root, &RepositoryIndexLimits::default()).unwrap()
    }

    fn remove(&self, artifact: RequiredArtifact) {
        std::fs::remove_file(self.artifact_path(artifact)).unwrap();
    }

    fn corrupt(&self, artifact: RequiredArtifact) {
        let path = self.artifact_path(artifact);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
    }

    fn artifact_path(&self, artifact: RequiredArtifact) -> PathBuf {
        let change_dir = self.root.join("openspec").join("changes").join(CHANGE_ID);
        match artifact {
            RequiredArtifact::Proposal => change_dir.join("proposal.md"),
            RequiredArtifact::Tasks => change_dir.join("tasks.md"),
            RequiredArtifact::Spec => change_dir
                .join("specs")
                .join("deepseek-custom")
                .join("procedure-sandbox-e2e-test")
                .join("spec.md"),
        }
    }
}

impl Drop for SandboxFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn write_strict_openspec_fixture(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = root.join("fixture-openspec.cmd");
        std::fs::write(
            &path,
            concat!(
                "@echo off\r\n",
                "if not \"%~1\"==\"validate\" (echo strict fixture validation failed: expected validate 1>&2 & exit /b 64)\r\n",
                "if not \"%~3\"==\"--strict\" (echo strict fixture validation failed: expected --strict 1>&2 & exit /b 64)\r\n",
                "if not \"%~4\"==\"--no-interactive\" (echo strict fixture validation failed: expected --no-interactive 1>&2 & exit /b 64)\r\n",
                "set \"change_dir=openspec\\changes\\%~2\"\r\n",
                "if not exist \"%change_dir%\\proposal.md\" (echo strict fixture validation failed: missing proposal.md 1>&2 & exit /b 17)\r\n",
                "if not exist \"%change_dir%\\tasks.md\" (echo strict fixture validation failed: missing tasks.md 1>&2 & exit /b 18)\r\n",
                "if not exist \"%change_dir%\\specs\\deepseek-custom\\procedure-sandbox-e2e-test\\spec.md\" (echo strict fixture validation failed: missing spec.md 1>&2 & exit /b 19)\r\n",
                "if exist \"%change_dir%\\proposal.md\\*\" (echo strict fixture validation failed: invalid proposal.md 1>&2 & exit /b 27)\r\n",
                "if exist \"%change_dir%\\tasks.md\\*\" (echo strict fixture validation failed: invalid tasks.md 1>&2 & exit /b 28)\r\n",
                "if exist \"%change_dir%\\specs\\deepseek-custom\\procedure-sandbox-e2e-test\\spec.md\\*\" (echo strict fixture validation failed: invalid spec.md 1>&2 & exit /b 29)\r\n",
                "echo strict fixture validation passed: %~2\r\n",
                "exit /b 0\r\n",
            ),
        )
        .unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fixture-openspec");
        std::fs::write(
            &path,
            concat!(
                "#!/bin/sh\n",
                "[ \"$1\" = validate ] || { echo 'strict fixture validation failed: expected validate' >&2; exit 64; }\n",
                "[ \"$3\" = --strict ] || { echo 'strict fixture validation failed: expected --strict' >&2; exit 64; }\n",
                "[ \"$4\" = --no-interactive ] || { echo 'strict fixture validation failed: expected --no-interactive' >&2; exit 64; }\n",
                "change_dir=openspec/changes/$2\n",
                "[ -e \"$change_dir/proposal.md\" ] || { echo 'strict fixture validation failed: missing proposal.md' >&2; exit 17; }\n",
                "[ -e \"$change_dir/tasks.md\" ] || { echo 'strict fixture validation failed: missing tasks.md' >&2; exit 18; }\n",
                "[ -e \"$change_dir/specs/deepseek-custom/procedure-sandbox-e2e-test/spec.md\" ] || { echo 'strict fixture validation failed: missing spec.md' >&2; exit 19; }\n",
                "[ -f \"$change_dir/proposal.md\" ] || { echo 'strict fixture validation failed: invalid proposal.md' >&2; exit 27; }\n",
                "[ -f \"$change_dir/tasks.md\" ] || { echo 'strict fixture validation failed: invalid tasks.md' >&2; exit 28; }\n",
                "[ -f \"$change_dir/specs/deepseek-custom/procedure-sandbox-e2e-test/spec.md\" ] || { echo 'strict fixture validation failed: invalid spec.md' >&2; exit 29; }\n",
                "echo \"strict fixture validation passed: $2\"\n",
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn write_passing_verifier(root: &Path) -> String {
    #[cfg(windows)]
    {
        let path = root.join("fixture-verifier.cmd");
        std::fs::write(&path, "@echo off\r\nexit /b 0\r\n").unwrap();
        format!("\"{}\"", path.display())
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fixture-verifier");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        format!("\"{}\"", path.display())
    }
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance fixture uses a valid isolated proposal :: Minimal proposal passes the preflight gate
#[test]
fn valid_isolated_proposal_passes_preflight_and_indexes_fixture_files() {
    let fixture = SandboxFixture::new("valid-preflight");

    let validated = fixture.preflight();
    let index = fixture.index();

    assert_eq!(validated.validation.exit_code, Some(0));
    assert_eq!(
        validated.validation.stdout.trim(),
        format!("strict fixture validation passed: {CHANGE_ID}")
    );
    assert_eq!(validated.contract.change_id, CHANGE_ID);
    assert_eq!(validated.contract.task.id, TASK_ID);
    assert_eq!(validated.contract.task.covers.as_deref(), Some(COVERS));
    assert!(matches!(
        validated.contract.selection,
        ContractSelection::Bound { ref capability, ref requirement }
            if capability == "deepseek-custom/procedure-sandbox-e2e-test"
                && requirement.name == "Apply fixture edit"
                && requirement.scenarios.len() == 1
                && requirement.scenarios[0].name == "Target is updated"
    ));
    assert!(index.iter().any(|entry| {
        entry.path == TARGET_PATH && entry.symbols == ["target_symbol".to_string()]
    }));
    assert!(index.iter().any(|entry| entry.path == UNRELATED_PATH));
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance test is runnable without live model services :: The test runs offline
#[test]
fn fixture_preflight_uses_only_the_local_deterministic_validator() {
    let fixture = SandboxFixture::new("offline-preflight");

    let first = fixture.preflight();
    let second = fixture.preflight();

    assert_eq!(first.validation.exit_code, Some(0));
    assert_eq!(first.validation.stdout, second.validation.stdout);
    assert_eq!(first.contract, second.contract);
    assert!(
        first
            .validation
            .command
            .iter()
            .any(|argument| Path::new(argument) == fixture.openspec_command)
    );
    assert_eq!(
        &first.validation.command[first.validation.command.len() - 4..],
        ["validate", CHANGE_ID, "--strict", "--no-interactive"]
    );
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance fixture uses a valid isolated proposal :: Invalid proposal stops before execution
#[test]
fn invalid_required_artifacts_report_exact_preflight_failure_before_downstream_work() {
    let cases = [
        (
            "missing-proposal",
            RequiredArtifact::Proposal,
            true,
            17,
            "strict fixture validation failed: missing proposal.md",
        ),
        (
            "corrupt-spec",
            RequiredArtifact::Spec,
            false,
            29,
            "strict fixture validation failed: invalid spec.md",
        ),
        (
            "corrupt-tasks",
            RequiredArtifact::Tasks,
            false,
            28,
            "strict fixture validation failed: invalid tasks.md",
        ),
    ];

    for (tag, artifact, remove, exit_code, expected_stderr) in cases {
        let fixture = SandboxFixture::new(tag);
        if remove {
            fixture.remove(artifact);
        } else {
            fixture.corrupt(artifact);
        }
        let activity = PreflightActivity::default();

        let error = fixture
            .run_after_preflight(|_| activity.record_downstream_execution())
            .unwrap_err();

        match error {
            OpenSpecInputError::ValidationFailed(failure) => {
                assert_eq!(failure.exit_code, Some(exit_code));
                assert_eq!(failure.stdout, "");
                assert_eq!(failure.stderr.trim(), expected_stderr);
            }
            other => panic!("expected strict validation failure, got {other:?}"),
        }
        assert_eq!(activity.counts(), [0, 0, 0, 0]);
        assert_eq!(
            std::fs::read(fixture.root.join(TARGET_PATH)).unwrap(),
            TARGET_SOURCE.as_bytes()
        );
        assert_eq!(
            std::fs::read(fixture.root.join(UNRELATED_PATH)).unwrap(),
            UNRELATED_BYTES
        );
    }
}

async fn assert_recording_seams_share_ordered_events_and_bounded_call_counters_offline() {
    let fixture = SandboxFixture::new("recording-seams");
    let state = RecordingState::default();
    let localization = RecordingLocalization::new(
        state.clone(),
        [Err(LocalizationDispatchError::Request {
            backend: "fixture-localizer".to_string(),
            reason: "placeholder response reserved for task 2.2".to_string(),
        })],
    );
    let patch = RecordingPatch::new(
        state.clone(),
        [Err(LocalPatchDraftError::MissingFinalContent)],
    );
    let _frontier = RecordingFrontier::new(
        state.clone(),
        std::iter::empty::<Result<PatchCandidate, FrontierPatchDraftError>>(),
    );
    let verifier = RecordingVerifier::new(state.clone());

    fixture.preflight();
    state.record(RecordedEvent::ValidationCompleted);
    localization
        .dispatch_prompt("bounded fixture prompt".to_string(), &fixture.index())
        .await
        .unwrap_err();
    patch
        .draft("bounded fixture patch prompt".to_string())
        .await
        .unwrap_err();
    let verifier_run = verifier
        .run(&fixture.root, &[write_passing_verifier(&fixture.root)])
        .await;
    assert!(verifier_run.commands.iter().all(|command| command.success));
    state.record(RecordedEvent::PromotionObserved);

    assert_eq!(
        state.events(),
        [
            RecordedEvent::ValidationCompleted,
            RecordedEvent::LocalizationDispatched,
            RecordedEvent::PatchDispatched,
            RecordedEvent::VerifierStarted,
            RecordedEvent::VerifierCompleted,
            RecordedEvent::PromotionObserved,
        ]
    );
    assert_eq!(state.counts(), [1, 1, 0, 1]);
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: A passing fixture proves the complete procedure lifecycle :: Stage order and dispatch boundaries are recorded
#[test]
fn recording_seams_record_stage_order_and_dispatch_boundaries() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(assert_recording_seams_share_ordered_events_and_bounded_call_counters_offline());
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance test is runnable without live model services :: The test runs offline
#[tokio::test]
async fn recording_seams_share_ordered_events_and_bounded_call_counters_offline() {
    assert_recording_seams_share_ordered_events_and_bounded_call_counters_offline().await;
}
