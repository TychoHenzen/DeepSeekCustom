# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

DeepSeekCustom is an experimental Rust harness for AI coding agents. It runs one of three backends behind a shared GUI: DeepSeek or Ollama through its own in-process agent loop, or Anthropic through a `claude -p` child process. It piggybacks on Claude Code's file formats (settings.json, skills/*.md, CLAUDE.md, MEMORY.md), so the same project config works with either harness.

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
                     ┌─────────────────────────┐
                     │        Backend          │
                     │  (chosen at startup)     │
                     └────────────┬────────────┘
                    ┌─────────────┴─────────────┐
                    ▼                           ▼
        ┌────────────────────┐      ┌────────────────────────┐
        │  Api(AgentLoop)     │      │  ClaudeCli(driver)     │
        │  DeepSeek / Ollama  │      │  spawns `claude -p`    │
        │  (OpenAI-format     │      │  stream-json over      │
        │  chat completions)  │      │  stdin/stdout          │
        └──────────┬──────────┘      └────────────┬───────────┘
                   │                               │
        ┌──────────┴──────────┐         Claude Code owns its own
        ▼          ▼          ▼         tools, skills, hooks, and
     Tools      Hooks      Memory       CLAUDE.md loading. None of
   (Bash, Read,          (CLAUDE.md,    this harness's ToolRegistry,
    Write,               MEMORY.md)     HookRunner, MemoryStore, or
    Reset)                              pruning code runs on this path.
```

Both variants read the same `Settings` and stream `StreamEvent` values to the GUI over the same channel. The GUI does not need to know which one is active.

**Backend kinds:** `src/backend/mod.rs` defines `enum Backend { Api(Box<AgentLoop>), ClaudeCli(ClaudeCliDriver) }`. `main.rs` builds one variant at startup from the resolved config entry and never switches at runtime. The build path itself lives in `src/backend/factory.rs`'s `BackendFactory`, extracted from `main.rs`. It runs again at runtime, on a possibly different entry, whenever the `Task` tool dispatches a subagent. See "Task tool (subagent dispatch)" below.

The `Api` variant is the harness's own in-process HTTP client. It serves both DeepSeek and Ollama. Everything this harness does applies to it: its own `ToolRegistry`, `HookRunner`, `MemoryStore`, skills, context pruning, and relevance scoring, all described below.

The `ClaudeCli` variant is a long-lived `claude -p` child process, for Anthropic. Claude Code owns the whole turn loop on this path. It uses its own tools, its own skills, its own hooks, its own CLAUDE.md loading, and its own compaction and permissions. This harness's `ToolRegistry`, `HookRunner`, `MemoryStore`, pruning, and relevance scoring do not run on this path at all. State that plainly. A future reader will assume this harness's tool and memory machinery always applies. On this path it does not.

**The Ollama provider:** `src/api/client.rs` defines `enum Provider { DeepSeek, Ollama }` on `ApiClient`. `prepare_request` adapts each outgoing request per provider before it goes out. For Ollama it clears `thinking_mode` and `tool_choice` and sets `reasoning_effort` instead: `thinking` and `thinking_max` map to `"high"`, `non-thinking` maps to `"none"`. A request with no `thinking_mode` leaves the caller's `reasoning_effort` untouched.

These facts about Ollama's OpenAI-compatible endpoint are confirmed against a live local Ollama 0.32.5 and its docs. The endpoint is `http://localhost:11434/v1/chat/completions`, so the client's base URL is `http://localhost:11434/v1`. It accepts this harness's existing request shape and ignores unknown fields instead of rejecting them. `tools` is supported. `tool_choice` is not. Its thinking control is a top-level `reasoning_effort` field taking `"high"`, `"medium"`, `"low"`, `"max"`, or `"none"`. Its SSE chunks parse with the existing parser, so no response-side change was needed. It needs an `Authorization` header but ignores its value, so `resolve_api_key` returns the placeholder `"ollama"` for `Provider::Ollama` without reading the environment or disk at all. The `DEEPSEEK_API_KEY` -> `ANTHROPIC_AUTH_TOKEN` -> `settings.json` -> `~/.claude/settings.json` -> `~/.claude/backends.json` chain in "API Key Resolution" below applies only to `Provider::DeepSeek`.

**The claude -p transport:** `src/backend/claude_cli/process.rs` spawns the child as:

```
claude -p --output-format stream-json --input-format stream-json
  --include-partial-messages --verbose --model <model> --permission-mode <mode>
```

`permission_mode` defaults to `bypassPermissions`. This GUI has no permission prompt. This harness's own Bash tool already runs without asking. Any other mode would silently deny every tool call on this path.

A user turn is one line on the child's stdin, shaped `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}`. This was verified against the real CLI.

Output is parsed by `src/backend/claude_cli/events.rs` and mapped to the existing `StreamEvent` by `src/backend/claude_cli/map.rs`, in `EventMapper`, so the GUI needed no change at all. The mapping: a `text_delta` becomes `Text`. A `thinking_delta` becomes `Reasoning`. A `tool_use` content block becomes `ToolCallStart`. A `tool_result` becomes `ToolCallEnd`. A `result` event becomes `TurnEnd`, with `cache_read_input_tokens` as the cache hit count and `cache_creation_input_tokens` as the miss count.

Tool call arguments arrive as `input_json_delta` fragments after the block's `content_block_start`, not inside it. `EventMapper` buffers those fragments per content-block index and only emits `ToolCallStart` at `content_block_stop`, once the full arguments have accumulated. This is easy to miss: the `input` field on `content_block_start` itself is still an empty object at that point in the protocol. The first implementation read `input` directly there and emitted a `ToolCallStart` with empty arguments every time.

Voice reply mode works on this path too. `voice_mode_instructions()` passes through `--append-system-prompt` at spawn. Since that flag is spawn-time only, `ClaudeCliDriver` restarts the child whenever the voice-mode flag changes between turns.

`ClaudeCliDriver::send` writes the turn line, then waits for that turn's `result` event before returning. The stdout reader signals it over a `turn_done` channel. One `send` is one whole turn, which is what `Backend::run` and the autopilot runner both assume. While it waits it polls `interrupt_flag` every 100ms, so Escape reaches this backend too.

Interrupt kills the child outright, since the protocol carries no cancel message. It also drops the child and stdin handles, so the next turn spawns a fresh child instead of writing into a dead pipe. `ensure_ready` respawns on the same grounds whenever `try_wait` shows the child already exited.

**Autopilot across backends:** `run_repeat` in `src/agent/repeat.rs` is generic over a `RepeatTarget` trait, with one implementation per backend kind. The `Api` path resets by calling `AgentLoop::clear_history`. The `ClaudeCli` path resets by shutting the child down, so the next turn spawns a fresh one. Both give the same guarantee: no iteration sees an earlier iteration's conversation. `repeat_interrupt_flag` still stops the whole run on either path.

**Agent loop:** applies to the `Api` variant only. User input builds into messages (system prompt, history, tools), goes to the DeepSeek or Ollama API, and comes back as text or tool calls. Tools run through `ToolRegistry`, and results append to history before the next round starts. A max-turns guard defaults to 100. Streaming runs over `reqwest` plus `tokio::sync::mpsc`. Events reach the GUI through the `StreamEvent` enum over an unbounded channel. That decouples the agent from the UI layer. A user interrupt works through an `Arc<AtomicBool>` flag. The GUI sets it on Escape. The agent checks it during stream receive and before tool execution, then sends `StreamEvent::Interrupted`.

**Voice reply mode:** while text to speech is on, the agent appends a "## Voice reply mode" block to the system prompt each turn. The block tells the model to answer in at most two sentences with a spoken cadence. No markdown, no lists, no code, plain wording for file paths and identifiers. Tool use is unaffected. This is not a separate setting. It follows the text-to-speech checkbox in the settings sidebar, and it starts from the `tts_enabled` value in the `settings.json` voice block. Mechanism: a shared `voice_mode_flag` (`Arc<AtomicBool>`) on `AgentLoop`, read each turn in `sync_dynamic_config` alongside the thinking flag and model name, driving `MessageHistory::set_system_suffix`. The instruction text lives in `voice_mode_instructions()` in `src/agent/prompt.rs`. The GUI holds the same flag and writes it on every text-to-speech toggle. This is separate from and additional to `filter_for_speech` in `src/voice/mod.rs`, which still strips markdown and caps spoken length on the reply the agent sends back. The prompt shortens the reply at the source. The filter cleans whatever comes back regardless.

**Prompt cache stats:** applies to the `Api` variant only. `Usage` carries `prompt_cache_hit_tokens`, `prompt_cache_miss_tokens`, and `prompt_cache_write_tokens`. All three default to zero, so a response without them still parses. DeepSeek sends usage in the last streaming chunk, before `[DONE]`, so `StreamChunk` carries an optional `usage` too. The agent loop keeps that final usage and passes the hit and miss counts on in `StreamEvent::TurnEnd`. The GUI adds them up across turns and shows the running totals in the status bar. A session reset zeroes them.

**Context pruning:** applies to the `Api` variant only. Claude Code manages its own compaction on the `ClaudeCli` path. History grows freely until it passes a high-water mark, the context budget, 100000 tokens by default. Crossing it triggers one hard prune down to a low-water mark, one third of the budget. Pruning rarely and deeply beats pruning every turn. Each prune invalidates the API's prompt cache from the cut point onward. A stable prefix between prunes keeps hitting cache instead. `AgentLoop::maybe_prune_context`, in `src/agent/agent_loop.rs`, checks the budget at the top of every turn before the request is built.

A turn group is one `Role::User` message plus every message that follows it, up to the next `Role::User` message. Groups come fresh from the message vector on every prune, in `src/agent/pruning.rs`. They are never cached, so there is no parallel metadata to drift. The last 2 groups are pinned. No tier touches them.

Three tiers run in order. Each stops the moment the token count reaches the low-water mark. Tier one elides tool bodies. A `Role::Tool` message's content becomes `[elided: N chars of tool output]`. Role, `tool_call_id`, and position stay untouched. An already-elided message is skipped, so a second pass does not double-wrap it. Tier two collapses groups. It keeps the leading user message and the last assistant message with content and no tool calls. It drops everything else in the group, so tool-call pairing survives. Tier three drops groups outright. Ordering inside every tier is lowest relevance score first, then oldest first on a tie.

When the high-water mark trips, `src/context/relevance.rs` makes one extra non-streaming call before pruning. It sends an index of the history, not the history itself: id, role, token count, and a 100-character preview per message. It asks for a JSON array of `{"id","score"}` scores from 0.0 to 1.0. The model it runs on comes from `scoring_model`. A DeepSeek backend always scores on `deepseek-v4-flash`, whatever the main conversation model is, since it only ranks short previews. Any other provider scores on the conversation model itself: a DeepSeek model name would just fail there, and an Ollama call is local, so there is nothing to save. Scoring with hindsight is the point: at prune time the model already knows which messages mattered. Any failure returns `None` and is logged at `warn`: a network error, malformed JSON, a missing or duplicate id, an out-of-range id, or a non-finite score. The prune then proceeds with uniform scores, which degrades to oldest-first. A scoring failure never blocks a turn.

The budget lives on a slider in the Experimental section of the settings sidebar, 32000 to 200000 in steps of 1000. A grey caption shows the derived low-water mark. The slider writes an `Arc<AtomicUsize>` shared with the agent, the same way the thinking toggle and voice controls do. The floor is 32000. Below that, the low-water mark lands inside the pinned region and pruning cannot reach its target.

**Transcript model:** `src/gui/transcript.rs` holds the Chat tab's history as a `Transcript`, a `Vec<Block>`. A `Block` carries a `BlockId`, a `collapsed` flag, and a `BlockKind`. The kinds are `User`, holding the raw text. `Assistant`, holding an ordered `Vec<Span>`. `ToolCall`, holding the tool name, its arguments, an optional output, and an error flag. `Notice`, carrying a `Severity`. A `Span` is `Text` or `Reasoning`, kept separate so a run of deltas coalesces within a kind but never merges across kinds. This file holds no egui type at all, on purpose. That is what makes the model testable without a window. `BlockKind` also names where a later phase's variants land: a `Subagent` variant and an `Image` variant, both still to come.

This replaces the flat `output_lines: Vec<(String, Color32)>` the GUI used to hold, which is gone. That old shape could only express a line's colour, not a message boundary, a nested block, or a value that changes after it was appended, such as a tool call's output arriving after its start line. Colour is now a rendering choice, made from `BlockKind` and `Severity` inside `src/gui/mod.rs`, not a property stored on the data itself.

`Transcript::apply_stream_event` maps every `StreamEvent` variant to a block mutation. There is no catch-all arm, so a variant a later phase adds breaks the build here instead of silently vanishing from the transcript. A text or reasoning delta coalesces onto the open `Assistant` block, opening one first if none is open. A tool call start closes that open block, so any text arriving after a tool result starts a fresh `Assistant` block rather than joining the one before the call. A tool call end fills the most recent `ToolCall` block whose output is still empty, because `StreamEvent` carries no id to match a result back to its start. `SessionReset`, `RepeatIterationStart`, and `RepeatFinished` are explicit no-ops here: the GUI clears the transcript on a session reset through its own handler, not through this method, and the other two drive the Autopilot tab's progress readout instead of the Chat transcript.

**Rendering:** `render_chat_output` in `src/gui/mod.rs` dispatches on `BlockKind`. A `User` or `Assistant` block draws as a bubble with a role label. Within an `Assistant` block, a `Text` span renders through the markdown viewer and a `Reasoning` span renders dimmed inside a collapsing header, closed by default, so a long chain of thought does not push the reply off screen. A `ToolCall` block draws as a one-line summary that expands, on click, to its arguments and output. An errored call marks itself in that summary line without needing to be opened. A turn boundary shows as extra vertical space before a `User` bubble, not as a divider line. The raw-output toggle (`show_raw_output`) draws the same blocks as plain coloured text instead, through `render_raw_blocks`, and is just as exhaustive over every `BlockKind` and `Span` as the bubble path.

**Tool call streaming:** DeepSeek streams tool calls across multiple SSE chunks (first chunk: id+name, subsequent: argument fragments). `merge_tool_call()` matches by index and accumulates partial fields. `reasoning_content` must be echoed back to API in next request or API returns 400.

**Tools:** `Tool` trait (`name`, `description`, `input_schema`, `execute`) with dynamic `ToolRegistry`. Six tools now: Bash, Read, Write, Reset, AskUserQuestion, and Task. Bash runs a shell command with a timeout. Its `shell` param accepts `auto`, `cmd`, or `powershell`. It auto-detects powershell and pwsh commands and runs them directly through `Command::new("powershell")`. That avoids cmd.exe inner-quote mangling. Read reads a file with line numbers. Write writes a file. Reset does a hard session reset. AskUserQuestion asks a small set of labelled-option questions. A policy-driven model call always answers it. See Autopilot below. No human ever answers it directly. Task dispatches a subagent onto a named backend. See "Task tool (subagent dispatch)" below. It is the one tool that is conditional. Past the configured depth limit, a backend's registry gets no Task tool at all, so a subagent cannot dispatch one of its own. Permission check via settings `allow`/`deny` lists.

**Piggybacking formats** (drop-in compatible with Claude Code files):
- `settings.json` - project root or `~/.claude/`. Backend definitions, permissions, hooks, voice config, autopilot config, `context_budget`, and `show_raw_output`. See "Config" below for the `backends` block. The file is a save file. The GUI writes it back on every settings-panel control change, see "Settings persistence" below, so its current contents are whatever the last session left. No document should state what is in it. Read the file itself to find the current backend or settings.
- Skills — `skills/*.md` with YAML frontmatter (`name`, `description`, `tools`).
- Hooks — shell commands receive JSON on stdin, return JSON on stdout. Events: PreToolUse, PostToolUse, SessionStart, SessionEnd, SessionReset.
- Memory - `CLAUDE.md` (project instructions), `MEMORY.md` (persistent memory). Injected into system prompt.
- The repo root also holds an `AGENTS.md`, the same guide written for Codex. Edit both together.

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

**Autopilot:** runs one task text N times in a row without a human in the loop. `run_repeat` in `src/agent/repeat.rs` is generic over the `RepeatTarget` trait, so it drives both backend kinds through one shared loop. See "Autopilot across backends" above for the `ClaudeCli` side. On the `Api` side, before each iteration it calls `AgentLoop::clear_history`, which rebuilds `MessageHistory` from just the base system prompt, dropping every message and any voice-mode suffix. No iteration sees anything an earlier iteration said or did in conversation. What does carry across iterations: files the agent wrote or edited on disk, and the autopilot decision log (below). Nothing else. `sync_dynamic_config` restores the voice-mode suffix on the next `run` call, so a cleared history does not disable voice mode.

The `AskUserQuestion` tool is registered on every run, autopilot or not. A separate, non-streaming model call always answers its questions, never the user. There is no human-answer path at all. A plain chat run gets the exact same machine answers an autopilot run gets. This tool never pauses for a person, in either mode.

The policy file lives at `<project_root>/autopilot-policy.md` by default. `PolicyStore::new` in `src/autopilot/policy.rs` takes an optional override from `settings.autopilot_policy_path()`. A relative override resolves against the project root. An absolute one is used as is. `PolicyStore::load_policy` reads the file straight off disk on every question. It never caches it, so a user can edit the policy mid-run and the next question sees the change. A missing file is not an error. It logs at `debug` and yields an empty policy. A present but unreadable file logs at `warn` and also yields an empty policy, so a policy problem never blocks a turn.

The decision log lives at `<project_root>/.autopilot/decisions.log`. `PolicyStore::append_decision` opens it in append mode and adds one line per resolved question, shaped `question=<text> answer=<labels>`. Embedded newlines get flattened to spaces so the line format holds. `PolicyStore::recent_decisions` reads the newest 20 lines back (`RECENT_DECISIONS_LIMIT` in `src/autopilot/answerer.rs`). `format_policy_prompt_section` folds them into the next answerer prompt alongside the policy text. Feeding recent decisions back in keeps the answerer consistent with itself across a run. It sees what it already decided, not just the static policy.

`PolicyAnswerer::answer`, in `src/autopilot/answerer.rs`, makes one non-streaming `ChatRequest` per batch of questions from one `AskUserQuestion` call. It runs on `settings.autopilot_answerer_model()`, `deepseek-v4-flash` by default, since answering a policy question does not need a heavier model. That default only fits a DeepSeek backend, so `answerer_model` in `src/backend/factory.rs` falls back to the backend's own model for any other provider. An explicit `autopilot.answerer_model` still wins over both. Temperature 0, `thinking_mode: "non-thinking"`. The prompt asks for a JSON array, one entry per question in the same order, shaped `{"question", "labels"}`. Every label must be copied exactly from the offered options. `parse_reply` tolerates a markdown fence or surrounding prose by locating the outermost `[...]` span. `resolve_answers` and `resolve_one` then validate the parsed reply against the real question set. A bad label, a wrong answer count, malformed or unparseable JSON, and a network or API error all get logged at `warn`. All of them fall back to the first option of the affected question, rather than blocking the turn. A single-select question that comes back with more than one label gets trimmed to the first, also with a `warn` log. `answer` never returns an `Err` for a model problem. Fallback happens inside it, so the calling tool always gets a usable answer.

Escape stops the whole repeat run, not just the iteration in flight. `AgentLoop::run` consumes its own `interrupt_flag` internally and resets it once it breaks out of a stream. That flag cannot carry a signal from one iteration to the next. `run_repeat` instead checks a second flag, `AgentLoop::repeat_interrupt_flag`, before every iteration. The GUI's Escape handler sets both flags together. That second flag is the only thing that survives between iterations to say "stop the whole thing."

The `settings.json` autopilot block, with defaults:

| Field | Default |
|---|---|
| `iterations` | `5` |
| `policy_path` | none (falls back to `<project_root>/autopilot-policy.md`) |
| `answerer_model` | `"deepseek-v4-flash"` |
| `task` | none (Autopilot tab starts with an empty task box) |

**Settings persistence:** every settings-panel control writes its change back to `<project_root>/settings.json`, so the panel reads the same way it started. `Settings` derives `Serialize` as well as `Deserialize`. Every optional field carries `skip_serializing_if = "Option::is_none"`, so a save never invents a field the user did not set. `TriggerMode` gets a hand-written `serialize_trigger_mode` to match its hand-written deserializer, keeping the snake_case wire form.

`Settings::save` writes pretty JSON to `settings.json` in the given directory. The GUI holds a `Settings` value and the project root, calls one small `apply_*` free function per control, then calls `persist_settings`. The `apply_*` functions live at the bottom of `src/gui/mod.rs` and are plain functions over `&mut Settings`, so a round-trip test can call them without building a GUI. A save failure is logged at `warn` and otherwise ignored: losing a preference must never take the session down.

`Settings::voice_mut` and `Settings::thinking_mut` create their block with defaults when it is missing. Without them a control change would be silently dropped whenever `settings.json` had no such block.

Startup runs the other direction. `DeepSeekGui::new` seeds every panel control from the settings value it is handed, including the voice mode flag. It falls back to the first Kokoro voice when `tts_voice` names an unknown id. `main` seeds `thinking_flag` and `context_budget_flag` from settings before spawning the agent. The old `with_tts_enabled` builder is gone, since `new` now seeds all of it from one source.

**Config:** the `backends` block in `settings.json` replaced the old top-level `model` field, which is deleted. Each entry is a `BackendConfig` in `src/config/settings.rs`, tagged on `kind`, either `"api"` or `"claude_cli"`.

| `kind` | Fields |
|---|---|
| `api` | `provider` (`"deepseek"` or `"ollama"`), `model`, optional `base_url`, optional `api_key`, optional `models` |
| `claude_cli` | `model`, optional `permission_mode`, optional `env` map, optional `models` |

`default_backend` names the active entry. `BackendFactory::default_backend_name` reads it, falling back to `"deepseek"` when the field is absent. `main.rs` calls that, then `BackendFactory::build` at depth 0 to construct the main session's own backend. An unknown name is a hard startup error naming both the requested entry and the entries that exist. It never falls back silently. Switching backends still takes effect only on the next app start, since the client or driver is built once at startup.

The `settings.json` on disk is a save file the GUI rewrites on every control change. Its current `backends` entries and `default_backend` value are whatever the last session left, not something this document can state. Read the file to see the current backend.

**Task tool (subagent dispatch):** `src/tools/task.rs` adds a tool named `Task`, registered on every `Api` backend's tool registry, subject to the depth limit below. Its schema: `description`, a short label for the model's own bookkeeping, never read back. `prompt`, required, is the subagent's full self-contained instruction. `backend`, required, names an entry in the `backends` map in settings.json. `model` is optional and overrides what that entry declares. The tool dispatches a subagent onto the named backend and returns only that subagent's final text.

The subagent starts with no conversation history of its own. It sees only `prompt`, nothing else from the calling conversation. Its output never streams into the main transcript. Interleaving several models' token streams into one window would be unreadable. Progress logs at `info` instead, inside `run_subagent` in `src/backend/subagent.rs`. One line fires when a subagent starts, naming its backend and depth. Another fires when it finishes, adding elapsed time and the resolved model.

The purpose, in the user's own words: a strong model plans, a cheaper model orchestrates, and a local model iterates on small pieces. The `Task` tool exists to serve that chain: dispatch one well-specified piece of work to whichever backend fits it best.

Errors never kill the caller's turn. An unknown backend name, a subagent failure, or an interrupt all come back as a tool error, never a hard `Result::Err` from `execute`. An unknown name lists the backends that do exist, from `resolve_named_backend` in `src/backend/factory.rs`.

**Subagent depth limit:** `subagent_max_depth` in settings.json controls how deep the dispatch chain can go. It defaults to 2 (`Settings::subagent_max_depth`). The main session dispatches at depth 1. That subagent dispatches at depth 2, if it still carries a `Task` tool. At the limit, a subagent's tool registry carries no `Task` tool at all. So it cannot dispatch further. `may_dispatch(depth, max_depth)` in `src/backend/factory.rs` is the predicate. It is true while `depth < max_depth`. `BackendFactory::build` registers the tool only when the check holds. That happens one depth deeper than the backend it just built.

The limit exists for two reasons. It fits the intended three-level chain: plan, orchestrate, iterate. It also stops a subagent from spawning subagents without bound.

**Subagent machinery:** `src/backend/factory.rs` holds `BackendFactory`, pulled out of `main.rs` so a backend can be built at runtime the same way startup builds one. `BackendFactory::build(self: &Arc<Self>, name, model_override, tx_events, depth)` resolves a named entry. The `depth` argument is what `may_dispatch` gates the `Task` tool on.

`src/backend/subagent.rs` holds `run_subagent`, which builds the backend through the factory, runs it to completion, and returns the final text. Each subagent gets its own event channel. `spawn_event_drain` drains and discards it on a background task, so nothing a subagent streams ever reaches the GUI.

`src/backend/claude_cli/one_shot.rs` holds `ClaudeCliDriver::run_once`. A `claude_cli` subagent runs through this, not the long-lived child `process.rs` owns. The prompt is a positional argument here, there is no `--input-format` flag, and the process exits once the answer is done.

`AgentLoop` now takes an injected interrupt flag instead of creating its own. That lets the same flag the GUI's Escape handler sets reach a running subagent, not just the main turn.

**Model picker:** the settings sidebar now has a model dropdown under the backend picker. `list_models` in `src/api/models.rs` fills it. An explicit `models` array on the backend entry always wins. Otherwise discovery runs by kind and provider. Ollama is queried live at `/api/tags`. DeepSeek returns the known pair `deepseek-v4-flash` and `deepseek-v4-pro`. `claude_cli` returns the known aliases `opus`, `sonnet`, `haiku`, and `fable`. When discovery yields nothing, the result falls back to the model the entry declares. That way the dropdown is never empty and always holds the current selection.

Ollama being down must never stop the GUI from opening. `query_ollama_models` turns every failure into an empty list instead of an error: a connection error, a timeout, a bad status, or malformed JSON. `apply_fallback` covers an empty list from there.

The list resolves on a background task, `spawn_model_list_fetch`, and arrives over a channel the paint loop polls each frame. No network call happens on the paint loop itself. A result tagged with a backend name the user has since switched away from gets dropped as stale.

A model change persists onto that backend's entry in settings.json, through `apply_backend_model`. It writes the shared `model_flag` too, but only while the picked backend is still the one the session is running on. The picker may point at another entry, since a backend switch only takes effect on the next start, and sending that entry's model name to the running backend would fail every following turn. When the two do agree, the change takes effect on the next turn for DeepSeek and Ollama, since `sync_dynamic_config` re-reads `model_flag` every turn. The `claude_cli` backend instead respawns its child to pick up a new model, since `--model` is a spawn-time flag. A backend change itself still needs an app restart, same as before.

The new optional `models` array sits on a backend entry in settings.json. It is `Option<Vec<String>>` on both the `Api` and `ClaudeCli` variants of `BackendConfig`, absent by default.

**Key architectural choices:**
- No conversation persistence between runs (only memory files and `settings.json` survive)
- Session reset is hard cut (clear context, reload memory files, start fresh with prompt)
- Context pruning is a hysteresis oscillator. History grows freely to a high-water mark, then prunes hard to a low-water mark a third of the way down. This keeps the API's prompt cache warm between prunes
- Hemisphere model (Phase 3): two model instances, different system prompts, right side sees compressed context
- Dynamic config sync: agent reads `thinking_flag` (AtomicBool) and `model_name` (Mutex<String>) from GUI each turn before building API request. It also reads `voice_mode_flag` (AtomicBool) each turn to set or clear the voice reply mode system prompt suffix.
- Autopilot repeat needs its own interrupt flag. `AgentLoop::run` resets its normal `interrupt_flag` on every stream break. That flag cannot carry a stop signal from one iteration to the next. `repeat_interrupt_flag` is a second, separate `Arc<AtomicBool>` that stays set until the run stops.

## Implementation Status

Phase 1-2 complete, plus a voice subsystem, a second backend kind, and subagent dispatch. Core modules filled in with implementations, tests, and native GUI. Voice adds local speech to text and text to speech, confirmed working against a real microphone and real speakers. The `ClaudeCli` backend was confirmed end to end against the real `claude` binary: a turn went through `ClaudeCliDriver` and `Text` plus `TurnEnd` events came back on the channel. The `Task` tool was also confirmed end to end. A dispatch onto the `ollama` backend returned that subagent's real reply. `list_models` returned the real models installed on that Ollama instance.

**Done:**
- Backend: `src/backend/mod.rs` (the `Backend` enum and its shared flags), `src/backend/claude_cli/process.rs` (`ClaudeCliDriver`, the child process owner), `src/backend/claude_cli/events.rs` (stream-json event parsing), `src/backend/claude_cli/map.rs` (`EventMapper`, mapping to `StreamEvent`). See the Backend section above for the full mechanism.
- API client: `ApiClient` talks to both DeepSeek and Ollama. It streams replies, retries on failure, and reads its key from an env var or `settings.json`. It uses the V4 thinking_mode format. `prepare_request` changes the outgoing request to fit whichever provider is active.
- Agent loop: turn cycle, tool execution, session reset, system prompt rebuild. Echoes reasoning_content back. Filters nameless tool calls (V4 thinking deltas). Syncs config from the GUI each turn (thinking_flag, model_flag, voice_mode_flag). Emits StreamEvent::Reasoning, ToolCallStart, ToolCallEnd, and TurnEnd. TurnEnd carries the prompt cache hit and miss counts.
- Tools: Bash, Read, Write, Reset, AskUserQuestion, Task (Tool trait + ToolRegistry + permission check). See "Task tool (subagent dispatch)" above for Task.
- Subagent dispatch: `src/backend/factory.rs` (`BackendFactory`, `may_dispatch`), `src/backend/subagent.rs` (`run_subagent`), `src/backend/claude_cli/one_shot.rs` (`ClaudeCliDriver::run_once`), `src/tools/task.rs` (the `Task` tool). See "Task tool (subagent dispatch)", "Subagent depth limit", and "Subagent machinery" above.
- Model picker: `src/api/models.rs` (`list_models`, live Ollama discovery, static DeepSeek and claude_cli lists, fallback to the declared model). See "Model picker" above.
- Config: Settings loading from project/global JSON, saving back to the project `settings.json`, PermissionsConfig, HooksConfig, the `backends` map and `default_backend` (see "Config" above)
- Context: `src/agent/pruning.rs` (three-tier prune over the message vector) and `src/context/relevance.rs` (relevance scoring call), both `Api`-only. `src/context/mod.rs` now holds only the `relevance` module declaration. `ContextPruner`, `ThinkingStore`, and `parse_thinking_tags` were all dead code and have been deleted from it
- Hooks: HookRunner with JSON stdin/stdout for lifecycle events, `Api`-only
- Memory: MemoryManager loading CLAUDE.md/MEMORY.md, `Api`-only
- Skills: Skill loader parsing .md with YAML frontmatter, `Api`-only
- Hemisphere: Stub for Phase 3 dual-model
- GUI: egui/eframe native GUI. Has an output scroll drawn from `Transcript`, an input bar, a status bar, a settings sidebar (Tab key), and a Chat/Autopilot tab bar above the central panel. See "Transcript model" and "Rendering" above for how a `StreamEvent` becomes a drawn block. The sidebar holds a backend picker, a model picker, a thinking toggle, voice controls, and an Experimental section with a context budget slider. The backend picker lists the names from the `backends` map. Beneath it sits a model dropdown, filled by `list_models` resolved in the background. A grey caption says a backend switch takes effect on the next start. A second grey caption says a model change applies next turn for DeepSeek and Ollama, and that claude respawns its child. That budget slider runs 32000 to 200000 tokens in steps of 1000, with a grey caption showing the derived low-water mark. An assistant `Text` span renders via egui_commonmark; every other block and span is styled directly, not routed through markdown. Includes a raw/output display toggle and a StreamEvent channel for GUI updates. Escape interrupts the agent, stops any speech in progress, and stops a running autopilot repeat, in whichever tab is open. Ctrl+Q quits. The text-to-speech checkbox also writes the agent's voice_mode_flag, so voice reply mode turns on and off with text to speech. Every control seeds from `settings.json` at startup and writes back to it on change. The Autopilot tab holds a task text box and an iteration count slider. Below those sits a grey caption with the resolved policy file path. Below that sits a Run button and a progress readout. The readout reads Idle, Running iteration N of M, or Finished with a completed count. The status bar shows `Backend: name (model)`.
- Voice: `src/voice/` module. Local speech to text via whisper-rs. Local speech output via Kokoro through kokoro-en. Push-to-talk and wake-word triggers, switchable by `trigger_mode`. A `VoiceService` state machine drives it, wired into `main.rs` and the GUI. Speech to text and text to speech degrade independently if a model file is missing. See `docs/voice-setup.md` for model setup. The settings sidebar's voice section has checkboxes for voice enabled, speech to text, and text to speech. It also has trigger mode radio buttons, a wake phrase box, a Kokoro voice picker, and a speed slider.
- Autopilot: `src/autopilot/` (policy file and decision log, plus the policy-driven answerer), `src/agent/repeat.rs` (the `RepeatTarget` trait and the shared `run_repeat` runner, driving both backend kinds), and the `AskUserQuestion` tool in `src/tools/ask.rs`. See the Autopilot section above for the full mechanism.

**GUI key bindings:**
- `Enter` - send message to agent
- `Escape` - interrupt running agent, stop any speech in progress, and stop a running autopilot repeat (does not quit)
- `Tab` - toggle settings sidebar (backend picker, thinking toggle, voice controls)
- `Ctrl+Q` - quit application
- `Space` (held) - push to talk. Fires only when the input box is not focused and the settings panel is closed.
- `Ctrl+Space` - push to talk toggle. Works even when the input box is focused. Still blocked while the settings panel is open.

**Tests:** 504 lib tests plus 5 integration tests, all passing. The binary target carries 0 tests. `backend_resolution_tests` moved out of `src/main.rs`. It now lives in `src/backend/factory.rs`, as `factory_tests.rs`, covering `resolve_active_backend`, `may_dispatch`, and the depth-gated `Task` tool wiring. No failures, no ignored tests. `voice/stt.rs` and `voice/tts.rs` each carry one more test that needs the Whisper and Kokoro model files on disk, see `docs/voice-setup.md`. Those two sit behind the `voice-models` cargo feature, off by default, and run with `cargo test --features voice-models`, which brings the lib total to 506.

`tests/api_turn.rs` is the crate's first integration test target. It points a real `ApiClient` at a local `wiremock` server through `BackendConfig`'s existing optional `base_url`, so no production code changed to add it. `wiremock` is a dev dependency. Five tests: a full turn read back from a canned SSE stream, a tool call round trip checked against the recorded second request body, a retry that follows a 500, a malformed chunk that gets skipped without failing the turn, and a stream that never sends the `[DONE]` sentinel but still terminates.

| Module | Tests |
|---|---|
| `agent/agent_loop.rs` | 23 |
| `agent/history.rs` | 10 |
| `agent/prompt.rs` | 5 |
| `agent/pruning.rs` | 10 |
| `agent/repeat.rs` | 5 |
| `api/client.rs` | 11 |
| `api/models.rs` | 14 |
| `api/types.rs` | 10 |
| `autopilot/answerer.rs` | 9 |
| `autopilot/policy.rs` | 10 |
| `autopilot/question.rs` | 10 |
| `backend/mod.rs` | 3 |
| `backend/factory.rs` | 14 |
| `backend/subagent.rs` | 2 |
| `backend/claude_cli/process.rs` | 15 |
| `backend/claude_cli/events.rs` | 10 |
| `backend/claude_cli/map.rs` | 7 |
| `backend/claude_cli/one_shot.rs` | 4 |
| `config/settings.rs` | 37 |
| `context/relevance.rs` | 15 |
| `gui/mod.rs` | 93 |
| `gui/transcript.rs` | 19 |
| `hemisphere/mod.rs` | 4 |
| `hooks/mod.rs` | 5 |
| `memory/mod.rs` | 4 |
| `skills/mod.rs` | 5 |
| `tools/ask.rs` | 5 |
| `tools/bash.rs` | 11 |
| `tools/mod.rs` | 4 |
| `tools/read.rs` | 3 |
| `tools/reset.rs` | 1 |
| `tools/task.rs` | 5 |
| `tools/write.rs` | 2 |
| `voice/mod.rs` | 27 |
| `voice/service.rs` | 23 |
| `voice/tts.rs` | 16 |
| `voice/playback.rs` | 13 |
| `voice/capture.rs` | 12 |
| `voice/cuda_dlls.rs` | 10 |
| `voice/wake.rs` | 9 |
| `voice/vad.rs` | 7 |
| `voice/stt.rs` | 2 |

Plus `tests/fixtures/chat_response.json`, `tests/fixtures/claude_stream_json.jsonl`, and `tests/fixtures/claude_stream_json_tools.jsonl`.

**Next:** Phase 3 (hemisphere model), hook execution integration, or skill injection into agent context.

## API Key Resolution

This chain applies to `Provider::DeepSeek` only. `Provider::Ollama` skips it and returns a placeholder key, see "The Ollama provider" above.

Priority chain: `DEEPSEEK_API_KEY` env → `ANTHROPIC_AUTH_TOKEN` env → `settings.json` (project) → `settings.json` (~/.claude) → `~/.claude/backends.json`. Sourced via `resolve_api_key()` in `api/client.rs`.

`~/.claude/backends.json` is the CustomClaude launcher's own config file, unrelated to the `backends` block this harness reads from `settings.json` (see "Config" above), despite the shared name. It holds a `default` backend name and a `backends` map. Each entry may carry an `apiKey`. The default backend's key wins. If that entry has no key, the first backend that has one wins.

## Logging

Dual output: stderr (colored, human-readable) + `deepseek_custom.log` file (no ANSI). Project root discovered by walking up from cwd until `CLAUDE.md` found. Env filter: `RUST_LOG` or defaults to `info`.

See `docs/plans/2026-05-27-deepseek-harness-implementation-plan.md` for full task checklist.

See `docs/plans/2026-08-04-long-term-roadmap.md` for the seven themes of future work and the order they depend on each other in, with `docs/plans/2026-08-04-long-term-roadmap-checklist.md` as its task list. Theme 1, the structured transcript, and the first slice of theme 7, the test layer, are done.

## Platform

**Windows native.** Batch files have BOM and percent-sign issues in Git Bash; use PowerShell (`.ps1`) for automation scripts. Incremental compilation disabled in `.cargo/config.toml` — ballooned to 10+ GB temp files after a few builds. Hook scripts run via `powershell.exe -NoProfile -ExecutionPolicy Bypass -File <path>`. Bash tool defaults to `cmd /C`; auto-detects commands starting with `powershell`/`pwsh` and runs them directly (avoids `cmd.exe` inner-quote mangling). Use `shell` param for explicit control.

RTK convention: prefix commands with `rtk` for token savings on build/test/git output.
