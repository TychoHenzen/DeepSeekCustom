//! Proves the fake `claude` binary (`src/bin/fake_claude.rs`) actually
//! drives `ClaudeCliDriver` end to end: a turn goes in over stdin and the
//! mapped `StreamEvent`s come out on the driver's event channel, with no
//! real `claude` binary spawned and no network call made.
//!
//! `CARGO_BIN_EXE_fake_claude` is set by cargo at build time to the path of
//! the compiled `fake_claude` binary from `src/bin/`, so this test never
//! hardcodes a path or relies on the binary being on `PATH`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::agent_loop::StreamEvent;
use deepseek_custom::backend::claude_cli::process::ClaudeCliDriver;

/// Matches `CLAUDE_CLI_PATH_KEY` in `src/backend/claude_cli/process.rs`.
/// That constant is `pub(super)`, not reachable from an external
/// integration test crate, so this test spells the key literally. Several
/// unit tests inside `process.rs` itself already use the real constant to
/// set the same environment key, so both paths stay in sync as long as
/// this string matches.
const CLAUDE_CLI_PATH_KEY: &str = "CLAUDE_CLI_PATH";

fn fake_claude_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        CLAUDE_CLI_PATH_KEY.to_string(),
        env!("CARGO_BIN_EXE_fake_claude").to_string(),
    );
    env
}

#[tokio::test]
async fn a_turn_against_the_fake_binary_yields_the_mapped_text_and_turn_end() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let working_dir = Arc::new(Mutex::new(PathBuf::from(".")));
    let mut driver = ClaudeCliDriver::new(
        "opus".to_string(),
        None,
        Some(fake_claude_env()),
        working_dir,
        tx,
    );

    let result = driver.send("hello there").await;
    assert!(result.is_ok(), "turn against the fake binary should succeed: {result:?}");

    let mut events = Vec::new();
    while let Ok(routed) = rx.try_recv() {
        events.push(routed.event);
    }

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "echo: hello there");

    let turn_end_count = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::TurnEnd { .. }))
        .count();
    assert_eq!(turn_end_count, 1, "expected exactly one TurnEnd");

    driver.shutdown().await;
}
