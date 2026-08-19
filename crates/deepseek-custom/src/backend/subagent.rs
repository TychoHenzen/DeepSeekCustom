//! One subagent dispatch: build a backend from a named entry, run a single
//! turn on it with no history of its own, relay its events up to the caller
//! tagged with this dispatch's own hop, and then either drop the backend or
//! hand it to the caller's registry so later turns can continue it.
//!
//! Every failure comes back as `Err(String)`. The `Task` tool turns that
//! into a tool error, so a dispatch problem never fails the caller's turn.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::agent::events::{RouteHop, RoutedEvent, StreamEvent, SubagentId, SubagentMeta};
use crate::backend::Backend;
use crate::backend::claude_cli::one_shot::OneShotResult;
use crate::backend::claude_cli::process::ClaudeCliDriver;
use crate::backend::factory::BackendFactory;
use crate::backend::registry::SubagentRegistry;
use crate::backend::resolved::ResolvedBackend;
use crate::effort::Effort;

/// What one `Task` call asks for: which backend and model, the subagent's
/// whole instruction, how deep in the dispatch chain it sits, whether its
/// session outlives this call, where its tools act, and how hard it thinks.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentRequest {
    pub backend: String,
    pub model: Option<String>,
    pub prompt: String,
    pub depth: u32,
    pub keep_open: bool,
    pub working_dir_override: Option<PathBuf>,
    pub effort: Effort,
}

/// What one dispatch produced: the subagent's reply, the entry it ran on,
/// the model that resolved for it, and the session id when the session was
/// kept open.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentOutcome {
    pub text: String,
    pub backend: String,
    pub model: String,
    pub session_id: Option<SubagentId>,
}

/// What one drained batch of a subagent's own events said: the reply text,
/// whether the turn was interrupted, and the error message if one arrived.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrainedReply {
    pub text: String,
    pub interrupted: bool,
    pub error: Option<String>,
}

/// The six values one relayed hop carries, grouped so a relay point passes
/// them as a single value instead of six loose arguments.
struct Hop {
    id: SubagentId,
    meta: SubagentMeta,
    turns: u32,
    turn_cap: u32,
    calls: u32,
    call_cap: u32,
}

/// Send one event onward with `hop` prepended to whatever route it already
/// carries. Prepending, not appending, is what keeps a route outermost
/// first as it travels up a chain of dispatches.
///
/// Reports whether the event landed. A parent that has dropped its receiver
/// is never coming back, so a caller relaying a stream stops on `false`
/// rather than draining the rest into a channel nobody reads.
fn forward(hop: &Hop, mut event: RoutedEvent, tx: &mpsc::UnboundedSender<RoutedEvent>) -> bool {
    event.route.insert(
        0,
        RouteHop {
            id: hop.id,
            meta: hop.meta.clone(),
            session_turns: hop.turns,
            session_turn_cap: hop.turn_cap,
            send_message_calls: hop.calls,
            send_message_call_cap: hop.call_cap,
        },
    );
    if tx.send(event).is_err() {
        return false;
    }
    true
}

/// Fold one event into the running drain state. Text accumulates in arrival
/// order; an interrupt and an error each record themselves without
/// discarding the text seen so far.
fn absorb(drained: &mut DrainedReply, event: &StreamEvent) {
    match event {
        StreamEvent::Text { text, .. } => drained.text.push_str(text),
        StreamEvent::Interrupted { .. } => drained.interrupted = true,
        StreamEvent::Error { message } => drained.error = Some(message.clone()),
        _ => {}
    }
}

/// Take everything currently buffered on `rx`, concatenate the text, and
/// forward every event onward with one hop prepended. Never waits, so a
/// plain `#[test]` can call it with no runtime: it stops as soon as the
/// channel has nothing more in hand.
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
    let hop = Hop {
        id,
        meta: meta.clone(),
        turns: session_turns,
        turn_cap: session_turn_cap,
        calls: send_message_calls,
        call_cap: send_message_call_cap,
    };
    let mut drained = DrainedReply::default();
    while let Ok(event) = rx.try_recv() {
        absorb(&mut drained, &event.event);
        if !forward(&hop, event, parent_tx) {
            break;
        }
    }
    drained
}

/// Relay every event from a subagent's own channel onto the parent's, one
/// hop richer, for as long as that channel lives. Returns at once: the
/// relay runs on its own task, so an event sent later still arrives.
///
/// `turns` and the registry's call count are read per event rather than
/// copied once, so a hop reports the counts current at the moment it was
/// forwarded.
#[allow(clippy::too_many_arguments)]
pub fn spawn_event_forwarder(
    id: SubagentId,
    meta: SubagentMeta,
    rx: mpsc::UnboundedReceiver<RoutedEvent>,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    turns: Arc<AtomicU32>,
    session_turn_cap: u32,
    registry: Arc<SubagentRegistry>,
    send_message_call_cap: u32,
) {
    let mut rx = rx;
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            debug!(?event, "subagent event forwarded");
            let hop = Hop {
                id,
                meta: meta.clone(),
                turns: turns.load(Ordering::SeqCst),
                turn_cap: session_turn_cap,
                calls: registry.send_message_call_count(),
                call_cap: send_message_call_cap,
            };
            if !forward(&hop, event, &parent_tx) {
                return;
            }
        }
    });
}

/// Where a subagent's own tools act, as a handle it owns outright.
///
/// An override must already exist and be a directory, and resolves to its
/// canonical form. Without one, the subagent starts from the value the
/// factory currently holds, copied into a fresh handle: writing either side
/// afterwards never moves the other.
pub fn resolve_subagent_working_dir(
    factory: &Arc<BackendFactory>,
    override_path: Option<&Path>,
) -> Result<Arc<Mutex<PathBuf>>, String> {
    let Some(path) = override_path else {
        return Ok(Arc::new(Mutex::new(factory.working_dir_snapshot())));
    };
    if !path.exists() {
        return Err(format!("working_dir {} does not exist", path.display()));
    }
    if !path.is_dir() {
        return Err(format!("working_dir {} is not a directory", path.display()));
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| format!("working_dir {} could not be resolved: {e}", path.display()))?;
    Ok(Arc::new(Mutex::new(canonical)))
}

/// The model a resolved entry runs, whichever kind it turned out to be.
fn resolved_model(resolved: &ResolvedBackend) -> String {
    match resolved {
        ResolvedBackend::Api { model, .. } => model.clone(),
        ResolvedBackend::ClaudeCli { model, .. } => model.clone(),
        ResolvedBackend::CodexCli { model, .. } => model.clone(),
        #[cfg(feature = "test-support")]
        ResolvedBackend::Stub { model, .. } => model.clone(),
    }
}

/// The spawn parameters of a `claude -p` run that answers once and exits.
struct OneShotSpec {
    model: String,
    permission_mode: Option<String>,
    env: Option<HashMap<String, String>>,
}

/// `Some` only for the one dispatch shape that has no session to keep: a
/// `claude_cli` entry asked for a single answer. Every other shape wants a
/// backend built through the factory instead.
fn one_shot_spec(resolved: ResolvedBackend, keep_open: bool) -> Option<OneShotSpec> {
    if keep_open {
        return None;
    }
    let ResolvedBackend::ClaudeCli {
        model,
        permission_mode,
        env,
        ..
    } = resolved
    else {
        return None;
    };
    Some(OneShotSpec {
        model,
        permission_mode,
        env,
    })
}

/// The terminal event a one-shot run stands in for, since it streams
/// nothing of its own. Token totals are the run's own; a one-shot call has
/// no prompt cache reading to report. Only a run that succeeded reaches
/// this: a run that ended in error is a dispatch failure, not a turn end.
fn one_shot_turn_end(run: &OneShotResult) -> StreamEvent {
    StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".to_string(),
        total_tokens: (run.input_tokens + run.output_tokens) as usize,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    }
}

/// The terminal event for a one-shot run that did not finish. An interrupt
/// and a failure are told apart by the shared flag, so the transcript can
/// badge the block correctly.
fn one_shot_failure(message: &str, interrupt: &AtomicBool) -> StreamEvent {
    if interrupt.load(Ordering::SeqCst) {
        return StreamEvent::Interrupted {
            message: message.to_string(),
        };
    }
    StreamEvent::Error {
        message: message.to_string(),
    }
}

/// One dispatch in progress: the identity its events carry, where its
/// reply goes, and the caps its hops report.
struct Dispatch {
    id: SubagentId,
    meta: SubagentMeta,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    registry: Arc<SubagentRegistry>,
    working_dir: Arc<Mutex<PathBuf>>,
    turn_cap: u32,
    call_cap: u32,
}

/// A finished turn plus, for a kept-open session whose registration the
/// caller still owes, the live backend and the turn counter it shares.
type RanTurn = (SubagentOutcome, Option<(Backend, Arc<AtomicU32>)>);

impl Dispatch {
    /// Run the turn on whichever path this request calls for, then register
    /// a kept-open session the built path handed back.
    async fn run(
        &self,
        factory: &Arc<BackendFactory>,
        req: &SubagentRequest,
        resolved: ResolvedBackend,
    ) -> Result<SubagentOutcome, String> {
        if let Some(spec) = one_shot_spec(resolved, req.keep_open) {
            return self.run_one_shot(factory, req, spec).await;
        }
        let (outcome, kept) = self.run_built(factory, req).await?;
        if let Some((backend, turns)) = kept {
            self.registry
                .register_with_turns_handle(self.id, backend, turns)
                .await;
        }
        Ok(outcome)
    }

    /// Build the subagent's own backend, in its own working directory, with
    /// the requested effort on its own flag. `build` is what wires the tools
    /// and applies the depth gate, so this goes through it rather than
    /// constructing a backend by hand.
    fn build(
        &self,
        factory: &Arc<BackendFactory>,
        req: &SubagentRequest,
    ) -> Result<(Backend, mpsc::UnboundedReceiver<RoutedEvent>), String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let own = factory.with_working_dir(Arc::clone(&self.working_dir));
        let backend = own.build(&req.backend, req.model.as_deref(), tx, req.depth)?;
        req.effort.store(&backend.effort_flag());
        Ok((backend, rx))
    }

    /// The built path. A `claude_cli` driver reports its reply only through
    /// events, so that kind is drained; every other kind hands its text back
    /// from the run itself.
    async fn run_built(
        &self,
        factory: &Arc<BackendFactory>,
        req: &SubagentRequest,
    ) -> Result<RanTurn, String> {
        let (backend, rx) = self.build(factory, req)?;
        if matches!(backend, Backend::ClaudeCli(_)) {
            return self.run_relayed(backend, rx, req).await;
        }
        self.run_streamed(backend, rx, req).await
    }

    /// A backend whose turn returns its own text. Its events relay upward on
    /// their own task for the whole life of the session, sharing the turn
    /// counter the registry entry will hold.
    async fn run_streamed(
        &self,
        mut backend: Backend,
        rx: mpsc::UnboundedReceiver<RoutedEvent>,
        req: &SubagentRequest,
    ) -> Result<RanTurn, String> {
        let turns = Arc::new(AtomicU32::new(1));
        spawn_event_forwarder(
            self.id,
            self.meta.clone(),
            rx,
            self.parent_tx.clone(),
            Arc::clone(&turns),
            self.turn_cap,
            Arc::clone(&self.registry),
            self.call_cap,
        );
        let segments = backend.run(&req.prompt).await.map_err(|e| e.to_string())?;
        let outcome = self.outcome(segments.concat(), req.keep_open);
        Ok((outcome, req.keep_open.then_some((backend, turns))))
    }

    /// A long-lived `claude_cli` session. Its reply, and any spawn failure,
    /// arrive on its channel rather than from the send call, so the channel
    /// is drained before either is reported. The receiver goes to the
    /// registry, which needs it to recover a later turn's text.
    async fn run_relayed(
        &self,
        mut backend: Backend,
        mut rx: mpsc::UnboundedReceiver<RoutedEvent>,
        req: &SubagentRequest,
    ) -> Result<RanTurn, String> {
        let sent = backend.run(&req.prompt).await;
        let drained = self.drain(&mut rx);
        // The channel message is the detailed one, so it wins where both
        // exist. A turn that succeeded is still a success even if an error
        // event passed by on the way, which is why this only reads
        // `drained.error` on a failed send.
        if let Err(e) = sent {
            return Err(drained.error.unwrap_or_else(|| e.to_string()));
        }
        if drained.interrupted {
            return Err(format!(
                "subagent on backend \"{}\" was interrupted",
                self.meta.backend
            ));
        }
        let meta = self.meta.clone();
        self.registry
            .register_claude_cli_session(self.id, backend, meta, self.parent_tx.clone(), rx)
            .await;
        Ok((self.outcome(drained.text, req.keep_open), None))
    }

    /// One `claude -p` run that answers and exits. It streams nothing of its
    /// own, so this posts the single terminal event a consumer needs to see
    /// the dispatch reach an end.
    async fn run_one_shot(
        &self,
        factory: &Arc<BackendFactory>,
        req: &SubagentRequest,
        spec: OneShotSpec,
    ) -> Result<SubagentOutcome, String> {
        let dir = self.working_dir.lock().unwrap().clone();
        let interrupt = factory.interrupt_flag();
        let result = ClaudeCliDriver::run_once(
            &spec.model,
            spec.permission_mode.as_deref(),
            spec.env.as_ref(),
            &dir,
            &req.prompt,
            Arc::clone(&interrupt),
            req.effort,
        )
        .await;
        let run = match result {
            Ok(run) => run,
            Err(message) => {
                self.emit(one_shot_failure(&message, &interrupt));
                return Err(message);
            }
        };
        // A run that reports its own error still returns `Ok` from the CLI
        // wrapper. Only this branch notices it, so without it a refused or
        // failed `claude -p` run would be handed back as a normal reply.
        if run.is_error {
            let message = format!(
                "subagent on backend \"{}\" returned an error result: {}",
                self.meta.backend, run.text
            );
            self.emit(StreamEvent::Error {
                message: message.clone(),
            });
            return Err(message);
        }
        self.emit(one_shot_turn_end(&run));
        Ok(self.outcome(run.text, false))
    }

    /// Take whatever this dispatch's backend has already put on its channel,
    /// forwarding each event upward with this dispatch's hop.
    fn drain(&self, rx: &mut mpsc::UnboundedReceiver<RoutedEvent>) -> DrainedReply {
        drain_reply_and_forward(
            self.id,
            &self.meta,
            1,
            self.turn_cap,
            self.registry.send_message_call_count(),
            self.call_cap,
            rx,
            &self.parent_tx,
        )
    }

    /// Post one event of this dispatch's own making, routed the same way a
    /// relayed event would be.
    fn emit(&self, event: StreamEvent) {
        let hop = Hop {
            id: self.id,
            meta: self.meta.clone(),
            turns: 1,
            turn_cap: self.turn_cap,
            calls: self.registry.send_message_call_count(),
            call_cap: self.call_cap,
        };
        forward(&hop, RoutedEvent::own(event), &self.parent_tx);
    }

    fn outcome(&self, text: String, keep_open: bool) -> SubagentOutcome {
        SubagentOutcome {
            text,
            backend: self.meta.backend.clone(),
            model: self.meta.model.clone(),
            session_id: keep_open.then_some(self.id),
        }
    }
}

/// Dispatch one subagent and wait for its turn.
///
/// The working directory is settled before the backend name is, so a bad
/// override never reaches backend resolution. The subagent starts with no
/// history of its own: it sees `prompt` and nothing else. Its events reach
/// `parent_tx` carrying one hop naming this dispatch. A `keep_open` request
/// whose turn succeeded leaves its session in `registry`, under the id the
/// outcome reports and its events carried.
pub async fn run_subagent(
    factory: &Arc<BackendFactory>,
    req: SubagentRequest,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    registry: Arc<SubagentRegistry>,
) -> Result<SubagentOutcome, String> {
    let working_dir = resolve_subagent_working_dir(factory, req.working_dir_override.as_deref())?;
    let resolved = factory.resolve(&req.backend, req.model.as_deref())?;
    let dispatch = Dispatch {
        id: SubagentId::next(),
        meta: SubagentMeta {
            backend: req.backend.clone(),
            model: resolved_model(&resolved),
            depth: req.depth,
        },
        parent_tx,
        registry,
        working_dir,
        turn_cap: factory.session_turn_cap(),
        call_cap: factory.send_message_call_cap(),
    };
    info!(
        "subagent {} starting: backend={} depth={} keep_open={}",
        dispatch.id, dispatch.meta.backend, dispatch.meta.depth, req.keep_open
    );
    let started = Instant::now();
    let outcome = dispatch.run(factory, &req, resolved).await;
    info!(
        "subagent {} finished in {:?}: model={} ok={}",
        dispatch.id,
        started.elapsed(),
        dispatch.meta.model,
        outcome.is_ok(),
    );
    outcome
}

/// Run one turn on a backend built through the factory, handing the live
/// backend back instead of registering it, so a test can inspect what a
/// `keep_open` dispatch would have registered.
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
    let dispatch = Dispatch {
        id,
        meta,
        parent_tx,
        registry: Arc::clone(registry),
        working_dir,
        turn_cap: factory.session_turn_cap(),
        call_cap: factory.send_message_call_cap(),
    };
    dispatch.run_built(factory, req).await
}
