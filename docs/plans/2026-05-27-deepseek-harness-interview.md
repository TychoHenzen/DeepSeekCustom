# DeepSeekCustom Harness — Requirements Spec

> **For Claude:** This spec was produced by /interview. Use /writing-plans to expand into an implementation plan, or /tdd to implement directly.

**Goal:** Build a Rust-based experimental AI coding harness for DeepSeek that piggybacks on Claude Code's ecosystem (settings, skills, hooks) while exploring biology-inspired interaction patterns.

**Date:** 2026-05-27

---

## Requirements

### Base Layer (implement first)
- Agent loop calling DeepSeek API (standard chat completions endpoint)
- Tool system: Bash, Read, Write (minimum). Extensible to match Claude Code's full set later
- Skills loader: reads Claude Code-compatible skill files from filesystem
- Hooks: executes shell commands with JSON on stdin (Claude Code protocol)
- Settings: reads Claude Code-compatible `settings.json`
- Memory files: reads/writes CLAUDE.md, MEMORY.md (no full session persistence)
- TUI terminal interface (Rust/Ratatui)
- Session reset tool: model can call a tool to hard-cut its own session, clear all context, spawn new session with supplied prompt parameter. Both hemispheres reset together. Memory files reloaded fresh from disk.

### Hemisphere Model (add-on)
- Two instances of same DeepSeek model, different system prompts
- Left: primary agent (logic, code, tool execution)
- Right: always-on background advisor (compressed context, short response cap)
- Right can request clarification via tool call to left
- Exact interaction pattern flagged for experimentation — not locked in

### Thinking Interleave (add-on)
- Prompt-engineered `<think>...</think>` markers inline in model output
- Harness strips markers, stores thinking blocks separately from visible output
- Next turn: thinking blocks are redacted from context
- Relevance-score decay: each block scored; below-threshold blocks pruned over time

### Dynamic Context Curation (add-on)
- Relevance-based pruning of conversation history
- Target: maintain roughly fixed-size context (~100k tokens)
- Continuous gradual forgetting instead of discrete compression turns

### Explicit Non-Goals
- Does NOT support models other than DeepSeek (V3/R1) initially
- Does NOT persist full conversation history between sessions
- Does NOT include Claude Code's full 40+ tool set (starts minimal)
- Does NOT include authentication/OAuth services
- Does NOT include telemetry/analytics
- Does NOT include multi-agent swarm (coordinator) in initial scope
- Session reset is hard cut: no summary carried over (only memory files survive)

### Error Handling
- API failures → retry with backoff, surface to user
- Hook failures → log, continue (non-blocking)
- Tool failures → return error to model as tool result
- Marker parsing failures → fall back to treating entire response as visible
- Session reset failure → error returned to model, session continues

### Constraints
- Rust, edition 2024
- Drop-in compatibility with Claude Code files (settings.json, skills/, hooks/)
- DeepSeek API standard chat completions endpoint
- TUI via Ratatui (or equivalent Rust terminal framework)

### Open Questions (deferred)
- Exact hemisphere interaction protocol → needs experimentation
- Marker token format exact spec → TBD during implementation
- Relevance scoring algorithm for context pruning → TBD
- ~~Which DeepSeek model version~~ → Resolved: `deepseek-v4-flash` (default, thinking toggled per-request), `deepseek-v4-pro` (for heavy tasks)
- OpenAI vs Anthropic API format → Preference TBD. Anthropic format has richer content block streaming useful for thinking interleave. OpenAI format simpler, better SDK support in Rust.

---

## Subtask Checklist

- [ ] **T1: Project scaffolding** — Set up Rust project structure with crates for agent loop, tools, TUI, config. Add dependencies (reqwest/tokio for HTTP, ratatui/crossterm for TUI, serde for config parsing).
- [ ] **T2: DeepSeek API client** — Implement async client for DeepSeek chat completions endpoint. Handle auth via API key (env var or settings). Support streaming responses. Error handling with retry/backoff.
- [ ] **T3: Settings loader** — Parse Claude Code-compatible settings.json (serde). Extract model config, tool permissions, hook definitions. Default values for missing keys.
- [ ] **T4: Tool trait and built-in tools** — Define Tool trait (name, description, input schema, execute). Implement Bash, Read, Write tools. Tool registry for dynamic dispatch.
- [ ] **T5: Agent loop core** — Implement the main turn loop: build messages from context → call DeepSeek API → parse response (tool calls vs text) → execute tools → append results → repeat. Max turns guard. Abort handling.
- [ ] **T6: Skills loader** — Read Claude Code-compatible skill files from filesystem. Parse markdown frontmatter. Inject skill definitions into system prompt. Watch for changes.
- [ ] **T7: Hooks system** — Execute shell commands on hook events (PreToolUse, PostToolUse, SessionStart, etc.). Pass JSON on stdin. Non-blocking, log failures.
- [ ] **T8: TUI shell** — Terminal UI with Ratatui: input area, output scrollback, status bar. Stream model responses. Display tool calls in progress. Handle resize.
- [ ] **T9: Session reset tool** — Implement the self-reset mechanism: model calls a special tool, harness clears context, reloads memory files, starts new session with provided prompt. Both hemispheres reset.
- [ ] **T10: Memory files** — Load CLAUDE.md / MEMORY.md at session start and after reset. Inject into system prompt. Write to memory files via tool calls.
- [ ] **T11: Hemisphere model** — Dual agent instances with different system prompts. Left=primary, right=advisor with compressed context. Right sees summary, responds with short cap. Clarification request tool.
- [ ] **T12: Thinking interleave** — Parse `<think>...</think>` markers in streaming output. Strip from display, store in side-channel. Redact on next turn. Relevance-score decay for old blocks.
- [ ] **T13: Dynamic context pruning** — Track message relevance scores (heuristic-based initially). Prune below-threshold content to maintain ~100k token target. Gradual removal over turns.

---

## Reference Documentation

Sources to consult during implementation. Context7 IDs are resolvable via `ctx_resolve_library_id` + `ctx_query_docs`.

### Primary Template: Claude Code Source
| Resource | Link |
|----------|------|
| Full source tree | https://github.com/yasasbanukaofficial/claude-code/tree/main/src |
| QueryEngine (agent loop) | https://github.com/yasasbanukaofficial/claude-code/blob/main/src/QueryEngine.ts (1295 lines) |
| Tool base definition | https://github.com/yasasbanukaofficial/claude-code/blob/main/src/Tool.ts |
| Tools directory (40+ tools) | https://github.com/yasasbanukaofficial/claude-code/tree/main/src/tools |
| Hooks directory | https://github.com/yasasbanukaofficial/claude-code/tree/main/src/hooks |
| Skills directory | https://github.com/yasasbanukaofficial/claude-code/tree/main/src/skills |
| Context management | https://github.com/yasasbanukaofficial/claude-code/tree/main/src/context |
| Coordinator (multi-agent) | https://github.com/yasasbanukaofficial/claude-code/tree/main/src/coordinator |
| Services (MCP, autoDream) | https://github.com/yasasbanukaofficial/claude-code/tree/main/src/services |

### DeepSeek API (v4 — current as of 2026-05-27)
| Resource | Link / ID |
|----------|-----------|
| API docs home | https://api-docs.deepseek.com/ |
| API reference (chat completions) | https://api-docs.deepseek.com/api/create-chat-completion |
| Function calling guide | https://api-docs.deepseek.com/guides/function_calling |
| Anthropic API guide | https://api-docs.deepseek.com/guides/anthropic_api |
| Agent integrations (Claude Code etc) | https://api-docs.deepseek.com/quick_start/agent_integrations/claude_code |
| Context7 lookup (newer snippets) | `/websites/api-docs_deepseek` |

**Two API formats available:**

| Format | Base URL | SDK |
|--------|----------|-----|
| OpenAI-compatible | `https://api.deepseek.com` | `openai` Python/Node SDK |
| Anthropic-compatible | `https://api.deepseek.com/anthropic` | `anthropic` Python SDK |

**Auth:** `Authorization: Bearer $DEEPSEEK_API_KEY` (get key at https://platform.deepseek.com/api_keys)

**Models (v4):**

| Model | Description |
|-------|-------------|
| `deepseek-v4-flash` | Fast/cheap. Thinking toggleable (replaces `deepseek-chat` + `deepseek-reasoner`) |
| `deepseek-v4-pro` | Powerful. Thinking toggleable (same interface) |
| `deepseek-chat` | ⚠️ Deprecated 2026/07/24 — maps to v4-flash non-thinking mode |
| `deepseek-reasoner` | ⚠️ Deprecated 2026/07/24 — maps to v4-flash thinking mode |

**Thinking control (v4 native):**
- `thinking: {type: "enabled"|"disabled"}` — toggle thinking per request
- `reasoning_effort: "high"|"max"` — control reasoning depth
- `reasoning_content` field in assistant message response — the model's chain of thought
- This means thinking IS native, not just prompt-engineered. The harness can use this for the interleave feature rather than (or in addition to) marker tokens.

**Endpoint:** `POST /chat/completions`

**Tool calling (OpenAI format):**
```json
{
  "tools": [{
    "type": "function",
    "function": {
      "name": "bash",
      "description": "Execute a shell command",
      "parameters": { "type": "object", "properties": {...} }
    }
  }],
  "tool_choice": "auto"  // none | auto | required | {type:"function", function:{name:"X"}}
}
```
Response includes `tool_calls[]` with `id`, `type: "function"`, `function: {name, arguments}`.
Max 128 functions. Strict mode available (Beta): `strict: true`.

**Tool calling (Anthropic format):**
- `tool_choice`: `none` | `auto` | `any` | `tool` all supported
- `disable_parallel_tool_use` is ignored (parallel always allowed)
- Content blocks: `tool_use` type with `id`, `name`, `input`

**Streaming (OpenAI format):**
- SSE: `data: {"choices":[{"delta":{"content":"...","role":"assistant"},"finish_reason":null,...}]}`
- Terminates with `data: [DONE]`
- Optional usage chunk before DONE: `stream_options: {include_usage: true}`
- Tool call deltas streamed in delta.tool_calls

**Streaming (Anthropic format):**
- SSE events: `content_block_start`, `content_block_delta`, `content_block_stop`, `message_delta`, `message_stop`
- Native distinction between `text` and `tool_use` content blocks

**Response fields:**
- `choices[].message.content` — visible text
- `choices[].message.reasoning_content` — thinking/chain-of-thought (only when thinking enabled)
- `choices[].message.tool_calls[]` — function calls requested
- `choices[].finish_reason` — `stop` | `length` | `content_filter` | `tool_calls` | `insufficient_system_resource`
- `usage` — `{prompt_tokens, completion_tokens, total_tokens}`

**Other parameters:**
- `temperature` (0-2, default 1), `top_p` (0-1, default 1)
- `max_tokens` (default 4096)
- `response_format: {type: "json_object"}` for JSON mode
- `stop` — up to 16 stop sequences
- `frequency_penalty` / `presence_penalty` — deprecated, no effect

### Rust Crates
| Crate | Context7 ID | Purpose |
|-------|-------------|---------|
| reqwest | `/websites/rs_reqwest` | HTTP client: streaming chunks via `res.chunk().await`, JSON POST, timeout config |
| ratatui | `/ratatui/ratatui` | TUI: `ratatui::run(|terminal| loop { terminal.draw(render)?; ... })`, Layout, Paragraph, Block |
| crossterm | (bundled with ratatui) | Terminal backend: raw mode, alternate screen, event::read() for key/resize |
| serde / serde_json | `/websites/rs_serde_json_1_0_149_serde_json` | Config deserialization: `#[derive(Deserialize)]`, `serde_json::from_str()` |
| serde_yaml | — | Skills frontmatter parsing (YAML in markdown) |
| tokio | `/websites/rs_tokio` | Async runtime, `tokio::sync::mpsc` channels, `tokio::spawn` |
| color_eyre | — | Error reporting with backtraces (used by ratatui examples) |
| tracing / log | — | Structured logging for agent loop observability |

### Claude Code Piggybacking Formats

**settings.json** (project root or `~/.claude/`):
```json
{
  "model": "deepseek-chat",
  "permissions": {
    "allow": ["Bash", "Read", "Write"],
    "deny": []
  },
  "hooks": {
    "PreToolUse": [
      {"command": "echo '...' | my-hook", "timeout": 5000}
    ],
    "PostToolUse": [],
    "SessionStart": []
  }
}
```

**Skills** (`.md` files in `skills/` directory):
```markdown
---
name: my-skill
description: Does something useful
tools:
  - Bash
  - Read
---

# Skill instructions

Detailed prompt injected into system message when skill is active.
```

**Hooks protocol:**
- Shell command invoked with JSON on stdin
- JSON includes: `event` type, tool name, tool input, session context
- Hook stdout JSON returned to harness (can modify tool input, approve/deny)
- Non-zero exit = hook failure (logged, execution continues)

**Memory files:**
- `CLAUDE.md`: project instructions, loaded into system prompt
- `MEMORY.md`: persistent memory across sessions (autoDream target in CC)


## Research Notes

### Claude Code Architecture (from leaked source)
- **Language:** TypeScript. Entry: `src/main.tsx` (785KB, Commander.js + React/Ink)
- **Core loop:** `src/QueryEngine.ts` (~1295 lines). `submitMessage()` async generator yields SDK messages. Builds system prompt from tools, MCP clients, model, skills, plugins. Main loop: call model → parse tool calls → execute → append results → repeat.
- **Hooks:** `src/hooks/` directory. Events: PreToolUse, PostToolUse, SessionStart, SessionEnd, etc. Shell commands receive JSON on stdin.
- **Skills:** `src/skills/` directory. Markdown files with YAML frontmatter. Loaded via `getSlashCommandToolSkills()`.
- **Context:** `src/context/` directory. Context assembly and pruning logic.
- **Coordinator:** `src/coordinator/coordinatorMode.ts`. Multi-agent orchestration (out of scope for POC).
- **Tools:** 40+ tools in `src/tools/`. Base definition in `src/Tool.ts`.
- **MCP:** `src/services/mcp/`. MCP server integration for external tools.
- **Plugins:** Loaded via `loadAllPluginsCacheOnly()`. External packages extending functionality.

### DeepSeek API (v4, current)
- Endpoint: `POST https://api.deepseek.com/chat/completions`
- Two API formats: OpenAI-compatible + Anthropic-compatible (`/anthropic`)
- Models: `deepseek-v4-flash` (fast), `deepseek-v4-pro` (powerful). Legacy `deepseek-chat`/`deepseek-reasoner` deprecated 2026/07/24.
- Native thinking: `thinking: {type: "enabled"|"disabled"}`, `reasoning_effort: "high"|"max"`. Response includes `reasoning_content` field.
- OpenAI-format tool calling: `tools[]`, `tool_choice`, `tool_calls[]` in response. Max 128 tools. Strict mode beta.
- Anthropic-format tool calling: native `tool_use` content blocks with `content_block_start/delta/stop` SSE events.
- Streaming: SSE with `data: [DONE]` (OpenAI) or `message_stop` (Anthropic).

### Rust Ecosystem
- HTTP client: `reqwest` with `tokio` runtime
- TUI: `ratatui` + `crossterm` (standard Rust TUI stack)
- Config: `serde` + `serde_json` + `serde_yaml` (for skills frontmatter)
- Async: `tokio` (required by reqwest, ratatui crossterm backend)
- Streaming: `reqwest` streaming + `tokio::sync::mpsc` for TUI updates

### Piggybacking Surface
- `settings.json`: JSON file in project root / `~/.claude/`. Model, permissions, hooks config.
- Skills: `.md` files in `skills/` directory. YAML frontmatter with name, description, tools.
- Hooks: JSON on stdin, JSON on stdout. Event types: `PreToolUse`, `PostToolUse`, `SessionStart`, `SessionEnd`.
