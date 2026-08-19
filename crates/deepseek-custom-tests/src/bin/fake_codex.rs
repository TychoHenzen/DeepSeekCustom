//! Deterministic `codex exec --json` replacement for lifecycle tests.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::time::Duration;

const THREAD_ID: &str = "fake-thread-42";
const BLOCK_MARKER: &str = "__FAKE_CODEX_BLOCK__";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    record_args(&args);

    let prompt = args.last().map(String::as_str).unwrap_or_default();
    emit(&serde_json::json!({"type":"thread.started","thread_id":THREAD_ID}));
    emit(&serde_json::json!({"type":"turn.started"}));
    if prompt.contains(BLOCK_MARKER) {
        std::thread::sleep(Duration::from_secs(10));
    }
    emit(&serde_json::json!({
        "type":"item.completed",
        "item":{"id":"reply-1","type":"agent_message","text":format!("echo: {prompt}")}
    }));
    emit(&serde_json::json!({
        "type":"turn.completed",
        "usage":{"input_tokens":4,"cached_input_tokens":1,"output_tokens":2}
    }));
}

fn record_args(args: &[String]) {
    let Some(path) = std::env::var_os("FAKE_CODEX_ARGS_FILE") else {
        return;
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("fake Codex should open its argument log");
    writeln!(file, "{}", serde_json::to_string(args).unwrap()).unwrap();
}

fn emit(value: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}
