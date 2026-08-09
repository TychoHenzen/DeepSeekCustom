//! The handles the GUI and the agent share.
//!
//! Each one is a value the agent re-reads on its own schedule rather than
//! being told about: the interrupt flag on every stream chunk, the effort
//! level and the model name at the top of each turn, the context budget
//! before each prune, and the working directory on each tool call. The
//! GUI writes them; nothing here calls the agent.
//!
//! They travel as one group because they are handed over together, once,
//! at startup. Passing six of them as loose arguments made the GUI
//! constructor ten arguments wide, where two `Arc<AtomicBool>` sat next to
//! each other and a caller could swap them without the compiler noticing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize};
use std::sync::{Arc, Mutex};

/// Shared state between the GUI and the running agent.
pub struct AgentHandles {
    /// Set on Escape to abort the turn in flight.
    pub interrupt: Arc<AtomicBool>,
    /// The reasoning effort level, read at the top of each turn.
    pub effort: Arc<AtomicU8>,
    /// Whether to shape replies for speech, read at the top of each turn.
    pub voice_mode: Arc<AtomicBool>,
    /// The context budget in tokens, read before each prune.
    pub context_budget: Arc<AtomicUsize>,
    /// The model name, read at the top of each turn.
    pub model: Arc<Mutex<String>>,
    /// Where the Bash, Read, Write, and Cd tools act. Distinct from the
    /// project root, which never moves.
    pub working_dir: Arc<Mutex<PathBuf>>,
    /// Bumped once per Cascade call, resolved or not.
    pub cascade_total: Arc<AtomicUsize>,
    /// Bumped per escalation (Cascade vote did not reach `vote_k`).
    pub cascade_escalated: Arc<AtomicUsize>,
}
