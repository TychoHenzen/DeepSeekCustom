# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

DeepSeekCustom — Rust-based experimental AI coding harness. Runs DeepSeek models (v4 flash/pro) in an agent loop with tool calling, terminal UI, skills, and hooks. Piggybacks on Claude Code's file formats (settings.json, skills/*.md, CLAUDE.md, MEMORY.md) so the same project config works with either harness.

**Target:** Rust edition 2024, DeepSeek API v4 (OpenAI-compatible format), Ratatui TUI.

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

No tests exist yet. TDD from scratch following the implementation plan.

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

**Agent loop:** user input → build messages (system prompt + history + tools) → call DeepSeek API → parse response (text or tool calls) → execute tools via `ToolRegistry` → append tool results to history → repeat. Max turns guard (default 100). Streaming via `reqwest` + `tokio::sync::mpsc`.

**Tools:** `Tool` trait (`name`, `description`, `input_schema`, `execute`) with dynamic `ToolRegistry`. Minimum set: Bash (shell execution with timeout), Read (file read with line numbers), Write (file write), Reset (hard session reset). Permission check via settings `allow`/`deny` lists.

**Piggybacking formats** (drop-in compatible with Claude Code files):
- `settings.json` — project root or `~/.claude/`. Model, permissions, hooks config.
- Skills — `skills/*.md` with YAML frontmatter (`name`, `description`, `tools`).
- Hooks — shell commands receive JSON on stdin, return JSON on stdout. Events: PreToolUse, PostToolUse, SessionStart, SessionEnd, SessionReset.
- Memory — `CLAUDE.md` (project instructions), `MEMORY.md` (persistent memory). Injected into system prompt.

**API format decision:** Start with OpenAI-compatible format (`https://api.deepseek.com/chat/completions`) — simpler SDK support in Rust. Anthropic format added later only if content block streaming needed for thinking interleave.

**Models:** `deepseek-v4-flash` (default, thinking toggleable), `deepseek-v4-pro` (heavy tasks).

**Key architectural choices:**
- No full session persistence between runs (only memory files survive)
- Session reset is hard cut (clear context, reload memory files, start fresh with prompt)
- Context pruning is gradual (relevance-score decay) not discrete compression turns
- Hemisphere model (Phase 3): two model instances, different system prompts, right side sees compressed context

## Implementation Status

Pre-implementation. See `docs/plans/` for full spec and step-by-step checklist:
- `2026-05-27-deepseek-harness-interview.md` — requirements spec (from /interview)
- `2026-05-27-deepseek-harness-implementation-plan.md` — 5-phase, 13-task checklist

Next: Phase 0, Task T1 — project scaffolding and dependency setup.

## Platform

**Windows native.** Batch files have BOM and percent-sign issues in Git Bash; use PowerShell (`.ps1`) for automation scripts. Hook scripts run via `powershell.exe -NoProfile -ExecutionPolicy Bypass -File <path>`.

RTK convention: prefix commands with `rtk` for token savings on build/test/git output.
