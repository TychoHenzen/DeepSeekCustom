//! Tests for `deepseek_custom::mcp::protocol`: the JSON-RPC shapes this
//! client puts on the wire and reads back off it.

use deepseek_custom::mcp::protocol::{
    PROTOCOL_VERSION, Request, Response, ToolsCallResult, ToolsListResult, flatten_content,
    initialize_params,
};
use serde_json::{Value, json};

#[test]
fn a_call_carries_an_id() {
    let value = serde_json::to_value(Request::call(7, "tools/list", json!({}))).unwrap();
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], 7);
    assert_eq!(value["method"], "tools/list");
}

#[test]
fn a_notification_carries_no_id() {
    // A notification with an `id` is a request, and a server would try to
    // answer it.
    let value = serde_json::to_value(Request::notify("notifications/initialized")).unwrap();
    assert!(value.get("id").is_none());
    assert!(value.get("params").is_none());
}

#[test]
fn initialize_params_announce_a_protocol_version_and_a_client() {
    let params = initialize_params();
    assert_eq!(params["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(params["clientInfo"]["name"], "deepseek-custom");
    assert!(params["clientInfo"]["version"].is_string());
}

#[test]
fn a_result_response_parses() {
    let response: Response =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
    assert_eq!(response.id, Some(1));
    assert_eq!(response.result.unwrap()["ok"], true);
    assert!(response.error.is_none());
}

#[test]
fn an_error_response_parses() {
    let response: Response = serde_json::from_str(
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#,
    )
    .unwrap();
    let error = response.error.unwrap();
    assert_eq!(error.code, -32601);
    assert_eq!(error.message, "nope");
}

#[test]
fn a_server_notification_parses_with_no_id() {
    // A server may push a message this client never asked for. It must
    // parse and then be ignored, not break the reader.
    let response: Response =
        serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/message"}"#).unwrap();
    assert!(response.id.is_none());
}

#[test]
fn a_tools_list_result_parses() {
    let value = json!({
        "tools": [
            {"name": "dod_check", "description": "check", "inputSchema": {"type": "object"}},
            {"name": "dod_create"}
        ]
    });
    let listed: ToolsListResult = serde_json::from_value(value).unwrap();
    assert_eq!(listed.tools.len(), 2);
    assert_eq!(listed.tools[0].name, "dod_check");
    assert_eq!(listed.tools[0].description.as_deref(), Some("check"));
    // A server may omit both optional fields.
    assert!(listed.tools[1].description.is_none());
    assert!(listed.tools[1].input_schema.is_none());
}

#[test]
fn an_empty_tools_list_parses() {
    let listed: ToolsListResult = serde_json::from_value(json!({})).unwrap();
    assert!(listed.tools.is_empty());
}

#[test]
fn a_tools_call_result_parses_its_error_flag() {
    // `isError` is the protocol's way of saying the tool ran and failed,
    // as opposed to an RPC-level error.
    let value = json!({"content": [{"type": "text", "text": "boom"}], "isError": true});
    let result: ToolsCallResult = serde_json::from_value(value).unwrap();
    assert!(result.is_error);
    assert_eq!(flatten_content(&result.content), "boom");
}

#[test]
fn a_tools_call_result_defaults_to_no_error() {
    let result: ToolsCallResult = serde_json::from_value(json!({"content": []})).unwrap();
    assert!(!result.is_error);
}

#[test]
fn flatten_joins_text_blocks_with_newlines() {
    let content = vec![
        json!({"type": "text", "text": "first"}),
        json!({"type": "text", "text": "second"}),
    ];
    assert_eq!(flatten_content(&content), "first\nsecond");
}

#[test]
fn flatten_keeps_a_non_text_block_as_json() {
    // Dropping it would leave the model with a silently short answer.
    let content = vec![json!({"type": "resource", "uri": "file:///x"})];
    let flat = flatten_content(&content);
    assert!(flat.contains("resource"));
    assert!(flat.contains("file:///x"));
}

#[test]
fn flatten_of_no_content_is_empty() {
    let content: Vec<Value> = Vec::new();
    assert_eq!(flatten_content(&content), "");
}
