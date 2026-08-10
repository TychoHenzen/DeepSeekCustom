//! Tests for `deepseek_custom::search::cascade` (`src/search/cascade.rs`).
//!
//! These drive the runner directly, the way the agent task does, rather
//! than through a tool call. That is the point of the module: a cascade is
//! a procedure with a shape fixed before it starts, so a test fixes the
//! same shape a person would fix in the Cascade tab.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deepseek_custom::agent::agent_loop::{RoutedEvent, StreamEvent};
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::effort::Effort;
use deepseek_custom::search::cascade::{CascadeCounters, CascadeParams, run_cascade};
use deepseek_custom::search::{SearchKind, SearchSnapshot};

use tokio::sync::mpsc;

/// A factory whose `work_dir` points at a fresh temp directory, so a check
/// command runs somewhere it can write.
fn factory_in(dir: &std::path::Path) -> BackendFactory {
    let factory = BackendFactory::new(Settings::default(), PathBuf::from("."));
    *factory.working_dir().lock().unwrap() = dir.to_path_buf();
    factory
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("should create temp dir");
    dir
}

fn params(prompt: &str, backend: &str, n: u32) -> CascadeParams {
    CascadeParams {
        prompt: prompt.to_string(),
        backend: backend.to_string(),
        n,
        vote_k: 1,
        check_cmd: None,
        diversity_hints: Vec::new(),
        escalate_backend: None,
        effort: Effort::None,
    }
}

fn not_interrupted() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Every snapshot the run reported, plus its terminal event.
fn drain(rx: &mut mpsc::UnboundedReceiver<RoutedEvent>) -> (Vec<SearchSnapshot>, Option<String>) {
    let mut snapshots = Vec::new();
    let mut finished = None;
    while let Ok(routed) = rx.try_recv() {
        match routed.event {
            StreamEvent::SearchProgress(snapshot) => snapshots.push(*snapshot),
            StreamEvent::SearchFinished { summary, .. } => finished = Some(summary),
            _ => {}
        }
    }
    (snapshots, finished)
}

/// Five attempts that agree pick that answer, and the run reports progress
/// as each one lands rather than only at the end.
#[tokio::test]
async fn agreeing_attempts_produce_a_winner_and_report_progress() {
    let factory = Arc::new(
        BackendFactory::new(Settings::default(), PathBuf::from(".")).with_stub(
            "stub-cascade",
            vec![StubTurn::Text("correct answer".to_string()); 5],
        ),
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    let report = run_cascade(
        &factory,
        params("what is the answer", "stub-cascade", 5),
        tx,
        not_interrupted(),
        CascadeCounters::new(),
    )
    .await;

    assert!(!report.is_error);
    assert_eq!(report.winner.as_deref(), Some("correct answer"));

    let (snapshots, finished) = drain(&mut rx);
    // One before the first attempt, then one per attempt.
    assert_eq!(snapshots.len(), 6);
    assert!(snapshots.iter().all(|s| s.kind == SearchKind::Cascade));
    assert_eq!(snapshots.first().unwrap().done, 0);
    assert_eq!(snapshots.last().unwrap().done, 5);
    assert_eq!(snapshots.last().unwrap().top.len(), 1);
    assert_eq!(snapshots.last().unwrap().top[0].score, Some(5.0));
    assert!(finished.expect("a finished event").contains("won"));
}

/// A check command that fails every other candidate drops those candidates
/// before the vote, and the survivors decide the winner.
#[tokio::test]
async fn check_cmd_rejects_some_candidates_and_the_correct_winner_returns() {
    let work_dir = temp_dir("dsc-cascade-test-check-cmd");
    let counter_path = work_dir.join("counter.txt");
    std::fs::write(&counter_path, "0").expect("should write counter seed");

    // Reads a counter, increments it, and exits 1 on odd values. With five
    // candidates that seeds two passes and three rejections.
    let check_cmd = format!(
        "powershell -Command \"$f='{c}'; $n=[int](Get-Content $f -Raw -ErrorAction SilentlyContinue); $n++; Set-Content $f $n; if($n % 2 -eq 1){{exit 1}}else{{exit 0}}\"",
        c = counter_path.display()
    );

    let factory = Arc::new(factory_in(&work_dir).with_stub(
        "stub-cascade",
        vec![StubTurn::Text("correct answer".to_string()); 5],
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let mut config = params("what is the answer", "stub-cascade", 5);
    config.check_cmd = Some(check_cmd);
    let report = run_cascade(
        &factory,
        config,
        tx,
        not_interrupted(),
        CascadeCounters::new(),
    )
    .await;

    assert!(!report.is_error);
    assert_eq!(report.winner.as_deref(), Some("correct answer"));
    assert!(
        report.summary.contains("2 vote"),
        "expected 2 votes from the two passing candidates, got: {}",
        report.summary
    );
    assert!(
        report.summary.contains("2 of 5 passed check_cmd"),
        "expected a pass note showing 2 of 5, got: {}",
        report.summary
    );
    assert!(
        report.summary.contains("rejected by check_cmd"),
        "expected the rejected candidates to be reported, got: {}",
        report.summary
    );
}

/// Five attempts that all disagree escalate to the stronger backend, and
/// both counters move.
#[tokio::test]
async fn all_disagree_with_escalate_backend_returns_escalated_answer_and_bumps_counters() {
    let factory = Arc::new(
        BackendFactory::new(Settings::default(), PathBuf::from("."))
            .with_stub(
                "stub-cascade",
                vec![
                    StubTurn::Text("answer alpha".to_string()),
                    StubTurn::Text("answer beta".to_string()),
                    StubTurn::Text("answer gamma".to_string()),
                    StubTurn::Text("answer delta".to_string()),
                    StubTurn::Text("answer epsilon".to_string()),
                ],
            )
            .with_stub(
                "stub-escalate",
                vec![StubTurn::Text("chosen escalated answer".to_string())],
            ),
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let counters = CascadeCounters::new();

    let mut config = params("what is the answer", "stub-cascade", 5);
    config.escalate_backend = Some("stub-escalate".to_string());
    let report = run_cascade(&factory, config, tx, not_interrupted(), counters.clone()).await;

    assert!(!report.is_error);
    assert_eq!(report.winner.as_deref(), Some("chosen escalated answer"));
    assert!(
        report.summary.contains("[escalated]"),
        "expected the escalation marker, got: {}",
        report.summary
    );
    assert!(
        report.summary.contains("stub-escalate"),
        "expected the escalation backend named, got: {}",
        report.summary
    );

    assert_eq!(counters.total.load(Ordering::SeqCst), 1);
    assert_eq!(counters.escalated.load(Ordering::SeqCst), 1);
}

/// Disagreeing attempts with no escalation backend are an error, and the
/// escalation counter stays put.
#[tokio::test]
async fn all_disagree_without_escalate_backend_is_an_error() {
    let factory = Arc::new(
        BackendFactory::new(Settings::default(), PathBuf::from(".")).with_stub(
            "stub-cascade",
            vec![
                StubTurn::Text("answer alpha".to_string()),
                StubTurn::Text("answer beta".to_string()),
            ],
        ),
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let counters = CascadeCounters::new();

    let mut config = params("what is the answer", "stub-cascade", 2);
    config.vote_k = 2;
    let report = run_cascade(&factory, config, tx, not_interrupted(), counters.clone()).await;

    assert!(report.is_error);
    assert!(report.winner.is_none());
    assert!(
        report.summary.contains("No consensus"),
        "expected a no-consensus summary, got: {}",
        report.summary
    );
    assert_eq!(counters.total.load(Ordering::SeqCst), 1);
    assert_eq!(counters.escalated.load(Ordering::SeqCst), 0);
}

/// The attempt count is clamped, so a form asking for a hundred attempts
/// runs the cap and says so.
#[tokio::test]
async fn attempt_count_is_clamped_to_the_cap() {
    let factory = Arc::new(
        BackendFactory::new(Settings::default(), PathBuf::from("."))
            .with_stub("stub-cascade", vec![StubTurn::Text("same".to_string()); 64]),
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    let report = run_cascade(
        &factory,
        params("q", "stub-cascade", 100),
        tx,
        not_interrupted(),
        CascadeCounters::new(),
    )
    .await;

    assert!(report.summary.contains("16 attempt(s)"));
    let (snapshots, _) = drain(&mut rx);
    assert_eq!(snapshots.first().unwrap().total, 16);
}
