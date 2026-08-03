# AGENTS.md

This file provides guidance to Codex when working with code in this repository.

## Project

DeepSeekCustom - Rust-based experimental AI coding harness. Runs DeepSeek models (v4 flash/pro) in an agent loop with tool calling, terminal UI, skills, and hooks. Piggybacks on Claude Code's file formats (settings.json, skills/*.md, CLAUDE.md, MEMORY.md) so the same project config works with either harness.

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

**Prompt cache stats:** `Usage` carries `prompt_cache_hit_tokens`, `prompt_cache_miss_tokens`, and `prompt_cache_write_tokens`. All three default to zero, so a response without them still parses. DeepSeek sends usage in the last streaming chunk, before `[DONE]`, so `StreamChunk` carries an optional `usage` too. The agent loop keeps that final usage and passes the hit and miss counts on in `StreamEvent::TurnEnd`. The GUI adds them up across turns and shows the running totals in the status bar. A session reset zeroes them.

**Tool call streaming:** DeepSeek streams tool calls across multiple SSE chunks (first chunk: id+name, subsequent: argument fragments). `merge_tool_call()` matches by index and accumulates partial fields. `reasoning_content` must be echoed back to API in next request or API returns 400.

**Tools:** `Tool` trait (`name`, `description`, `input_schema`, `execute`) with dynamic `ToolRegistry`. Minimum set: Bash (shell execution with timeout; `shell` param accepts `auto`/`cmd`/`powershell`; auto-detects powershell/pwsh commands and runs directly via `Command::new("powershell")` to avoid cmd.exe inner-quote mangling), Read (file read with line numbers), Write (file write), Reset (hard session reset). Permission check via settings `allow`/`deny` lists.

**Piggybacking formats** (drop-in compatible with Codex files):
- `settings.json` - project root or `~/.claude/`. Model, permissions, hooks, and voice config. The repo ships one at the project root that turns voice on.
- Skills — `skills/*.md` with YAML frontmatter (`name`, `description`, `tools`).
- Hooks — shell commands receive JSON on stdin, return JSON on stdout. Events: PreToolUse, PostToolUse, SessionStart, SessionEnd, SessionReset.
- Memory - `CLAUDE.md` (project instructions), `MEMORY.md` (persistent memory). Injected into system prompt.
- The repo root also holds a `CLAUDE.md`, the same guide written for Claude Code. Edit both together.

**API format decision:** Start with OpenAI-compatible format (`https://api.deepseek.com/chat/completions`) — simpler SDK support in Rust. Anthropic format added later only if content block streaming needed for thinking interleave.

**Thinking mode:** DeepSeek V4 uses `thinking_mode` string field (`"thinking"`, `"non-thinking"`, `"thinking_max"`) instead of legacy `thinking: {type: "enabled"}` object. Set in `ChatRequest.thinking_mode`; `thinking` field kept but always `None` for V4. Reasoning content streamed via `delta.reasoning_content` and must be echoed back in subsequent requests or API returns 400.

**Models:** `deepseek-v4-flash` (default, thinking toggleable), `deepseek-v4-pro` (heavy tasks).

**Voice:** `src/voice/` runs local speech to text and text to speech, so the harness can act as a hands-free assistant. `capture.rs` pulls microphone audio into a bounded 16 kHz mono ring buffer. `vad.rs` finds utterance boundaries by energy level. `stt.rs` wraps whisper-rs 0.16 against `models/ggml-base.en.bin`. Transcription takes about 1.0s per utterance on CPU. `tts.rs` wraps Kokoro 82M ONNX through kokoro-en 0.1.4, using `models/model.onnx` (fp32) and voice packs in `voices/`, plus a threaded `TtsHandle`. Synthesis takes about 0.25s per line on CUDA and 1.0s on CPU. The int8 `model_quantized.onnx` sounds worse and runs slower, at 3.7s. `model_q8f16.onnx` crashes ONNX Runtime and was deleted. `playback.rs` handles cpal output, with drain support and barge-in. `wake.rs` matches a wake phrase within Levenshtein distance 2. `service.rs` holds `VoiceService`, the state machine and the GUI's only seam into voice. Its states are `Idle`, `Listening`, `Transcribing`, and `Speaking`. Its commands are `StartListening`, `StopListening`, `Speak`, `StopSpeaking`, `SetTriggerMode`, `SetEnabled`, `SetSttEnabled`, `SetTtsEnabled`, `SetWakePhrase`, `SetVoice`, `SetSpeed`, and `Shutdown`. Its events are `StateChanged`, `Transcript`, `WakeDetected`, and `Error`. `cuda_dlls.rs` finds GPU DLLs for text to speech and registers them with the Windows loader, so `PATH` does not matter. GPU support for whisper is not wired up yet. `mod.rs` resolves model paths and filters agent replies down to speech-safe text.

Phonemes come from the bundled Misaki phonemizer, not the espeak-ng subprocess. `main()` sets `KOKORO_G2P_SEGMENT_ESPEAK=0` as its first statement. The subprocess path drops the last phoneme of every utterance. This was confirmed against espeak-ng directly: it renders "Hello" as "hell" and "own" as "oh". `ort` is pinned to `=2.0.0-rc.12`, because kokoro-en 0.1.4 only compiles against that release. `[profile.dev.build-override] debug-assertions = false` in `Cargo.toml` is load bearing. Without it, whisper-rs-sys builds whisper.cpp with `-DWHISPER_DEBUG`, while Visual Studio actually builds the Release config, and loading a model aborts. `.cargo/config.toml` sets `CMAKE_CXX_FLAGS_RELEASE` and `CMAKE_C_FLAGS_RELEASE` to `/O2 /Ob2 /DNDEBUG`, because cmake-rs strips optimization flags on MSVC otherwise. Without this fix, transcription takes 11.5s instead of 1.0s.

Speech to text and text to speech degrade independently. A missing whisper model leaves speech output working. A missing Kokoro model leaves speech recognition working. Neither stops the app from starting. Voice needs both local models downloaded separately, they do not ship in the repo. See `docs/voice-setup.md` for what to download and where the files go.

The `settings.json` voice block, with defaults:

| Field | Default |
|---|---|
| `enabled` | `false` |
| `stt_enabled` | `false` |
| `tts_enabled` | `false` |
| `stt_model_path` | none (falls back to `models/ggml-base.en.bin`) |
| `tts_model_path` | none (falls back to `models/model.onnx`) |
| `tts_voices_path` | none (falls back to `voices/`) |
| `trigger_mode` | `"push_to_talk"` |
| `wake_phrase` | `"hey deepseek"` |
| `tts_voice` | `"af_heart"` |
| `tts_speed` | `1.0`, clamped 0.5-2.0 |

**Key architectural choices:**
- No full session persistence between runs (only memory files survive)
- Session reset is hard cut (clear context, reload memory files, start fresh with prompt)
- Context pruning is gradual (relevance-score decay) not discrete compression turns
- Hemisphere model (Phase 3): two model instances, different system prompts, right side sees compressed context
- Dynamic config sync: agent reads `thinking_flag` (AtomicBool) and `model_name` (Mutex<String>) from GUI each turn before building API request

## Implementation Status

Phase 1-2 complete, plus a voice subsystem. Core modules filled in with implementations, tests, and native GUI. Voice adds local speech to text and text to speech, confirmed working against a real microphone and real speakers.

**Done:**
- API client: DeepSeekClient (streaming SSE + non-streaming, retry, auth via env/settings.json, V4 thinking_mode format)
- Agent loop: turn cycle, tool execution, session reset, system prompt rebuild. Echoes reasoning_content back. Filters nameless tool calls (V4 thinking deltas). Syncs config from the GUI each turn (thinking_flag, model_flag). Emits StreamEvent::Reasoning, ToolCallStart, ToolCallEnd, and TurnEnd. TurnEnd carries the prompt cache hit and miss counts.
- Tools: Bash, Read, Write, Reset (Tool trait + ToolRegistry + permission check)
- Config: Settings loading from project/global JSON, PermissionsConfig, HooksConfig
- Context: ThinkingStore with relevance decay
- Hooks: HookRunner with JSON stdin/stdout for lifecycle events
- Memory: MemoryManager loading CLAUDE.md/MEMORY.md
- Skills: Skill loader parsing .md with YAML frontmatter
- Hemisphere: Stub for Phase 3 dual-model
- GUI: egui/eframe native GUI. Has output scroll, input bar, status bar, and a settings sidebar (Tab key). The sidebar holds a model selector, a thinking toggle, and voice controls. Markdown renders via egui_commonmark: white text is markdown, non-white stays raw or styled. Includes a raw/output display toggle and a StreamEvent channel for GUI updates. Escape interrupts the agent. Ctrl+Q quits.
- Voice: `src/voice/` module. Local speech to text via whisper-rs. Local speech output via Kokoro through kokoro-en. Push-to-talk and wake-word triggers, switchable by `trigger_mode`. A `VoiceService` state machine drives it, wired into `main.rs` and the GUI. Speech to text and text to speech degrade independently if a model file is missing. See `docs/voice-setup.md` for model setup. The settings sidebar's voice section has checkboxes for voice enabled, speech to text, and text to speech. It also has trigger mode radio buttons, a wake phrase box, a Kokoro voice picker, and a speed slider.

**GUI key bindings:**
- `Enter` - send message to agent
- `Escape` - interrupt running agent and stop any speech in progress (does not quit)
- `Tab` - toggle settings sidebar (model selector, thinking toggle, voice controls)
- `Ctrl+Q` - quit application
- `Space` (held) - push to talk. Fires only when the input box is not focused and the settings panel is closed.
- `Ctrl+Space` - push to talk toggle. Works even when the input box is focused. Still blocked while the settings panel is open.

**Tests:** 242 total: 239 passing, 1 known failure, 2 ignored.

| Module | Tests |
|---|---|
| `agent/agent_loop.rs` | 7 |
| `agent/history.rs` | 4 |
| `agent/prompt.rs` | 2 |
| `api/client.rs` | 1 |
| `api/types.rs` | 9 |
| `config/settings.rs` | 11 |
| `context/mod.rs` | 6 |
| `gui/mod.rs` | 42 |
| `hemisphere/mod.rs` | 4 |
| `hooks/mod.rs` | 5 |
| `memory/mod.rs` | 4 |
| `skills/mod.rs` | 5 |
| `tools/bash.rs` | 11 |
| `tools/mod.rs` | 4 |
| `tools/read.rs` | 3 |
| `tools/write.rs` | 2 |
| `voice/mod.rs` | 27 |
| `voice/service.rs` | 23 |
| `voice/tts.rs` | 17 |
| `voice/playback.rs` | 13 |
| `voice/capture.rs` | 12 |
| `voice/cuda_dlls.rs` | 10 |
| `voice/wake.rs` | 9 |
| `voice/vad.rs` | 7 |
| `voice/stt.rs` | 3 |

Plus `tests/fixtures/chat_response.json`.

Known failure: `tools::bash::tests::timeout_kills_long_running_command` in `src/tools/bash.rs`. It predates the voice work and is unrelated to it.

**Next:** Phase 3 (hemisphere model), hook execution integration, or skill injection into agent context.

## API Key Resolution

Priority chain: `DEEPSEEK_API_KEY` env → `ANTHROPIC_AUTH_TOKEN` env → `settings.json` (project) → `settings.json` (~/.claude) → `CustomClaude.ps1` on PATH (parses `ANTHROPIC_AUTH_TOKEN` assignment). Sourced via `resolve_api_key()` in `api/client.rs`.

## Logging

Dual output: stderr (colored, human-readable) + `deepseek_custom.log` file (no ANSI). Project root discovered by walking up from cwd until `CLAUDE.md` found. Env filter: `RUST_LOG` or defaults to `info`.

See `docs/plans/2026-05-27-deepseek-harness-implementation-plan.md` for full task checklist.

## Platform

**Windows native.** Batch files have BOM and percent-sign issues in Git Bash; use PowerShell (`.ps1`) for automation scripts. Incremental compilation disabled in `.cargo/config.toml` — ballooned to 10+ GB temp files after a few builds. Hook scripts run via `powershell.exe -NoProfile -ExecutionPolicy Bypass -File <path>`. Bash tool defaults to `cmd /C`; auto-detects commands starting with `powershell`/`pwsh` and runs them directly (avoids `cmd.exe` inner-quote mangling). Use `shell` param for explicit control.

RTK convention: prefix commands with `rtk` for token savings on build/test/git output.
