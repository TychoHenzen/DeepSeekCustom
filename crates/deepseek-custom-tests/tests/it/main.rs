//! The one integration-test target for this crate.
//!
//! Every test file sits in `tests/it/` beside this file and is declared here as a module,
//! rather than sitting directly in `tests/` where cargo would compile each
//! one into a binary of its own.
//!
//! That default is expensive here. Cargo builds one linked executable per
//! `.rs` file directly under `tests/`, and each one statically links the
//! whole dependency tree: ONNX Runtime, whisper.cpp, browser support, and cpal.
//! Measured on this tree at 71 files, that came to 2.1 GB of executables
//! and 2.9 GB of debug symbols, about 5 GB rebuilt from scratch on every
//! full test run. Cleaning `target/` could not help, because the next run
//! recreated all of it. One target pays that cost once.
//!
//! The naming rule is unchanged. A file still maps to one production
//! module, by the rule in CLAUDE.md, and keeps the same name it had. Only
//! its directory moved. Fixtures moved with it, to `tests/it/fixtures/`,
//! because `include_str!` resolves relative to the file that calls it.
//!
//! Filtering still works, with the target named first:
//! `cargo test -p deepseek-custom-tests --test it skills`.

use std::sync::OnceLock;

fn process_environment_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Make a scratch directory no other call can collide with: the process id
/// and a nanosecond timestamp go into the name, so two tests running at
/// once, or one test run twice, never share a path.
///
/// `prefix` names the test file that asked for it, which is what makes a
/// directory left behind by a failed run traceable to its test.
fn scratch_dir(prefix: &str, tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

mod agent_agent_loop;
mod agent_history;
mod agent_prompt;
mod agent_pruning;
mod agent_repeat;
mod api_client;
mod api_key;
mod api_models;
mod api_turn;
mod api_types;
mod application_actor;
mod application_dto;
mod application_services;
mod application_session;
mod application_session_state;
mod application_test_control;
mod application_transcript;
mod autopilot_answerer;
mod autopilot_policy;
mod autopilot_question;
mod backend;
mod backend_claude_cli_events;
mod backend_claude_cli_map;
mod backend_claude_cli_one_shot;
mod backend_claude_cli_process;
mod backend_codex_cli_events;
mod backend_codex_cli_map;
mod backend_codex_cli_spawn;
mod backend_factory;
mod backend_registry;
mod backend_stub;
mod backend_subagent;
mod claude_cli_fake_binary;
mod claude_cli_lifecycle;
mod codex_cli_lifecycle;
mod config_settings;
mod context_relevance;
mod controlled_development;
mod effort;
mod evolution;
#[path = "web_browser_milestone.rs"]
mod focused_browser_command;
mod hemisphere;
mod hooks;
mod image_bytes;
mod json_reply;
mod mcp_client;
mod mcp_config;
mod mcp_manager;
mod mcp_protocol;
mod mcp_spawn;
mod mcp_tool;
mod memory;
mod path_repair;
mod plugins;
mod procedure_apply;
mod procedure_bounded_repair_end_to_end;
mod procedure_dispatch;
mod procedure_disposable_workspace;
mod procedure_failure_digest;
mod procedure_frontier_patch_draft;
mod procedure_frontier_repair;
mod procedure_index;
mod procedure_input;
mod procedure_local_repair;
mod procedure_patch_apply_check;
mod procedure_patch_envelope;
mod procedure_patch_preview;
mod procedure_preview_input;
mod procedure_promotion;
mod procedure_prompt;
mod procedure_repair_input;
mod procedure_repair_prompt;
mod procedure_repair_state;
mod procedure_repair_structural;
mod procedure_report;
mod procedure_route;
mod procedure_run;
mod procedure_runner;
mod procedure_sampling;
mod procedure_sandbox_e2e;
mod procedure_trace_export;
mod procedure_verification_input;
mod procedure_verifier;
mod search_cascade;
mod search_evolve;
mod session;
mod session_store;
mod skills;
mod skills_discovery;
mod skills_loader;
mod style;
mod tools;
mod tools_ask;
mod tools_bash;
mod tools_cd;
mod tools_close_session;
mod tools_edit;
mod tools_glob;
mod tools_grep;
mod tools_line_endings;
mod tools_read;
mod tools_read_image;
mod tools_reset;
mod tools_send_message;
mod tools_skill;
mod tools_task;
mod tools_task_input;
mod tools_task_tool;
mod tools_write;
mod voice;
mod voice_capture;
mod voice_cuda_dlls;
mod voice_playback;
mod voice_service;
mod voice_stt;
mod voice_tts;
mod voice_vad;
mod voice_wake;
mod web_assets;
mod web_browser;
mod web_server;
