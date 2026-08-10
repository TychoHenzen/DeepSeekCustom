//! The `Task` tool: dispatches a subagent onto a named backend. A model
//! plans, then hands a self-contained piece of work to a subagent running
//! on whatever backend fits -- a cheaper or local model for mechanical work,
//! a strong one for judgment calls. See `src/backend/subagent.rs` for the
//! machinery this sits on.

pub mod input;
pub mod tool;

pub use input::build_request;
pub use tool::TaskTool;
