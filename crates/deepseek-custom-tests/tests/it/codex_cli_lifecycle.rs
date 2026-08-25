//! End-to-end lifecycle coverage using the local fake Codex binary.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deepseek_custom::agent::events::{RoutedEvent, StreamEvent};
use deepseek_custom::backend::codex_cli::CodexCliDriver;

const BLOCK_MARKER: &str = "__FAKE_CODEX_BLOCK__";

struct TestPath {
    old_path: Option<OsString>,
    dir: PathBuf,
    args_file: PathBuf,
}

impl TestPath {
    fn install() -> Self {
        let dir =
            std::env::temp_dir().join(format!("deepseek-fake-codex-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let extension = std::env::consts::EXE_EXTENSION;
        let target = dir.join(if extension.is_empty() {
            "codex".to_owned()
        } else {
            format!("codex.{extension}")
        });
        std::fs::copy(env!("CARGO_BIN_EXE_fake_codex"), &target).unwrap();
        let old_path = std::env::var_os("PATH");
        let mut paths = vec![dir.clone()];
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
            args_file: dir.join("args.jsonl"),
            dir,
        }
    }
}

impl Drop for TestPath {
    fn drop(&mut self) {
        match &self.old_path {
            Some(path) => unsafe { std::env::set_var("PATH", path) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn driver(
    args_file: &Path,
) -> (
    CodexCliDriver,
    tokio::sync::mpsc::UnboundedReceiver<RoutedEvent>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let env = HashMap::from([(
        "FAKE_CODEX_ARGS_FILE".to_owned(),
        args_file.display().to_string(),
    )]);
    (
        CodexCliDriver::new(
            "test-model".to_owned(),
            None,
            Some(env),
            Arc::new(Mutex::new(PathBuf::from("."))),
            tx,
        ),
        rx,
    )
}

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<RoutedEvent>) -> Vec<StreamEvent> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .map(|r| r.event)
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn two_turn_resume_interrupt_and_recovery() {
    let _guard = super::process_environment_lock().lock().await;
    let path = TestPath::install();
    let (mut driver, mut rx) = driver(&path.args_file);

    tokio::time::timeout(Duration::from_secs(5), driver.send("first turn"))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), driver.send("second turn"))
        .await
        .unwrap()
        .unwrap();
    let events = drain(&mut rx);
    let texts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["echo: first turn", "echo: second turn"]);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::TurnEnd { .. }))
            .count(),
        2
    );

    let invocations: Vec<Vec<String>> = std::fs::read_to_string(&path.args_file)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(&invocations[0][..2], ["exec", "--json"]);
    assert_eq!(
        &invocations[1][..4],
        ["exec", "resume", "fake-thread-42", "--json"]
    );

    let interrupt = driver.interrupt_flag();
    let task = tokio::spawn(async move {
        let result = driver.send(&format!("block {BLOCK_MARKER}")).await;
        (driver, result)
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    interrupt.store(true, Ordering::SeqCst);
    let (mut driver, result) = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    result.unwrap();
    assert!(
        drain(&mut rx)
            .iter()
            .any(|event| matches!(event, StreamEvent::Interrupted { .. }))
    );

    tokio::time::timeout(Duration::from_secs(5), driver.send("after interrupt"))
        .await
        .unwrap()
        .unwrap();
    assert!(drain(&mut rx).iter().any(
        |event| matches!(event, StreamEvent::Text { text, .. } if text == "echo: after interrupt")
    ));
    driver.shutdown().await;
}
