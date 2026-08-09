//! Unit tests for `deepseek_custom::tools::evolve` (`src/tools/evolve.rs`).
//!
//! E10: a `StubBackend` generator run through several rounds against synthetic
//! `fitness_cmd`/`feature_cmd` scripts. Assert the final best candidate is the
//! one the synthetic fitness function actually favors.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::Mutex;

use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::effort::Effort;
use deepseek_custom::tools::Tool;
use deepseek_custom::tools::evolve::EvolveTool;

use tokio::sync::mpsc;

fn factory_with_stub(name: &str, script: Vec<StubTurn>) -> Arc<BackendFactory> {
    Arc::new(
        BackendFactory::new(Settings::default(), PathBuf::from(".")).with_stub(name, script),
    )
}

fn test_parent_tx() -> mpsc::UnboundedSender<deepseek_custom::agent::agent_loop::RoutedEvent> {
    let (tx, _rx) = mpsc::unbounded_channel();
    tx
}

fn empty_registry() -> Arc<SubagentRegistry> {
    Arc::new(SubagentRegistry::new())
}

fn effort_flag_at(level: Effort) -> Arc<AtomicU8> {
    let flag = Arc::new(AtomicU8::new(0));
    level.store(&flag);
    flag
}

/// Write a batch file that reads a counter, increments it, and echoes the
/// score for that dispatch number. Returns the path.
///
/// The batch file writes a known score to a fixed temp file, then echoes the
/// counter value itself as the output. This avoids quoting issues with
/// `set /p n=<path` that arise when the path contains quotes.
fn write_score_batch(work_dir: &PathBuf, counter_path: &PathBuf, scores: &[&str], name: &str) -> PathBuf {
    std::fs::write(counter_path, "0").expect("seed counter");
    let batch_path = work_dir.join(name);
    // Strategy: read counter from a file, write the corresponding score to
    // a known output file, then echo the counter value (which parses as f64).
    // The caller reads stdout as the score.
    let mut script = String::from("@echo off\r\nsetlocal enabledelayedexpansion\r\n");
    // Read counter without quotes (the path has no spaces in our test).
    script.push_str(&format!(
        "set /p n=<{}\r\n",
        counter_path.display()
    ));
    script.push_str("set /a n+=1\r\n");
    // Write the incremented counter back.
    script.push_str(&format!(
        "echo !n!>{}\r\n",
        counter_path.display()
    ));
    // Chain of if/else to echo the right score to stdout.
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
    let work_dir = std::env::temp_dir().join("dsc-evolve-test-e10");
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).expect("create temp dir");

    let factory = factory_with_stub(
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

    // Six dispatches (2 gen * 3 pop): alpha(1), beta(5), gamma(3), delta(7), epsilon(2), zeta(9)
    let fit_batch = write_score_batch(
        &work_dir,
        &work_dir.join("fit_count.txt"),
        &["1", "5", "3", "7", "2", "9"],
        "fit.bat",
    );
    let fitness_cmd = fit_batch.to_string_lossy().to_string();

    // Feature command via batch: cell [0] for alpha/beta/epsilon, [1] for gamma/delta, [2] for zeta
    let feat_batch = write_score_batch(
        &work_dir,
        &work_dir.join("feat_count.txt"),
        &["0", "0", "1", "1", "0", "2"],
        "feat.bat",
    );
    let feature_cmd = feat_batch.to_string_lossy().to_string();

    let work_dir_flag = Arc::new(Mutex::new(work_dir));
    let tool = EvolveTool::new(
        factory, 1, test_parent_tx(), empty_registry(),
        effort_flag_at(Effort::None), work_dir_flag,
    );

    let input = serde_json::json!({
        "prompt": "solve the problem",
        "backend": "stub-evolve",
        "fitness_cmd": fitness_cmd,
        "feature_cmd": feature_cmd,
        "generations": 2,
        "population": 3,
        "islands": 1,
    });

    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error, "got error: {}", output.content);
    assert!(output.content.contains("zeta"), "no zeta: {}", output.content);
    assert!(output.content.contains("9"), "no fitness 9: {}", output.content);
    assert!(output.content.contains("Archive summary"), "no archive: {}", output.content);
}

#[tokio::test]
async fn stub_backed_evolve_without_feature_cmd_finds_best() {
    let work_dir = std::env::temp_dir().join("dsc-evolve-test-e10-nf");
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).expect("create temp dir");

    let factory = factory_with_stub(
        "stub-evolve-nf",
        vec![
            StubTurn::Text("first".to_string()),
            StubTurn::Text("second".to_string()),
            StubTurn::Text("third".to_string()),
            StubTurn::Text("fourth".to_string()),
        ],
    );

    // 4 dispatches (2 gen * 2 pop): first(1), second(2), third(3), fourth(7)
    let fit_batch = write_score_batch(
        &work_dir,
        &work_dir.join("fit_count2.txt"),
        &["1", "2", "3", "7"],
        "fit.bat",
    );
    let fitness_cmd = fit_batch.to_string_lossy().to_string();

    let work_dir_flag = Arc::new(Mutex::new(work_dir));
    let tool = EvolveTool::new(
        factory, 1, test_parent_tx(), empty_registry(),
        effort_flag_at(Effort::None), work_dir_flag,
    );

    let input = serde_json::json!({
        "prompt": "solve",
        "backend": "stub-evolve-nf",
        "fitness_cmd": fitness_cmd,
        "generations": 2,
        "population": 2,
        "islands": 1,
    });

    let output = tool.execute(input).await.expect("execute");
    assert!(!output.is_error, "got error: {}", output.content);
    assert!(output.content.contains("fourth"), "no fourth: {}", output.content);
    assert!(output.content.contains("7"), "no fitness 7: {}", output.content);
}

#[tokio::test]
async fn all_failed_generations_produces_tool_error() {
    let work_dir = std::env::temp_dir().join("dsc-evolve-test-e10-fail");
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).expect("create temp dir");

    let factory = factory_with_stub(
        "stub-evolve-fail",
        vec![
            StubTurn::Error("gen failed".to_string()),
            StubTurn::Error("gen failed".to_string()),
        ],
    );

    let fitness_cmd = "echo 1".to_string();
    let work_dir_flag = Arc::new(Mutex::new(work_dir));

    let tool = EvolveTool::new(
        factory, 1, test_parent_tx(), empty_registry(),
        effort_flag_at(Effort::None), work_dir_flag,
    );

    let input = serde_json::json!({
        "prompt": "solve",
        "backend": "stub-evolve-fail",
        "fitness_cmd": fitness_cmd,
        "generations": 2,
        "population": 1,
        "islands": 1,
    });

    let output = tool.execute(input).await.expect("execute");
    assert!(output.is_error, "expected error");
    assert!(output.content.contains("no viable candidate"), "got: {}", output.content);
    assert!(output.content.contains("stub-evolve-fail"), "got: {}", output.content);
}
