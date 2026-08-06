//! Tests for `deepseek_custom::mcp::client`, driven against the real child
//! process in `src/bin/fake_mcp_server.rs`.
//!
//! These run a spawn, a handshake, and a request-response round trip for
//! real. The reader task and the pending-request table are what a
//! hand-rolled parse test cannot reach: correlating a reply to its caller
//! only matters when a live process is answering.

use std::collections::HashMap;

use deepseek_custom::mcp::ServerConfig;
use deepseek_custom::mcp::client::McpClient;
use serde_json::json;

/// A config pointing at the fake server, with the given markers.
fn fake_server(markers: &[&str]) -> ServerConfig {
    ServerConfig {
        name: "fake".to_string(),
        command: env!("CARGO_BIN_EXE_fake_mcp_server").to_string(),
        args: markers.iter().map(|m| m.to_string()).collect(),
        env: HashMap::new(),
    }
}

#[tokio::test]
async fn connects_and_completes_the_handshake() {
    let client = McpClient::connect(&fake_server(&[])).await.unwrap();
    assert_eq!(client.name(), "fake");
    client.shutdown().await;
}

#[tokio::test]
async fn lists_the_servers_tools() {
    let client = McpClient::connect(&fake_server(&[])).await.unwrap();

    let tools = client.list_tools().await.unwrap();

    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "boom"]);
    assert_eq!(tools[0].description.as_deref(), Some("Echo the text back"));
    assert!(tools[0].input_schema.is_some());
    client.shutdown().await;
}

#[tokio::test]
async fn calls_a_tool_and_gets_its_text_back() {
    let client = McpClient::connect(&fake_server(&[])).await.unwrap();

    let outcome = client
        .call_tool("echo", json!({"text": "hello there"}))
        .await
        .unwrap();

    assert_eq!(outcome.text, "hello there");
    assert!(!outcome.is_error);
    client.shutdown().await;
}

#[tokio::test]
async fn a_tool_that_reports_failure_comes_back_flagged_not_as_an_error() {
    // `isError` on the result means the tool ran and failed. That is a
    // different path from an RPC error, and the call itself still succeeds.
    let client = McpClient::connect(&fake_server(&[])).await.unwrap();

    let outcome = client.call_tool("boom", json!({})).await.unwrap();

    assert!(outcome.is_error);
    assert_eq!(outcome.text, "it broke");
    client.shutdown().await;
}

#[tokio::test]
async fn an_rpc_error_comes_back_as_an_err() {
    let client = McpClient::connect(&fake_server(&[])).await.unwrap();

    let result = client.call_tool("no-such-tool", json!({})).await;

    let message = result.unwrap_err().to_string();
    assert!(message.contains("no-such-tool"), "got: {message}");
    client.shutdown().await;
}

#[tokio::test]
async fn several_calls_in_a_row_each_get_their_own_answer() {
    // The pending-request table exists for this: a server may interleave
    // answers, so matching by id beats reading the next line and hoping.
    let client = McpClient::connect(&fake_server(&[])).await.unwrap();

    let first = client.call_tool("echo", json!({"text": "one"})).await.unwrap();
    let second = client.call_tool("echo", json!({"text": "two"})).await.unwrap();

    assert_eq!(first.text, "one");
    assert_eq!(second.text, "two");
    client.shutdown().await;
}

#[tokio::test]
async fn a_non_json_line_from_the_server_is_skipped() {
    // A server is free to write a banner to stdout before its first answer.
    let client = McpClient::connect(&fake_server(&["--noise"])).await.unwrap();

    let tools = client.list_tools().await.unwrap();

    assert_eq!(tools.len(), 2);
    client.shutdown().await;
}

#[tokio::test]
async fn an_empty_tool_list_is_not_an_error() {
    let client = McpClient::connect(&fake_server(&["--no-tools"]))
        .await
        .unwrap();

    assert!(client.list_tools().await.unwrap().is_empty());
    client.shutdown().await;
}

#[tokio::test]
async fn a_server_that_refuses_the_handshake_fails_to_connect() {
    let result = McpClient::connect(&fake_server(&["--fail-init"])).await;

    let message = result.err().unwrap().to_string();
    assert!(message.contains("initialize"), "got: {message}");
}

#[tokio::test]
async fn a_command_that_does_not_exist_fails_to_connect() {
    let config = ServerConfig {
        name: "ghost".to_string(),
        command: "definitely-not-a-real-command-xyz".to_string(),
        args: Vec::new(),
        env: HashMap::new(),
    };

    let message = McpClient::connect(&config).await.err().unwrap().to_string();

    assert!(message.contains("ghost"), "got: {message}");
}

#[tokio::test]
async fn a_request_to_a_dead_server_errors_rather_than_hanging() {
    // The server exits right after the init handshake. Either connect
    // itself fails (pipe breaks during init) or the follow-up call
    // fails (server gone). Both prove the code errors out instead of
    // hanging on a dead stdout.
    let config = fake_server(&["--exit-after-init"]);
    let client = match McpClient::connect(&config).await {
        Ok(c) => c,
        Err(_) => return,
    };

    let result = client.list_tools().await;

    assert!(result.is_err());
}
