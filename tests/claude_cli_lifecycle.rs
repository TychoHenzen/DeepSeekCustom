//! Covers the `claude_cli` process lifecycle against the fake `claude`
//! binary (`src/bin/fake_claude.rs`): a turn boundary, an interrupt that
//! kills the child, a respawn after the child exits on its own, and a
//! voice-mode-triggered restart. No real `claude` binary is spawned, no
//! network call is made, and no money is spent.
//!
//! Every test either lets its child finish on its own (the fake exits when
//! stdin closes, at `driver.shutdown().await`) or kills it via
//! `driver.interrupt()`/the interrupt flag before the test ends. The one
//! test that deliberately hangs a child (`interrupt_during_a_turn_kills_...`)
//! bounds the fake's hang to `HANG_SLEEP_SECS` (3s, mirrored below) inside
//! the fake itself, so even a broken interrupt cannot leave the suite
//! waiting forever or leak a process past that window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use DeepSeekCustom::agent::agent_loop::StreamEvent;
use DeepSeekCustom::backend::claude_cli::process::ClaudeCliDriver;

/// Matches `CLAUDE_CLI_PATH_KEY` in `src/backend/claude_cli/process.rs`,
/// the same way `tests/claude_cli_fake_binary.rs` does. That constant is
/// `pub(super)`, not reachable from an external integration test crate.
const CLAUDE_CLI_PATH_KEY: &str = "CLAUDE_CLI_PATH";

/// Matches `HANG_MARKER` in `src/bin/fake_claude.rs`. A turn whose text
/// contains this sleeps for `HANG_SLEEP_SECS` before replying, giving the
/// interrupt test a wide, reliable window to kill the child mid-turn.
const HANG_MARKER: &str = "__FAKE_CLAUDE_HANG__";
/// Matches `EXIT_MARKER` in `src/bin/fake_claude.rs`. A turn whose text
/// contains this replies normally, then exits the fake process instead of
/// reading the next stdin line.
const EXIT_MARKER: &str = "__FAKE_CLAUDE_EXIT_AFTER_REPLY__";
/// Matches `HANG_SLEEP_SECS` in `src/bin/fake_claude.rs`.
const HANG_SLEEP_SECS: u64 = 3;

fn fake_claude_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        CLAUDE_CLI_PATH_KEY.to_string(),
        env!("CARGO_BIN_EXE_fake_claude").to_string(),
    );
    env
}

fn new_driver(
    working_dir: Arc<Mutex<PathBuf>>,
) -> (
    ClaudeCliDriver,
    tokio::sync::mpsc::UnboundedReceiver<DeepSeekCustom::agent::agent_loop::RoutedEvent>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let driver = ClaudeCliDriver::new(
        "opus".to_string(),
        None,
        Some(fake_claude_env()),
        working_dir,
        tx,
    );
    (driver, rx)
}

fn drain_texts(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<DeepSeekCustom::agent::agent_loop::RoutedEvent>,
) -> Vec<String> {
    let mut texts = Vec::new();
    while let Ok(routed) = rx.try_recv() {
        if let StreamEvent::Text { text, .. } = routed.event {
            texts.push(text);
        }
    }
    texts
}

fn drain_all(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<DeepSeekCustom::agent::agent_loop::RoutedEvent>,
) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    while let Ok(routed) = rx.try_recv() {
        events.push(routed.event);
    }
    events
}

/// Proves `send` waits for the turn it just sent, not any later one: two
/// turns sent one after another each come back with their own reply, in
/// order, and each produces exactly one `TurnEnd`. If `send` returned
/// before the child's `result` event, the second turn's text could arrive
/// mixed into the first's, or a `TurnEnd` could go missing.
#[tokio::test]
async fn two_turns_in_a_row_each_get_their_own_reply_and_turn_end() {
    let working_dir = Arc::new(Mutex::new(PathBuf::from(".")));
    let (mut driver, mut rx) = new_driver(working_dir);

    let first = driver.send("turn one").await;
    assert!(first.is_ok(), "first turn should succeed: {first:?}");
    let second = driver.send("turn two").await;
    assert!(second.is_ok(), "second turn should succeed: {second:?}");

    let events = drain_all(&mut rx);
    let texts: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["echo: turn one".to_string(), "echo: turn two".to_string()],
        "each turn's reply should arrive on its own, in order"
    );

    let turn_end_count = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::TurnEnd { .. }))
        .count();
    assert_eq!(turn_end_count, 2, "expected one TurnEnd per turn");

    driver.shutdown().await;
}

/// Proves an interrupt during a turn in flight kills the child, and that
/// the next turn works because a fresh child is spawned rather than
/// writing into a dead pipe. Uses the fake's `HANG_MARKER` so the turn
/// never finishes on its own within the test's timeout, removing the race
/// there would otherwise be against the fake's near-instant reply.
///
/// If `interrupt` stopped dropping the child and stdin handles, the next
/// `send` would write into the killed child's dead pipe. The reader task
/// for that child already exited, so `turn_done` would never fire again,
/// and the assertion below on the second turn's reply would time out.
#[tokio::test]
async fn interrupt_during_a_turn_kills_the_child_and_the_next_turn_spawns_a_fresh_one() {
    let working_dir = Arc::new(Mutex::new(PathBuf::from(".")));
    let (mut driver, mut rx) = new_driver(working_dir);
    let interrupt_flag = driver.interrupt_flag();

    let handle = tokio::spawn(async move {
        let result = driver.send(&format!("please hang {HANG_MARKER}")).await;
        (driver, result)
    });

    // Give the child time to spawn and the turn line time to reach it
    // before signalling interrupt, so the interrupt lands mid-turn rather
    // than racing the spawn itself.
    tokio::time::sleep(Duration::from_millis(400)).await;
    interrupt_flag.store(true, Ordering::SeqCst);

    let (mut driver, result) = tokio::time::timeout(
        Duration::from_secs(HANG_SLEEP_SECS + 2),
        handle,
    )
    .await
    .expect("send should return once interrupted, not wait out the full hang")
    .expect("the spawned task should not panic");
    assert!(result.is_ok(), "an interrupted send still returns Ok: {result:?}");

    let events = drain_all(&mut rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::Interrupted { .. })),
        "expected an Interrupted event, got {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, StreamEvent::TurnEnd { .. })),
        "the hung turn must never complete once its child is killed, got {events:?}"
    );

    let result2 = tokio::time::timeout(
        Duration::from_secs(10),
        driver.send("turn after interrupt"),
    )
    .await
    .expect("the turn after an interrupt should not hang");
    assert!(result2.is_ok(), "the turn after an interrupt should succeed: {result2:?}");

    let texts = drain_texts(&mut rx);
    assert_eq!(
        texts,
        vec!["echo: turn after interrupt".to_string()],
        "the fresh child should reply normally"
    );

    driver.shutdown().await;
}

/// Proves `ensure_ready` respawns after the child has exited on its own,
/// and that the next turn succeeds against the new child. Uses the fake's
/// `EXIT_MARKER` so the child ends itself right after replying, the same
/// shape a real crash or a `claude` process ending unexpectedly would take:
/// gone by the time the next turn's `ensure_ready` runs, with nothing
/// (interrupt, shutdown) telling the driver in advance.
#[tokio::test]
async fn ensure_ready_respawns_after_the_child_exits_on_its_own() {
    let working_dir = Arc::new(Mutex::new(PathBuf::from(".")));
    let (mut driver, mut rx) = new_driver(working_dir);

    let first = driver.send("turn one").await;
    assert!(first.is_ok(), "first turn should succeed: {first:?}");

    let second = driver.send(&format!("turn two {EXIT_MARKER}")).await;
    assert!(
        second.is_ok(),
        "the turn that makes the child exit afterward should still reply: {second:?}"
    );

    // Give the OS a moment to finish reaping the process the fake exited
    // from on its own, so `child_exited`'s `try_wait` observes it as gone
    // instead of racing the exit.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let third = tokio::time::timeout(Duration::from_secs(10), driver.send("turn three"))
        .await
        .expect("the respawned turn should not hang");
    assert!(third.is_ok(), "a fresh child should be spawned and reply: {third:?}");

    let texts = drain_texts(&mut rx);
    assert!(
        texts.iter().any(|t| t == "echo: turn three"),
        "expected turn three's reply from the respawned child, got {texts:?}"
    );

    driver.shutdown().await;
}

/// Proves flipping the voice-mode flag between turns respawns the child.
/// The fake ignores `--append-system-prompt`, so the reply text cannot show
/// a respawn happened, and the session id cannot either: the driver
/// auto-captures the live child's session id after every turn
/// (`drain_session_id`) and reuses it as `--resume` on the next spawn, so a
/// respawned child deliberately keeps reporting the same id as the one it
/// replaces. Process identity is the only thing that actually changes on a
/// respawn, so this asserts on `child_pid()` instead. No workaround is
/// needed to isolate the trigger: with the respawn bug fixed, an ordinary
/// turn no longer respawns on its own, so a pid change here can only mean
/// the voice-mode flip caused it.
#[tokio::test]
async fn flipping_voice_mode_between_turns_respawns_the_child() {
    let working_dir = Arc::new(Mutex::new(PathBuf::from(".")));
    let (mut driver, _rx) = new_driver(working_dir);
    let voice_mode_flag = driver.voice_mode_flag();

    let first = driver.send("turn one").await;
    assert!(first.is_ok(), "first turn should succeed: {first:?}");
    let pid_before = driver.child_pid().expect("expected a running child after turn one");

    voice_mode_flag.store(true, Ordering::SeqCst);

    let second = driver.send("turn two").await;
    assert!(second.is_ok(), "second turn should succeed: {second:?}");
    let pid_after = driver.child_pid().expect("expected a running child after turn two");

    assert_ne!(
        pid_before, pid_after,
        "a voice-mode change should respawn the child under a new process id"
    );

    driver.shutdown().await;
}

/// Proves the same working-directory restart the briefing calls out, using
/// the same process-identity technique as the voice-mode test above. No
/// workaround is needed: with the respawn bug fixed, an ordinary turn no
/// longer respawns on its own, so a pid change here can only mean the
/// working-directory change caused it.
#[tokio::test]
async fn changing_the_working_directory_between_turns_respawns_the_child() {
    let working_dir = Arc::new(Mutex::new(PathBuf::from(".")));
    let (mut driver, _rx) = new_driver(Arc::clone(&working_dir));

    let first = driver.send("turn one").await;
    assert!(first.is_ok(), "first turn should succeed: {first:?}");
    let pid_before = driver.child_pid().expect("expected a running child after turn one");

    *working_dir.lock().unwrap() = std::env::temp_dir();

    let second = driver.send("turn two").await;
    assert!(second.is_ok(), "second turn should succeed: {second:?}");
    let pid_after = driver.child_pid().expect("expected a running child after turn two");

    assert_ne!(
        pid_before, pid_after,
        "a working-directory change should respawn the child under a new process id"
    );

    driver.shutdown().await;
}

/// Proves the defect: turn 2 of an ordinary conversation, with nothing
/// changed between turns, must not respawn the child. This fails against
/// the pre-fix code, where `drain_session_id` recorded the newly-learned
/// session id without updating `spawned_resume_id` to match, so
/// `ensure_ready` read the live child's own id as a change from the `None`
/// it was spawned under and killed it needlessly.
#[tokio::test]
async fn turn_two_of_an_ordinary_conversation_does_not_respawn_the_child() {
    let working_dir = Arc::new(Mutex::new(PathBuf::from(".")));
    let (mut driver, _rx) = new_driver(working_dir);

    let first = driver.send("turn one").await;
    assert!(first.is_ok(), "first turn should succeed: {first:?}");
    let pid_before = driver.child_pid().expect("expected a running child after turn one");

    let second = driver.send("turn two").await;
    assert!(second.is_ok(), "second turn should succeed: {second:?}");
    let pid_after = driver.child_pid().expect("expected a running child after turn two");

    assert_eq!(
        pid_before, pid_after,
        "an ordinary second turn must reuse the first turn's child, not respawn it"
    );

    driver.shutdown().await;
}

/// Proves a genuine session change, the `LoadSession` path where the user
/// reopens a different saved conversation, still respawns the child with
/// `--resume <id>`. `set_claude_session_id` is exactly the call
/// `Backend::load_session` makes. The fake echoes back whatever `--resume`
/// value it was given as its own `session_id`, so `driver.claude_session_id()`
/// reporting the exact id set here, after a pid change, proves both that a
/// respawn happened and that it carried the right `--resume` argument.
#[tokio::test]
async fn loading_a_different_session_still_respawns_the_child_with_resume() {
    let working_dir = Arc::new(Mutex::new(PathBuf::from(".")));
    let (mut driver, _rx) = new_driver(working_dir);

    let first = driver.send("turn one").await;
    assert!(first.is_ok(), "first turn should succeed: {first:?}");
    let pid_before = driver.child_pid().expect("expected a running child after turn one");

    let other_session_id = "deadbeef-0000-4000-8000-000000000000".to_string();
    driver.set_claude_session_id(Some(other_session_id.clone()));

    let second = driver.send("turn two").await;
    assert!(second.is_ok(), "second turn should succeed: {second:?}");
    let pid_after = driver.child_pid().expect("expected a running child after turn two");

    assert_ne!(
        pid_before, pid_after,
        "loading a different session should respawn the child under a new process id"
    );
    assert_eq!(
        driver.claude_session_id(),
        Some(other_session_id.as_str()),
        "the respawned child should have been resumed with the loaded session's id"
    );

    driver.shutdown().await;
}
