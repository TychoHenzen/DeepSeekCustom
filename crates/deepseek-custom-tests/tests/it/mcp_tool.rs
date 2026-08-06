//! Tests for `deepseek_custom::mcp::tool`: how one MCP tool presents
//! itself to the model, and what a caller sees when the server misbehaves.

use std::collections::HashMap;
use std::sync::Arc;

use deepseek_custom::mcp::ServerConfig;
use deepseek_custom::mcp::client::McpClient;
use deepseek_custom::mcp::protocol::McpToolDef;
use deepseek_custom::mcp::tool::{McpTool, qualified_name};
use deepseek_custom::tools::Tool;
use serde_json::json;

fn fake_server(markers: &[&str]) -> ServerConfig {
    ServerConfig {
        name: "fake".to_string(),
        command: env!("CARGO_BIN_EXE_fake_mcp_server").to_string(),
        args: markers.iter().map(|m| m.to_string()).collect(),
        env: HashMap::new(),
    }
}

async fn tool_named(remote: &str) -> (Arc<McpClient>, McpTool) {
    let client = Arc::new(McpClient::connect(&fake_server(&[])).await.unwrap());
    let defs = client.list_tools().await.unwrap();
    let def = defs.iter().find(|d| d.name == remote).unwrap().clone();
    let tool = McpTool::new(Arc::clone(&client), "fake", &def);
    (client, tool)
}

#[test]
fn a_name_is_prefixed_with_its_server() {
    // The prefix is what keeps a server's `read` from colliding with this
    // harness's own `read`.
    assert_eq!(qualified_name("dod-guard", "dod_check"), "mcp__dod-guard__dod_check");
}

#[test]
fn an_over_long_name_is_cut_to_what_the_api_accepts() {
    // A request carrying a name past the schema limit is rejected outright,
    // so the name is cut rather than sent.
    let long = "x".repeat(80);
    let name = qualified_name("server", &long);
    assert_eq!(name.len(), 64);
    assert!(name.starts_with("mcp__server__"));
}

#[tokio::test]
async fn the_tool_reports_its_qualified_name() {
    let (client, tool) = tool_named("echo").await;
    assert_eq!(tool.name(), "mcp__fake__echo");
    client.shutdown().await;
}

#[tokio::test]
async fn the_tool_carries_the_servers_description() {
    let (client, tool) = tool_named("echo").await;
    assert_eq!(tool.description(), "Echo the text back");
    client.shutdown().await;
}

#[tokio::test]
async fn the_tool_carries_the_servers_schema() {
    let (client, tool) = tool_named("echo").await;
    assert_eq!(tool.input_schema()["properties"]["text"]["type"], "string");
    client.shutdown().await;
}

#[tokio::test]
async fn a_tool_with_no_description_gets_one_naming_its_server() {
    // The model needs something to decide on. An empty description reads as
    // a tool with no purpose.
    let client = Arc::new(McpClient::connect(&fake_server(&[])).await.unwrap());
    let def = McpToolDef {
        name: "bare".to_string(),
        description: None,
        input_schema: None,
    };

    let tool = McpTool::new(Arc::clone(&client), "fake", &def);

    assert!(tool.description().contains("bare"));
    assert!(tool.description().contains("fake"));
    client.shutdown().await;
}

#[tokio::test]
async fn a_tool_with_no_schema_gets_an_empty_object_schema() {
    // `boom` is the fake server's one tool that declares no `inputSchema`.
    let (client, tool) = tool_named("boom").await;
    assert_eq!(tool.input_schema()["type"], "object");
    client.shutdown().await;
}

#[tokio::test]
async fn executing_the_tool_calls_through_to_the_server() {
    let (client, tool) = tool_named("echo").await;

    let out = tool.execute(json!({"text": "round trip"})).await.unwrap();

    assert!(!out.is_error);
    assert_eq!(out.content, "round trip");
    client.shutdown().await;
}

#[tokio::test]
async fn a_server_side_tool_failure_becomes_a_tool_error() {
    let (client, tool) = tool_named("boom").await;

    let out = tool.execute(json!({})).await.unwrap();

    assert!(out.is_error);
    assert_eq!(out.content, "it broke");
    client.shutdown().await;
}

#[tokio::test]
async fn a_dead_server_is_a_tool_error_not_a_hard_failure() {
    // One dead MCP server must not end the turn, the same rule the Task
    // tool already follows for a failed dispatch.
    let client = Arc::new(McpClient::connect(&fake_server(&[])).await.unwrap());
    let def = McpToolDef {
        name: "echo".to_string(),
        description: None,
        input_schema: None,
    };
    let tool = McpTool::new(Arc::clone(&client), "fake", &def);
    client.shutdown().await;

    let out = tool.execute(json!({"text": "x"})).await.unwrap();

    assert!(out.is_error);
}
