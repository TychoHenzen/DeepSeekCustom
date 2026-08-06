//! Adapting one MCP tool onto this harness's own `Tool` trait.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::client::McpClient;
use super::protocol::McpToolDef;
use crate::error::Result;
use crate::tools::{Tool, ToolOutput};

/// The longest tool name the OpenAI-compatible schema accepts. A server
/// name and a tool name joined with the prefix below can exceed it, and a
/// request carrying an over-long name is rejected outright, so the joined
/// name is cut rather than sent.
const MAX_TOOL_NAME: usize = 64;

/// One tool belonging to one MCP server.
pub struct McpTool {
    /// The name the model sees: `mcp__<server>__<tool>`, matching Claude
    /// Code's own convention. The prefix is what keeps a server's `read`
    /// from colliding with this harness's own `read`.
    name: String,
    /// The name the server itself knows the tool by, which is what goes
    /// back out in `tools/call`.
    remote_name: String,
    description: String,
    schema: Value,
    client: Arc<McpClient>,
}

impl McpTool {
    pub fn new(client: Arc<McpClient>, server: &str, def: &McpToolDef) -> McpTool {
        McpTool {
            name: qualified_name(server, &def.name),
            remote_name: def.name.clone(),
            description: def
                .description
                .clone()
                .unwrap_or_else(|| format!("{} tool from the {server} MCP server", def.name)),
            // A server may omit `inputSchema`. An empty object schema is
            // the honest reading of that: the tool takes no arguments.
            schema: def
                .input_schema
                .clone()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            client,
        }
    }
}

/// Join a server and tool name into the name the model calls, cut to what
/// the API accepts. The cut takes from the tool name's tail rather than
/// the prefix, so the server a tool belongs to stays readable.
pub fn qualified_name(server: &str, tool: &str) -> String {
    let full = format!("mcp__{server}__{tool}");
    if full.len() <= MAX_TOOL_NAME {
        return full;
    }
    full.chars().take(MAX_TOOL_NAME).collect()
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    async fn execute(&self, input: Value) -> Result<ToolOutput> {
        // A transport or protocol failure comes back as a tool error, not a
        // hard `Err`: one dead MCP server must not end the turn, the same
        // rule the `Task` tool already follows for a failed dispatch.
        match self.client.call_tool(&self.remote_name, input).await {
            Ok(outcome) => Ok(ToolOutput {
                content: outcome.text,
                is_error: outcome.is_error,
                image: None,
            }),
            Err(e) => Ok(ToolOutput {
                content: e.to_string(),
                is_error: true,
                image: None,
            }),
        }
    }
}
