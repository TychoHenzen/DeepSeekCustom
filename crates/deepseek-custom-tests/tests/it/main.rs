//! The one integration-test target for this crate.
//!
//! Every test file sits in `tests/it/` beside this file and is declared here as a module,
//! rather than sitting directly in `tests/` where cargo would compile each
//! one into a binary of its own.
//!
//! That default is expensive here. Cargo builds one linked executable per
//! `.rs` file directly under `tests/`, and each one statically links the
//! whole dependency tree: ONNX Runtime, whisper.cpp, egui, eframe, cpal.
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
mod effort;
mod evolution;
mod gui;
mod gui_attachment;
mod gui_autopilot_tab;
mod gui_backend_picker;
mod gui_cascade_tab;
mod gui_evolve_tab;
mod gui_procedure_tab;
mod gui_search_view;
mod gui_session_state;
mod gui_sessions_tab;
mod gui_transcript;
mod gui_voice_ui;
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
mod procedure_dispatch;
mod procedure_index;
mod procedure_input;
mod procedure_patch_envelope;
mod procedure_preview_input;
mod procedure_prompt;
mod procedure_report;
mod procedure_route;
mod procedure_run;
mod procedure_runner;
mod process_group;
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
