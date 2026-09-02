use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use deepseek_custom::backend::Backend;
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::config::settings::{BackendConfig, Settings};
use tokio::sync::mpsc;

const SIDE_EFFECT_PATH: &str = "src/controlled-side-effect.txt";

struct FakeCodexPath {
    old_path: Option<OsString>,
    directory: PathBuf,
}

impl FakeCodexPath {
    fn install() -> Self {
        let directory = super::scratch_dir("controlled-codex", "fake-path");
        let extension = std::env::consts::EXE_EXTENSION;
        let target = directory.join(if extension.is_empty() {
            "codex".to_string()
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

fn codex_backend(env: HashMap<String, String>) -> BackendConfig {
    BackendConfig::CodexCli {
        model: "gpt-5-codex".to_string(),
        sandbox: None,
        env: Some(env),
        models: None,
    }
}

fn settings(backends: HashMap<String, BackendConfig>) -> Settings {
    Settings {
        default_backend: Some("planning".to_string()),
        backends: Some(backends),
        ..Settings::default()
    }
}

fn recorded_args(path: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn fake_codex_receives_exact_controlled_profiles_and_writes_only_in_the_disposable_root() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let _environment = super::process_environment_lock().lock().await;
        let _fake_path = FakeCodexPath::install();
        let real_root = super::scratch_dir("controlled-codex", "real-root");
        let planning_root = super::scratch_dir("controlled-codex", "planning-root");
        let execution_root = super::scratch_dir("controlled-codex", "execution-root");
        let evidence = super::scratch_dir("controlled-codex", "evidence");
        let args_file = evidence.join("args.jsonl");
        let planning_cwd_file = evidence.join("planning-cwd.txt");
        let execution_cwd_file = evidence.join("execution-cwd.txt");
        let planning_env = HashMap::from([
            (
                "FAKE_CODEX_ARGS_FILE".to_string(),
                args_file.display().to_string(),
            ),
            (
                "FAKE_CLI_CWD_FILE".to_string(),
                planning_cwd_file.display().to_string(),
            ),
        ]);
        let execution_env = HashMap::from([
            (
                "FAKE_CODEX_ARGS_FILE".to_string(),
                args_file.display().to_string(),
            ),
            (
                "FAKE_CLI_CWD_FILE".to_string(),
                execution_cwd_file.display().to_string(),
            ),
            (
                "FAKE_CLI_SIDE_EFFECT_PATH".to_string(),
                SIDE_EFFECT_PATH.to_string(),
            ),
        ]);
        let factory = Arc::new(BackendFactory::new(
            settings(HashMap::from([
                ("planning".to_string(), codex_backend(planning_env)),
                ("execution".to_string(), codex_backend(execution_env)),
            ])),
            real_root.clone(),
        ));

        let (planning_tx, _planning_rx) = mpsc::unbounded_channel();
        let Backend::CodexCli(mut planning) = factory
            .build_controlled_planning("planning", None, planning_tx, planning_root.clone())
            .unwrap()
        else {
            panic!("expected controlled Codex planning profile");
        };
        let schema_path = planning
            .planning_schema_path_for_test()
            .unwrap()
            .to_path_buf();
        planning.set_thread_id(Some("stored-thread-must-not-resume".to_string()));
        planning.send("produce one card").await.unwrap();
        assert!(planning.thread_id().is_none());

        let (execution_tx, _execution_rx) = mpsc::unbounded_channel();
        let Backend::CodexCli(mut execution) = factory
            .build_controlled_execution("execution", None, execution_tx, execution_root.clone())
            .unwrap()
        else {
            panic!("expected controlled Codex execution profile");
        };
        execution.set_thread_id(Some("stored-thread-must-not-resume".to_string()));
        execution.send("apply approved card").await.unwrap();
        assert!(execution.thread_id().is_none());

        let invocations = recorded_args(&args_file);
        assert_eq!(
            invocations[0],
            vec![
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--ephemeral",
                "--ignore-user-config",
                "--ignore-rules",
                "--output-schema",
                schema_path.to_str().unwrap(),
                "-m",
                "gpt-5-codex",
                "produce one card",
            ]
        );
        assert_eq!(
            invocations[1],
            vec![
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "--ephemeral",
                "--ignore-user-config",
                "--ignore-rules",
                "--disable",
                "multi_agent",
                "--disable",
                "multi_agent_v2",
                "-m",
                "gpt-5-codex",
                "apply approved card",
            ]
        );
        assert_eq!(
            std::fs::canonicalize(PathBuf::from(
                std::fs::read_to_string(&planning_cwd_file).unwrap()
            ))
            .unwrap(),
            std::fs::canonicalize(&planning_root).unwrap()
        );
        assert_eq!(
            std::fs::canonicalize(PathBuf::from(
                std::fs::read_to_string(&execution_cwd_file).unwrap()
            ))
            .unwrap(),
            std::fs::canonicalize(&execution_root).unwrap()
        );
        assert!(execution_root.join(SIDE_EFFECT_PATH).is_file());
        assert!(!planning_root.join(SIDE_EFFECT_PATH).exists());
        assert!(!real_root.join(SIDE_EFFECT_PATH).exists());

        drop(planning);
        drop(execution);
        std::fs::remove_dir_all(real_root).unwrap();
        std::fs::remove_dir_all(planning_root).unwrap();
        std::fs::remove_dir_all(execution_root).unwrap();
        std::fs::remove_dir_all(evidence).unwrap();
    });
}

#[test]
fn controlled_codex_fails_closed_when_a_required_isolation_flag_is_rejected() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let _environment = super::process_environment_lock().lock().await;
        let _fake_path = FakeCodexPath::install();
        let real_root = super::scratch_dir("controlled-codex", "reject-real");
        let execution_root = super::scratch_dir("controlled-codex", "reject-execution");
        let evidence = super::scratch_dir("controlled-codex", "reject-evidence");
        let env = HashMap::from([
            (
                "FAKE_CODEX_ARGS_FILE".to_string(),
                evidence.join("args.jsonl").display().to_string(),
            ),
            (
                "FAKE_CODEX_REJECT_ARG".to_string(),
                "--ignore-user-config".to_string(),
            ),
            (
                "FAKE_CLI_SIDE_EFFECT_PATH".to_string(),
                SIDE_EFFECT_PATH.to_string(),
            ),
        ]);
        let factory = Arc::new(BackendFactory::new(
            settings(HashMap::from([(
                "execution".to_string(),
                codex_backend(env),
            )])),
            real_root.clone(),
        ));
        let (tx, _rx) = mpsc::unbounded_channel();
        let Backend::CodexCli(mut execution) = factory
            .build_controlled_execution("execution", None, tx, execution_root.clone())
            .unwrap()
        else {
            panic!("expected controlled Codex execution profile");
        };

        let error = execution.send("apply approved card").await.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("fake Codex rejected required argument --ignore-user-config"),
            "{error}"
        );
        assert!(!execution_root.join(SIDE_EFFECT_PATH).exists());
        assert!(!real_root.join(SIDE_EFFECT_PATH).exists());

        drop(execution);
        std::fs::remove_dir_all(real_root).unwrap();
        std::fs::remove_dir_all(execution_root).unwrap();
        std::fs::remove_dir_all(evidence).unwrap();
    });
}
