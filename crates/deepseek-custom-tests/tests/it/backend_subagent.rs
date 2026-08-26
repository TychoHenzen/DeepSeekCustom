//! Unit tests for `deepseek_custom::backend::subagent` (`src/backend/subagent.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use deepseek_custom::agent::events::{RoutedEvent, StreamEvent, SubagentId, SubagentMeta};
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::backend::subagent::{
    SubagentRequest, drain_reply_and_forward, resolve_subagent_working_dir, run_stub_subagent,
    run_subagent, spawn_event_forwarder,
};
use deepseek_custom::config::settings::Settings;
use deepseek_custom::effort::Effort;
use deepseek_custom::tools::Tool;
use deepseek_custom::tools::bash::BashTool;

use tokio::sync::mpsc;

fn empty_factory() -> Arc<BackendFactory> {
    Arc::new(BackendFactory::new(Settings::default(), PathBuf::from(".")))
}

fn empty_registry() -> Arc<SubagentRegistry> {
    Arc::new(SubagentRegistry::new())
}

fn stub_request(backend: &str, depth: u32) -> SubagentRequest {
    SubagentRequest {
        backend: backend.to_string(),
        model: None,
        prompt: "hello".to_string(),
        depth,
        keep_open: false,
        working_dir_override: None,
        effort: Effort::None,
    }
}

fn keep_open_stub_request(backend: &str, depth: u32) -> SubagentRequest {
    SubagentRequest {
        keep_open: true,
        ..stub_request(backend, depth)
    }
}

/// A non-executable file, so `Command::spawn` fails deterministically
/// instead of ever launching a real `claude` process. Used by every
/// test below that dispatches onto a `claude_cli` backend: this repo's
/// dev machine has a real `claude` binary on PATH, so leaving
/// `CLAUDE_CLI_PATH` unset would spawn it for real.
fn non_executable_file() -> PathBuf {
    let dir = std::env::temp_dir();
    let name = format!(
        "subagent_test_not_claude_{}_{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let path = dir.join(name);
    std::fs::write(&path, b"not a real binary").unwrap();
    path
}

fn claude_cli_factory_with_unspawnable_binary(name: &str) -> Arc<BackendFactory> {
    use deepseek_custom::config::settings::BackendConfig;
    use std::collections::HashMap;

    let mut env = HashMap::new();
    env.insert(
        "CLAUDE_CLI_PATH".to_string(),
        non_executable_file().to_string_lossy().to_string(),
    );
    let mut backends = HashMap::new();
    backends.insert(
        name.to_string(),
        BackendConfig::ClaudeCli {
            model: "opus".to_string(),
            permission_mode: None,
            env: Some(env),
            models: None,
        },
    );
    let settings = Settings {
        backends: Some(backends),
        default_backend: Some(name.to_string()),
        ..Default::default()
    };
    Arc::new(BackendFactory::new(settings, PathBuf::from(".")))
}

fn claude_cli_request(backend: &str, keep_open: bool) -> SubagentRequest {
    SubagentRequest {
        backend: backend.to_string(),
        model: None,
        prompt: "hello".to_string(),
        depth: 1,
        keep_open,
        working_dir_override: None,
        effort: Effort::None,
    }
}

/// A `keep_open: true` dispatch onto a `claude_cli` backend takes the
/// long-lived driver in `process.rs`, not `run_once` in `one_shot.rs`.
/// The two spawn failure messages differ by one suffix
/// (`; set CLAUDE_CLI_PATH`, only `spawn_child` in `process.rs` adds
/// it), so which one comes back proves which path actually ran. No
/// real `claude` process is ever spawned: the binary this factory
/// points at exists but is not executable, so `Command::spawn` fails
/// before any subprocess starts.
#[tokio::test]
async fn keep_open_claude_cli_dispatch_takes_the_long_lived_driver_path() {
    let factory = claude_cli_factory_with_unspawnable_binary("claude-agent");
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();

    let err = run_subagent(
        &factory,
        claude_cli_request("claude-agent", true),
        parent_tx,
        empty_registry(),
    )
    .await
    .expect_err("spawn against a non-executable file should fail");

    assert!(
        err.contains("set CLAUDE_CLI_PATH"),
        "expected the long-lived driver's spawn error, got: {err}"
    );
}

/// A `keep_open: false` dispatch onto the very same backend still runs
/// through `run_once` in `one_shot.rs`, unchanged. Its spawn failure
/// message carries no `; set CLAUDE_CLI_PATH` suffix, unlike the
/// long-lived driver's.
#[tokio::test]
async fn one_shot_claude_cli_dispatch_still_takes_the_run_once_path() {
    let factory = claude_cli_factory_with_unspawnable_binary("claude-agent");
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();

    let err = run_subagent(
        &factory,
        claude_cli_request("claude-agent", false),
        parent_tx,
        empty_registry(),
    )
    .await
    .expect_err("spawn against a non-executable file should fail");

    assert!(
        !err.contains("set CLAUDE_CLI_PATH"),
        "expected the one-shot spawn error with no CLAUDE_CLI_PATH suffix, got: {err}"
    );
}

/// A failed spawn on the long-lived path never registers a session:
/// there is nothing worth keeping alive if the child never started.
#[tokio::test]
async fn failing_keep_open_claude_cli_dispatch_leaves_no_stale_session() {
    let factory = claude_cli_factory_with_unspawnable_binary("claude-agent");
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let registry = empty_registry();

    let _ = run_subagent(
        &factory,
        claude_cli_request("claude-agent", true),
        parent_tx,
        registry.clone(),
    )
    .await;

    assert_eq!(registry.len().await, 0);
}

fn factory_with_stub(name: &str, script: Vec<StubTurn>) -> Arc<BackendFactory> {
    Arc::new(BackendFactory::new(Settings::default(), PathBuf::from(".")).with_stub(name, script))
}

/// `run_subagent` end to end against a stub backend: no network call,
/// no child process, the answer comes straight from the script.
#[tokio::test]
async fn run_subagent_against_a_stub_backend_returns_the_scripted_text() {
    let factory = factory_with_stub(
        "stub-agent",
        vec![StubTurn::Text("scripted reply".to_string())],
    );
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();

    let outcome = run_subagent(
        &factory,
        stub_request("stub-agent", 1),
        parent_tx,
        empty_registry(),
    )
    .await
    .expect("stub dispatch should succeed");

    assert_eq!(outcome.text, "scripted reply");
    assert_eq!(outcome.backend, "stub-agent");
}

/// A stub scripted to fail makes the dispatch return an `Err`, the
/// same way a real backend's turn failure does.
#[tokio::test]
async fn run_subagent_against_a_stub_backend_propagates_a_scripted_error() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Error("boom".to_string())]);
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();

    let err = run_subagent(
        &factory,
        stub_request("stub-agent", 1),
        parent_tx,
        empty_registry(),
    )
    .await
    .expect_err("scripted error should surface as an Err");

    assert!(err.contains("boom"));
}

/// A stub-backed dispatch still emits its events through the same
/// forwarder path a real subagent uses: the parent sees a one-hop
/// route carrying this dispatch's id, exactly like `run_api_subagent`.
#[tokio::test]
async fn run_subagent_against_a_stub_backend_routes_events_to_the_parent() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let (parent_tx, mut parent_rx) = mpsc::unbounded_channel();

    run_subagent(
        &factory,
        stub_request("stub-agent", 1),
        parent_tx,
        empty_registry(),
    )
    .await
    .expect("stub dispatch should succeed");

    // The forwarder relays on its own spawned task, so the event may
    // not have crossed yet the instant `run_subagent` returns. `.await`
    // here, not `try_recv`, lets the runtime give that task a turn.
    let first = parent_rx.recv().await.expect("expected a routed event");
    assert_eq!(first.route.len(), 1);
    assert!(matches!(first.event, StreamEvent::Text { .. }));
}

/// `may_dispatch` still gates whether a *stub-backed* subagent's own
/// registry carries a `Task` tool: the depth check in
/// `build_api_backend` runs the same way regardless of which resolved
/// backend variant is being built around it. This exercises that at
/// the depth limit through `run_subagent`, not just through
/// `BackendFactory::build` directly.
#[tokio::test]
async fn run_subagent_at_the_depth_limit_still_succeeds_against_a_stub() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("done".to_string())]);
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();

    // Depth 2 sits at the default max depth of 2. A real Api backend
    // built at this depth gets no Task tool (see
    // `task_tool_absent_at_the_depth_limit` in `backend_factory.rs`). A
    // stub has no tool registry at all, so depth has no bearing on
    // whether the dispatch itself succeeds.
    let outcome = run_subagent(
        &factory,
        stub_request("stub-agent", 2),
        parent_tx,
        empty_registry(),
    )
    .await
    .expect("stub dispatch should succeed regardless of depth");

    assert_eq!(outcome.text, "done");
}

#[tokio::test]
async fn unknown_backend_name_names_the_request_and_lists_known_entries() {
    let factory = empty_factory();
    let req = SubagentRequest {
        backend: "nope".to_string(),
        model: None,
        prompt: "hello".to_string(),
        depth: 1,
        keep_open: false,
        working_dir_override: None,
        effort: Effort::None,
    };

    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let err = run_subagent(&factory, req, parent_tx, empty_registry())
        .await
        .expect_err("should error on an unknown backend name");

    assert!(err.contains("nope"));
    assert!(err.contains("none configured"));
}

/// `keep_open: true` against a stub backend leaves exactly one live
/// session in the registry, reachable by the id the outcome reports.
#[tokio::test]
async fn keep_open_dispatch_registers_exactly_one_reachable_session() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let registry = empty_registry();

    let outcome = run_subagent(
        &factory,
        keep_open_stub_request("stub-agent", 1),
        parent_tx,
        registry.clone(),
    )
    .await
    .expect("stub dispatch should succeed");

    let id = outcome
        .session_id
        .expect("keep_open dispatch should report a session id");
    assert_eq!(registry.len().await, 1);
    assert!(registry.contains(id).await);
}

/// `keep_open: false`, the default, leaves nothing behind: no session
/// id on the outcome, and the registry stays empty.
#[tokio::test]
async fn normal_dispatch_leaves_the_registry_empty() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let registry = empty_registry();

    let outcome = run_subagent(
        &factory,
        stub_request("stub-agent", 1),
        parent_tx,
        registry.clone(),
    )
    .await
    .expect("stub dispatch should succeed");

    assert!(outcome.session_id.is_none());
    assert_eq!(registry.len().await, 0);
}

/// The session id a kept-open dispatch registers under is the exact
/// same id its routed events carried, so a later turn into that
/// session renders inside the same `Subagent` block the first turn
/// opened.
#[tokio::test]
async fn kept_open_session_id_matches_the_route_id_events_carried() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let (parent_tx, mut parent_rx) = mpsc::unbounded_channel();
    let registry = empty_registry();

    let outcome = run_subagent(
        &factory,
        keep_open_stub_request("stub-agent", 1),
        parent_tx,
        registry,
    )
    .await
    .expect("stub dispatch should succeed");
    let session_id = outcome
        .session_id
        .expect("keep_open dispatch should report a session id");

    let routed = parent_rx.recv().await.expect("expected a routed event");
    assert_eq!(routed.route.len(), 1);
    assert_eq!(routed.route[0].id, session_id);
}

/// `req.effort` reaches the built subagent's own effort flag: calling
/// `run_stub_subagent` directly (now `pub`, reachable from this crate)
/// so the built `Backend` is still in hand afterward, since a normal
/// `run_subagent` dispatch drops it once the registry takes ownership.
#[tokio::test]
async fn run_stub_subagent_stores_the_requests_effort_on_the_built_backend() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let registry = empty_registry();
    let req = SubagentRequest {
        effort: Effort::Max,
        ..keep_open_stub_request("stub-agent", 1)
    };
    let working_dir = Arc::new(std::sync::Mutex::new(PathBuf::from(".")));

    let (_outcome, kept) = run_stub_subagent(
        &factory,
        &req,
        SubagentId::next(),
        test_meta(1),
        parent_tx,
        &registry,
        working_dir,
    )
    .await
    .expect("stub dispatch should succeed");

    let (backend, _turns) = kept.expect("keep_open dispatch should keep the backend");
    assert_eq!(Effort::load(&backend.effort_flag()), Effort::Max);
}

/// A subagent that fails its turn leaves no stale session behind even
/// when `keep_open` was requested: there is nothing worth keeping
/// alive if the turn itself never completed.
#[tokio::test]
async fn failing_keep_open_dispatch_leaves_no_stale_session() {
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Error("boom".to_string())]);
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let registry = empty_registry();

    let err = run_subagent(
        &factory,
        keep_open_stub_request("stub-agent", 1),
        parent_tx,
        registry.clone(),
    )
    .await
    .expect_err("scripted error should surface as an Err");

    assert!(err.contains("boom"));
    assert_eq!(registry.len().await, 0);
}

// `drain_reply_and_forward` is the mechanism `SendMessage` relies on to
// recover a kept-open `claude_cli` session's reply text (see
// `SubagentRegistry::send_message` in `src/backend/registry.rs`), and
// the same mechanism the turn that opens such a session already uses
// in `run_claude_cli_keep_open_subagent`. A real `claude` child cannot
// be spawned in this suite, so these tests drive the recovery logic
// directly over a scripted event sequence on a plain channel, exactly
// as a driver's stdout reader would populate one.

/// Two `Text` events land as one concatenated reply, and both events
/// come out the other side carrying the new route hop.
#[test]
fn drain_reply_and_forward_concatenates_text_and_forwards_with_route_hop() {
    let (tx, rx_input) = mpsc::unbounded_channel();
    let (parent_tx, mut parent_rx) = mpsc::unbounded_channel();
    let id = SubagentId::next();
    let meta = test_meta(1);

    tx.send(RoutedEvent::own(StreamEvent::Text {
        turn: 1,
        text: "hello ".to_string(),
    }))
    .unwrap();
    tx.send(RoutedEvent::own(StreamEvent::Text {
        turn: 1,
        text: "world".to_string(),
    }))
    .unwrap();
    drop(tx);

    let mut rx = rx_input;
    let drained = drain_reply_and_forward(id, &meta, 2, 20, 1, 10, &mut rx, &parent_tx);

    assert_eq!(drained.text, "hello world");
    assert!(!drained.interrupted);
    assert!(drained.error.is_none());

    let first = parent_rx.try_recv().expect("first event forwarded");
    assert_eq!(first.route.len(), 1);
    assert_eq!(first.route[0].id, id);
    assert_eq!(first.route[0].session_turns, 2);
    assert_eq!(first.route[0].session_turn_cap, 20);
    assert_eq!(first.route[0].send_message_calls, 1);
    assert_eq!(first.route[0].send_message_call_cap, 10);
    let second = parent_rx.try_recv().expect("second event forwarded");
    assert_eq!(second.route.len(), 1);
    assert_eq!(second.route[0].id, id);
    assert!(parent_rx.try_recv().is_err());
}

/// An `Interrupted` event in the drained batch sets the flag, alongside
/// whatever text arrived before it.
#[test]
fn drain_reply_and_forward_reports_interrupted() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let id = SubagentId::next();
    let meta = test_meta(1);

    tx.send(RoutedEvent::own(StreamEvent::Text {
        turn: 1,
        text: "partial".to_string(),
    }))
    .unwrap();
    tx.send(RoutedEvent::own(StreamEvent::Interrupted {
        message: "Interrupted by user (Escape)".to_string(),
    }))
    .unwrap();
    drop(tx);

    let drained = drain_reply_and_forward(id, &meta, 1, 20, 0, 10, &mut rx, &parent_tx);

    assert_eq!(drained.text, "partial");
    assert!(drained.interrupted);
}

/// An `Error` event in the drained batch is captured verbatim.
#[test]
fn drain_reply_and_forward_reports_error() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let id = SubagentId::next();
    let meta = test_meta(1);

    tx.send(RoutedEvent::own(StreamEvent::Error {
        message: "boom".to_string(),
    }))
    .unwrap();
    drop(tx);

    let drained = drain_reply_and_forward(id, &meta, 1, 20, 0, 10, &mut rx, &parent_tx);

    assert_eq!(drained.error, Some("boom".to_string()));
    assert!(!drained.interrupted);
}

fn text_event() -> RoutedEvent {
    RoutedEvent::own(StreamEvent::Text {
        turn: 1,
        text: "hello".to_string(),
    })
}

fn test_meta(depth: u32) -> SubagentMeta {
    SubagentMeta {
        backend: "ollama".to_string(),
        model: "test-model".to_string(),
        depth,
    }
}

/// A main-session event, never touched by any forwarder, carries an
/// empty route. `RoutedEvent::own` is exactly what `AgentLoop` and
/// `ClaudeCliDriver` wrap their own events in.
#[test]
fn main_session_event_carries_an_empty_route() {
    let event = text_event();
    assert!(event.route.is_empty());
}

/// A depth-1 subagent's own event, relayed through one forwarder,
/// arrives with a one-element route: just that subagent's id.
#[tokio::test]
async fn depth_one_forwarder_produces_a_one_element_route() {
    let (tx_sub, rx_sub) = mpsc::unbounded_channel();
    let (parent_tx, mut parent_rx) = mpsc::unbounded_channel();
    let id = SubagentId::next();
    spawn_event_forwarder(
        id,
        test_meta(1),
        rx_sub,
        parent_tx,
        Arc::new(AtomicU32::new(1)),
        20,
        empty_registry(),
        10,
    );

    tx_sub.send(text_event()).expect("send into own channel");
    drop(tx_sub);

    let routed = parent_rx.recv().await.expect("forwarded event");
    assert_eq!(routed.route.len(), 1);
    assert_eq!(routed.route[0].id, id);
    assert_eq!(routed.route[0].meta.depth, 1);
    assert_eq!(routed.route[0].session_turns, 1);
    assert_eq!(routed.route[0].session_turn_cap, 20);
    assert_eq!(routed.route[0].send_message_call_cap, 10);
}

/// A depth-2 subagent's event passes through two forwarders on its way
/// up: its own, then its depth-1 parent's. The route accumulates
/// outermost first: the depth-1 id (dispatched directly by the main
/// session) at index 0, the depth-2 id (dispatched by that subagent)
/// at index 1.
#[tokio::test]
async fn depth_two_forwarder_chain_produces_a_two_element_outermost_first_route() {
    let (tx_inner, rx_inner) = mpsc::unbounded_channel();
    let (mid_tx, mid_rx) = mpsc::unbounded_channel();
    let (outer_tx, mut outer_rx) = mpsc::unbounded_channel();
    let depth_two_id = SubagentId::next();
    let depth_one_id = SubagentId::next();

    // The depth-2 subagent's own forwarder: its native events (empty
    // route) become one-element as they reach its depth-1 parent.
    spawn_event_forwarder(
        depth_two_id,
        test_meta(2),
        rx_inner,
        mid_tx,
        Arc::new(AtomicU32::new(1)),
        20,
        empty_registry(),
        10,
    );
    // The depth-1 subagent's own forwarder: relays both its own native
    // events and whatever its nested depth-2 dispatch already routed.
    spawn_event_forwarder(
        depth_one_id,
        test_meta(1),
        mid_rx,
        outer_tx,
        Arc::new(AtomicU32::new(1)),
        20,
        empty_registry(),
        10,
    );

    tx_inner.send(text_event()).expect("send into own channel");
    drop(tx_inner);

    let routed = outer_rx.recv().await.expect("forwarded event");
    assert_eq!(routed.route.len(), 2);
    assert_eq!(routed.route[0].id, depth_one_id);
    assert_eq!(routed.route[0].meta.depth, 1);
    assert_eq!(routed.route[1].id, depth_two_id);
    assert_eq!(routed.route[1].meta.depth, 2);
}

// `resolve_subagent_working_dir` and the working_dir override on `Task`
// (P4S05). These tests live near the top-level `run_subagent` tests
// above since they exercise the same entry points, not a separate
// module: the override is threaded through the exact same dispatch
// path every other test in this file already covers.

/// Create a uniquely named directory under the system temp dir, the
/// same helper shape `src/tools/cd.rs`'s own tests already use for the
/// same purpose.
fn unique_temp_dir(tag: &str) -> PathBuf {
    super::scratch_dir("dsc-subagent", tag)
}

#[test]
fn resolve_subagent_working_dir_with_a_valid_override_returns_its_canonical_path() {
    let dir = unique_temp_dir("valid-override");
    let factory = empty_factory();

    let resolved = resolve_subagent_working_dir(&factory, Some(dir.as_path()))
        .expect("an existing directory should resolve");

    let canonical = std::fs::canonicalize(&dir).unwrap();
    assert_eq!(*resolved.lock().unwrap(), canonical);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_subagent_working_dir_rejects_a_missing_path_naming_it() {
    let start = unique_temp_dir("missing-override-start");
    let missing = start.join("does-not-exist");
    let factory = empty_factory();

    let err = resolve_subagent_working_dir(&factory, Some(missing.as_path()))
        .expect_err("a missing path must fail, not build a backend anyway");

    assert!(err.contains("does not exist"));
    assert!(err.contains(&missing.display().to_string()));

    let _ = std::fs::remove_dir_all(&start);
}

#[test]
fn resolve_subagent_working_dir_rejects_a_file_naming_it() {
    let start = unique_temp_dir("file-override-start");
    let file = start.join("not_a_dir.txt");
    std::fs::write(&file, "hello").unwrap();
    let factory = empty_factory();

    let err = resolve_subagent_working_dir(&factory, Some(file.as_path()))
        .expect_err("a file path must fail, not build a backend anyway");

    assert!(err.contains("is not a directory"));
    assert!(err.contains(&file.display().to_string()));

    let _ = std::fs::remove_dir_all(&start);
}

/// Without an override, a subagent inherits: it starts from whatever
/// the parent factory's working directory currently holds, at the
/// moment of dispatch.
#[test]
fn resolve_subagent_working_dir_without_an_override_inherits_the_factorys_current_value() {
    let factory = empty_factory();
    let seeded = PathBuf::from("C:/parent-was-here");
    *factory.working_dir().lock().unwrap() = seeded.clone();

    let resolved =
        resolve_subagent_working_dir(&factory, None).expect("inheriting should never fail");

    assert_eq!(*resolved.lock().unwrap(), seeded);
}

/// The `Arc` `resolve_subagent_working_dir` hands back, override or
/// inherited, is never the factory's own shared `Arc`: mutating one
/// side must never move the other. This is the isolation guarantee the
/// briefing calls for: a subagent's own directory change must never
/// move its parent's, in either direction.
#[test]
fn resolved_working_dir_is_independent_of_the_factorys_own_arc() {
    let factory = empty_factory();
    let original_parent_value = factory.working_dir_snapshot_for_test();

    let resolved =
        resolve_subagent_working_dir(&factory, None).expect("inheriting should never fail");

    *resolved.lock().unwrap() = PathBuf::from("C:/moved/by/subagent");
    assert_eq!(
        factory.working_dir_snapshot_for_test(),
        original_parent_value,
        "a subagent's own directory change must never move its parent's"
    );

    *factory.working_dir().lock().unwrap() = PathBuf::from("C:/moved/by/parent");
    assert_eq!(
        *resolved.lock().unwrap(),
        PathBuf::from("C:/moved/by/subagent"),
        "a later change to the parent's directory must never reach an already-resolved subagent"
    );
}

/// The `Arc` an override resolves to is a real, usable working
/// directory: a tool built against it (`BashTool`, the same one
/// `build_api_backend` wires every `Api` subagent's tools to) acts in
/// the overridden directory, not wherever the factory itself points.
#[tokio::test]
async fn override_reaches_a_tool_built_against_the_resolved_working_dir() {
    let dir = unique_temp_dir("override-reaches-tool");
    let factory = empty_factory();

    let resolved = resolve_subagent_working_dir(&factory, Some(dir.as_path()))
        .expect("an existing directory should resolve");

    let bash = BashTool::new(resolved);
    let output = bash
        .execute(serde_json::json!({"command": "cd"}))
        .await
        .expect("execute");
    assert!(!output.is_error, "unexpected error: {}", output.content);

    let canonical = std::fs::canonicalize(&dir).unwrap();
    let dir_name = canonical.file_name().unwrap().to_string_lossy();
    assert!(
        output.content.contains(dir_name.as_ref()),
        "expected bash cwd to contain {}, got: {}",
        dir_name,
        output.content
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `working_dir_override` on the request reaches all the way through
/// `run_subagent` against a stub backend without error: the override
/// is validated and resolved before the backend is ever built.
#[tokio::test]
async fn run_subagent_with_a_working_dir_override_against_a_stub_backend_still_succeeds() {
    let dir = unique_temp_dir("run-subagent-override");
    let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("done".to_string())]);
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let request = SubagentRequest {
        working_dir_override: Some(dir.clone()),
        ..stub_request("stub-agent", 1)
    };

    let outcome = run_subagent(&factory, request, parent_tx, empty_registry())
        .await
        .expect("override should resolve and dispatch should succeed");

    assert_eq!(outcome.text, "done");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A bad `working_dir_override` fails before the named backend is even
/// resolved: an unknown backend name never surfaces here, only the
/// override error does. `TaskTool::execute` (see `src/tools/task.rs`)
/// turns this `Err` into a tool error, never a hard failure of the
/// caller's turn.
#[tokio::test]
async fn run_subagent_with_a_missing_working_dir_override_fails_before_resolving_the_backend() {
    let factory = empty_factory();
    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    let missing = std::env::temp_dir().join("dsc-subagent-does-not-exist-at-all");
    let request = SubagentRequest {
        backend: "nope".to_string(),
        working_dir_override: Some(missing.clone()),
        ..stub_request("nope", 1)
    };

    let err = run_subagent(&factory, request, parent_tx, empty_registry())
        .await
        .expect_err("a missing override must fail the dispatch");

    assert!(err.contains("does not exist"));
    assert!(err.contains(&missing.display().to_string()));
    // The unknown-backend message names "none configured"; seeing that
    // here would mean backend resolution ran before the override check.
    assert!(!err.contains("none configured"));
}
