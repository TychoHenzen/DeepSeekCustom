//! The wire types for MCP over stdio: JSON-RPC 2.0, one JSON object per
//! line, requests and responses correlated by `id`.
//!
//! Only the four messages this harness needs are modelled: `initialize`,
//! the `notifications/initialized` notification that follows it,
//! `tools/list`, and `tools/call`. A server may send other things, and
//! anything unrecognised is dropped rather than treated as an error, since
//! a server is free to log or to push notifications this client has not
//! asked for.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The protocol revision this client announces. A server that speaks a
/// different one still answers: the spec has servers reply with their own
/// version rather than refusing, and every message shape used here has been
/// stable across revisions.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// An outgoing JSON-RPC request. A notification is the same shape with no
/// `id`, which is what `is_notification` on the constructor decides.
#[derive(Debug, Serialize)]
pub struct Request {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn call(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(id),
            method: method.to_string(),
            params: Some(params),
        }
    }

    pub fn notify(method: &str) -> Self {
        Self {
            jsonrpc: "2.0",
            id: None,
            method: method.to_string(),
            params: None,
        }
    }
}

/// An incoming JSON-RPC message. Every field is optional because one type
/// has to cover a response, an error response, and a server-initiated
/// notification, and only `id` tells them apart.
#[derive(Debug, Deserialize)]
pub struct Response {
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// The `params` of an `initialize` request.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "deepseek-custom", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// One entry of a `tools/list` result.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Option<Value>,
}

/// A `tools/list` result.
#[derive(Debug, Default, Deserialize)]
pub struct ToolsListResult {
    #[serde(default)]
    pub tools: Vec<McpToolDef>,
}

/// A `tools/call` result. `is_error` is the protocol's own way of saying a
/// tool ran and failed, as opposed to an RPC-level error, which arrives as
/// `Response::error` instead.
#[derive(Debug, Default, Deserialize)]
pub struct ToolsCallResult {
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

/// Flatten a `tools/call` result's content blocks into plain text.
///
/// A text block contributes its `text`. Any other block kind contributes
/// its JSON, because dropping it would leave the model with a silently
/// short answer, and this harness's `ToolOutput` carries text.
pub fn flatten_content(content: &[Value]) -> String {
    let mut parts = Vec::with_capacity(content.len());
    for block in content {
        match block.get("text").and_then(Value::as_str) {
            Some(text) => parts.push(text.to_string()),
            None => parts.push(block.to_string()),
        }
    }
    parts.join("\n")
}
