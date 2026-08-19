## Context

The harness already has two backend kinds: `Api` (in-process HTTP client for DeepSeek/Ollama) and `ClaudeCli` (subprocess driving `claude -p` over stream-json). The `ClaudeCli` path established the pattern for a subprocess backend: spawn a child, parse its stdout protocol, map events to `StreamEvent`, manage the child's lifetime through the Windows job object, and support session resume through an id the child reports. See proposal.md for motivation.

The Codex CLI's `exec --json` mode outputs JSONL events with a `type` discriminator. Its event protocol is different from `claude -p`'s stream-json, but the integration shape is the same: spawn, parse, map, resume, kill.

## Goals / Non-Goals

**Goals:**

- A `CodexCli` backend that follows the same structural pattern as `ClaudeCli`, so every `Backend` match arm, every `SharedFlags` adoption, and every `RepeatTarget` implementation works the same way.
- JSONL event parsing and mapping as a standalone, testable layer, the same way `claude_cli/events.rs` and `claude_cli/map.rs` are structured.
- Session resume through `codex exec resume <thread_id>`, driven by the `thread_id` from the `thread.started` event.

**Non-Goals:**

- Image input through Codex. The Codex CLI accepts `--image` for initial prompts, but the harness's mid-conversation image attachment model does not map onto it cleanly. Defer until the need is real, the same way DeepSeek image support was deferred after the 400 was confirmed.
- MCP server management for Codex. Codex has its own MCP client. This harness's `mcp/` module does not run on subprocess backends, the same as `ClaudeCli`.
- Prompt caching stats. Codex's `turn.completed` carries `cached_input_tokens`, but the harness's prompt cache display is currently wired to the `Api` path's own fields. Wiring it through is a follow-up.
- Voice reply mode instructions through `--append-system-prompt` or equivalent. Codex has no such flag. Defer.

## Decisions

### 1. One child per turn vs. long-lived child

**Decision:** One child per turn, unlike `ClaudeCli` which keeps a long-lived child.

**Rationale:** `claude -p` accepts `--input-format stream-json` and reads multiple turns from stdin. Codex `exec` takes one prompt per invocation and exits after `turn.completed`. Resume is done by spawning `codex exec resume <thread_id>`, not by writing to the same child's stdin. This is a fundamental protocol difference, not a design choice.

**Alternative considered:** Wrapping multiple `codex exec` calls behind a single `CodexCliDriver` that looks long-lived to the caller. This is what the implementation does - the driver struct holds the `thread_id` and spawns a fresh child per turn - but the caller sees the same `send` / `send_with_image` interface as `ClaudeCli`.

### 2. Module structure mirrors claude_cli

**Decision:** `backend/codex_cli/` with `mod.rs` (driver struct), `spawn.rs` (argument assembly and child creation), `events.rs` (JSONL parsing into typed events), `map.rs` (event-to-StreamEvent mapping), and `repeat.rs` (RepeatTarget impl).

**Rationale:** The `claude_cli/` split is proven: events.rs and map.rs are independently testable, spawn.rs isolates the platform-specific child creation, and the driver coordinates them. Reusing the same shape means a reader familiar with one backend understands the other.

**Alternative considered:** A single file. Rejected because `claude_cli` started as one file and grew past 800 lines before the split.

### 3. Sandbox default

**Decision:** Default to `--dangerously-bypass-approvals-and-sandbox` when no `sandbox` field is set on the backend entry. Pass `--sandbox <value>` when one is set.

**Rationale:** Same reasoning as `ClaudeCli` defaulting `permission_mode` to `bypassPermissions`. This GUI has no interactive approval prompt. Any sandbox mode that would prompt blocks the turn forever.

### 4. Effort mapping through config override

**Decision:** Pass reasoning effort as `-c reasoning.effort=<value>` rather than as a dedicated CLI flag.

**Rationale:** Codex's `--config` flag (`-c`) accepts TOML dotted paths, and `reasoning.effort` is the documented config key. There is no `--effort` CLI flag on Codex like there is on `claude`. The `-c` form is the correct way to set it non-interactively.

### 5. Auth via environment variable

**Decision:** Expect `OPENAI_API_KEY` in the environment or pre-configured via `codex login`. The `env` field on the backend entry can inject it per-backend.

**Rationale:** Codex handles its own auth. The harness does not need an `api_key` field or a resolution chain the way the `Api` backend does for DeepSeek. If the key is not set, Codex itself reports the error, which reaches the transcript as a `turn.failed` event.

### 6. Model discovery fallback

**Decision:** Return a static list (`o3`, `o4-mini`) when no explicit `models` array is configured. No live query to OpenAI's API.

**Rationale:** Codex does not have a `--list-models` flag. Querying the OpenAI `/v1/models` endpoint would need an API key and would return hundreds of models, most irrelevant to Codex (embeddings, DALL-E, whisper). A static fallback with an explicit override is the same pattern `ClaudeCli` uses for its aliases. The user can set `models: [...]` on the entry to get whatever list they want.

## Risks / Trade-offs

- **[Codex CLI not installed]** -> The harness does not bundle Codex. If the binary is not on PATH, `Command::new("codex")` fails at spawn time. The error reaches the transcript as a tool error or a `StreamEvent::Error`, the same as a missing `claude` binary. No special handling needed.
- **[JSONL protocol changes]** -> Codex is pre-1.0 (v0.147.0 as of this writing). The event schema may change. Mitigation: the parser skips unrecognised event types with a `warn` log, so additive changes do not break the harness. A removal or rename of an event type the harness depends on breaks that event's mapping and needs a code update.
- **[One child per turn is slower]** -> Each turn pays the Codex CLI's startup cost (about 0.2s measured locally for a Node-based CLI). For a conversation with many short turns this adds up. Mitigation: this is inherent to the protocol, not a design choice. If Codex adds a long-lived mode later, the driver can adopt it without changing the rest of the integration.
- **[No image support at launch]** -> Codex accepts `--image` on the initial prompt only. A mid-conversation image attachment has no equivalent. The harness will post a transcript notice naming the backend, the same way it handles DeepSeek's lack of image support, rather than silently dropping the bytes.
