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
    /// session is registered under `id`.
    #[cfg(test)]
    pub(crate) async fn effort_flag_for_test(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::stub::{StubBackend, StubTurn};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn stub_backend() -> Backend {
        Backend::Stub(Box::new(StubBackend::new(
            Vec::new(),
            "stub-model".to_string(),
            Arc::new(AtomicBool::new(false)),
        )))
    }

    fn stub_backend_with_script(script: Vec<StubTurn>) -> Backend {
        Backend::Stub(Box::new(StubBackend::new(
            script,
            "stub-model".to_string(),
            Arc::new(AtomicBool::new(false)),
        )))
    }

    /// A `ClaudeCli` backend with no child ever spawned: registering and
    /// closing it exercises the registry's generic path for that variant.
    /// `Backend::start_new_session` on a `ClaudeCli` driver calls
    /// `shutdown`, which is a no-op when no child is running, so this does
    /// not spawn a real `claude` process.
    fn unspawned_claude_cli_backend() -> Backend {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let working_dir = Arc::new(std::sync::Mutex::new(PathBuf::from(".")));
        Backend::new_claude_cli("opus".to_string(), None, None, working_dir, tx)
    }

    /// Closing a `ClaudeCli`-backed session removes it from the registry
    /// and runs its shutdown path, the same as any other backend kind.
    /// This proves the registry treats a `ClaudeCli` entry no differently
    /// than a `Stub` one: `close` calls `start_new_session`, which is what
    /// actually kills a real child when one is running (see
    /// `Backend::start_new_session` and `ClaudeCliDriver::shutdown`).
    /// Exercising an actual live child needs a real `claude` process,
    /// which this suite must not spawn; see `P7S01` for the fake binary
    /// that will let a later test cover that.
    #[tokio::test]
    async fn closing_a_claude_cli_session_removes_it_and_runs_shutdown() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();
        registry.register(id, unspawned_claude_cli_backend()).await;
        assert!(registry.contains(id).await);

        let closed = registry.close(id).await;

        assert!(closed);
        assert!(!registry.contains(id).await);
        assert_eq!(registry.len().await, 0);
    }

    #[tokio::test]
    async fn a_registered_session_is_reachable_by_id() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();

        registry.register(id, stub_backend()).await;

        assert!(registry.contains(id).await);
        assert_eq!(registry.len().await, 1);
    }

    #[tokio::test]
    async fn an_unregistered_id_is_not_reachable() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();

        assert!(!registry.contains(id).await);
    }

    #[tokio::test]
    async fn closing_one_removes_only_that_session() {
        let registry = SubagentRegistry::new();
        let id_a = SubagentId::next();
        let id_b = SubagentId::next();
        registry.register(id_a, stub_backend()).await;
        registry.register(id_b, stub_backend()).await;

        let closed = registry.close(id_a).await;

        assert!(closed);
        assert!(!registry.contains(id_a).await);
        assert!(registry.contains(id_b).await);
        assert_eq!(registry.len().await, 1);
    }

    #[tokio::test]
    async fn closing_an_unknown_id_returns_false() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();

        let closed = registry.close(id).await;

        assert!(!closed);
    }

    #[tokio::test]
    async fn closing_all_empties_the_registry() {
        let registry = SubagentRegistry::new();
        registry.register(SubagentId::next(), stub_backend()).await;
        registry.register(SubagentId::next(), stub_backend()).await;
        registry.register(SubagentId::next(), stub_backend()).await;

        registry.close_all().await;

        assert_eq!(registry.len().await, 0);
    }

    #[tokio::test]
    async fn closing_all_on_an_empty_registry_is_a_no_op() {
        let registry = SubagentRegistry::new();

        registry.close_all().await;

        assert_eq!(registry.len().await, 0);
    }

    /// A registered session starts at 1 turn, since `keep_open` already
    /// ran its first turn before handing the backend to `register`.
    #[tokio::test]
    async fn a_freshly_registered_session_starts_at_one_turn() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();

        registry.register(id, stub_backend()).await;

        assert_eq!(registry.session_turns(id).await, Some(1));
    }

    #[tokio::test]
    async fn send_message_against_a_stub_session_returns_the_next_scripted_turn() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();
        registry
            .register(
                id,
                stub_backend_with_script(vec![
                    StubTurn::Text("first".to_string()),
                    StubTurn::Text("second".to_string()),
                ]),
            )
            .await;

        // The registry itself does not run a session's opening turn: that
        // happens in `run_subagent` before `register` is ever called (see
        // `send_message.rs`'s tests for that end-to-end path). Registering
        // a fresh script directly here means this call is the session's
        // first turn, so it consumes the script's first entry.
        let text = registry
            .send_message(id, "follow up", 20, 10)
            .await
            .expect("should succeed");

        assert_eq!(text, "first");
        assert_eq!(registry.session_turns(id).await, Some(2));
    }

    #[tokio::test]
    async fn send_message_against_an_unknown_id_is_an_error_naming_it() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();

        let err = registry
            .send_message(id, "hello", 20, 10)
            .await
            .expect_err("unknown id should fail");

        assert!(err.contains(&id.to_string()));
    }

    #[tokio::test]
    async fn send_message_against_a_closed_session_is_an_error() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();
        registry.register(id, stub_backend()).await;
        registry.close(id).await;

        let err = registry
            .send_message(id, "hello", 20, 10)
            .await
            .expect_err("closed session should fail");

        assert!(err.contains(&id.to_string()));
    }

    /// A session capped at 1 turn total has already used it up on
    /// registration, so the very next `send_message` trips the cap. The
    /// session stays open: only the turn was rejected.
    #[tokio::test]
    async fn send_message_trips_the_per_session_turn_cap() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();
        registry.register(id, stub_backend()).await;

        let err = registry
            .send_message(id, "hello", 1, 10)
            .await
            .expect_err("session turn cap should reject this call");

        assert!(err.contains("turn cap"));
        assert!(registry.contains(id).await);
    }

    /// A parent capped at 1 `SendMessage` call total lets the first call
    /// through and rejects the second, even against the same session.
    #[tokio::test]
    async fn send_message_trips_the_per_parent_turn_call_cap() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();
        registry
            .register(
                id,
                stub_backend_with_script(vec![
                    StubTurn::Text("first".to_string()),
                    StubTurn::Text("second".to_string()),
                ]),
            )
            .await;

        let first = registry.send_message(id, "one", 20, 1).await;
        assert!(first.is_ok());

        let second = registry.send_message(id, "two", 20, 1).await;
        let err = second.expect_err("parent call cap should reject the second call");
        assert!(err.contains("call limit"));
    }

    /// The per-parent-turn call count resets exactly when `close_all`
    /// runs, the same moment a parent's turn ends.
    #[tokio::test]
    async fn close_all_resets_the_send_message_call_count() {
        let registry = SubagentRegistry::new();
        let id = SubagentId::next();
        registry.register(id, stub_backend()).await;
        let _ = registry.send_message(id, "one", 20, 10).await;
        assert_eq!(registry.send_message_call_count(), 1);

        registry.close_all().await;

        assert_eq!(registry.send_message_call_count(), 0);
    }
}
