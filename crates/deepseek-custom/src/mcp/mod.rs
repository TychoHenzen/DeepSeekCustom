//! An MCP (Model Context Protocol) client, so the `Api` backend can call
//! the same servers Claude Code does.
//!
//! This applies to the `Api` variant only. The `claude_cli` backend runs
//! its own MCP client inside the `claude` child process, and always did:
//! that is why a `deepseek` session could never reach `dod_check` while a
//! `claude` session could.
//!
//! The pieces:
//!
//! - `config` finds server definitions in Claude Code's own files.
//! - `client` spawns one server and speaks JSON-RPC to it over stdio.
//! - `protocol` holds the wire types.
//! - `tool` adapts one MCP tool onto this harness's `Tool` trait.
//! - `manager` starts them all in the background and registers their tools.
//! - `spawn` resolves a command name into something Windows will start.

pub mod client;
pub mod config;
pub mod manager;
pub mod protocol;
pub mod spawn;
pub mod tool;

pub use config::{ServerConfig, discover_servers};
pub use manager::McpManager;
