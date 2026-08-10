pub mod ask;
pub mod bash;
pub mod cd;
pub mod close_session;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod read;
pub mod read_image;
pub mod reset;
pub mod send_message;
pub mod shell_stdin;
pub mod skill;
pub mod task;
pub mod write;

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak};

use async_trait::async_trait;

use crate::api::types::{ImageAttachment, ToolDef};
use crate::config::settings::PermissionsConfig;
use crate::error::Result;

/// Trait implemented by all tools the agent can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (e.g., "bash", "read").
    fn name(&self) -> &str;

    /// Human-readable description for the model's context.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's input parameters.
    fn input_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given JSON input.
    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput>;
}

/// Result of a tool execution.
///
/// `image` carries a decoded image an image-reading tool wants to hand
/// back, on top of `content`'s plain text. A `Role::Tool` message's content
/// has to stay text: the OpenAI-compatible schema both DeepSeek and Ollama
/// speak accepts an `image_url` content part only inside a `user` role
/// message, never a `tool` role message. So a tool cannot put the image
/// straight into its own result. `AgentLoop::run_turn`
/// (`src/agent/agent_loop.rs`) reads this field after the tool result
/// message is pushed and, when set, appends a synthetic `Role::User`
/// message carrying the image, mapped through the same
/// `build_user_content` a pasted or dropped image already goes through.
/// That is what actually gets the bytes in front of the model on a turn
/// after the one that read them: DeepSeek gets a transcript notice instead,
/// same as a pasted image, since it accepts no image content part at all.
/// Every other tool leaves this `None`, unaffected.
#[derive(Debug)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    pub image: Option<ImageAttachment>,
}

/// Registry of all available tools, keyed by name.
///
/// The map sits behind an `Arc<RwLock<_>>` rather than being owned
/// outright, so a clone of this registry shares one map with the original.
/// That is what lets an MCP server register its tools after the agent that
/// will call them was already built: an MCP server is a child process that
/// can take tens of seconds to answer `tools/list`, and blocking startup on
/// the slowest one would leave the window closed that whole time. The agent
/// re-reads `to_api_definitions` when it builds each request, so a tool that
/// lands mid-session is offered on the next turn with no prompt rebuild.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<RwLock<HashMap<String, Arc<dyn Tool>>>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Takes `&self`, not `&mut self`: registration goes through the shared
    /// lock, so a late registration from an MCP server holding a clone
    /// reaches the same map the agent reads.
    pub fn register(&self, tool: Arc<dyn Tool>) {
        self.write().insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.read().get(name).cloned()
    }

    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        self.read().values().cloned().collect()
    }

    pub fn to_api_definitions(&self) -> Vec<ToolDef> {
        self.read()
            .values()
            .map(|t| ToolDef {
                tool_type: "function".to_string(),
                function: crate::api::types::FunctionDef {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    parameters: t.input_schema(),
                },
            })
            .collect()
    }

    /// A handle that does not keep this registry alive.
    ///
    /// The MCP manager holds one per registry it feeds, and a subagent
    /// builds a registry per dispatch. Holding those strongly would pile up
    /// one dead registry per subagent for the life of the process, so the
    /// manager holds weak handles and drops the ones that no longer
    /// upgrade.
    pub fn downgrade(&self) -> WeakToolRegistry {
        WeakToolRegistry {
            tools: Arc::downgrade(&self.tools),
        }
    }

    /// Read guard that survives a poisoned lock. A panic while holding this
    /// lock leaves the map itself intact, since every write is a single
    /// `insert`, so recovering beats taking the whole session down.
    fn read(&self) -> RwLockReadGuard<'_, HashMap<String, Arc<dyn Tool>>> {
        self.tools.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Write guard, poison-recovering for the same reason as `read`.
    fn write(&self) -> RwLockWriteGuard<'_, HashMap<String, Arc<dyn Tool>>> {
        self.tools.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Check whether a tool is permitted based on allow/deny lists.
    ///
    /// Logic: deny checked first (always overrides), then allow.
    /// If allow is empty → all tools permitted (except denied ones).
    pub fn check_permission(&self, tool_name: &str, permissions: &PermissionsConfig) -> bool {
        // Deny list always takes precedence
        if let Some(ref deny) = permissions.deny {
            for pattern in deny {
                if tool_matches(tool_name, pattern) {
                    return false;
                }
            }
        }

        // If allow list is empty, permit everything not denied
        let Some(ref allow) = permissions.allow else {
            return true;
        };

        // Check allow list
        for pattern in allow {
            if tool_matches(tool_name, pattern) {
                return true;
            }
        }

        false
    }
}

/// A `ToolRegistry` handle that does not keep the registry alive. See
/// `ToolRegistry::downgrade`.
#[derive(Clone)]
pub struct WeakToolRegistry {
    tools: Weak<RwLock<HashMap<String, Arc<dyn Tool>>>>,
}

impl WeakToolRegistry {
    /// The registry, if anything still holds it.
    pub fn upgrade(&self) -> Option<ToolRegistry> {
        self.tools.upgrade().map(|tools| ToolRegistry { tools })
    }
}

/// Check if a tool name matches a pattern (exact or wildcard suffix).
fn tool_matches(name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    name == pattern
}
