//! Deterministic `codex exec --json` replacement for lifecycle tests.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::time::Duration;

const THREAD_ID: &str = "fake-thread-42";
const BLOCK_MARKER: &str = "__FAKE_CODEX_BLOCK__";
const VERBATIM_MARKER: &str = "__FAKE_FRONTIER_RESPONSE__";
const CWD_FILE_KEY: &str = "FAKE_CLI_CWD_FILE";
const SIDE_EFFECT_PATH_KEY: &str = "FAKE_CLI_SIDE_EFFECT_PATH";
const RESPONSE_KEY: &str = "FAKE_CLI_RESPONSE";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    record_args(&args);
    record_working_dir();
    write_beside_target();

    let prompt = args.last().map(String::as_str).unwrap_or_default();
    let reply = std::env::var(RESPONSE_KEY).unwrap_or_else(|_| {
        prompt
            .split_once(VERBATIM_MARKER)
            .map(|(_, response)| response.to_string())
            .unwrap_or_else(|| format!("echo: {prompt}"))
    });
    emit(&serde_json::json!({"type":"thread.started","thread_id":THREAD_ID}));
    emit(&serde_json::json!({"type":"turn.started"}));
    if prompt.contains(BLOCK_MARKER) {
        std::thread::sleep(Duration::from_secs(10));
    }
    emit(&serde_json::json!({
        "type":"item.completed",
        "item":{"id":"reply-1","type":"agent_message","text":reply}
    }));
    emit(&serde_json::json!({
        "type":"turn.completed",
        "usage":{"input_tokens":4,"cached_input_tokens":1,"output_tokens":2}
    }));
}

fn record_working_dir() {
    let Some(path) = std::env::var_os(CWD_FILE_KEY) else {
        return;
    };
    if let Ok(directory) = std::env::current_dir() {
        let _ = std::fs::write(path, directory.to_string_lossy().as_bytes());
    }
}

fn write_beside_target() {
    let Some(relative) = std::env::var_os(SIDE_EFFECT_PATH_KEY) else {
        return;
    };
    let path = std::path::PathBuf::from(relative);
    if path.is_absolute() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, b"fake Codex workspace side effect\n");
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
