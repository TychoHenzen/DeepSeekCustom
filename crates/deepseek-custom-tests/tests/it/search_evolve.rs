//! Tests for `deepseek_custom::search::evolve` (`src/search/evolve.rs`).
//!
//! A `StubBackend` generator run through several rounds against synthetic
//! fitness and feature scripts, asserting the run ends on the candidate the
//! synthetic fitness function actually favors. These drive the runner the
//! way the agent task does, not through a tool call.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use deepseek_custom::agent::agent_loop::{RoutedEvent, StreamEvent};
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::effort::Effort;
use deepseek_custom::search::SearchSnapshot;
use deepseek_custom::search::evolve::{EvolveParams, run_evolve};

use tokio::sync::mpsc;

/// A factory whose `work_dir` points at `dir`, so the fitness and feature
/// scripts run where the test wrote them.
fn factory_in(dir: &Path, name: &str, script: Vec<StubTurn>) -> Arc<BackendFactory> {
    let factory =
        BackendFactory::new(Settings::default(), PathBuf::from(".")).with_stub(name, script);
    *factory.working_dir().lock().unwrap() = dir.to_path_buf();
    Arc::new(factory)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn params(prompt: &str, backend: &str, fitness_cmd: &str) -> EvolveParams {
    EvolveParams {
        prompt: prompt.to_string(),
        backend: backend.to_string(),
        generations: 2,
        population: 3,
        fitness_cmd: fitness_cmd.to_string(),
        feature_cmd: None,
        islands: 1,
        migration_interval: 5,
        mutation_hints: Vec::new(),
        effort: Effort::None,
    }
}

fn not_interrupted() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Every snapshot the run reported.
fn drain(rx: &mut mpsc::UnboundedReceiver<RoutedEvent>) -> Vec<SearchSnapshot> {
    let mut snapshots = Vec::new();
    while let Ok(routed) = rx.try_recv() {
        if let StreamEvent::SearchProgress(snapshot) = routed.event {
            snapshots.push(*snapshot);
        }
    }
    snapshots
}

/// Write a batch file that reads a counter, increments it, and echoes the
/// score for that dispatch number. Returns the path.
///
/// The counter lives in its own file rather than in the environment,
/// because each dispatch is a separate process and nothing else carries
/// state from one to the next.
fn write_score_batch(work_dir: &Path, counter_path: &Path, scores: &[&str], name: &str) -> PathBuf {
    std::fs::write(counter_path, "0").expect("seed counter");
    let batch_path = work_dir.join(name);
    let mut script = String::from("@echo off\r\nsetlocal enabledelayedexpansion\r\n");
    script.push_str(&format!("set /p n=<{}\r\n", counter_path.display()));
    script.push_str("set /a n+=1\r\n");
    script.push_str(&format!("echo !n!>{}\r\n", counter_path.display()));
    for (i, score) in scores.iter().enumerate() {
        let n = i + 1;
        if i == 0 {
            script.push_str(&format!("if !n!=={n} (echo {score})"));
        } else {
            script.push_str(&format!(" else if !n!=={n} (echo {score})"));
        }
    }
    script.push_str(" else (echo 0)\r\n");
    script.push_str("endlocal\r\n");
    std::fs::write(&batch_path, &script).expect("write batch");
    batch_path
}

#[tokio::test]
async fn stub_backed_evolve_with_feature_cmd_finds_best_candidate() {
    let work_dir = temp_dir("dsc-evolve-test-e10");
    let factory = factory_in(
        &work_dir,
        "stub-evolve",
        vec![
            StubTurn::Text("alpha".to_string()),
            StubTurn::Text("beta".to_string()),
            StubTurn::Text("gamma".to_string()),
            StubTurn::Text("delta".to_string()),
            StubTurn::Text("epsilon".to_string()),
            StubTurn::Text("zeta".to_string()),
        ],
    );

    // Six dispatches (2 generations * 3 population): alpha(1), beta(5),
    // gamma(3), delta(7), epsilon(2), zeta(9).
    let fitness_cmd = write_score_batch(
        &work_dir,
        &work_dir.join("fit_count.txt"),
        &["1", "5", "3", "7", "2", "9"],
        "fit.bat",
    )
    .to_string_lossy()
    .to_string();
    // Cell [0] for alpha, beta and epsilon, [1] for gamma and delta, [2]
    // for zeta.
    let feature_cmd = write_score_batch(
        &work_dir,
        &work_dir.join("feat_count.txt"),
        &["0", "0", "1", "1", "0", "2"],
        "feat.bat",
    )
    .to_string_lossy()
    .to_string();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut config = params("solve the problem", "stub-evolve", &fitness_cmd);
    config.feature_cmd = Some(feature_cmd);
    let report = run_evolve(&factory, config, tx, not_interrupted()).await;

    assert!(!report.is_error, "got error: {}", report.summary);
    assert_eq!(report.best.as_deref(), Some("zeta"));
    assert_eq!(report.best_fitness, Some(9.0));
    assert!(
        report.summary.contains("Archive summary"),
        "no archive: {}",
        report.summary
    );

    // One snapshot before the first dispatch, then one per scored
    // candidate, and the dispatch count is reported against its cap.
    let snapshots = drain(&mut rx);
    assert_eq!(snapshots.len(), 7);
    assert_eq!(snapshots.last().unwrap().dispatches, Some((6, 200)));
    assert!(snapshots.last().unwrap().note.contains("best 9"));
    // Two finished generations, so two points on the sparkline.
    assert_eq!(snapshots.last().unwrap().history.len(), 1);
}

#[tokio::test]
async fn stub_backed_evolve_without_feature_cmd_finds_best() {
    let work_dir = temp_dir("dsc-evolve-test-e10-nf");
    let factory = factory_in(
        &work_dir,
        "stub-evolve-nf",
        vec![
            StubTurn::Text("first".to_string()),
            StubTurn::Text("second".to_string()),
            StubTurn::Text("third".to_string()),
            StubTurn::Text("fourth".to_string()),
        ],
    );

    // Four dispatches (2 generations * 2 population).
    let fitness_cmd = write_score_batch(
        &work_dir,
        &work_dir.join("fit_count2.txt"),
        &["1", "2", "3", "7"],
        "fit.bat",
    )
    .to_string_lossy()
    .to_string();

    let (tx, _rx) = mpsc::unbounded_channel();
    let mut config = params("solve", "stub-evolve-nf", &fitness_cmd);
    config.population = 2;
    let report = run_evolve(&factory, config, tx, not_interrupted()).await;

    assert!(!report.is_error, "got error: {}", report.summary);
    assert_eq!(report.best.as_deref(), Some("fourth"));
    assert_eq!(report.best_fitness, Some(7.0));
}

#[tokio::test]
async fn every_dispatch_failing_produces_an_error_report() {
    let work_dir = temp_dir("dsc-evolve-test-e10-fail");
    let factory = factory_in(
        &work_dir,
        "stub-evolve-fail",
        vec![
            StubTurn::Error("gen failed".to_string()),
            StubTurn::Error("gen failed".to_string()),
        ],
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    let mut config = params("solve", "stub-evolve-fail", "echo 1");
    config.population = 1;
    let report = run_evolve(&factory, config, tx, not_interrupted()).await;

    assert!(report.is_error, "expected an error report");
    assert!(report.best.is_none());
    assert!(
        report.summary.contains("no viable candidate"),
        "got: {}",
        report.summary
    );
    assert!(
        report.summary.contains("stub-evolve-fail"),
        "got: {}",
        report.summary
    );
}

/// An interrupt set before the run starts stops it at the first generation
/// boundary, so nothing is dispatched at all.
#[tokio::test]
async fn an_interrupt_stops_the_run_before_it_dispatches() {
    let work_dir = temp_dir("dsc-evolve-test-interrupt");
    let factory = factory_in(
        &work_dir,
        "stub-evolve-stop",
        vec![StubTurn::Text("never reached".to_string())],
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    let interrupt = Arc::new(AtomicBool::new(true));
    let report = run_evolve(
        &factory,
        params("solve", "stub-evolve-stop", "echo 1"),
        tx,
        interrupt,
    )
    .await;

    assert!(report.is_error);
    assert!(report.best.is_none());
}

/// A fitness command that prints something other than a number stops the
/// run and names the command, rather than scoring the candidate zero and
/// corrupting the archive without saying so.
#[tokio::test]
async fn an_unparseable_fitness_value_stops_the_run() {
    let work_dir = temp_dir("dsc-evolve-test-bad-fitness");
    let factory = factory_in(
        &work_dir,
        "stub-evolve-bad",
        vec![StubTurn::Text("candidate".to_string())],
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    let mut config = params("solve", "stub-evolve-bad", "echo not-a-number");
    config.population = 1;
    let report = run_evolve(&factory, config, tx, not_interrupted()).await;

    assert!(report.is_error);
    assert!(
        report.summary.contains("fitness_cmd"),
        "expected the command named, got: {}",
        report.summary
    );
}
