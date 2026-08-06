//! Starting every configured MCP server and getting its tools in front of
//! the model.
//!
//! Startup does not wait for any of it. A stdio server is a child process
//! that may fetch a package before it answers the handshake: a local
//! bundle answered in 0.2s on this machine, while an `npx`-launched one had
//! not answered after three minutes. Blocking the window on the slowest
//! server would be the difference between a GUI that opens now and one that
//! looks hung.
//!
//! So each server connects on its own task, and its tools are registered
//! into every registry the manager knows about as soon as they arrive. The
//! agent rebuilds its tool list from the registry when it builds each
//! request, so a tool that lands between turns is offered on the next one
//! with no prompt rebuild and no restart.

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{info, warn};

use super::client::McpClient;
use super::config::ServerConfig;
use super::tool::McpTool;
use crate::tools::{Tool, ToolRegistry, WeakToolRegistry};

/// Owns every running MCP server and the tools they have contributed.
#[derive(Default)]
pub struct McpManager {
    state: Mutex<ManagerState>,
}

#[derive(Default)]
struct ManagerState {
    clients: Vec<Arc<McpClient>>,
    /// Every tool contributed so far, kept so a registry attached later
    /// gets the ones that arrived before it existed.
    tools: Vec<Arc<dyn Tool>>,
    /// The registries to feed. Weak, so a subagent's registry does not
    /// outlive the subagent. See `ToolRegistry::downgrade`.
    registries: Vec<WeakToolRegistry>,
}

impl McpManager {
    pub fn new() -> Arc<McpManager> {
        Arc::new(McpManager::default())
    }

    /// Connect to every server, each on its own task. Returns at once.
    pub fn start(self: &Arc<Self>, servers: Vec<ServerConfig>) {
        if servers.is_empty() {
            info!("mcp: no servers configured");
            return;
        }
        info!("mcp: starting {} servers", servers.len());
        for config in servers {
            let manager = Arc::clone(self);
            tokio::spawn(async move { manager.start_one(config).await });
        }
    }

    /// Feed this registry every tool known so far, and remember it so later
    /// arrivals reach it too.
    pub async fn attach(&self, registry: &ToolRegistry) {
        let mut state = self.state.lock().await;
        for tool in &state.tools {
            registry.register(Arc::clone(tool));
        }
        state.registries.push(registry.downgrade());
    }

    /// Kill every running server.
    pub async fn shutdown(&self) {
        let clients = std::mem::take(&mut self.state.lock().await.clients);
        for client in clients {
            client.shutdown().await;
        }
    }

    /// Connect one server and register whatever it offers.
    ///
    /// Every failure path here is a `warn` and a return. One server that
    /// will not start must not stop the others, or the harness.
    async fn start_one(&self, config: ServerConfig) {
        let name = config.name.clone();
        let client = match McpClient::connect(&config).await {
            Ok(client) => Arc::new(client),
            Err(e) => {
                warn!("mcp: {name}: {e}");
                return;
            }
        };

        let defs = match client.list_tools().await {
            Ok(defs) => defs,
            Err(e) => {
                warn!("mcp: {name}: {e}");
                client.shutdown().await;
                return;
            }
        };

        let tools: Vec<Arc<dyn Tool>> = defs
            .iter()
            .map(|def| Arc::new(McpTool::new(Arc::clone(&client), &name, def)) as Arc<dyn Tool>)
            .collect();
        info!("mcp: {name}: {} tools", tools.len());
        self.publish(client, tools).await;
    }

    /// Record a connected server's tools and push them into every live
    /// registry, dropping the registries that have since gone away.
    async fn publish(&self, client: Arc<McpClient>, tools: Vec<Arc<dyn Tool>>) {
        let mut state = self.state.lock().await;
        state.clients.push(client);
        state.registries.retain(|weak| match weak.upgrade() {
            Some(registry) => {
                for tool in &tools {
                    registry.register(Arc::clone(tool));
                }
                true
            }
            None => false,
        });
        state.tools.extend(tools);
    }
}
