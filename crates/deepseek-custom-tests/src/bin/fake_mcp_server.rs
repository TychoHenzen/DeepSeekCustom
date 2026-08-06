//! A fake MCP server, so `tests/mcp_client.rs` and `tests/mcp_manager.rs`
//! can drive a real child process without depending on `node`, `npx`, or a
//! network.
//!
//! It lives in the test crate rather than the production one for the same
//! mechanical reason `fake_claude` does: `env!("CARGO_BIN_EXE_...")` only
//! resolves for a test in the package that declares the binary.
//!
//! It speaks the subset of MCP this harness uses: `initialize`,
//! `tools/list`, and `tools/call`, one JSON object per line on stdin and
//! stdout. Command-line markers put it into the failure modes a test needs,
//! since its normal behaviour is instant success.
//!
//! | Marker | Effect |
//! |---|---|
//! | `--no-tools` | `tools/list` comes back empty |
//! | `--hang` | never answer anything, so a caller waits |
//! | `--noise` | write a non-JSON line before the first answer |
//! | `--exit-after-init` | exit once the handshake is done |
//! | `--fail-init` | answer `initialize` with a JSON-RPC error |

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let markers: Vec<String> = std::env::args().skip(1).collect();
    let has = |m: &str| markers.iter().any(|a| a == m);

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    if has("--noise") {
        // A server is free to write a banner. The client must skip it
        // rather than treat it as a protocol violation.
        let _ = writeln!(stdout, "starting fake mcp server");
        let _ = stdout.flush();
    }

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        // A notification has no id and gets no answer.
        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            continue;
        };

        if has("--hang") {
            continue;
        }

        let response = match method {
            "initialize" if has("--fail-init") => error_response(id, "initialize refused"),
            "initialize" => result_response(
                id,
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "0.1.0"},
                }),
            ),
            "tools/list" if has("--no-tools") => result_response(id, json!({"tools": []})),
            "tools/list" => result_response(id, tools_list()),
            "tools/call" => call_response(id, &message),
            _ => error_response(id, &format!("unknown method {method}")),
        };

        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();

        if method == "initialize" && has("--exit-after-init") {
            return;
        }
    }
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "echo",
                "description": "Echo the text back",
                "inputSchema": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"]
                }
            },
            {
                "name": "boom",
                "description": "Always reports a tool failure"
            }
        ]
    })
}

/// Answer a `tools/call`. `echo` returns its `text` argument. `boom`
/// returns the protocol's own tool-failure flag rather than an RPC error,
/// which is a different path through the client.
fn call_response(id: u64, message: &Value) -> Value {
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

    match name {
        "echo" => {
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            result_response(
                id,
                json!({"content": [{"type": "text", "text": text}], "isError": false}),
            )
        }
        "boom" => result_response(
            id,
            json!({"content": [{"type": "text", "text": "it broke"}], "isError": true}),
        ),
        other => error_response(id, &format!("unknown tool {other}")),
    }
}

fn result_response(id: u64, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: u64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": message}})
}
