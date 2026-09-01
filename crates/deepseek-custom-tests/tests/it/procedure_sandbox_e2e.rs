use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepseek_custom::config::settings::{ProcedureSettings, RepositoryIndexLimits, Settings};
use deepseek_custom::procedure::ProcedureRunCoordinatorParams;
use deepseek_custom::procedure::{
    ContractSelection, FrontierPatchDraftError, FrontierRepairDispatch, FrontierRepairRequest,
    LocalPatchDraftDispatch, LocalPatchDraftError, LocalizationDispatch, LocalizationDispatchError,
    LocalizationEnvelope, OpenSpecInput, OpenSpecInputError, PatchCandidate,
    ProcedureAttemptDisposition, ProcedureMetricsDisposition, ProcedureReportRepository,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureRunRequest, ProcedureRunner,
    ProcedureScratchpad, ProcedureTerminalDisposition, RepositoryIndexEntry, RouteOverride,
    RouteTier, SamplingInputError, SamplingInputGate, SamplingInputRequest, ValidatedContractInput,
    VerifierCommandRunner, VerifierRun, VerifierRunProgress, WholeChangeProcedureOutcome,
    WholeChangeProcedureRequest, WholeChangeProcedureRunner, build_repository_index,
    check_patch_applicability, validate_patch_boundary,
};

const CHANGE_ID: &str = "sandbox-change";
const TASK_ID: &str = "1.1";
const TARGET_PATH: &str = "src/lib.rs";
const UNRELATED_PATH: &str = "notes.txt";
const TARGET_SOURCE: &str = "pub fn target_symbol() -> &'static str { \"before\" }";

fn whole_change_localization<L>(
    fixture: &SandboxFixture,
    dispatcher: Arc<L>,
    interrupted: bool,
) -> ProcedureRunCoordinatorParams<Arc<L>> {
    ProcedureRunCoordinatorParams {
        input: fixture.input(),
        working_dir: fixture.root.clone(),
        index_limits: RepositoryIndexLimits::default(),
        dispatcher,
        reports: ProcedureReportRepository::for_project(&fixture.root),
        interrupt: Arc::new(AtomicBool::new(interrupted)),
    }
}
const UNRELATED_BYTES: &[u8] = b"unrelated fixture bytes\n";
const COVERS: &str =
    "deepseek-custom/procedure-sandbox-e2e-test :: Apply fixture edit :: Target is updated";
const VALID_LOCALIZATION_CAPTURE: &str =
    include_str!("fixtures/procedure_sandbox_valid_localization.json");
const INVALID_SYMBOL_LOCALIZATION_CAPTURE: &str =
    include_str!("fixtures/procedure_sandbox_invalid_symbol_localization.json");
const VALID_PATCH_CAPTURE: &str = include_str!("fixtures/procedure_sandbox_valid_patch.json");
const EXPECTED_TARGET_SOURCE: &str = "pub fn target_symbol() -> &'static str { \"after\" }";

fn captured_localization(capture: &str) -> LocalizationEnvelope {
    let document: serde_json::Value = serde_json::from_str(capture).unwrap();
    serde_json::from_value(document["response"].clone()).unwrap()
}

fn captured_patch() -> PatchCandidate {
    let document: serde_json::Value = serde_json::from_str(VALID_PATCH_CAPTURE).unwrap();
    deepseek_custom::procedure::decode_patch_envelope(&document["response"].to_string()).unwrap()
}

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
    event_log: Arc<Mutex<Option<PathBuf>>>,
}

impl RecordingState {
    fn with_event_log(path: PathBuf) -> Self {
        Self {
            event_log: Arc::new(Mutex::new(Some(path))),
            ..Self::default()
        }
    }

    fn record(&self, event: RecordedEvent) {
        self.events.lock().unwrap().push(event);
        if let Some(path) = self.event_log.lock().unwrap().as_ref() {
            use std::io::Write;

            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(log, "host|{event:?}").unwrap();
        }
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
            "## Why\n\nProve the isolated procedure lifecycle.\n\n## What Changes\n\n- Rename before to after using exactly this diff string: diff --git a/src/lib.rs b/src/lib.rs\\n--- a/src/lib.rs\\n+++ b/src/lib.rs\\n@@ -1 +1 @@\\n-pub fn target_symbol() -> &'static str { \"before\" }\\n\\\\ No newline at end of file\\n+pub fn target_symbol() -> &'static str { \"after\" }\\n\\\\ No newline at end of file\n",
        )
        .unwrap();
        std::fs::write(
            change_dir.join("tasks.md"),
            format!("- [ ] {TASK_ID} Rename before to after with a one-line @@ -1 +1 @@ replacement\n  <!-- covers: {COVERS} -->\n"),
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("spec.md"),
            "## Purpose\n\nDefine the fixture edit.\n\n## ADDED Requirements\n\n### Requirement: Apply fixture edit\nThe procedure SHALL rename before to after while preserving the one-line function form and missing final newline. The unified_diff SHALL equal `diff --git a/src/lib.rs b/src/lib.rs\\n--- a/src/lib.rs\\n+++ b/src/lib.rs\\n@@ -1 +1 @@\\n-pub fn target_symbol() -> &'static str { \"before\" }\\n\\\\ No newline at end of file\\n+pub fn target_symbol() -> &'static str { \"after\" }\\n\\\\ No newline at end of file`.\n\n#### Scenario: Target is updated\n- **WHEN** the fixture procedure runs\n- **THEN** the diff uses @@ -1 +1 @@ with exactly one removed line and one added line\n",
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

fn sampling_settings() -> deepseek_custom::config::settings::ValidatedProcedureSamplingSettings {
    Settings {
        procedure: Some(ProcedureSettings {
            localization_sample_count: 3,
            localization_agreement_quorum: 2,
            local_patch_candidate_count: 3,
            ..ProcedureSettings::default()
        }),
        ..Settings::default()
    }
    .validated_procedure_sampling_settings()
    .unwrap()
}

fn write_recording_passing_verifier(root: &Path, event_log: &Path) -> String {
    #[cfg(windows)]
    {
        let path = root.join("fixture-recording-verifier.cmd");
        std::fs::write(
            &path,
            format!(
                "@echo off\r\necho %CD%^|VerifierStarted>>\"{}\"\r\nif not exist src\\lib.rs exit /b 41\r\necho %CD%^|VerifierCompleted>>\"{}\"\r\nexit /b 0\r\n",
                event_log.display(),
                event_log.display(),
            ),
        )
        .unwrap();
        format!("\"{}\"", path.display())
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fixture-recording-verifier");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s|VerifierStarted\\n' \"$PWD\" >> \"{}\"\ntest -f src/lib.rs || exit 41\nprintf '%s|VerifierCompleted\\n' \"$PWD\" >> \"{}\"\n",
                event_log.display(),
                event_log.display(),
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        format!("\"{}\"", path.display())
    }
}

fn stable_workspace_files(root: &Path, event_log: &Path) -> BTreeMap<String, Vec<u8>> {
    fn collect(
        root: &Path,
        directory: &Path,
        event_log: &Path,
        files: &mut BTreeMap<String, Vec<u8>>,
    ) {
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
            if path == event_log || relative == ".deepseek" || relative.starts_with(".deepseek/") {
                continue;
            }
            if path.is_dir() {
                collect(root, &path, event_log, files);
            } else {
                files.insert(relative, std::fs::read(path).unwrap());
            }
        }
    }

    let mut files = BTreeMap::new();
    collect(root, root, event_log, &mut files);
    files
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn bounded_real_checkout_files() -> BTreeMap<String, Vec<u8>> {
    fn collect_source_files(
        workspace_root: &Path,
        directory: &Path,
        files: &mut BTreeMap<String, Vec<u8>>,
    ) {
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_source_files(workspace_root, &path, files);
            } else {
                let relative = path
                    .strip_prefix(workspace_root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(relative, std::fs::read(path).unwrap());
            }
        }
    }

    let root = workspace_root();
    let mut files = BTreeMap::new();
    for relative in [
        "Cargo.toml",
        "Cargo.lock",
        "settings.json",
        "crates/deepseek-custom-tests/tests/it/procedure_sandbox_e2e.rs",
    ] {
        files.insert(
            relative.to_string(),
            std::fs::read(root.join(relative)).unwrap(),
        );
    }
    collect_source_files(&root, &root.join("crates/deepseek-custom/src"), &mut files);
    assert!(files.contains_key("settings.json"));
    files
}

fn single_report_document(root: &Path) -> serde_json::Value {
    let reports = root.join(".deepseek/procedure-runs");
    let paths = std::fs::read_dir(reports)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 1);
    serde_json::from_str(&std::fs::read_to_string(&paths[0]).unwrap()).unwrap()
}

struct PassingFixtureRun {
    fixture: SandboxFixture,
    state: RecordingState,
    outcome: WholeChangeProcedureOutcome,
    before_files: BTreeMap<String, Vec<u8>>,
    after_files: BTreeMap<String, Vec<u8>>,
    real_checkout_before: BTreeMap<String, Vec<u8>>,
    real_checkout_after: BTreeMap<String, Vec<u8>>,
    report: serde_json::Value,
    event_lines: Vec<String>,
}

async fn run_passing_fixture(tag: &str) -> PassingFixtureRun {
    let real_checkout_before = bounded_real_checkout_files();
    let fixture = SandboxFixture::new(tag);
    let event_log = fixture.root.join("procedure-events.log");
    let state = RecordingState::with_event_log(event_log.clone());
    let valid = captured_localization(VALID_LOCALIZATION_CAPTURE);
    let local = Arc::new(RecordingLocalization::new(
        state.clone(),
        [
            Ok(valid.clone()),
            Ok(valid.clone()),
            Ok(valid.clone()),
            Ok(valid),
        ],
    ));
    let frontier = Arc::new(RecordingLocalization::new(
        state.clone(),
        std::iter::empty::<Result<LocalizationEnvelope, LocalizationDispatchError>>(),
    ));
    let patch = captured_patch();
    let boundary = validate_patch_boundary(patch.clone(), &[TARGET_PATH.to_string()]).unwrap();
    check_patch_applicability(&fixture.root, boundary).unwrap();
    let patches = RecordingPatch::new(
        state.clone(),
        [Ok(patch.clone()), Ok(patch.clone()), Ok(patch)],
    );
    let verifier = write_recording_passing_verifier(&fixture.root, &event_log);
    let before_files = stable_workspace_files(&fixture.root, &event_log);
    state.record(RecordedEvent::ValidationCompleted);
    let runner = WholeChangeProcedureRunner::new(
        whole_change_localization(&fixture, local, false),
        frontier,
        sampling_settings(),
    );
    let outcome = runner
        .run(
            WholeChangeProcedureRequest {
                change_id: CHANGE_ID.to_string(),
                route_override: RouteOverride::Automatic,
            },
            &patches,
            &[verifier],
            None,
        )
        .await
        .unwrap();
    if std::fs::read_to_string(fixture.root.join(TARGET_PATH)).unwrap() == EXPECTED_TARGET_SOURCE {
        state.record(RecordedEvent::PromotionObserved);
    }
    let after_files = stable_workspace_files(&fixture.root, &event_log);
    let real_checkout_after = bounded_real_checkout_files();
    assert_eq!(real_checkout_after, real_checkout_before);
    let report = single_report_document(&fixture.root);
    let event_lines = std::fs::read_to_string(event_log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    PassingFixtureRun {
        fixture,
        state,
        outcome,
        before_files,
        after_files,
        real_checkout_before,
        real_checkout_after,
        report,
        event_lines,
    }
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: A passing fixture proves the complete procedure lifecycle :: One simple task is promoted end to end
#[test]
fn passing_fixture_promotes_one_captured_local_patch_with_terminal_evidence() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(
            assert_passing_fixture_promotes_one_captured_local_patch_with_terminal_evidence(),
        );
}

async fn assert_passing_fixture_promotes_one_captured_local_patch_with_terminal_evidence() {
    let run = run_passing_fixture("task-2-2-lifecycle").await;
    let report_id: ProcedureRunId = serde_json::from_value(run.report["id"].clone()).unwrap();
    let persisted = ProcedureReportRepository::for_project(&run.fixture.root)
        .load_with_fingerprints(&report_id)
        .unwrap();
    let metrics = persisted
        .metrics
        .as_ref()
        .expect("completed fixture persists bounded metrics");

    assert_eq!(
        run.outcome,
        WholeChangeProcedureOutcome::Completed {
            task_ids: vec![TASK_ID.to_string()],
        },
        "report={} events={:?}",
        serde_json::to_string_pretty(&run.report).unwrap(),
        run.event_lines,
    );
    assert_eq!(
        std::fs::read_to_string(run.fixture.root.join(TARGET_PATH)).unwrap(),
        EXPECTED_TARGET_SOURCE
    );
    assert_eq!(persisted.run.selected_task.id, TASK_ID);
    assert_eq!(persisted.run.selected_task.covers.as_deref(), Some(COVERS));
    assert_eq!(persisted.run.attempts.len(), 1);
    assert_eq!(
        persisted.run.attempts[0].disposition,
        ProcedureAttemptDisposition::Accepted
    );
    assert_eq!(persisted.run.attempts[0].targets.len(), 1);
    assert_eq!(persisted.run.attempts[0].targets[0].path, TARGET_PATH);
    assert_eq!(persisted.run.attempts[0].targets[0].symbol, None);
    assert_eq!(
        persisted.run.attempts[0].targets[0].evidence,
        "repository_index"
    );
    assert_eq!(
        persisted.run.validation.as_ref().unwrap().exit_code,
        Some(0)
    );
    assert_eq!(
        persisted.run.review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert_eq!(
        persisted.run.terminal_disposition,
        Some(ProcedureTerminalDisposition::AwaitingReview)
    );
    assert_eq!(metrics.route.selected_tier, Some(RouteTier::Local));
    assert_eq!(metrics.route.local_mechanical_success, Some(true));
    assert!(metrics.route.escalation_triggers.is_empty());
    assert_eq!(metrics.localization_attempt_count, 1);
    assert_eq!(metrics.schema_rejection_count, 0);
    assert_eq!(metrics.candidates.len(), 3);
    assert_eq!(
        metrics
            .candidates
            .iter()
            .map(|candidate| candidate.index)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(metrics.candidates.iter().all(|candidate| {
        candidate.changed_line_count == Some(2) && candidate.verifier_passed == Some(true)
    }));
    assert_eq!(
        metrics
            .stage_timings
            .iter()
            .map(|timing| timing.stage.as_str())
            .collect::<Vec<_>>(),
        ["agreement_sampling", "candidate_verification", "promotion"]
    );
    assert_eq!(
        metrics.terminal_disposition,
        ProcedureMetricsDisposition::Succeeded
    );
    assert_eq!(run.state.counts(), [4, 3, 0, 0]);
    let patch_capture: serde_json::Value = serde_json::from_str(VALID_PATCH_CAPTURE).unwrap();
    assert_eq!(patch_capture["provenance"]["backend"], "ollama");
    assert_eq!(
        patch_capture["response"]["route"]["effective_tier"],
        "local"
    );
    assert!(
        !patch_capture["prompt"]
            .as_str()
            .unwrap()
            .contains("settings.json")
    );
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The fixture proves workspace and evidence isolation :: Promotion is limited to the localized target
#[test]
fn passing_fixture_changes_only_the_localized_source_target() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(assert_passing_fixture_changes_only_the_localized_source_target());
}

async fn assert_passing_fixture_changes_only_the_localized_source_target() {
    let run = run_passing_fixture("task-2-2-isolation").await;

    assert_eq!(run.before_files.len(), run.after_files.len());
    for (path, before) in &run.before_files {
        let after = &run.after_files[path];
        if path == TARGET_PATH {
            assert_ne!(after, before);
            assert_eq!(after, EXPECTED_TARGET_SOURCE.as_bytes());
        } else {
            assert_eq!(after, before, "non-target fixture path changed: {path}");
        }
    }
    assert_eq!(
        std::fs::read(run.fixture.root.join(UNRELATED_PATH)).unwrap(),
        UNRELATED_BYTES
    );
    assert_eq!(run.real_checkout_after, run.real_checkout_before);
    let verifier_workspaces = run
        .event_lines
        .iter()
        .filter_map(|line| {
            let (workspace, event) = line.split_once('|')?;
            (event == "VerifierStarted").then_some(PathBuf::from(workspace))
        })
        .collect::<Vec<_>>();
    assert_eq!(verifier_workspaces.len(), 3);
    assert!(
        verifier_workspaces
            .iter()
            .all(|workspace| workspace != &run.fixture.root)
    );
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: A passing fixture proves the complete procedure lifecycle :: One simple task is promoted end to end
#[test]
fn whole_change_runner_composes_every_required_stage_for_one_task() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(assert_whole_change_runner_composes_every_required_stage_for_one_task());
}

async fn assert_whole_change_runner_composes_every_required_stage_for_one_task() {
    let run = run_passing_fixture("task-3-1-whole-change").await;

    assert_eq!(
        run.outcome,
        WholeChangeProcedureOutcome::Completed {
            task_ids: vec![TASK_ID.to_string()],
        }
    );
    assert_eq!(
        &run.report["validation"]["command"].as_array().unwrap()[run.report["validation"]["command"]
            .as_array()
            .unwrap()
            .len() - 4..],
        ["validate", CHANGE_ID, "--strict", "--no-interactive"]
    );
    assert_eq!(run.report["validation"]["exit_code"], 0);
    assert_eq!(run.report["review_disposition"], "approved");
    assert_eq!(run.report["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(run.report["attempts"][0]["disposition"], "accepted");
    assert_eq!(run.state.counts(), [4, 3, 0, 0]);
    assert_eq!(run.report["metrics"]["localization_attempt_count"], 1);
    assert_eq!(run.report["metrics"]["route"]["selected_tier"], "local");
    assert_eq!(
        run.report["metrics"]["route"]["local_mechanical_success"],
        true
    );
    assert!(
        run.report["metrics"]["route"]["escalation_triggers"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    let candidates = run.report["metrics"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 3);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate["verifier_passed"] == true)
    );
    let stages = run.report["metrics"]["stage_timings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|timing| timing["stage"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        stages,
        ["agreement_sampling", "candidate_verification", "promotion"]
    );
    let verifier_workspaces = run
        .event_lines
        .iter()
        .filter_map(|line| {
            let (workspace, event) = line.split_once('|')?;
            (event == "VerifierStarted").then_some(PathBuf::from(workspace))
        })
        .collect::<Vec<_>>();
    assert_eq!(verifier_workspaces.len(), 3);
    assert!(
        verifier_workspaces
            .iter()
            .all(|workspace| workspace != &run.fixture.root)
    );
    assert_eq!(
        std::fs::read_to_string(run.fixture.root.join(TARGET_PATH)).unwrap(),
        EXPECTED_TARGET_SOURCE
    );
    assert_eq!(
        std::fs::read(run.fixture.root.join(UNRELATED_PATH)).unwrap(),
        UNRELATED_BYTES
    );
    assert_eq!(run.report["metrics"]["terminal_disposition"], "succeeded");
}

struct LocalizationFixtureRun {
    fixture: SandboxFixture,
    state: RecordingState,
    run: ProcedureRun,
    reports: ProcedureReportRepository,
}

async fn run_localization_fixture(
    tag: &str,
    responses: impl IntoIterator<Item = Result<LocalizationEnvelope, LocalizationDispatchError>>,
) -> LocalizationFixtureRun {
    let real_checkout_before = bounded_real_checkout_files();
    let fixture = SandboxFixture::new(tag);
    let event_log = fixture.root.join("unused-event.log");
    let files_before = stable_workspace_files(&fixture.root, &event_log);
    let state = RecordingState::default();
    let reports = ProcedureReportRepository::for_project(&fixture.root);
    let runner = ProcedureRunner::new(
        fixture.input(),
        fixture.root.clone(),
        RepositoryIndexLimits::default(),
        RecordingLocalization::new(state.clone(), responses),
        reports.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    let run = runner
        .run(ProcedureRunRequest {
            change_id: CHANGE_ID.to_string(),
            task_id: TASK_ID.to_string(),
            scratchpad: ProcedureScratchpad::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        stable_workspace_files(&fixture.root, &event_log),
        files_before
    );
    assert_eq!(bounded_real_checkout_files(), real_checkout_before);
    LocalizationFixtureRun {
        fixture,
        state,
        run,
        reports,
    }
}

fn invalid_symbol_diagnostic() -> String {
    "localization target validation failed:\n- target[0] path=\"src/lib.rs\" symbol=\"missing_symbol\": symbol is not present under the indexed path\n"
        .to_string()
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: Localization failures remain diagnosable and bounded :: Invalid symbol produces the known immediate failure
#[test]
fn repeated_captured_invalid_symbol_stops_after_two_localization_attempts() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(assert_repeated_captured_invalid_symbol_stops_after_two_localization_attempts());
}

async fn assert_repeated_captured_invalid_symbol_stops_after_two_localization_attempts() {
    let invalid = captured_localization(INVALID_SYMBOL_LOCALIZATION_CAPTURE);
    let run = run_localization_fixture(
        "task-2-3-repeated-invalid",
        [Ok(invalid.clone()), Ok(invalid)],
    )
    .await;
    let diagnostic = invalid_symbol_diagnostic();

    assert!(matches!(
        run.run.terminal_disposition,
        Some(ProcedureTerminalDisposition::Failed { ref reason }) if reason == &diagnostic
    ));
    assert_eq!(run.run.attempts.len(), 2);
    assert!(run.run.attempts.iter().all(|attempt| {
        attempt.disposition == ProcedureAttemptDisposition::Rejected
            && attempt.validation_error.as_deref() == Some(diagnostic.as_str())
    }));
    let persisted = run.reports.load_with_fingerprints(&run.run.id).unwrap();
    assert_eq!(persisted.run, run.run);
    assert_eq!(
        persisted.run.terminal_disposition,
        Some(ProcedureTerminalDisposition::Failed {
            reason: diagnostic.clone(),
        })
    );
    let metrics = persisted
        .metrics
        .expect("failed localization persists bounded metrics");
    assert_eq!(metrics.localization_attempt_count, 2);
    assert_eq!(metrics.schema_rejection_count, 2);
    assert_eq!(
        metrics.terminal_disposition,
        ProcedureMetricsDisposition::Failed
    );
    assert_eq!(run.state.counts(), [2, 0, 0, 0]);
    assert_eq!(
        run.state.events(),
        [
            RecordedEvent::LocalizationDispatched,
            RecordedEvent::LocalizationDispatched,
        ]
    );
    assert_eq!(
        std::fs::read_to_string(run.fixture.root.join(TARGET_PATH)).unwrap(),
        TARGET_SOURCE
    );
    assert_eq!(
        std::fs::read(run.fixture.root.join(UNRELATED_PATH)).unwrap(),
        UNRELATED_BYTES
    );
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: Localization failures remain diagnosable and bounded :: One invalid response is repaired by the bounded retry
#[test]
fn captured_invalid_then_valid_localization_waits_for_explicit_approval() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(assert_captured_invalid_then_valid_localization_waits_for_explicit_approval());
}

async fn assert_captured_invalid_then_valid_localization_waits_for_explicit_approval() {
    let invalid = captured_localization(INVALID_SYMBOL_LOCALIZATION_CAPTURE);
    let valid = captured_localization(VALID_LOCALIZATION_CAPTURE);
    let run = run_localization_fixture("task-2-3-repaired", [Ok(invalid), Ok(valid.clone())]).await;
    let sampling_gate = SamplingInputGate::new(
        run.fixture.input(),
        run.fixture.root.clone(),
        run.reports.clone(),
    );
    let sampling_request = SamplingInputRequest {
        baseline_localization_run_id: run.run.id,
        change_id: CHANGE_ID.to_string(),
        task_id: TASK_ID.to_string(),
    };

    assert_eq!(
        run.run.terminal_disposition,
        Some(ProcedureTerminalDisposition::AwaitingReview)
    );
    assert_eq!(
        run.run.review_disposition,
        ProcedureReviewDisposition::Pending
    );
    assert_eq!(run.run.attempts.len(), 2);
    assert_eq!(
        run.run
            .attempts
            .iter()
            .map(|attempt| attempt.disposition)
            .collect::<Vec<_>>(),
        [
            ProcedureAttemptDisposition::Rejected,
            ProcedureAttemptDisposition::Accepted,
        ]
    );
    assert_eq!(
        run.run.attempts[0].validation_error.as_deref(),
        Some(invalid_symbol_diagnostic().as_str())
    );
    assert_eq!(run.run.attempts[1].targets, valid.targets);
    assert_eq!(run.run.attempts[1].targets.len(), 1);
    assert_eq!(run.run.attempts[1].targets[0].path, TARGET_PATH);
    assert_eq!(run.run.attempts[1].targets[0].symbol, None);
    assert_eq!(run.run.attempts[1].targets[0].evidence, "repository_index");
    let pending = run.reports.load(&run.run.id).unwrap();
    assert_eq!(pending.attempts, run.run.attempts);
    assert_eq!(
        sampling_gate.load(&sampling_request).unwrap_err(),
        SamplingInputError::ReviewDisposition {
            run_id: run.run.id.as_str(),
            disposition: ProcedureReviewDisposition::Pending,
        }
    );
    assert_eq!(run.state.counts(), [2, 0, 0, 0]);

    run.reports.approve(&run.run.id).unwrap();
    let approved = sampling_gate.load(&sampling_request).unwrap();
    assert_eq!(
        approved.report.review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert_eq!(approved.report.attempts, run.run.attempts);
    assert_eq!(approved.contract.contract.task.id, TASK_ID);
    assert_eq!(run.state.counts(), [2, 0, 0, 0]);
    assert_eq!(
        std::fs::read_to_string(run.fixture.root.join(TARGET_PATH)).unwrap(),
        TARGET_SOURCE
    );
    assert_eq!(
        std::fs::read(run.fixture.root.join(UNRELATED_PATH)).unwrap(),
        UNRELATED_BYTES
    );
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance fixture uses a valid isolated proposal :: Minimal proposal passes the preflight gate
#[test]
fn valid_isolated_proposal_passes_preflight_and_indexes_fixture_files() {
    let real_checkout_before = bounded_real_checkout_files();
    let fixture = SandboxFixture::new("valid-preflight");
    let event_log = fixture.root.join("unused-event.log");
    let files_before = stable_workspace_files(&fixture.root, &event_log);

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
    assert_eq!(
        stable_workspace_files(&fixture.root, &event_log),
        files_before
    );
    assert_eq!(bounded_real_checkout_files(), real_checkout_before);
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance test is runnable without live model services :: The test runs offline
#[test]
fn fixture_preflight_uses_only_the_local_deterministic_validator() {
    let real_checkout_before = bounded_real_checkout_files();
    let fixture = SandboxFixture::new("offline-preflight");
    let event_log = fixture.root.join("unused-event.log");
    let files_before = stable_workspace_files(&fixture.root, &event_log);

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
    assert_eq!(
        stable_workspace_files(&fixture.root, &event_log),
        files_before
    );
    assert_eq!(bounded_real_checkout_files(), real_checkout_before);
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance fixture uses a valid isolated proposal :: Invalid proposal stops before execution
#[test]
fn invalid_required_artifacts_report_exact_preflight_failure_before_downstream_work() {
    assert_invalid_required_artifacts_report_exact_preflight_failure_before_downstream_work();
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The fixture proves workspace and evidence isolation :: Failure does not mutate source files
#[test]
fn invalid_required_artifacts_do_not_mutate_source_files() {
    assert_invalid_required_artifacts_report_exact_preflight_failure_before_downstream_work();
}

fn assert_invalid_required_artifacts_report_exact_preflight_failure_before_downstream_work() {
    let real_checkout_before = bounded_real_checkout_files();
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
        let event_log = fixture.root.join("unused-event.log");
        let files_before = stable_workspace_files(&fixture.root, &event_log);
        let target_before = std::fs::read(fixture.root.join(TARGET_PATH)).unwrap();
        let unrelated_before = std::fs::read(fixture.root.join(UNRELATED_PATH)).unwrap();
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
            stable_workspace_files(&fixture.root, &event_log),
            files_before,
            "invalid-preflight case changed sandbox files: {tag}"
        );
        assert_eq!(
            std::fs::read(fixture.root.join(TARGET_PATH)).unwrap(),
            target_before
        );
        assert_eq!(
            std::fs::read(fixture.root.join(UNRELATED_PATH)).unwrap(),
            unrelated_before
        );
    }
    assert_eq!(bounded_real_checkout_files(), real_checkout_before);
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The fixture proves workspace and evidence isolation :: Failure does not mutate source files
#[test]
fn whole_change_interruption_before_promotion_preserves_source_bytes() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(assert_whole_change_interruption_before_promotion_preserves_source_bytes());
}

async fn assert_whole_change_interruption_before_promotion_preserves_source_bytes() {
    let real_checkout_before = bounded_real_checkout_files();
    let fixture = SandboxFixture::new("task-4-3-interrupted");
    let state = RecordingState::default();
    let local = Arc::new(RecordingLocalization::new(
        state.clone(),
        std::iter::empty::<Result<LocalizationEnvelope, LocalizationDispatchError>>(),
    ));
    let frontier = Arc::new(RecordingLocalization::new(
        state.clone(),
        std::iter::empty::<Result<LocalizationEnvelope, LocalizationDispatchError>>(),
    ));
    let patches = RecordingPatch::new(
        state.clone(),
        std::iter::empty::<Result<PatchCandidate, LocalPatchDraftError>>(),
    );
    let event_log = fixture.root.join("unused-event.log");
    let files_before = stable_workspace_files(&fixture.root, &event_log);
    let target_before = std::fs::read(fixture.root.join(TARGET_PATH)).unwrap();
    let unrelated_before = std::fs::read(fixture.root.join(UNRELATED_PATH)).unwrap();
    let runner = WholeChangeProcedureRunner::new(
        whole_change_localization(&fixture, local, true),
        frontier,
        sampling_settings(),
    );

    let outcome = runner
        .run(
            WholeChangeProcedureRequest {
                change_id: CHANGE_ID.to_string(),
                route_override: RouteOverride::Automatic,
            },
            &patches,
            &["must-not-run".to_string()],
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        WholeChangeProcedureOutcome::Interrupted {
            completed_task_ids: Vec::new(),
        }
    );
    assert_eq!(state.counts(), [0, 0, 0, 0]);
    assert!(state.events().is_empty());
    assert_eq!(
        stable_workspace_files(&fixture.root, &event_log),
        files_before
    );
    assert_eq!(
        std::fs::read(fixture.root.join(TARGET_PATH)).unwrap(),
        target_before
    );
    assert_eq!(
        std::fs::read(fixture.root.join(UNRELATED_PATH)).unwrap(),
        unrelated_before
    );
    assert!(!fixture.root.join(".deepseek/procedure-runs").exists());
    assert_eq!(bounded_real_checkout_files(), real_checkout_before);
}

async fn assert_recording_seams_share_ordered_events_and_bounded_call_counters_offline() {
    let real_checkout_before = bounded_real_checkout_files();
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
    let verifier_command = write_passing_verifier(&fixture.root);
    let event_log = fixture.root.join("unused-event.log");
    let files_before = stable_workspace_files(&fixture.root, &event_log);

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
    let verifier_run = verifier.run(&fixture.root, &[verifier_command]).await;
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
    assert_eq!(
        stable_workspace_files(&fixture.root, &event_log),
        files_before
    );
    assert_eq!(bounded_real_checkout_files(), real_checkout_before);
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: A passing fixture proves the complete procedure lifecycle :: Stage order and dispatch boundaries are recorded
#[test]
fn recording_seams_record_stage_order_and_dispatch_boundaries() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(assert_recording_seams_record_stage_order_and_dispatch_boundaries());
}

async fn assert_recording_seams_record_stage_order_and_dispatch_boundaries() {
    let run = run_passing_fixture("task-3-2-stage-order").await;
    let labels = run
        .event_lines
        .iter()
        .map(|line| line.split_once('|').unwrap().1)
        .collect::<Vec<_>>();

    assert_eq!(
        labels,
        [
            "ValidationCompleted",
            "LocalizationDispatched",
            "LocalizationDispatched",
            "LocalizationDispatched",
            "LocalizationDispatched",
            "PatchDispatched",
            "PatchDispatched",
            "PatchDispatched",
            "VerifierStarted",
            "VerifierCompleted",
            "VerifierStarted",
            "VerifierCompleted",
            "VerifierStarted",
            "VerifierCompleted",
            "PromotionObserved",
        ]
    );
    assert_eq!(run.state.counts(), [4, 3, 0, 0]);
    assert!(!labels.contains(&"FrontierDispatched"));
    assert_eq!(
        labels
            .iter()
            .filter(|event| **event == "VerifierStarted")
            .count(),
        3
    );
    assert_eq!(
        labels
            .iter()
            .filter(|event| **event == "VerifierCompleted")
            .count(),
        3
    );
    let promotion = labels
        .iter()
        .position(|event| *event == "PromotionObserved")
        .unwrap();
    assert!(
        labels
            .iter()
            .enumerate()
            .filter(|(_, event)| **event == "VerifierCompleted")
            .all(|(index, _)| index < promotion)
    );
    assert_eq!(run.report["metrics"]["terminal_disposition"], "succeeded");
    assert!(
        run.report["metrics"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| candidate["verifier_passed"] == true)
    );
    assert_eq!(
        std::fs::read_to_string(run.fixture.root.join(TARGET_PATH)).unwrap(),
        EXPECTED_TARGET_SOURCE
    );
}

// covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance test is runnable without live model services :: The test runs offline
#[test]
fn recording_seams_share_ordered_events_and_bounded_call_counters_offline() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(assert_recording_seams_share_ordered_events_and_bounded_call_counters_offline());
}
