//! Runs one prompt on a named backend to completion and returns the final
//! text. This is the machinery the `Task` tool sits on. T04 adds that
//! tool. It dispatches a subagent onto another backend, on a different
//! model or provider than the parent session.
//!
//! A subagent never shares the caller's own event channel directly.
//! `run_subagent` gives it a fresh channel of its own, then forwards
//! every event from that channel onto the caller's, tagging each one with
//! this dispatch's `SubagentId` prepended to its route. A nested dispatch
//! composes for free: a depth-2 subagent's forwarder prepends its own id
//! to a route that already carries its depth-1 parent's id, so the event
//! arrives at the top with a two-element, outermost-first route.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::agent::agent_loop::{RouteHop, RoutedEvent, StreamEvent, SubagentId, SubagentMeta};
use crate::backend::Backend;
use crate::backend::claude_cli::process::ClaudeCliDriver;
use crate::backend::factory::{BackendFactory, ResolvedBackend};
use crate::backend::registry::SubagentRegistry;
use crate::effort::Effort;

/// One subagent dispatch request. Picks the backend to run on, an
/// optional model override, and the prompt. `depth` says how deep in
/// the dispatch chain this subagent sits.
pub struct SubagentRequest {
    pub backend: String,
    pub model: Option<String>,
    pub prompt: String,
    /// 1 for a subagent dispatched by the main session.
    pub depth: u32,
    /// When true, the backend survives past this one turn: it is
    /// registered in the caller's `SubagentRegistry` under the id this
    /// dispatch allocates, instead of being dropped once the turn ends.
    pub keep_open: bool,
    /// Points the subagent at a working directory of its own, instead of
    /// wherever the calling session currently stands. `None` means
    /// inherit: the subagent starts from a snapshot of the parent's
    /// current working directory, taken at dispatch time. Either way the
    /// subagent gets a directory it owns; see `resolve_subagent_working_dir`.
    pub working_dir_override: Option<PathBuf>,
    /// The reasoning-effort level this subagent runs at, already resolved:
    /// an explicit override from the `Task` call, or the dispatching
    /// session's own current level when the call carried none. Resolution
    /// happens in `TaskTool::execute` (`src/tools/task.rs`), before this
    /// request is ever built, so this field is never itself optional here.
    /// Applied to the freshly built subagent's own effort flag, never the
    /// dispatching session's: see the `Effort::store` calls below.
    pub effort: Effort,
}

/// The result of a completed subagent run. Carries the final text.
/// Also carries the backend and model that actually produced it, once
/// any override resolves.
#[derive(Debug)]
pub struct SubagentOutcome {
    pub text: String,
    pub backend: String,
    pub model: String,
    /// The id the session was registered under, when `keep_open` was
    /// true and the dispatch succeeded. `None` for a normal, one-shot
    /// dispatch: nothing was left in the registry for it.
    pub session_id: Option<SubagentId>,
}

/// Resolve the working directory a dispatched subagent starts in.
///
/// An explicit override must exist and be a directory. Neither check nor
/// failure ever touches the caller's own working directory: on success the
/// override is canonicalized into a fresh `Arc` this subagent alone owns.
/// A bad override comes back as an `Err`, which `run_subagent`'s caller (the
/// `Task` tool) turns into a tool error naming the path, never a hard
/// failure of the caller's turn.
///
/// Without an override, the subagent inherits: it starts from a snapshot of
/// `factory`'s current working directory, copied into a fresh `Arc` of its
/// own rather than the shared one `factory.working_dir()` returns. That is
/// what keeps the two directions independent afterward: a `Cd` call inside
/// the subagent writes only its own `Arc`, so it can never move the
/// parent's, and nothing the parent does after dispatch (including its own
/// further `Cd` calls) reaches back into a subagent already running.
pub fn resolve_subagent_working_dir(
    factory: &Arc<BackendFactory>,
    override_path: Option<&Path>,
) -> Result<Arc<Mutex<PathBuf>>, String> {
    match override_path {
        Some(path) => {
            if !path.exists() {
                return Err(format!(
                    "working_dir override does not exist: {}",
                    path.display()
                ));
            }
            if !path.is_dir() {
                return Err(format!(
                    "working_dir override is not a directory: {}",
                    path.display()
                ));
            }
            let canonical = std::fs::canonicalize(path)
                .map_err(|e| format!("working_dir override {}: {e}", path.display()))?;
            Ok(Arc::new(Mutex::new(canonical)))
        }
        None => Ok(Arc::new(Mutex::new(factory.working_dir_snapshot()))),
    }
}

/// Run one prompt on a named backend to completion. An unknown backend
/// name returns the same error `BackendFactory::build` would return. It
/// names the request and lists the known entries.
///
/// An `Api` backend runs through the normal `AgentLoop::run` turn. Its
/// own max-turns guard bounds it. A `ClaudeCli` backend instead runs
/// through `ClaudeCliDriver::run_once`, not the long-lived, stdin-fed
/// path a GUI session would use. A subagent asks one question and wants
/// the process gone once it answers.
///
/// `registry` is the calling agent's own `SubagentRegistry`. When
/// `req.keep_open` is true and the dispatch succeeds, the backend that
/// ran the turn is registered there under this dispatch's `SubagentId`,
/// and that same id comes back on `SubagentOutcome::session_id`. A
/// `claude_cli` backend with `keep_open` set routes through the
/// long-lived `ClaudeCliDriver` (`src/backend/claude_cli/process.rs`)
/// instead of `run_once`, so the child survives past this one turn. See
/// "Phase 3: multi-turn subagent sessions" in
/// `docs/plans/2026-08-04-long-term-roadmap.md`.
pub async fn run_subagent(
    factory: &Arc<BackendFactory>,
    req: SubagentRequest,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    registry: Arc<SubagentRegistry>,
) -> Result<SubagentOutcome, String> {
    let start = Instant::now();
    let id = SubagentId::next();
    info!(
        backend = %req.backend,
        depth = req.depth,
        keep_open = req.keep_open,
        "subagent starting"
    );

    let working_dir = resolve_subagent_working_dir(factory, req.working_dir_override.as_deref())?;
    let resolved = factory.resolve(&req.backend, req.model.as_deref())?;
    // `kept_backend` covers the `Api` and `Stub` paths, whose events are
    // already relayed forever by `spawn_event_forwarder` and whose reply
    // text `Backend::run` already returns directly. It carries the shared
    // turn counter alongside the backend, so the same atomic the forwarder
    // has been reading all along becomes this session's `SessionEntry.turns`
    // (see `SubagentRegistry::register_with_turns_handle`). `claude_cli_relay`
    // covers the one path that needs more than the backend itself kept
    // alive: a kept-open `claude_cli` session also needs its still-open
    // event receiver and route metadata, so `send_message` can recover a
    // later turn's text through `drain_reply_and_forward`.
    let mut kept_backend: Option<(Backend, Arc<AtomicU32>)> = None;
    let mut claude_cli_relay: Option<(Backend, SubagentMeta, mpsc::UnboundedReceiver<RoutedEvent>)> = None;
    let mut outcome = match resolved {
        ResolvedBackend::Api { model, .. } => {
            let meta = SubagentMeta {
                backend: req.backend.clone(),
                model,
                depth: req.depth,
            };
            let (outcome, kept) = run_api_subagent(
                factory,
                &req,
                id,
                meta,
                parent_tx.clone(),
                &registry,
                working_dir.clone(),
            )
            .await?;
            kept_backend = kept;
            outcome
        }
        ResolvedBackend::ClaudeCli {
            model,
            permission_mode,
            env,
            ..
        } => {
            let meta = SubagentMeta {
                backend: req.backend.clone(),
                model: model.clone(),
                depth: req.depth,
            };
            if req.keep_open {
                let (outcome, backend, events_rx) = run_claude_cli_keep_open_subagent(
                    factory,
                    &req,
                    model,
                    permission_mode,
                    env,
                    id,
                    meta.clone(),
                    parent_tx.clone(),
                    &registry,
                    working_dir.clone(),
                )
                .await?;
                if let (Some(backend), Some(events_rx)) = (backend, events_rx) {
                    claude_cli_relay = Some((backend, meta, events_rx));
                }
                outcome
            } else {
                run_claude_cli_subagent(
                    factory,
                    &req,
                    model,
                    permission_mode,
                    env,
                    id,
                    meta,
                    parent_tx.clone(),
                    &registry,
                    working_dir.clone(),
                )
                .await?
            }
        }
        #[cfg(feature = "test-support")]
        ResolvedBackend::Stub { model, .. } => {
            let meta = SubagentMeta {
                backend: req.backend.clone(),
                model,
                depth: req.depth,
            };
            let (outcome, kept) = run_stub_subagent(
                factory,
                &req,
                id,
                meta,
                parent_tx.clone(),
                &registry,
                working_dir.clone(),
            )
            .await?;
            kept_backend = kept;
            outcome
        }
    };

    if let Some((backend, turns)) = kept_backend {
        registry.register_with_turns_handle(id, backend, turns).await;
        outcome.session_id = Some(id);
    } else if let Some((backend, meta, events_rx)) = claude_cli_relay {
        registry
            .register_claude_cli_session(id, backend, meta, parent_tx, events_rx)
            .await;
        outcome.session_id = Some(id);
    }

    info!(
        backend = %outcome.backend,
        model = %outcome.model,
        depth = req.depth,
        elapsed_ms = start.elapsed().as_millis() as u64,
        "subagent finished"
    );
    Ok(outcome)
}

/// Drive an `Api` backend to completion. Builds it through the factory,
/// so the depth-gated `Task` tool wiring applies the same way it would
/// for any other backend. Runs one turn and joins the response
/// segments.
pub async fn run_api_subagent(
    factory: &Arc<BackendFactory>,
    req: &SubagentRequest,
    id: SubagentId,
    meta: SubagentMeta,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    registry: &Arc<SubagentRegistry>,
    working_dir: Arc<Mutex<PathBuf>>,
) -> Result<(SubagentOutcome, Option<(Backend, Arc<AtomicU32>)>), String> {
    let (tx_events, rx_events) = mpsc::unbounded_channel::<RoutedEvent>();
    let turns = Arc::new(AtomicU32::new(1));
    spawn_event_forwarder(
        id,
        meta,
        rx_events,
        parent_tx,
        Arc::clone(&turns),
        factory.session_turn_cap(),
        Arc::clone(registry),
        factory.send_message_call_cap(),
    );

    let sub_factory = factory.with_working_dir(working_dir);
    let backend = sub_factory.build(&req.backend, req.model.as_deref(), tx_events, req.depth)?;
    let Backend::Api(mut agent) = backend else {
        return Err(format!(
            "backend \"{}\" resolved as api but built as claude_cli",
            req.backend
        ));
    };
    // Written onto the freshly built subagent's own flag, never the
    // dispatching session's: `req.effort` is already fully resolved by
    // `TaskTool::execute`, whether that came from an explicit override or
    // from inheriting the parent's own current level.
    req.effort.store(&agent.effort_flag());

    let model = agent.model_flag().lock().unwrap().clone();
    let segments = agent.run(&req.prompt).await.map_err(|e| e.to_string())?;
    let outcome = SubagentOutcome {
        text: segments.join(""),
        backend: req.backend.clone(),
        model,
        session_id: None,
    };
    let kept = req.keep_open.then(|| (Backend::Api(agent), turns));
    Ok((outcome, kept))
}

/// Drive a `Stub` backend to completion. Builds it through the factory,
/// the same way `run_api_subagent` does, so a stub-backed dispatch goes
/// through the exact `BackendFactory::build` path a real one does. Runs
/// one turn against the stub's script and returns its text, or its
/// scripted error.
#[cfg(feature = "test-support")]
pub async fn run_stub_subagent(
    factory: &Arc<BackendFactory>,
    req: &SubagentRequest,
    id: SubagentId,
    meta: SubagentMeta,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    registry: &Arc<SubagentRegistry>,
    working_dir: Arc<Mutex<PathBuf>>,
) -> Result<(SubagentOutcome, Option<(Backend, Arc<AtomicU32>)>), String> {
    let (tx_events, rx_events) = mpsc::unbounded_channel::<RoutedEvent>();
    let turns = Arc::new(AtomicU32::new(1));
    spawn_event_forwarder(
        id,
        meta,
        rx_events,
        parent_tx,
        Arc::clone(&turns),
        factory.session_turn_cap(),
        Arc::clone(registry),
        factory.send_message_call_cap(),
    );

    let sub_factory = factory.with_working_dir(working_dir);
    let backend = sub_factory.build(&req.backend, req.model.as_deref(), tx_events, req.depth)?;
    let Backend::Stub(mut stub) = backend else {
        return Err(format!(
            "backend \"{}\" resolved as stub but built as something else",
            req.backend
        ));
    };
    // Same rule as `run_api_subagent`: only the freshly built subagent's
    // own flag is written.
    req.effort.store(&stub.effort_flag());

    let model = stub.model_flag().lock().unwrap().clone();
    let segments = stub.run(&req.prompt).await.map_err(|e| e.to_string())?;
    let outcome = SubagentOutcome {
        text: segments.join(""),
        backend: req.backend.clone(),
        model,
        session_id: None,
    };
    let kept = req.keep_open.then(|| (Backend::Stub(stub), turns));
    Ok((outcome, kept))
}

/// Drive a `ClaudeCli` backend through one `run_once` call. A single
/// prompt goes in. A single answer comes out, then the child process
/// exits. This never touches the long-lived, stdin-fed driver a GUI
/// session would build.
///
/// `run_once` streams no `StreamEvent`s of its own: there is no long-lived
/// `AgentLoop` here for a forwarder to relay from. So this function sends
/// exactly one routed event once the call resolves, carrying `id` and
/// `meta` as its one-hop route. That single event both creates the
/// `Subagent` block (it did not exist before) and immediately advances it
/// to its terminal state, since `Transcript::apply_routed_event` does both
/// steps for a route that bottoms out on this event. There is no partial
/// token stream to forward, and none is invented here.
async fn run_claude_cli_subagent(
    factory: &Arc<BackendFactory>,
    req: &SubagentRequest,
    model: String,
    permission_mode: Option<String>,
    env: Option<std::collections::HashMap<String, String>>,
    id: SubagentId,
    meta: SubagentMeta,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    registry: &Arc<SubagentRegistry>,
    working_dir: Arc<Mutex<PathBuf>>,
) -> Result<SubagentOutcome, String> {
    // The factory's own flag, not a fresh one. `run_once` polls it between
    // lines, so Escape reaches a claude_cli subagent the same way it
    // reaches an api one.
    let interrupt_flag = factory.interrupt_flag();
    // A one-shot run never sees a later write to `working_dir`, so a plain
    // snapshot is enough: unlike the long-lived driver below, there is no
    // second turn for a live re-read to matter to.
    let dir = working_dir.lock().unwrap().clone();
    let result = ClaudeCliDriver::run_once(
        &model,
        permission_mode.as_deref(),
        env.as_ref(),
        &dir,
        &req.prompt,
        Arc::clone(&interrupt_flag),
        req.effort,
    )
    .await;

    // A one-shot dispatch never runs a second turn and never opens a
    // session `SendMessage` could call into, so `session_turns` is always
    // 1 and `send_message_calls` is just whatever the owner's registry
    // already stood at from its other open sessions.
    let route = vec![RouteHop {
        id,
        meta,
        session_turns: 1,
        session_turn_cap: factory.session_turn_cap(),
        send_message_calls: registry.send_message_call_count(),
        send_message_call_cap: factory.send_message_call_cap(),
    }];
    let send_terminal = |event: StreamEvent| {
        let _ = parent_tx.send(RoutedEvent { route, event });
    };

    match result {
        Err(e) if interrupt_flag.load(Ordering::SeqCst) => {
            send_terminal(StreamEvent::Interrupted {
                message: e.clone(),
            });
            Err(e)
        }
        Err(e) => {
            send_terminal(StreamEvent::Error {
                message: e.clone(),
            });
            Err(e)
        }
        Ok(result) if result.is_error => {
            let message = format!(
                "subagent on backend \"{}\" returned an error result: {}",
                req.backend, result.text
            );
            send_terminal(StreamEvent::Error {
                message: message.clone(),
            });
            Err(message)
        }
        Ok(result) => {
            let total_tokens = (result.input_tokens + result.output_tokens) as usize;
            send_terminal(StreamEvent::TurnEnd {
                turn: 1,
                finish_reason: "stop".to_string(),
                total_tokens,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
            });
            Ok(SubagentOutcome {
                text: result.text,
                backend: req.backend.clone(),
                model,
                session_id: None,
            })
        }
    }
}

/// One turn's reply, recovered by draining whatever a `ClaudeCliDriver`
/// placed on its own event channel for that turn. Shared between the first
/// turn a `keep_open` dispatch runs here and every later turn
/// `SubagentRegistry::send_message` sends into the same session (see
/// `src/backend/registry.rs`), so the two call sites recover text the same
/// one way instead of drifting apart.
pub struct DrainedReply {
    pub text: String,
    pub interrupted: bool,
    pub error: Option<String>,
}

/// Drain every event currently buffered on `rx`, accumulating this turn's
/// reply text, then forward each drained event onward to `parent_tx` with
/// `id` and `meta` prepended as a new route hop.
///
/// `ClaudeCliDriver::send` streams its events onto an ordinary `mpsc`
/// channel rather than returning them, so a caller cannot read the reply
/// text off `send`'s return value the way `run_api_subagent` reads it off
/// `agent.run`. Draining the channel right after `send` resolves is
/// race-free, not merely likely to work: the driver's stdout reader task
/// places every mapped event for a turn onto the channel before it signals
/// `turn_done`, and `send` only returns once it has received that signal.
/// So every event the turn produced is already sitting in the channel, in
/// order, the moment `send` hands control back to the caller, even though
/// nothing has drained it yet. A plain `try_recv` loop picks all of it up
/// without waiting on whichever task happens to run next.
#[allow(clippy::too_many_arguments)]
pub fn drain_reply_and_forward(
    id: SubagentId,
    meta: &SubagentMeta,
    session_turns: u32,
    session_turn_cap: u32,
    send_message_calls: u32,
    send_message_call_cap: u32,
    rx: &mut mpsc::UnboundedReceiver<RoutedEvent>,
    parent_tx: &mpsc::UnboundedSender<RoutedEvent>,
) -> DrainedReply {
    let mut text = String::new();
    let mut interrupted = false;
    let mut error = None;
    let mut drained = Vec::new();
    while let Ok(routed) = rx.try_recv() {
        match &routed.event {
            StreamEvent::Text { text: chunk, .. } => text.push_str(chunk),
            StreamEvent::Interrupted { .. } => interrupted = true,
            StreamEvent::Error { message } => error = Some(message.clone()),
            _ => {}
        }
        drained.push(routed);
    }
    for routed in drained {
        let mut route = routed.route;
        route.insert(
            0,
            RouteHop {
                id,
                meta: meta.clone(),
                session_turns,
                session_turn_cap,
                send_message_calls,
                send_message_call_cap,
            },
        );
        if parent_tx.send(RoutedEvent { route, event: routed.event }).is_err() {
            break;
        }
    }
    DrainedReply {
        text,
        interrupted,
        error,
    }
}

/// Drive a `ClaudeCli` backend that must survive past this one turn. Builds
/// a long-lived `ClaudeCliDriver`, the same one `Backend::new_claude_cli`
/// builds for a GUI session, and runs exactly one turn on it through
/// `send`. The driver and its still-open event receiver are both returned
/// alongside the outcome, so the caller can register them in the
/// `SubagentRegistry`: the child process stays alive for a later turn
/// instead of exiting the way `run_once` does, and the receiver stays
/// available for `SubagentRegistry::send_message` to drain on every turn
/// after this one, through the same `drain_reply_and_forward` this
/// function itself uses for its own turn. Unlike the `Api` and `Stub`
/// paths, no background forwarder task ever takes over this receiver: a
/// `claude_cli` session's only way to run a later turn is through
/// `send_message`, so draining happens there, synchronously, turn by turn,
/// rather than on a detached task with no way to hand text back.
pub async fn run_claude_cli_keep_open_subagent(
    factory: &Arc<BackendFactory>,
    req: &SubagentRequest,
    model: String,
    permission_mode: Option<String>,
    env: Option<std::collections::HashMap<String, String>>,
    id: SubagentId,
    meta: SubagentMeta,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    registry: &Arc<SubagentRegistry>,
    working_dir: Arc<Mutex<PathBuf>>,
) -> Result<(SubagentOutcome, Option<Backend>, Option<mpsc::UnboundedReceiver<RoutedEvent>>), String> {
    let (tx_events, mut rx_events) = mpsc::unbounded_channel::<RoutedEvent>();
    // This subagent's own `working_dir`, not `factory.working_dir()`: the
    // long-lived driver re-reads it before every later turn (see
    // `ensure_ready` in `process.rs`), so handing it the shared factory
    // value would let this subagent's own directory changes, or the
    // parent's, cross into the other session.
    let mut driver = ClaudeCliDriver::new(
        model.clone(),
        permission_mode,
        env,
        working_dir,
        tx_events,
    );
    // Written before the first `send`, so `ensure_ready`'s spawn-time
    // effort check (see `process.rs`) sees this level on the very first
    // spawn, not just from the second turn onward.
    req.effort.store(&driver.effort_flag());

    // Captured, not `?`-propagated immediately: a spawn failure inside
    // `send` sends its full, detailed message on `tx_events` (see
    // `spawn_child` in `process.rs`) but returns only a bare
    // `HarnessError::Io`, whose `Display` drops that detail. Draining the
    // channel below recovers the fuller message before this function
    // decides what to return.
    let send_result = driver.send(&req.prompt).await;
    // This is the session's opening turn, so `session_turns` is always 1
    // here. Later turns run through `SubagentRegistry::send_message`,
    // which computes its own current count directly off the
    // `SessionEntry` this dispatch is about to register.
    let drained = drain_reply_and_forward(
        id,
        &meta,
        1,
        factory.session_turn_cap(),
        registry.send_message_call_count(),
        factory.send_message_call_cap(),
        &mut rx_events,
        &parent_tx,
    );

    if let Err(e) = send_result {
        return Err(drained.error.unwrap_or_else(|| e.to_string()));
    }
    if drained.interrupted {
        return Err(format!(
            "subagent on backend \"{}\" was interrupted",
            req.backend
        ));
    }

    let outcome = SubagentOutcome {
        text: drained.text,
        backend: req.backend.clone(),
        model,
        session_id: None,
    };
    Ok((outcome, Some(Backend::ClaudeCli(driver)), Some(rx_events)))
}

/// Relay every event a subagent emits onto `parent_tx`, prepending `id` to
/// each event's route before it goes out. The subagent's own events arrive
/// on `rx` with whatever route it already assembled: empty for an event
/// the subagent's own `AgentLoop` sent directly, or a shorter route
/// already carrying a deeper subagent's id when this subagent itself
/// forwarded on a nested dispatch. Either way, prepending `id` here adds
/// exactly one route element, so nesting composes without this function
/// needing to know how deep it sits.
///
/// A send that fails because `parent_tx`'s receiver has been dropped is
/// swallowed rather than treated as an error: the caller may simply not
/// be listening yet, the same way the drain this replaces never demanded
/// a live receiver either.
#[allow(clippy::too_many_arguments)]
pub fn spawn_event_forwarder(
    id: SubagentId,
    meta: SubagentMeta,
    mut rx: mpsc::UnboundedReceiver<RoutedEvent>,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    turns: Arc<AtomicU32>,
    session_turn_cap: u32,
    registry: Arc<SubagentRegistry>,
    send_message_call_cap: u32,
) {
    tokio::spawn(async move {
        while let Some(RoutedEvent { mut route, event }) = rx.recv().await {
            route.insert(
                0,
                RouteHop {
                    id,
                    meta: meta.clone(),
                    session_turns: turns.load(Ordering::SeqCst),
                    session_turn_cap,
                    send_message_calls: registry.send_message_call_count(),
                    send_message_call_cap,
                },
            );
            debug!(?event, "subagent event forwarded");
            if parent_tx.send(RoutedEvent { route, event }).is_err() {
                return;
            }
        }
    });
}

