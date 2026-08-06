//! Live subagent sessions kept open across more than one turn. See "Phase
//! 3: multi-turn subagent sessions" in
//! `docs/plans/2026-08-04-long-term-roadmap.md` for the lifetime rule this
//! enforces: a session lives until its parent's turn ends, or until it is
//! closed, whichever comes first. `AgentLoop::run` and the `SessionReset`
//! branch of `AgentLoop::execute_tool` both call into this, so neither a
//! finished turn nor a `Reset` can leave a session behind.
//!
//! This also holds the two runaway-cost caps from that same section: a
//! per-session cap on how many turns one kept-open session may run, and a
//! per-parent-turn cap on how many `SendMessage` calls a parent may make in
//! total, across every session it has open. Both come back as a tool error
//! rather than a hard failure when tripped, and neither cap is a follow-up:
//! the roadmap names this theme as the one most likely to produce a runaway
//! cost, and says the caps ship in the same change as the feature.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::{Mutex, mpsc};

use crate::agent::agent_loop::{RoutedEvent, SubagentId, SubagentMeta};
use crate::backend::Backend;
use crate::backend::subagent::drain_reply_and_forward;

/// The relay state a kept-open `claude_cli` session needs so
/// `SubagentRegistry::send_message` can recover a later turn's reply text
/// and forward its events onward, the same way the turn that opened the
/// session already does. `Backend::run` on a `ClaudeCli` driver always
/// returns an empty vector: that variant streams its reply only through
/// `StreamEvent`s on its own channel, never through its return value. So
/// without this, `send_message` would have nothing to read a reply from.
/// See `drain_reply_and_forward` in `src/backend/subagent.rs`.
struct ClaudeCliRelay {
    meta: SubagentMeta,
    parent_tx: mpsc::UnboundedSender<RoutedEvent>,
    events_rx: mpsc::UnboundedReceiver<RoutedEvent>,
}

/// One live session: the backend driving it, plus how many turns it has
/// run so far. `turns` starts at 1 on registration, since a `keep_open`
/// dispatch has already run the session's first turn by the time it is
/// handed to `SubagentRegistry::register`. `claude_cli_relay` is `Some`
/// only for a kept-open `claude_cli` session; `None` for `Api` and `Stub`,
/// whose reply text already comes back directly from `Backend::run`.
///
/// `turns` is an `Arc<AtomicU32>`, not a plain `u32`, because an `Api` or
/// `Stub` session's events keep flowing through the same forwarder task
/// `run_subagent` spawned back when the session was opened (see
/// `spawn_event_forwarder` in `src/backend/subagent.rs`): that task runs
/// for the whole life of the session, independent of any one
/// `send_message` call, and needs a live read on the current turn count
/// for every event it forwards, including one made mid-turn while this
/// struct's own `entry` is checked out of `sessions` inside
/// `send_message`. A plain field guarded only by the registry's mutex
/// could not be read from outside that lock while checked out; the atomic
/// can. `register_with_turns_handle` is what lets the forwarder and this
/// entry share the exact same atomic from the moment the session opens.
struct SessionEntry {
    backend: Backend,
    turns: Arc<AtomicU32>,
    claude_cli_relay: Option<ClaudeCliRelay>,
}

/// Live subagent sessions, keyed by the id `run_subagent` allocates for
/// each dispatch. Each entry owns its `Backend` outright. Closing a
/// session removes it here and shuts that backend down through
/// `Backend::start_new_session`, the same method a normal session reset
/// already uses: for `ClaudeCli` it kills the child process, for `Api` it
/// clears history, for `Stub` it rewinds the script. Reusing that method,
/// rather than inventing a second shutdown path, keeps "how a backend
/// releases its resources" in one place.
#[derive(Default)]
pub struct SubagentRegistry {
    sessions: Mutex<HashMap<SubagentId, SessionEntry>>,
    /// Total `SendMessage` calls made against this registry since the last
    /// time a parent turn ended. `close_all` resets this to zero every
    /// time it runs, which is every turn end, whether or not any session
    /// was actually open. That is the "resets when the parent's turn
    /// ends" rule from the roadmap.
    send_message_calls: AtomicU32,
}

impl SubagentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a live session under `id`. `run_subagent` allocates a
    /// fresh `SubagentId` per dispatch, so a caller passing an id already
    /// in use is a bug in that caller. This overwrites the earlier entry
    /// without shutting it down first, rather than silently masking that
    /// bug behind an unasked-for shutdown.
    pub async fn register(&self, id: SubagentId, backend: Backend) {
        self.register_with_turns_handle(id, backend, Arc::new(AtomicU32::new(1)))
            .await;
    }

    /// Register a live session under `id`, sharing `turns` with whatever
    /// already holds a clone of it. `run_subagent` uses this for an `Api`
    /// or `Stub` session instead of plain `register`: it creates `turns`
    /// before the session's first turn even runs, hands one clone to
    /// `spawn_event_forwarder` so that task can read the live count on
    /// every event it forwards, and hands the other clone here once the
    /// dispatch turns out to be a `keep_open` one. See the `turns` field
    /// comment on `SessionEntry` for why a shared atomic is needed at all.
    pub async fn register_with_turns_handle(&self, id: SubagentId, backend: Backend, turns: Arc<AtomicU32>) {
        self.sessions.lock().await.insert(
            id,
            SessionEntry {
                backend,
                turns,
                claude_cli_relay: None,
            },
        );
    }

    /// Register a kept-open `claude_cli` session, alongside the relay
    /// state `send_message` needs to recover each later turn's reply text:
    /// the route metadata, the parent's own event sender, and the receiver
    /// still holding this driver's un-drained events. See `ClaudeCliRelay`.
    pub async fn register_claude_cli_session(
        &self,
        id: SubagentId,
        backend: Backend,
        meta: SubagentMeta,
        parent_tx: mpsc::UnboundedSender<RoutedEvent>,
        events_rx: mpsc::UnboundedReceiver<RoutedEvent>,
    ) {
        self.sessions.lock().await.insert(
            id,
            SessionEntry {
                backend,
                turns: Arc::new(AtomicU32::new(1)),
                claude_cli_relay: Some(ClaudeCliRelay {
                    meta,
                    parent_tx,
                    events_rx,
                }),
            },
        );
    }

    /// Whether a live session is registered under `id`.
    pub async fn contains(&self, id: SubagentId) -> bool {
        self.sessions.lock().await.contains_key(&id)
    }

    /// The number of live sessions.
    pub async fn len(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// How many turns a live session has run so far, including the turn
    /// that opened it. `None` when no session is registered under `id`.
    /// Read-only visibility for a later step's GUI header, see the
    /// roadmap's "both counts visible in the subagent block header".
    pub async fn session_turns(&self, id: SubagentId) -> Option<u32> {
        self.sessions
            .lock()
            .await
            .get(&id)
            .map(|e| e.turns.load(Ordering::SeqCst))
    }

    /// Total `SendMessage` calls made against this registry since the last
    /// turn end. Same visibility purpose as `session_turns`.
    pub fn send_message_call_count(&self) -> u32 {
        self.send_message_calls.load(Ordering::SeqCst)
    }

    /// Test-only: the effort flag of the backend registered under `id`, so
    /// a test can prove a `Task` dispatch's resolved effort landed on the
    /// exact subagent this registry is keeping alive, without needing a
    /// production getter over `SessionEntry` itself. `None` when no
    /// session is registered under `id`. Gated on
    /// `#[cfg(feature = "test-support")]`, matching every other test-only
    /// seam in this module. `pub`, not `pub(crate)`: `src/tools/task.rs`'s
    /// tests, the only caller, moved to the external
    /// `deepseek-custom-tests` crate as part of the workspace split, so
    /// `pub(crate)` can no longer reach them. There is no side-effect-free
    /// public seam that reports a registered session's effort level;
    /// `SessionEntry` itself is private.
    #[cfg(feature = "test-support")]
    pub async fn effort_flag_for_test(
        &self,
        id: SubagentId,
    ) -> Option<Arc<std::sync::atomic::AtomicU8>> {
        self.sessions.lock().await.get(&id).map(|e| e.backend.effort_flag())
    }

    /// Close one session: removes it and shuts its backend down. Returns
    /// `false` when no session was registered under `id`.
    pub async fn close(&self, id: SubagentId) -> bool {
        let removed = self.sessions.lock().await.remove(&id);
        match removed {
            Some(mut entry) => {
                entry.backend.start_new_session().await;
                true
            }
            None => false,
        }
    }

    /// Close every live session, and reset the `SendMessage` call count.
    /// Used when a parent's turn ends and when `Reset` fires, so neither
    /// can leave a session behind, and so the next turn starts with a
    /// fresh call budget.
    pub async fn close_all(&self) {
        let drained: Vec<Backend> = {
            let mut sessions = self.sessions.lock().await;
            sessions.drain().map(|(_, entry)| entry.backend).collect()
        };
        for mut backend in drained {
            backend.start_new_session().await;
        }
        self.send_message_calls.store(0, Ordering::SeqCst);
    }

    /// Send another turn into an open session. `session_turn_cap` bounds
    /// how many turns this one session may run in total, counting the
    /// turn that opened it. `parent_call_cap` bounds how many
    /// `SendMessage` calls this registry's owner may make in total during
    /// one of its own turns, across every session it has open.
    ///
    /// Every call counts against `parent_call_cap`, successful or not:
    /// the cap exists to stop a runaway loop of calls, not just a loop of
    /// successful ones. The session, if any, stays open regardless of
    /// whether this particular turn succeeded; only `close`/`close_all`
    /// remove a session.
    ///
    /// The backend is removed from the map for the duration of the turn,
    /// rather than held under the lock across the `await`, the same
    /// pattern `close_all` already uses. That keeps a long-running turn
    /// from blocking every other registry operation for its whole
    /// duration.
    pub async fn send_message(
        &self,
        id: SubagentId,
        prompt: &str,
        session_turn_cap: u32,
        parent_call_cap: u32,
    ) -> std::result::Result<String, String> {
        let calls_so_far = self.send_message_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if calls_so_far > parent_call_cap {
            return Err(format!(
                "SendMessage call limit for this turn reached ({parent_call_cap}); \
                no more sessions can be messaged until the parent's next turn"
            ));
        }

        let Some(mut entry) = self.sessions.lock().await.remove(&id) else {
            return Err(format!("no open session with id {id}"));
        };

        if entry.turns.load(Ordering::SeqCst) >= session_turn_cap {
            self.sessions.lock().await.insert(id, entry);
            return Err(format!(
                "session {id} has reached its turn cap ({session_turn_cap} turns); \
                open a new session instead"
            ));
        }

        let new_turns = entry.turns.fetch_add(1, Ordering::SeqCst) + 1;
        let run_result = entry.backend.run(prompt).await;

        // A `claude_cli` session's reply never comes back through
        // `Backend::run`'s return value: that variant always returns an
        // empty vector, streaming its reply only through `StreamEvent`s
        // on its own channel. `claude_cli_relay` is exactly the state
        // needed to drain that channel and recover the text, the same
        // way the turn that opened this session already does. See
        // `drain_reply_and_forward` in `src/backend/subagent.rs`.
        let text_result: std::result::Result<String, String> = match run_result {
            Ok(segments) => match entry.claude_cli_relay.as_mut() {
                Some(relay) => {
                    let drained = drain_reply_and_forward(
                        id,
                        &relay.meta,
                        new_turns,
                        session_turn_cap,
                        calls_so_far,
                        parent_call_cap,
                        &mut relay.events_rx,
                        &relay.parent_tx,
                    );
                    if drained.interrupted {
                        Err(format!("session {id} was interrupted"))
                    } else if let Some(err) = drained.error {
                        Err(err)
                    } else {
                        Ok(drained.text)
                    }
                }
                None => Ok(segments.join("")),
            },
            Err(e) => Err(e.to_string()),
        };

        self.sessions.lock().await.insert(id, entry);
        text_result
    }
}

