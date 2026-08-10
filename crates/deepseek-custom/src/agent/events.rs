use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::api::types::{ImageAttachment, Message};

/// Commands sent from the GUI to the agent task over the input channel.
/// This is the asymmetric partner of `StreamEvent`, which carries events
/// the other way. A bare `String` could only ever mean "run this turn",
/// which left session switching with no way to reach the backend.
#[derive(Debug)]
pub enum AgentCommand {
    /// Run one user turn with this text and, when the user pasted or
    /// dropped one, the image to send alongside it. Text and image travel
    /// together on one command so a single `send` is one indivisible turn:
    /// two turns sent back to back can never cross-pair their images, and
    /// nothing needs a side channel to carry per-turn payload.
    UserTurn {
        text: String,
        image: Option<ImageAttachment>,
    },
    /// Start a fresh, empty conversation.
    NewSession,
    /// Load a saved conversation. `messages` restores the `Api` backend's
    /// history. `claude_session_id` is stored for a later `--resume` on
    /// the `ClaudeCli` backend. This carries only what the agent needs,
    /// not a whole `SessionRecord`: the display transcript stays in the
    /// GUI, the only thing that renders it.
    LoadSession {
        messages: Vec<Message>,
        claude_session_id: Option<String>,
    },
    /// Replace the running backend with the entry `name` selects, keeping
    /// the shared handles the GUI already holds. `model` overrides the
    /// model that entry declares, which is what the model dropdown holds
    /// for the incoming backend at the moment of the switch.
    SwitchBackend { name: String, model: Option<String> },
}

/// Events sent from the agent loop to the TUI (or caller) during streaming.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Text chunk received from the model.
    Text { turn: u32, text: String },
    /// A tool call has started.
    ToolCallStart {
        turn: u32,
        tool: String,
        args: String,
    },
    /// A tool call completed.
    ToolCallEnd {
        turn: u32,
        tool: String,
        output: String,
        is_error: bool,
    },
    /// The agent has finished its turn.
    TurnEnd {
        turn: u32,
        finish_reason: String,
        total_tokens: usize,
        prompt_cache_hit_tokens: u32,
        prompt_cache_miss_tokens: u32,
    },
    /// Session was reset.
    SessionReset,
    /// A snapshot of the conversation, sent alongside `TurnEnd` so the GUI
    /// can persist a session without owning `MessageHistory` itself.
    ConversationSnapshot {
        messages: Vec<Message>,
        claude_session_id: Option<String>,
    },
    /// An error occurred.
    Error { message: String },
    /// Agent was interrupted by user (Escape key).
    Interrupted { message: String },
    /// Reasoning/thinking chunk received (when thinking is enabled).
    Reasoning { turn: u32, text: String },
    /// A repeat run started a new iteration. Carries the task text, since
    /// an autopilot iteration has no other user message: the transcript
    /// draws it as the iteration's `User` block, and `derive_title` picks
    /// it up from there when the iteration's own session is saved.
    RepeatIterationStart {
        index: u32,
        total: u32,
        task: String,
    },
    /// A repeat run finished, whether by completing every iteration or by
    /// being interrupted partway through.
    RepeatFinished { completed: u32, total: u32 },
    /// An informational notice, such as a plain-language revision marker.
    Info { message: String },
    /// A running search reported its current state. Sent once per scored
    /// candidate, carrying the whole snapshot rather than a delta, so the
    /// view never rebuilds state from a partial history. Boxed because the
    /// snapshot is much larger than every other variant, and an enum is as
    /// wide as its widest arm.
    SearchProgress(Box<crate::search::SearchSnapshot>),
    /// A search run ended, whether it found an answer or not.
    SearchFinished {
        kind: crate::search::SearchKind,
        summary: String,
        is_error: bool,
    },
}

/// Identifies one subagent dispatch, for event routing. Cheap to copy and
/// compare, and usable as a map key: a later phase keys a subagent
/// registry by it. Allocated by `run_subagent` in
/// `src/backend/subagent.rs`, one id per dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubagentId(u64);

impl SubagentId {
    /// Allocate a fresh, process-unique id. Each call returns a distinct
    /// value, so two concurrent dispatches never collide.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        SubagentId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for SubagentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for SubagentId {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(SubagentId(s.parse()?))
    }
}

/// The dispatch-time facts about a subagent that the transcript needs to
/// draw its block, but that no `StreamEvent` carries on its own: which
/// backend it runs on, which model resolved for it, and how deep in the
/// dispatch chain it sits.
#[derive(Debug, Clone)]
pub struct SubagentMeta {
    pub backend: String,
    pub model: String,
    pub depth: u32,
}

/// One hop in a `RoutedEvent`'s route: the subagent that relayed the event
/// upward, plus the dispatch-time facts about it.
///
/// `session_turns` and `send_message_calls` change turn by turn, so every
/// event carries the values current at the moment it was forwarded.
#[derive(Debug, Clone)]
pub struct RouteHop {
    pub id: SubagentId,
    pub meta: SubagentMeta,
    pub session_turns: u32,
    pub session_turn_cap: u32,
    pub send_message_calls: u32,
    pub send_message_call_cap: u32,
}

/// One `StreamEvent`, tagged with the chain of subagents it passed through
/// on its way up to the top of the dispatch tree. Empty for an event the
/// main session emits directly.
#[derive(Debug, Clone)]
pub struct RoutedEvent {
    pub route: Vec<RouteHop>,
    pub event: StreamEvent,
}

impl RoutedEvent {
    /// Wrap `event` with an empty route: the shape every direct sender
    /// uses for its own events. Routing is added later, only by a
    /// forwarder relaying the event up from a nested dispatch.
    pub fn own(event: StreamEvent) -> Self {
        Self {
            route: Vec::new(),
            event,
        }
    }
}
