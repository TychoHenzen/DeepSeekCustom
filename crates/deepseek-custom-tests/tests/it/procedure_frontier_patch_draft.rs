use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::config::settings::{BackendConfig, Settings};
use deepseek_custom::effort::Effort;
use deepseek_custom::procedure::{
    FrontierPatchDraftRequest, check_patch_applicability, draft_frontier_patch,
    validate_patch_boundary,
};

const VERBATIM_MARKER: &str = "__FAKE_FRONTIER_RESPONSE__";
const SIDE_EFFECT_PATH: &str = "src/preview-side-effect.txt";

fn temp_dir(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("dsc frontier draft {tag} {}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn valid_envelope() -> String {
    serde_json::json!({
        "route": {
            "automatic_tier": "frontier",
            "effective_tier": "frontier",
            "selected_override": "automatic",
            "overridden": false,
            "signals": [{"kind": "substantive_logic"}]
        },
        "targets": ["src/lib.rs"],
        "rationale": "Use the isolated CLI draft.",
        "unified_diff": "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn old() {}\n+pub fn new() {}\n"
    })
    .to_string()
}

fn request(backend: &str) -> FrontierPatchDraftRequest {
    FrontierPatchDraftRequest {
        backend: backend.to_string(),
        model: None,
        prompt: format!("{VERBATIM_MARKER}{}", valid_envelope()),
        effort: Effort::None,
    }
}

fn read_recorded_directory(path: &Path) -> PathBuf {
    PathBuf::from(std::fs::read_to_string(path).unwrap())
}

fn workspace_hash(root: &Path) -> u64 {
    fn collect(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(root, &path, files);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    collect(root, root, &mut files);
    files.sort();
    let mut hash = 0xcbf29ce484222325_u64;
    for path in files {
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for byte in relative
            .bytes()
            .chain([0])
            .chain(std::fs::read(path).unwrap())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

fn side_effect_env(cwd_file: &Path) -> HashMap<String, String> {
    HashMap::from([
        (
            "FAKE_CLI_CWD_FILE".to_string(),
            cwd_file.display().to_string(),
        ),
        (
            "FAKE_CLI_SIDE_EFFECT_PATH".to_string(),
            SIDE_EFFECT_PATH.to_string(),
        ),
    ])
}

fn assert_isolated_directory_was_discarded(record_file: &Path, source: &Path) {
    let recorded = read_recorded_directory(record_file);
    assert_ne!(recorded, std::fs::canonicalize(source).unwrap());
    assert!(
        recorded
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("deepseek-draft-workspace-")
    );
    assert!(
        !recorded.exists(),
        "disposable CLI working directory still exists: {}",
        recorded.display()
    );
}

struct FakeCodexPath {
    old_path: Option<OsString>,
    directory: PathBuf,
}

impl FakeCodexPath {
    fn install() -> Self {
        let directory = temp_dir("fake codex path");
        let extension = std::env::consts::EXE_EXTENSION;
        let target = directory.join(if extension.is_empty() {
            "codex".to_owned()
        } else {
            format!("codex.{extension}")
        });
        std::fs::copy(env!("CARGO_BIN_EXE_fake_codex"), target).unwrap();
        let old_path = std::env::var_os("PATH");
        let mut paths = vec![directory.clone()];
        paths.extend(
            old_path
                .as_ref()
                .map(std::env::split_paths)
                .into_iter()
                .flatten(),
        );
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
        Self {
            old_path,
            directory,
        }
    }
}

impl Drop for FakeCodexPath {
    fn drop(&mut self) {
        match &self.old_path {
            Some(path) => unsafe { std::env::set_var("PATH", path) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        std::fs::remove_dir_all(&self.directory).ok();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn claude_and_codex_drafts_run_in_disposable_directories_and_return_only_validated_text() {
    let _environment = super::process_environment_lock().lock().await;
    let _codex_path = FakeCodexPath::install();
    let source = temp_dir("source with spaces");
    std::fs::create_dir_all(source.join("src")).unwrap();
    std::fs::write(source.join("src/lib.rs"), "pub fn old() {}\n").unwrap();
    let source_hash = workspace_hash(&source);
    let evidence = temp_dir("cwd evidence");
    let claude_cwd = evidence.join("claude.txt");
    let codex_cwd = evidence.join("codex.txt");

    let backends = HashMap::from([
        (
            "claude-frontier".to_string(),
            BackendConfig::ClaudeCli {
                model: "test-claude".to_string(),
                permission_mode: None,
                env: Some(
                    side_effect_env(&claude_cwd)
                        .into_iter()
                        .chain([(
                            "CLAUDE_CLI_PATH".to_string(),
                            env!("CARGO_BIN_EXE_fake_claude").to_string(),
                        )])
                        .collect(),
                ),
                models: None,
            },
        ),
        (
            "codex-frontier".to_string(),
            BackendConfig::CodexCli {
                model: "test-codex".to_string(),
                sandbox: Some("workspace-write".to_string()),
                env: Some(side_effect_env(&codex_cwd)),
                models: None,
            },
        ),
    ]);
    let factory = Arc::new(BackendFactory::new(
        Settings {
            backends: Some(backends),
            ..Settings::default()
        },
        source.clone(),
    ));

    for (backend, cwd_file) in [
        ("claude-frontier", claude_cwd.as_path()),
        ("codex-frontier", codex_cwd.as_path()),
    ] {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let candidate = draft_frontier_patch(
            &factory,
            &source,
            request(backend),
            tx,
            Arc::new(SubagentRegistry::new()),
        )
        .await
        .unwrap();

        let boundary = validate_patch_boundary(candidate, ["src/lib.rs"]).unwrap();
        let checked = check_patch_applicability(&source, boundary).unwrap();
        assert_eq!(checked.envelope().targets, ["src/lib.rs"]);
        assert_isolated_directory_was_discarded(cwd_file, &source);
        assert_eq!(workspace_hash(&source), source_hash);
        assert!(!source.join(SIDE_EFFECT_PATH).exists());
    }

    std::fs::remove_dir_all(source).ok();
    std::fs::remove_dir_all(evidence).ok();
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_frontier_output_still_discards_the_cli_working_directory() {
    let source = temp_dir("malformed source");
    std::fs::create_dir_all(source.join("src")).unwrap();
    std::fs::write(source.join("src/lib.rs"), "pub fn old() {}\n").unwrap();
    let source_hash = workspace_hash(&source);
    let evidence = temp_dir("malformed evidence");
    let cwd_file = evidence.join("claude.txt");
    let settings = Settings {
        backends: Some(HashMap::from([(
            "claude-frontier".to_string(),
            BackendConfig::ClaudeCli {
                model: "test-claude".to_string(),
                permission_mode: None,
                env: Some(
                    side_effect_env(&cwd_file)
                        .into_iter()
                        .chain([(
                            "CLAUDE_CLI_PATH".to_string(),
                            env!("CARGO_BIN_EXE_fake_claude").to_string(),
                        )])
                        .collect(),
                ),
                models: None,
            },
        )])),
        ..Settings::default()
    };
    let factory = Arc::new(BackendFactory::new(settings, source.clone()));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut malformed = request("claude-frontier");
    malformed.prompt = format!("{VERBATIM_MARKER}not-json");

    assert!(
        draft_frontier_patch(
            &factory,
            &source,
            malformed,
            tx,
            Arc::new(SubagentRegistry::new()),
        )
        .await
        .is_err()
    );
    assert_isolated_directory_was_discarded(&cwd_file, &source);
    assert_eq!(workspace_hash(&source), source_hash);
    assert!(!source.join(SIDE_EFFECT_PATH).exists());

    std::fs::remove_dir_all(source).ok();
    std::fs::remove_dir_all(evidence).ok();
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_codex_dispatch_drops_its_disposable_working_directory() {
    let _environment = super::process_environment_lock().lock().await;
    let _codex_path = FakeCodexPath::install();
    let source = temp_dir("cancelled source");
    std::fs::create_dir_all(source.join("src")).unwrap();
    std::fs::write(source.join("src/lib.rs"), "pub fn old() {}\n").unwrap();
    let source_hash = workspace_hash(&source);
    let evidence = temp_dir("cancelled evidence");
    let cwd_file = evidence.join("codex.txt");
    let settings = Settings {
        backends: Some(HashMap::from([(
            "codex-frontier".to_string(),
            BackendConfig::CodexCli {
                model: "test-codex".to_string(),
                sandbox: Some("workspace-write".to_string()),
                env: Some(side_effect_env(&cwd_file)),
                models: None,
            },
        )])),
        ..Settings::default()
    };
    let factory = Arc::new(BackendFactory::new(settings, source.clone()));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let task_source = source.clone();
    let task = tokio::spawn(async move {
        let mut request = request("codex-frontier");
        request.prompt = "__FAKE_CODEX_BLOCK__".to_string();
        draft_frontier_patch(
            &factory,
            &task_source,
            request,
            tx,
            Arc::new(SubagentRegistry::new()),
        )
        .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !cwd_file.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("fake Codex should record its isolated working directory");
    let disposable_path = read_recorded_directory(&cwd_file);
    assert!(disposable_path.exists());
    assert_eq!(
        std::fs::read_to_string(disposable_path.join(SIDE_EFFECT_PATH)).unwrap(),
        "fake Codex workspace side effect\n"
    );

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!disposable_path.exists());
    assert_eq!(workspace_hash(&source), source_hash);
    assert!(!source.join(SIDE_EFFECT_PATH).exists());

    std::fs::remove_dir_all(source).ok();
    std::fs::remove_dir_all(evidence).ok();
}
