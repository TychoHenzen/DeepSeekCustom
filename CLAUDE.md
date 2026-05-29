# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

DeepSeekCustom — Rust-based experimental AI coding harness. Runs DeepSeek models (v4 flash/pro) in an agent loop with tool calling, terminal UI, skills, and hooks. Piggybacks on Claude Code's file formats (settings.json, skills/*.md, CLAUDE.md, MEMORY.md) so the same project config works with either harness.

**Target:** Rust edition 2024, DeepSeek API v4 (OpenAI-compatible format), egui/eframe native GUI.

## Build & Test

```bash
cargo check                                          # Fast compile-check, no codegen
cargo build                                          # Debug build
cargo build --release                                # Release build
cargo test                                           # All tests
cargo test -- --test-threads=1                       # Tests sequentially
cargo test --lib                                      # Library tests only
cargo clippy -- -D warnings                          # Lint (treat warnings as errors)
cargo fmt -- --check                                  # Format check
```

Tests in `src/agent/agent_loop.rs`, `src/api/client.rs`, `src/tools/mod.rs`. Test fixture at `tests/fixtures/chat_response.json`.

## Architecture

```
DeepSeek API (OpenAI-format chat completions)
        │
        ▼
┌──────────────────┐    ┌──────────────┐
│   Agent Loop     │◄───│  Tools       │ Bash, Read, Write, Reset
│   (turn cycle)   │    │  (trait)      │
└────────┬─────────┘    └──────────────┘
         │
    ┌────┴─────┬──────────┬──────────┐
    ▼          ▼          ▼          ▼
 Settings   Skills     Hooks      Memory
(JSON)     (.md w/    (shell     (CLAUDE.md,
           fm)        cmds)      MEMORY.md)
```

**Agent loop:** user input → build messages (system prompt + history + tools) → call DeepSeek API → parse response (text or tool calls) → execute tools via `ToolRegistry` → append tool results to history → repeat. Max turns guard (default 100). Streaming via `reqwest` + `tokio::sync::mpsc`. Events sent to GUI via `StreamEvent` enum over unbounded channel — decouples agent from UI layer. User interrupt via `Arc<AtomicBool>` flag: GUI sets it on Escape, agent checks during stream receive and before tool execution, sends `StreamEvent::Interrupted`.

**Tool call streaming:** DeepSeek streams tool calls across multiple SSE chunks (first chunk: id+name, subsequent: argument fragments). `merge_tool_call()` matches by index and accumulates partial fields. `reasoning_content` must be echoed back to API in next request or API returns 400.

**Tools:** `Tool` trait (`name`, `description`, `input_schema`, `execute`) with dynamic `ToolRegistry`. Minimum set: Bash (shell execution with timeout; `shell` param accepts `auto`/`cmd`/`powershell`; auto-detects powershell/pwsh commands and runs directly via `Command::new("powershell")` to avoid cmd.exe inner-quote mangling), Read (file read with line numbers), Write (file write), Reset (hard session reset). Permission check via settings `allow`/`deny` lists.

**Piggybacking formats** (drop-in compatible with Claude Code files):
- `settings.json` — project root or `~/.claude/`. Model, permissions, hooks config.
- Skills — `skills/*.md` with YAML frontmatter (`name`, `description`, `tools`).
- Hooks — shell commands receive JSON on stdin, return JSON on stdout. Events: PreToolUse, PostToolUse, SessionStart, SessionEnd, SessionReset.
- Memory — `CLAUDE.md` (project instructions), `MEMORY.md` (persistent memory). Injected into system prompt.

**API format decision:** Start with OpenAI-compatible format (`https://api.deepseek.com/chat/completions`) — simpler SDK support in Rust. Anthropic format added later only if content block streaming needed for thinking interleave.

**Thinking mode:** DeepSeek V4 uses `thinking_mode` string field (`"thinking"`, `"non-thinking"`, `"thinking_max"`) instead of legacy `thinking: {type: "enabled"}` object. Set in `ChatRequest.thinking_mode`; `thinking` field kept but always `None` for V4. Reasoning content streamed via `delta.reasoning_content` and must be echoed back in subsequent requests or API returns 400.

**Models:** `deepseek-v4-flash` (default, thinking toggleable), `deepseek-v4-pro` (heavy tasks).

**Key architectural choices:**
- No full session persistence between runs (only memory files survive)
- Session reset is hard cut (clear context, reload memory files, start fresh with prompt)
- Context pruning is gradual (relevance-score decay) not discrete compression turns
- Hemisphere model (Phase 3): two model instances, different system prompts, right side sees compressed context
- Dynamic config sync: agent reads `thinking_flag` (AtomicBool) and `model_name` (Mutex<String>) from GUI each turn before building API request

## Implementation Status

Phase 1-2 complete. Core modules filled in with implementations, tests, and native GUI.

**Done:**
- API client: DeepSeekClient (streaming SSE + non-streaming, retry, auth via env/settings.json, V4 thinking_mode format)
- Agent loop: turn cycle, tool execution, session reset, system prompt rebuild, reasoning_content echo-back, nameless tool call filtering (V4 thinking deltas), dynamic config sync from GUI (thinking_flag, model_flag), reasoning events via StreamEvent::Reasoning
- Tools: Bash, Read, Write, Reset (Tool trait + ToolRegistry + permission check)
- Config: Settings loading from project/global JSON, PermissionsConfig, HooksConfig
- Context: ThinkingStore with relevance decay
- Hooks: HookRunner with JSON stdin/stdout for lifecycle events
- Memory: MemoryManager loading CLAUDE.md/MEMORY.md
- Skills: Skill loader parsing .md with YAML frontmatter
- Hemisphere: Stub for Phase 3 dual-model
- GUI: egui/eframe native GUI with output scroll, input bar, status bar, settings sidebar (model selector + thinking toggle, Tab key), markdown rendering via egui_commonmark (white text = markdown, non-white = raw/styled), raw/output display toggle, StreamEvent channel, Escape-to-interrupt agent, Ctrl+Q to quit

**GUI key bindings:**
- `Enter` — send message to agent
- `Escape` — interrupt running agent (does not quit)
- `Tab` — toggle settings sidebar (model selector, thinking toggle)
- `Ctrl+Q` — quit application

**Tests:** 78 tests across `agent_loop.rs` (7), `api/client.rs` (1), `api/types.rs` (7), `gui/mod.rs` (7), `tools/bash.rs` (11), `tools/mod.rs` (4), `tools/read.rs` (3), `tools/write.rs` (2), `tests/fixtures/chat_response.json`.

**Next:** Phase 3 (hemisphere model), hook execution integration, or skill injection into agent context.

## API Key Resolution

Priority chain: `DEEPSEEK_API_KEY` env → `ANTHROPIC_AUTH_TOKEN` env → `settings.json` (project) → `settings.json` (~/.claude) → `CustomClaude.ps1` on PATH (parses `ANTHROPIC_AUTH_TOKEN` assignment). Sourced via `resolve_api_key()` in `api/client.rs`.

## Logging

Dual output: stderr (colored, human-readable) + `deepseek_custom.log` file (no ANSI). Project root discovered by walking up from cwd until `CLAUDE.md` found. Env filter: `RUST_LOG` or defaults to `info`.

See `docs/plans/2026-05-27-deepseek-harness-implementation-plan.md` for full task checklist.

## Platform

**Windows native.** Batch files have BOM and percent-sign issues in Git Bash; use PowerShell (`.ps1`) for automation scripts. Incremental compilation disabled in `.cargo/config.toml` — ballooned to 10+ GB temp files after a few builds. Hook scripts run via `powershell.exe -NoProfile -ExecutionPolicy Bypass -File <path>`. Bash tool defaults to `cmd /C`; auto-detects commands starting with `powershell`/`pwsh` and runs them directly (avoids `cmd.exe` inner-quote mangling). Use `shell` param for explicit control.

RTK convention: prefix commands with `rtk` for token savings on build/test/git output.
