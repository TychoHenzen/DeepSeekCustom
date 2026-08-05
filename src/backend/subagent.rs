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
fn resolve_subagent_working_dir(
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
async fn run_api_subagent(
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
async fn run_stub_subagent(
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
pub(crate) struct DrainedReply {
    pub(crate) text: String,
    pub(crate) interrupted: bool,
    pub(crate) error: Option<String>,
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
pub(crate) fn drain_reply_and_forward(
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
async fn run_claude_cli_keep_open_subagent(
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
fn spawn_event_forwarder(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agent_loop::StreamEvent;
    use crate::backend::stub::StubTurn;
    use crate::config::settings::Settings;
    use std::path::PathBuf;

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
        use crate::config::settings::{BackendConfig, Settings};
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
        let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("scripted reply".to_string())]);
        let (parent_tx, _parent_rx) = mpsc::unbounded_channel();

        let outcome = run_subagent(&factory, stub_request("stub-agent", 1), parent_tx, empty_registry())
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

        let err = run_subagent(&factory, stub_request("stub-agent", 1), parent_tx, empty_registry())
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

        run_subagent(&factory, stub_request("stub-agent", 1), parent_tx, empty_registry())
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
        // `task_tool_absent_at_the_depth_limit` in `factory_tests.rs`). A
        // stub has no tool registry at all, so depth has no bearing on
        // whether the dispatch itself succeeds.
        let outcome = run_subagent(&factory, stub_request("stub-agent", 2), parent_tx, empty_registry())
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

        let outcome = run_subagent(&factory, keep_open_stub_request("stub-agent", 1), parent_tx, registry.clone())
            .await
            .expect("stub dispatch should succeed");

        let id = outcome.session_id.expect("keep_open dispatch should report a session id");
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

        let outcome = run_subagent(&factory, stub_request("stub-agent", 1), parent_tx, registry.clone())
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

        let outcome = run_subagent(&factory, keep_open_stub_request("stub-agent", 1), parent_tx, registry)
            .await
            .expect("stub dispatch should succeed");
        let session_id = outcome.session_id.expect("keep_open dispatch should report a session id");

        let routed = parent_rx.recv().await.expect("expected a routed event");
        assert_eq!(routed.route.len(), 1);
        assert_eq!(routed.route[0].id, session_id);
    }

    /// `req.effort` reaches the built subagent's own effort flag: calling
    /// `run_stub_subagent` directly (a private helper, reachable from this
    /// same file's test module) so the built `Backend` is still in hand
    /// afterward, since a normal `run_subagent` dispatch drops it once the
    /// registry takes ownership.
    #[tokio::test]
    async fn run_stub_subagent_stores_the_requests_effort_on_the_built_backend() {
        let factory = factory_with_stub("stub-agent", vec![StubTurn::Text("hi".to_string())]);
        let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
        let registry = empty_registry();
        let req = SubagentRequest {
            effort: Effort::Max,
            ..keep_open_stub_request("stub-agent", 1)
        };
        let working_dir = Arc::new(Mutex::new(PathBuf::from(".")));

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

        let err = run_subagent(&factory, keep_open_stub_request("stub-agent", 1), parent_tx, registry.clone())
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
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "dsc-subagent-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        let original_parent_value = factory.working_dir_snapshot();

        let resolved =
            resolve_subagent_working_dir(&factory, None).expect("inheriting should never fail");

        *resolved.lock().unwrap() = PathBuf::from("C:/moved/by/subagent");
        assert_eq!(
            factory.working_dir_snapshot(),
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

        use crate::tools::Tool;
        let bash = crate::tools::bash::BashTool::new(resolved);
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
    async fn run_subagent_with_a_missing_working_dir_override_fails_before_resolving_the_backend()
    {
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
}
