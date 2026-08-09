//! Where MCP server definitions come from, in Claude Code's own files.
//!
//! Four sources, lowest precedence first. Each later one overrides an
//! earlier entry of the same name.
//!
//! 1. Each enabled plugin's `<plugin_root>/.mcp.json`.
//! 2. `~/.claude.json`, top-level `mcpServers`.
//! 3. `~/.mcp.json`.
//! 4. `<project_root>/.mcp.json`.
//!
//! Only stdio servers are built. An entry naming an HTTP or SSE transport
//! is skipped with a `warn`, because this client speaks stdio alone. Saying
//! so out loud beats a server that silently never appears.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, warn};

use crate::plugins::{PluginRoot, claude_home, enabled_plugin_roots};

/// The placeholder a plugin's own `.mcp.json` uses for its install
/// directory. Claude Code expands it, and a plugin server will not start
/// without it: `node ${CLAUDE_PLUGIN_ROOT}/dist/bundle.js` is the usual
/// shape.
const PLUGIN_ROOT_VAR: &str = "${CLAUDE_PLUGIN_ROOT}";

/// A stdio MCP server this harness will spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// The name it is known by, and the middle segment of every tool name
    /// it contributes.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// The `mcpServers` block, as it appears in all four source files.
#[derive(Deserialize, Default)]
struct McpFile {
    #[serde(rename = "mcpServers", default)]
    mcp_servers: HashMap<String, RawServer>,
}

#[derive(Deserialize, Clone)]
struct RawServer {
    #[serde(rename = "type", default)]
    transport: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

/// Every stdio server reachable from this project, already expanded and
/// deduplicated by name.
pub fn discover_servers(project_root: &Path) -> Vec<ServerConfig> {
    discover_servers_from(project_root, claude_home(), &enabled_plugin_roots())
}

/// The same discovery against explicit roots, so a test can point it at a
/// fixture tree rather than the running machine's real configuration.
pub fn discover_servers_from(
    project_root: &Path,
    claude_dir: Option<PathBuf>,
    plugins: &[PluginRoot],
) -> Vec<ServerConfig> {
    let mut merged: HashMap<String, ServerConfig> = HashMap::new();

    for plugin in plugins {
        let file: McpFile = read_json(&plugin.root.join(".mcp.json"));
        insert_all(&mut merged, file, Some(&plugin.root));
    }

    if let Some(claude) = &claude_dir {
        // `~/.claude.json` sits beside `~/.claude/`, not inside it.
        if let Some(home) = claude.parent() {
            insert_all(&mut merged, read_json(&home.join(".claude.json")), None);
            insert_all(&mut merged, read_json(&home.join(".mcp.json")), None);
        }
    }

    insert_all(
        &mut merged,
        read_json(&project_root.join(".mcp.json")),
        None,
    );

    let mut servers: Vec<ServerConfig> = merged.into_values().collect();
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    servers
}

/// Fold one file's entries into the merged map, dropping what this client
/// cannot run.
fn insert_all(
    merged: &mut HashMap<String, ServerConfig>,
    file: McpFile,
    plugin_root: Option<&Path>,
) {
    for (name, raw) in file.mcp_servers {
        match build_server(&name, raw, plugin_root) {
            Some(server) => {
                merged.insert(name, server);
            }
            None => debug!("mcp: skipping server {name}"),
        }
    }
}

/// Build one runnable server config, or `None` when the entry names a
/// transport this client does not speak or carries no command to run.
fn build_server(name: &str, raw: RawServer, plugin_root: Option<&Path>) -> Option<ServerConfig> {
    // An absent `type` means stdio: that is the default in Claude Code's
    // own files, and several real entries leave it out.
    if let Some(transport) = &raw.transport
        && transport != "stdio"
    {
        warn!("mcp: server {name} uses {transport} transport, which is not supported");
        return None;
    }
    let command = raw.command?;
    Some(ServerConfig {
        name: name.to_string(),
        command: expand(&command, plugin_root),
        args: raw.args.iter().map(|a| expand(a, plugin_root)).collect(),
        env: raw
            .env
            .into_iter()
            .map(|(k, v)| (k, expand(&v, plugin_root)))
            .collect(),
    })
}

/// Expand `${CLAUDE_PLUGIN_ROOT}` in one string. Outside a plugin file
/// there is nothing to expand it to, so the text is left alone and the
/// spawn fails loudly rather than silently running the wrong thing.
fn expand(value: &str, plugin_root: Option<&Path>) -> String {
    let Some(root) = plugin_root else {
        return value.to_string();
    };
    if !value.contains(PLUGIN_ROOT_VAR) {
        return value.to_string();
    }
    value.replace(PLUGIN_ROOT_VAR, &root.display().to_string())
}

/// Read and parse a JSON file, falling back to an empty block. A missing
/// file is the normal case. A present but unparseable one logs at `warn`.
fn read_json(path: &Path) -> McpFile {
    let Ok(text) = std::fs::read_to_string(path) else {
        return McpFile::default();
    };
    match serde_json::from_str(&text) {
        Ok(file) => file,
        Err(e) => {
            warn!("mcp: failed to parse {}: {e}", path.display());
            McpFile::default()
        }
    }
}
