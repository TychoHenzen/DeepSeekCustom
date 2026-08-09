//! Unit tests for `deepseek_custom::tools::cascade` (`src/tools/cascade.rs`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::effort::Effort;
use deepseek_custom::tools::Tool;
use deepseek_custom::tools::cascade::CascadeTool;

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

/// A cascade call with a `check_cmd` that rejects some candidates using a
/// counter file. The command passes on even invocations and fails on odd ones,
/// so with five candidates all returning the same text, two pass and three
/// are rejected. The winner is the text shared by the two survivors.
#[tokio::test]
async fn check_cmd_rejects_some_candidates_and_the_correct_winner_returns() {
    let work_dir = std::env::temp_dir().join("dsc-cascade-test-check-cmd");
    // Clean up from a previous run that might have left stale state.
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).expect("should create temp dir");

    // Seed the counter file so the first invocation exits 0 (pass).
    let counter_path = work_dir.join("counter.txt");
    std::fs::write(&counter_path, "0").expect("should write counter seed");

    // A check_cmd that reads a counter from counter.txt, increments it, and
    // fails every other invocation (odd values exit 1, even exit 0).
    // With seed 0: invocation 1 reads 0→1 (odd)→fail, invocation 2 reads
    // 1→2 (even)→pass, invocation 3 reads 2→3 (odd)→fail, invocation 4 reads
    // 3→4 (even)→pass, invocation 5 reads 4→5 (odd)→fail. Two pass, three fail.
    let check_cmd = format!(
        "powershell -Command \"$f='{c}'; $n=[int](Get-Content $f -Raw -ErrorAction SilentlyContinue); $n++; Set-Content $f $n; if($n % 2 -eq 1){{exit 1}}else{{exit 0}}\"",
        c = counter_path.display()
    );

    let factory = factory_with_stub(
        "stub-cascade",
        vec![
            StubTurn::Text("correct answer".to_string()),
            StubTurn::Text("correct answer".to_string()),
            StubTurn::Text("correct answer".to_string()),
            StubTurn::Text("correct answer".to_string()),
            StubTurn::Text("correct answer".to_string()),
        ],
    );
    let work_dir_flag = Arc::new(std::sync::Mutex::new(work_dir));

    let tool = CascadeTool::new(
        factory,
        1, // dispatch_depth
        test_parent_tx(),
        empty_registry(),
        effort_flag_at(Effort::None),
        work_dir_flag,
    );

    let input = serde_json::json!({
        "prompt": "what is the answer",
        "backend": "stub-cascade",
        "n": 5,
        "check_cmd": check_cmd,
    });

    let output = tool
        .execute(input)
        .await
        .expect("execute should not return a hard error");

    // The cascade should not be an error: two candidates survive the check
    // and agree on "correct answer", so there is a clear winner.
    assert!(!output.is_error);
    assert!(
        output.content.contains("correct answer"),
        "expected winner text in output, got: {}",
        output.content
    );
    assert!(
        output.content.contains("won"),
        "expected a winner declaration, got: {}",
        output.content
    );
    assert!(
        output.content.contains("2 vote"),
        "expected 2 votes from the two passing candidates, got: {}",
        output.content
    );
    assert!(
        output.content.contains("2 of 5 passed check_cmd"),
        "expected pass note showing 2 of 5, got: {}",
        output.content
    );
    assert!(
        output.content.contains("rejected by check_cmd"),
        "expected rejected candidates to be reported, got: {}",
        output.content
    );
}
