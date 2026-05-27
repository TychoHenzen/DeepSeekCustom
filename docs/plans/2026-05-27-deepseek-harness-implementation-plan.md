# DeepSeekCustom Harness — Implementation Plan

> Derived from: `docs/plans/2026-05-27-deepseek-harness-interview.md`
> Date: 2026-05-27
> Target: Rust edition 2024, Ratatui TUI, DeepSeek API (v4)

---

## Dependency Overview

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json", "stream"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
ratatui = "0.29"
crossterm = "0.28"
color-eyre = "0.6"
tracing = "0.1"
tracing-subscriber = "0.3"
```

---

# Phase 0: Foundation

## T1: Project Scaffolding

### T1.1 Crate structure
- [ ] T1.1.1 Create `src/main.rs` — entry point, tokio runtime bootstrap | Start: --- | End: ---
- [ ] T1.1.2 Create `src/lib.rs` — re-export all public modules | Start: --- | End: ---
- [ ] T1.1.3 Create module tree: | Start: --- | End: ---
  ```
  src/
  ├── main.rs
  ├── lib.rs
  ├── api/           # DeepSeek API client
  │   └── mod.rs
  ├── agent/         # Agent loop
  │   └── mod.rs
  ├── tools/         # Tool trait + built-in tools
  │   ├── mod.rs
  │   ├── bash.rs
  │   ├── read.rs
  │   └── write.rs
  ├── tui/           # Terminal UI
  │   └── mod.rs
  ├── config/        # Settings loader
  │   └── mod.rs
  ├── skills/        # Skills loader
  │   └── mod.rs
  ├── hooks/         # Hook system
  │   └── mod.rs
  ├── memory/        # Memory files (CLAUDE.md, MEMORY.md)
  │   └── mod.rs
  ├── context/       # Context assembly & pruning
  │   └── mod.rs
  └── hemisphere/    # Dual-agent model (Phase 3)
      └── mod.rs
  ```
- [ ] T1.1.4 Add all dependencies to `Cargo.toml` (see dependency overview above) | Start: --- | End: ---
- [ ] T1.1.5 Verify: `cargo check` passes with empty stubs | Start: --- | End: ---

### T1.2 Error type
- [ ] T1.2.1 Create `src/error.rs` with `HarnessError` enum using `thiserror`: | Start: --- | End: ---
  ```rust
  #[derive(Error, Debug)]
  pub enum HarnessError {
      #[error("API error: {0}")]
      Api(String),
      #[error("Config error: {0}")]
      Config(String),
      #[error("Tool error: {0}")]
      Tool(String),
      #[error("IO error: {0}")]
      Io(#[from] std::io::Error),
      #[error("Hook error: {0}")]
      Hook(String),
      #[error("Parse error: {0}")]
      Parse(String),
      #[error("Session reset")]
      SessionReset,
  }
  ```
- [ ] T1.2.2 Add `thiserror` to `Cargo.toml` | Start: --- | End: ---
- [ ] T1.2.3 Verify: `cargo check` passes | Start: --- | End: ---

### T1.3 Logging setup
- [ ] T1.3.1 Configure `tracing-subscriber` in `main.rs` — `EnvFilter` from `RUST_LOG` env var, default `info` | Start: --- | End: ---
- [ ] T1.3.2 Verify: app starts with timestamped log output | Start: --- | End: ---

---

## T2: DeepSeek API Client

### T2.1 Types
- [ ] T2.1.1 Define request types in `src/api/types.rs`: | Start: --- | End: ---
  ```rust
  pub struct ChatRequest {
      pub model: String,
      pub messages: Vec<Message>,
      pub tools: Option<Vec<ToolDef>>,
      pub tool_choice: Option<ToolChoice>,
      pub stream: bool,
      pub temperature: Option<f32>,
      pub max_tokens: Option<u32>,
      pub thinking: Option<ThinkingConfig>,
  }

  pub struct Message {
      pub role: Role,  // system | user | assistant | tool
      pub content: Option<String>,
      pub tool_calls: Option<Vec<ToolCall>>,
      pub tool_call_id: Option<String>,
      pub reasoning_content: Option<String>,  // v4 thinking
  }

  pub enum Role { System, User, Assistant, Tool }

  pub struct ToolDef {
      pub r#type: String,  // "function"
      pub function: FunctionDef,
  }

  pub struct FunctionDef {
      pub name: String,
      pub description: String,
      pub parameters: serde_json::Value,  // JSON Schema
  }

  pub struct ThinkingConfig {
      pub r#type: String,  // "enabled" | "disabled"
      pub reasoning_effort: Option<String>,  // "high" | "max"
  }
  ```
- [ ] T2.1.2 Define response types: `ChatResponse`, `Choice`, `Usage`, `StreamChunk`, `ToolCall` | Start: --- | End: ---
- [ ] T2.1.3 Define `ToolResult` for feeding tool outputs back: | Start: --- | End: ---
  ```rust
  pub struct ToolResult {
      pub tool_call_id: String,
      pub role: String,  // "tool"
      pub content: String,
  }
  ```

### T2.2 Client struct
- [ ] T2.2.1 Create `src/api/client.rs` — `DeepSeekClient` struct: | Start: --- | End: ---
  ```rust
  pub struct DeepSeekClient {
      client: reqwest::Client,
      base_url: String,
      api_key: String,
      default_model: String,
  }
  ```
- [ ] T2.2.2 Implement `DeepSeekClient::new(api_key: String, base_url: Option<String>)` — default to `https://api.deepseek.com` | Start: --- | End: ---
- [ ] T2.2.3 Implement `DeepSeekClient::chat(&self, req: ChatRequest) -> Result<ChatResponse>` | Start: --- | End: ---
- [ ] T2.2.4 Implement `DeepSeekClient::chat_stream(&self, req: ChatRequest) -> impl Stream<Item = Result<StreamChunk>>` | Start: --- | End: ---
  - Use `reqwest::Response::chunk()` for SSE streaming
  - Parse SSE lines: `data: {...}\n\n`, terminal `data: [DONE]`
  - Parse `reasoning_content` field from delta messages
- [ ] T2.2.5 Implement auth: `Authorization: Bearer $DEEPSEEK_API_KEY` header on every request | Start: --- | End: ---

### T2.3 Retry & backoff
- [ ] T2.3.1 Add retry config: `max_retries: u32`, `base_delay_ms: u64` | Start: --- | End: ---
- [ ] T2.3.2 Implement exponential backoff wrapper: retry 429/5xx, max 3 attempts, delays: 1s → 2s → 4s | Start: --- | End: ---
- [ ] T2.3.3 Log retry attempts with attempt number and delay | Start: --- | End: ---

### T2.4 API key source
- [ ] T2.4.1 Read from `DEEPSEEK_API_KEY` env var (primary) | Start: --- | End: ---
- [ ] T2.4.2 Read from `settings.json` `"api_key"` field (fallback) | Start: --- | End: ---
- [ ] T2.4.3 Error with clear message if no key found: "DEEPSEEK_API_KEY not set. Get one at https://platform.deepseek.com/api_keys" | Start: --- | End: ---

### T2.5 Tests
- [ ] T2.5.1 Unit test: `ChatRequest` serializes to correct JSON | Start: --- | End: ---
- [ ] T2.5.2 Unit test: `ChatResponse` deserializes from sample JSON (store sample in `tests/fixtures/`) | Start: --- | End: ---
- [ ] T2.5.3 Unit test: `StreamChunk` parses SSE data line correctly | Start: --- | End: ---
- [ ] T2.5.4 Unit test: missing API key returns clear error | Start: --- | End: ---

---

## T3: Settings Loader

### T3.1 Settings struct
- [ ] T3.1.1 Define `src/config/settings.rs`: | Start: --- | End: ---
  ```rust
  #[derive(Deserialize, Debug, Clone)]
  pub struct Settings {
      pub model: Option<String>,
      pub api_key: Option<String>,
      pub permissions: Option<PermissionsConfig>,
      pub hooks: Option<HooksConfig>,
      pub thinking: Option<ThinkingSettingsConfig>,
  }

  #[derive(Deserialize, Debug, Clone)]
  pub struct PermissionsConfig {
      pub allow: Option<Vec<String>>,
      pub deny: Option<Vec<String>>,
  }

  #[derive(Deserialize, Debug, Clone)]
  pub struct HooksConfig {
      #[serde(rename = "PreToolUse")]
      pub pre_tool_use: Option<Vec<HookDef>>,
      #[serde(rename = "PostToolUse")]
      pub post_tool_use: Option<Vec<HookDef>>,
      #[serde(rename = "SessionStart")]
      pub session_start: Option<Vec<HookDef>>,
      #[serde(rename = "SessionEnd")]
      pub session_end: Option<Vec<HookDef>>,
  }

  #[derive(Deserialize, Debug, Clone)]
  pub struct HookDef {
      pub command: String,
      pub timeout: Option<u64>,
  }
  ```
- [ ] T3.1.2 Define defaults via `Default` trait: model=`deepseek-v4-flash`, empty permissions/hooks | Start: --- | End: ---

### T3.2 Loader
- [ ] T3.2.1 Implement `Settings::load()`: | Start: --- | End: ---
  1. Try `<project_root>/settings.json` → `~/.claude/settings.json` → `~/.deepseek/settings.json`
  2. Merge with defaults (later sources override earlier)
  3. Return `Settings` or `HarnessError::Config`
- [ ] T3.2.2 Implement `Settings::model()` — returns model string, resolving env var first: `model` field → `DEEPSEEK_MODEL` env var → `settings.json` model → `"deepseek-v4-flash"` | Start: --- | End: ---
- [ ] T3.2.3 Log loaded config at session start (redact `api_key`) | Start: --- | End: ---

### T3.3 Tests
- [ ] T3.3.1 Unit test: load from `tests/fixtures/settings.json` fixture | Start: --- | End: ---
- [ ] T3.3.2 Unit test: missing keys get defaults | Start: --- | End: ---
- [ ] T3.3.3 Unit test: model resolution priority order correct | Start: --- | End: ---
- [ ] T3.3.4 Unit test: nonexistent file returns Config error | Start: --- | End: ---

---

# Phase 1: Core Agent

## T4: Tool Trait and Built-in Tools

### T4.1 Tool trait
- [ ] T4.1.1 Define `src/tools/mod.rs` — `Tool` trait: | Start: --- | End: ---
  ```rust
  #[async_trait]
  pub trait Tool: Send + Sync {
      fn name(&self) -> &str;
      fn description(&self) -> &str;
      fn input_schema(&self) -> serde_json::Value;
      async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput>;
  }

  pub struct ToolOutput {
      pub content: String,
      pub is_error: bool,
  }
  ```
- [ ] T4.1.2 Implement `ToolRegistry`: | Start: --- | End: ---
  ```rust
  pub struct ToolRegistry {
      tools: HashMap<String, Arc<dyn Tool>>,
  }

  impl ToolRegistry {
      pub fn register(&mut self, tool: Arc<dyn Tool>);
      pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;
      pub fn list(&self) -> Vec<&Arc<dyn Tool>>;
      pub fn to_api_definitions(&self) -> Vec<ToolDef>;
  }
  ```
- [ ] T4.1.3 Add `async_trait` to `Cargo.toml` | Start: --- | End: ---

### T4.2 Bash tool
- [ ] T4.2.1 Create `src/tools/bash.rs` — `BashTool` struct | Start: --- | End: ---
- [ ] T4.2.2 Input schema: `{"command": string, "timeout_ms": optional int}` | Start: --- | End: ---
- [ ] T4.2.3 Execute: spawn `sh -c "{command}"`, capture stdout+stderr, enforce timeout | Start: --- | End: ---
- [ ] T4.2.4 Return stdout/stderr + exit code in output | Start: --- | End: ---
- [ ] T4.2.5 Sandboxing: working directory is project root by default | Start: --- | End: ---
- [ ] T4.2.6 Test: simple command (`echo hello`) returns correct stdout | Start: --- | End: ---
- [ ] T4.2.7 Test: timeout kills long-running command | Start: --- | End: ---

### T4.3 Read tool
- [ ] T4.3.1 Create `src/tools/read.rs` — `ReadTool` struct | Start: --- | End: ---
- [ ] T4.3.2 Input schema: `{"file_path": string, "offset": optional int, "limit": optional int}` | Start: --- | End: ---
- [ ] T4.3.3 Execute: read file at path, return content with line numbers (cat -n format) | Start: --- | End: ---
- [ ] T4.3.4 Path safety: resolve relative to project root, reject `..` escape attempts | Start: --- | End: ---
- [ ] T4.3.5 Test: reads known file correctly | Start: --- | End: ---
- [ ] T4.3.6 Test: rejects paths outside project root | Start: --- | End: ---

### T4.4 Write tool
- [ ] T4.4.1 Create `src/tools/write.rs` — `WriteTool` struct | Start: --- | End: ---
- [ ] T4.4.2 Input schema: `{"file_path": string, "content": string}` | Start: --- | End: ---
- [ ] T4.4.3 Execute: write content to file, create parent dirs if needed | Start: --- | End: ---
- [ ] T4.4.4 Path safety: same as Read tool | Start: --- | End: ---
- [ ] T4.4.5 Test: writes and verifies file | Start: --- | End: ---
- [ ] T4.4.6 Test: rejects paths outside project root | Start: --- | End: ---

### T4.5 Session Reset tool
- [ ] T4.5.1 Create `src/tools/reset.rs` — `ResetTool` struct | Start: --- | End: ---
- [ ] T4.5.2 Input schema: `{"prompt": string}` — new session's initial prompt | Start: --- | End: ---
- [ ] T4.5.3 Execute: returns a special `SessionReset` error variant (caught by agent loop) | Start: --- | End: ---
- [ ] T4.5.4 Test: returns correct error variant | Start: --- | End: ---

### T4.6 Tool permission check
- [ ] T4.6.1 Implement `ToolRegistry::check_permission(&self, tool_name: &str, settings: &Settings) -> bool` | Start: --- | End: ---
- [ ] T4.6.2 Logic: deny list checked first, then allow list. If allow list empty → all allowed | Start: --- | End: ---
- [ ] T4.6.3 Test: deny overrides allow | Start: --- | End: ---
- [ ] T4.6.4 Test: empty allow = all permitted | Start: --- | End: ---

---

## T5: Agent Loop Core

### T5.1 Message history
- [ ] T5.1.1 Create `src/agent/history.rs` — `MessageHistory` struct: | Start: --- | End: ---
  ```rust
  pub struct MessageHistory {
      system_prompt: String,
      messages: Vec<Message>,
      token_count: usize,  // approximate
  }
  ```
- [ ] T5.1.2 Implement `push()`, `iter()`, `len()`, `to_api_messages()` | Start: --- | End: ---
- [ ] T5.1.3 Implement `estimated_tokens()` — rough count: 1 token ≈ 4 chars | Start: --- | End: ---

### T5.2 System prompt builder
- [ ] T5.2.1 Create `src/agent/prompt.rs` — `SystemPromptBuilder` | Start: --- | End: ---
- [ ] T5.2.2 Assembles from: base instructions, skill definitions, memory files, tool list | Start: --- | End: ---
- [ ] T5.2.3 Format: include current date, working directory, available tools with schemas | Start: --- | End: ---

### T5.3 Turn loop
- [ ] T5.3.1 Create `src/agent/loop.rs` — `AgentLoop` struct: | Start: --- | End: ---
  ```rust
  pub struct AgentLoop {
      client: DeepSeekClient,
      tools: ToolRegistry,
      history: MessageHistory,
      config: AgentConfig,
  }

  pub struct AgentConfig {
      pub max_turns: u32,       // default: 100
      pub model: String,
      pub thinking: bool,
  }
  ```
- [ ] T5.3.2 Implement `AgentLoop::run(&mut self, user_input: &str) -> Result<()>`: | Start: --- | End: ---
  ```
  loop:
    1. Add user message to history
    2. Build API request from history + tools
    3. Call client.chat_stream(request)
    4. Accumulate streaming chunks into response
    5. Parse response:
       - If text: display to user, append to history
       - If tool_calls: for each tool call:
           a. Resolve tool from registry
           b. Check permission
           c. Execute tool
           d. Append tool result to history
       - If finish_reason == "stop": break
    6. If turns > max_turns: break with warning
    7. Repeat
  ```
- [ ] T5.3.3 Handle `SessionReset` error: clear history, reload memory files, start new loop with prompt param | Start: --- | End: ---
- [ ] T5.3.4 Log: each turn number, model called, tokens used, tool invocations | Start: --- | End: ---
- [ ] T5.3.5 Handle abort signal (Ctrl+C) — flush and exit gracefully | Start: --- | End: ---

### T5.4 Streaming display
- [ ] T5.4.1 Accumulate text chunks in buffer | Start: --- | End: ---
- [ ] T5.4.2 Send incremental text to TUI via `tokio::sync::mpsc` channel | Start: --- | End: ---
- [ ] T5.4.3 Detect tool call boundaries in stream (tool_calls delta chunks) | Start: --- | End: ---
- [ ] T5.4.4 Send tool call start/end events to TUI | Start: --- | End: ---

### T5.5 Tests
- [ ] T5.5.1 Integration test: mock DeepSeek API (text-only response) | Start: --- | End: ---
- [ ] T5.5.2 Integration test: mock DeepSeek API (tool call → tool result cycle) | Start: --- | End: ---
- [ ] T5.5.3 Integration test: max turns guard triggers | Start: --- | End: ---
- [ ] T5.5.4 Integration test: session reset clears history and reloads memory | Start: --- | End: ---

---

## T6: Skills Loader

### T6.1 Skill type
- [ ] T6.1.1 Define `src/skills/mod.rs` — `Skill` struct: | Start: --- | End: ---
  ```rust
  pub struct Skill {
      pub name: String,
      pub description: String,
      pub tools: Vec<String>,
      pub content: String,  // markdown body after frontmatter
  }
  ```

### T6.2 Parser
- [ ] T6.2.1 Implement `Skill::from_markdown(content: &str) -> Result<Skill>`: | Start: --- | End: ---
  1. Split on `---` delimiters (YAML frontmatter)
  2. Parse frontmatter: `name`, `description`, `tools` (optional)
  3. Remainder = `content` (skill instructions)
- [ ] T6.2.2 Fallback: if no frontmatter, treat entire file as content, use filename as name | Start: --- | End: ---

### T6.3 Loader
- [ ] T6.3.1 Implement `SkillLoader::load_all(project_root: &Path) -> Result<Vec<Skill>>`: | Start: --- | End: ---
  1. Scan `<project_root>/skills/*.md`
  2. Parse each file
  3. Skip files that fail to parse (log warning)
  4. Return successfully parsed skills
- [ ] T6.3.2 Implement `SkillLoader::load_global() -> Result<Vec<Skill>>`: | Start: --- | End: ---
  1. Scan `~/.claude/skills/*.md`
  2. Same parsing logic
- [ ] T6.3.3 Merge project + global skills (project overrides global by name) | Start: --- | End: ---

### T6.4 System prompt injection
- [ ] T6.4.1 Implement `format_skills_for_prompt(skills: &[Skill]) -> String` | Start: --- | End: ---
- [ ] T6.4.2 Format: skill name, description, instructions, available tools | Start: --- | End: ---
- [ ] T6.4.3 Called by `SystemPromptBuilder` during prompt assembly | Start: --- | End: ---

### T6.5 Tests
- [ ] T6.5.1 Unit test: parse skill markdown with frontmatter | Start: --- | End: ---
- [ ] T6.5.2 Unit test: parse skill markdown without frontmatter (filename fallback) | Start: --- | End: ---
- [ ] T6.5.3 Unit test: loader scans directory and parses all .md files | Start: --- | End: ---
- [ ] T6.5.4 Unit test: malformed file → warning, not crash | Start: --- | End: ---

---

## T7: Hooks System

### T7.1 Hook types
- [ ] T7.1.1 Define `src/hooks/mod.rs` — `HookEvent` enum: | Start: --- | End: ---
  ```rust
  pub enum HookEvent {
      PreToolUse { tool: String, input: serde_json::Value },
      PostToolUse { tool: String, input: serde_json::Value, output: ToolOutput },
      SessionStart,
      SessionEnd,
      SessionReset,
  }
  ```

### T7.2 Hook runner
- [ ] T7.2.1 Implement `HookRunner::run(event: HookEvent, hooks: &[HookDef]) -> Result<HookResult>`: | Start: --- | End: ---
  ```rust
  pub struct HookResult {
      pub approved: bool,
      pub modified_input: Option<serde_json::Value>,
      pub message: Option<String>,
  }
  ```
- [ ] T7.2.2 Serialize event to JSON, write to hook command's stdin | Start: --- | End: ---
- [ ] T7.2.3 Execute hook command via `tokio::process::Command` | Start: --- | End: ---
- [ ] T7.2.4 Parse stdout JSON as `HookResult` | Start: --- | End: ---
- [ ] T7.2.5 Enforce timeout per `HookDef.timeout` (kill process if exceeded) | Start: --- | End: ---
- [ ] T7.2.6 Non-zero exit → log error, return approved=true (don't block on hook failure) | Start: --- | End: ---

### T7.3 Integration points
- [ ] T7.3.1 Call PreToolUse hooks before each tool execution | Start: --- | End: ---
- [ ] T7.3.2 Call PostToolUse hooks after each tool execution | Start: --- | End: ---
- [ ] T7.3.3 Call SessionStart hooks at agent loop start | Start: --- | End: ---
- [ ] T7.3.4 Call SessionReset hooks during session reset | Start: --- | End: ---
- [ ] T7.3.5 Call SessionEnd hooks on agent loop exit | Start: --- | End: ---

### T7.4 Tests
- [ ] T7.4.1 Integration test: hook script that echoes modified input | Start: --- | End: ---
- [ ] T7.4.2 Integration test: hook script that returns approved=false (tool blocked) | Start: --- | End: ---
- [ ] T7.4.3 Integration test: hook timeout kills process | Start: --- | End: ---
- [ ] T7.4.4 Integration test: hook failure (exit 1) doesn't block execution | Start: --- | End: ---

---

# Phase 2: Interface & Memory

## T8: TUI Shell

### T8.1 Terminal setup
- [ ] T8.1.1 Create `src/tui/mod.rs` — `Tui` struct | Start: --- | End: ---
- [ ] T8.1.2 Enter raw mode + alternate screen on start (`crossterm::terminal`) | Start: --- | End: ---
- [ ] T8.1.3 Restore terminal on exit (panic hook + normal exit) | Start: --- | End: ---
- [ ] T8.1.4 Event loop: read keyboard input + resize events via `crossterm::event::read()` | Start: --- | End: ---

### T8.2 Layout
- [ ] T8.2.1 Split screen into 3 areas (Ratatui `Layout`): | Start: --- | End: ---
  ```
  ┌─────────────────────────────────┐
  │         Output Scrollback        │  ← 70% height
  │                                  │
  ├─────────────────────────────────┤
  │          Input Area              │  ← 15% height
  ├─────────────────────────────────┤
  │ Mode: Chat | Model: v4-flash    │  ← Status bar, 1 line
  └─────────────────────────────────┘
  ```
- [ ] T8.2.2 Output area: scrollable, word-wrap, color-code user/assistant/tool messages | Start: --- | End: ---
- [ ] T8.2.3 Input area: multi-line text editing (Enter to submit, Ctrl+D for newline) | Start: --- | End: ---
- [ ] T8.2.4 Status bar: current model, token count, session status | Start: --- | End: ---

### T8.3 Streaming display
- [ ] T8.3.1 Receive text chunks from agent loop via `mpsc::UnboundedReceiver<StreamEvent>` | Start: --- | End: ---
- [ ] T8.3.2 Append text incrementally to output buffer (no flicker) | Start: --- | End: ---
- [ ] T8.3.3 Show tool calls with spinner/different color while executing | Start: --- | End: ---
- [ ] T8.3.4 Show tool results when complete | Start: --- | End: ---

### T8.4 Input handling
- [ ] T8.4.1 Tab: cycle through available skills/commands (autocomplete) | Start: --- | End: ---
- [ ] T8.4.2 Up/Down: scroll output history | Start: --- | End: ---
- [ ] T8.4.3 Ctrl+C: abort current operation / quit | Start: --- | End: ---
- [ ] T8.4.4 Resize: recalculate layout | Start: --- | End: ---

### T8.5 Integration
- [ ] T8.5.1 `Tui::run()` takes agent loop, spawns TUI event loop + agent loop on separate tasks | Start: --- | End: ---
- [ ] T8.5.2 On user input submit → send to agent loop via mpsc channel | Start: --- | End: ---
- [ ] T8.5.3 On agent output → send to TUI via mpsc channel | Start: --- | End: ---
- [ ] T8.5.4 Both tasks run on single tokio runtime | Start: --- | End: ---

### T8.6 Tests
- [ ] T8.6.1 Unit test: layout calculation with different terminal sizes | Start: --- | End: ---
- [ ] T8.6.2 Unit test: stream event handling updates output buffer correctly | Start: --- | End: ---
- [ ] T8.6.3 Manual verification: test with real terminal at 80x24, 120x40, 200x60 | Start: --- | End: ---

---

## T9: Session Reset Tool

> Already implemented as tool in T4.5. This task covers the agent-loop side.

### T9.1 Reset sequence
- [ ] T9.1.1 When tool returns `HarnessError::SessionReset`: | Start: --- | End: ---
  1. Log reset event
  2. Fire SessionReset hooks
  3. Clear `MessageHistory` (drop all messages)
  4. Reload memory files from disk (`MemoryStore::reload()`)
  5. Reload skills from filesystem
  6. Rebuild system prompt with fresh memory + skills
  7. Add user's reset prompt as first user message
  8. Begin new turn loop
- [ ] T9.1.2 Both hemisphere instances reset together (when hemisphere mode active) | Start: --- | End: ---
- [ ] T9.1.3 Verify: TUI shows "Session reset" indicator briefly | Start: --- | End: ---

### T9.2 Tests
- [ ] T9.2.1 Integration: session reset clears all prior messages | Start: --- | End: ---
- [ ] T9.2.2 Integration: memory files survive reset (content preserved) | Start: --- | End: ---
- [ ] T9.2.3 Integration: new session uses reset prompt as first message | Start: --- | End: ---

---

## T10: Memory Files

### T10.1 Memory store
- [ ] T10.1.1 Create `src/memory/mod.rs` — `MemoryStore` struct: | Start: --- | End: ---
  ```rust
  pub struct MemoryStore {
      project_claude_md: Option<String>,
      project_memory_md: Option<String>,
      global_claude_md: Option<String>,
      global_memory_md: Option<String>,
  }
  ```
- [ ] T10.1.2 Implement `MemoryStore::load(project_root: &Path)`: | Start: --- | End: ---
  1. Read `<project_root>/CLAUDE.md` → project_claude_md
  2. Read `<project_root>/MEMORY.md` → project_memory_md (optional)
  3. Read `~/.claude/CLAUDE.md` → global_claude_md (optional)
  4. Read `~/.claude/MEMORY.md` → global_memory_md (optional)
  5. Missing files = None (not an error)
- [ ] T10.1.3 Implement `MemoryStore::reload()` — re-read all files from disk | Start: --- | End: ---
- [ ] T10.1.4 Implement `MemoryStore::to_system_prompt_fragment(&self) -> String` | Start: --- | End: ---
  - Format: `## Project Instructions\n\n{project_claude_md}\n\n## Memory\n\n{project_memory_md}`
  - Global files appended with `(global)` label

### T10.2 Write-back tool
- [ ] T10.2.1 Add memory write capability: model calls Write tool targeting `CLAUDE.md` or `MEMORY.md` | Start: --- | End: ---
- [ ] T10.2.2 No separate "update memory" tool — Write tool is sufficient per non-goals | Start: --- | End: ---

### T10.3 Tests
- [ ] T10.3.1 Unit test: loads all memory files when present | Start: --- | End: ---
- [ ] T10.3.2 Unit test: missing MEMORY.md = None (no error) | Start: --- | End: ---
- [ ] T10.3.3 Unit test: reload picks up disk changes | Start: --- | End: ---
- [ ] T10.3.4 Unit test: formatting for system prompt includes all non-empty files | Start: --- | End: ---

---

# Phase 3: Hemisphere Model

## T11: Dual-Agent System

### T11.1 Hemisphere config
- [ ] T11.1.1 Define `src/hemisphere/mod.rs` — config: | Start: --- | End: ---
  ```rust
  pub struct HemisphereConfig {
      pub enabled: bool,
      pub left_model: String,          // default: from settings
      pub right_model: Option<String>, // default: same as left
      pub right_context_window: usize, // compressed context size, default: 4000 tokens
      pub right_max_response: usize,   // response cap, default: 200 tokens
  }

  pub struct HemisphereState {
      pub left: AgentLoop,
      pub right: AgentLoop,
      pub config: HemisphereConfig,
  }
  ```

### T11.2 System prompts
- [ ] T11.2.1 Left system prompt: full agent instructions (standard, as T5) | Start: --- | End: ---
- [ ] T11.2.2 Right system prompt template: | Start: --- | End: ---
  ```
  You are a background advisor watching a coding session.
  You see a compressed summary of the conversation.
  Your role: detect issues, suggest alternatives, request clarification.
  Keep responses short (max 200 tokens). 
  To request clarification from the main agent, use the clarifier tool.
  Be concise. Only speak when you have something useful to add.
  ```

### T11.3 Right-side context compression
- [ ] T11.3.1 Implement `compress_for_right(history: &MessageHistory) -> String`: | Start: --- | End: ---
  1. Keep system prompt (abbreviated)
  2. Summarize conversation: keep last N turns verbatim, earlier turns as 1-line summaries
  3. Target: fit within `right_context_window` tokens
- [ ] T11.3.2 Format: `[Summary of earlier conversation]\n\n[Last 3 turns verbatim]` | Start: --- | End: ---

### T11.4 Right-side tool
- [ ] T11.4.1 Create `src/tools/clarifier.rs` — `ClarifierTool` | Start: --- | End: ---
- [ ] T11.4.2 Input schema: `{"question": string}` — question for the left agent | Start: --- | End: ---
- [ ] T11.4.3 Execute: injects user-level message into left agent's history: `"Right hemisphere asks: {question}"` | Start: --- | End: ---
- [ ] T11.4.4 Left agent processes clarification at next turn | Start: --- | End: ---

### T11.5 Hemisphere loop
- [ ] T11.5.1 After each left-agent turn: | Start: --- | End: ---
  1. Compress conversation for right
  2. Send compressed context + "Anything to add?" to right
  3. If right responds: check for clarifier tool call
  4. If right says nothing useful → continue
- [ ] T11.5.2 Right runs in background (non-blocking on left's next turn) | Start: --- | End: ---
- [ ] T11.5.3 Right responses appear in TUI with distinct color/prefix (`🌐 Right:` or similar) | Start: --- | End: ---

### T11.6 Session reset integration
- [ ] T11.6.1 Both hemispheres reset together on SessionReset tool call | Start: --- | End: ---
- [ ] T11.6.2 Both reload memory files independently | Start: --- | End: ---

### T11.7 Tests
- [ ] T11.7.1 Unit test: right context compression reduces token count | Start: --- | End: ---
- [ ] T11.7.2 Unit test: clarifier tool injects message into left history | Start: --- | End: ---
- [ ] T11.7.3 Integration test: right receives compressed context after left turn | Start: --- | End: ---
- [ ] T11.7.4 Integration test: both reset together | Start: --- | End: ---

---

# Phase 4: Advanced Features

## T12: Thinking Interleave

### T12.1 Strategy selection
- [ ] T12.1.1 PRIMARY PATH: Use DeepSeek v4 native `thinking` + `reasoning_content` field | Start: --- | End: ---
  - Enable thinking: `thinking: {type: "enabled"}`
  - Parse `reasoning_content` from streaming deltas
  - Store thinking blocks separately from visible content

- [ ] T12.1.2 FALLBACK / ENHANCEMENT: Parse `<think>...</think>` markers in content | Start: --- | End: ---
  - Handles models that embed thinking markers in text
  - Also handles the case where arbitrary models produce markers
  - Strips markers before displaying to user

### T12.2 Thinking store
- [ ] T12.2.1 Create `src/context/thinking.rs` — `ThinkingStore`: | Start: --- | End: ---
  ```rust
  pub struct ThinkingBlock {
      pub turn: usize,
      pub content: String,
      pub token_count: usize,
      pub relevance_score: f32,  // 1.0 = highly relevant
      pub timestamp: Instant,
  }

  pub struct ThinkingStore {
      blocks: Vec<ThinkingBlock>,
      decay_threshold: f32,
  }
  ```

### T12.3 Native thinking path
- [ ] T12.3.1 In agent loop, after receiving `reasoning_content` from API: | Start: --- | End: ---
  1. Extract reasoning_content from response deltas
  2. Create `ThinkingBlock` with relevance_score = 1.0
  3. Store in ThinkingStore
  4. Do NOT include reasoning_content in next turn's messages
  5. Log: "turn N: captured M tokens of native reasoning"

### T12.4 Marker-based fallback
- [ ] T12.4.1 Implement `parse_thinking_tags(content: &str) -> (String, Vec<ThinkingBlock>)`: | Start: --- | End: ---
  - Regex: `<think>(.*?)</think>` (multiline, dotall)
  - Returns: (visible_content_without_tags, extracted_thinking_blocks)
- [ ] T12.4.2 Apply to every assistant message content before display | Start: --- | End: ---
- [ ] T12.4.3 Parsing failure: treat entire content as visible (no crash) | Start: --- | End: ---

### T12.5 Relevance decay
- [ ] T12.5.1 Implement `ThinkingStore::decay_all()` — called each turn: | Start: --- | End: ---
  1. For each block: `relevance_score *= 0.85`
  2. Blocks with score < `decay_threshold` (default 0.2) → pruned
- [ ] T12.5.2 Implement heuristic scoring (initial implementation: simple decay only) | Start: --- | End: ---
- [ ] T12.5.3 Log: "pruned N thinking blocks, M remain" | Start: --- | End: ---

### T12.6 Display integration
- [ ] T12.6.1 Add toggle key in TUI (Ctrl+T) to show/hide thinking blocks | Start: --- | End: ---
- [ ] T12.6.2 Thinking panel: show recent thinking blocks with relevance scores | Start: --- | End: ---
- [ ] T12.6.3 Default: thinking hidden from main output | Start: --- | End: ---

### T12.7 Tests
- [ ] T12.7.1 Unit test: parse_thinking_tags extracts markers correctly | Start: --- | End: ---
- [ ] T12.7.2 Unit test: parse_thinking_tags handles malformed input | Start: --- | End: ---
- [ ] T12.7.3 Unit test: decay reduces scores, prunes below threshold | Start: --- | End: ---
- [ ] T12.7.4 Integration test: native reasoning_content captured and excluded from next turn | Start: --- | End: ---
- [ ] T12.7.5 Integration test: marker-based thinking stripped from display | Start: --- | End: ---

---

## T13: Dynamic Context Pruning

### T13.1 Message scoring
- [ ] T13.1.1 Create `src/context/pruning.rs` — `ContextPruner`: | Start: --- | End: ---
  ```rust
  pub struct ContextPruner {
      target_tokens: usize,     // default: 100_000
      messages: Vec<ScoredMessage>,
  }

  pub struct ScoredMessage {
      pub message: Message,
      pub relevance: f32,
      pub turn: usize,
      pub token_count: usize,
  }
  ```
- [ ] T13.1.2 Implement initial scoring heuristic: | Start: --- | End: ---
  - Most recent message: 1.0
  - Each turn older: `score *= 0.9`
  - System prompt: always 1.0 (never pruned)
  - Tool results with errors: +0.2 bonus
  - Messages containing file paths: +0.1 bonus (likely important references)
- [ ] T13.1.3 These are starting heuristics — flagged for tuning | Start: --- | End: ---

### T13.2 Pruning algorithm
- [ ] T13.2.1 Implement `ContextPruner::prune(&mut self)`: | Start: --- | End: ---
  ```
  1. Calculate total token count
  2. If total <= target_tokens: return (no pruning)
  3. Sort messages by relevance (lowest first)
  4. Remove lowest-scored messages until under target
  5. Never prune: system prompt, current turn
  6. Log pruned count and new total
  ```
- [ ] T13.2.2 Call pruner at end of each turn (before building next request) | Start: --- | End: ---
- [ ] T13.2.3 Log: `"pruned N messages (M tokens freed), context now K tokens"` | Start: --- | End: ---

### T13.3 Gradual forgetting
- [ ] T13.3.1 Each message older than 10 turns loses 0.05 relevance per turn (in addition to base decay) | Start: --- | End: ---
- [ ] T13.3.2 This creates "gradual forgetting" — old info fades, not suddenly dropped | Start: --- | End: ---
- [ ] T13.3.3 Messages below 0.1 relevance → eligible for complete removal | Start: --- | End: ---

### T13.4 Memory persistence
- [ ] T13.4.1 Memory files never pruned (injected fresh each turn in system prompt) | Start: --- | End: ---
- [ ] T13.4.2 Tool call → tool result pairs pruned together (atomically) | Start: --- | End: ---

### T13.5 Tests
- [ ] T13.5.1 Unit test: messages scored correctly by age | Start: --- | End: ---
- [ ] T13.5.2 Unit test: pruning removes lowest-scored first | Start: --- | End: ---
- [ ] T13.5.3 Unit test: system prompt never pruned | Start: --- | End: ---
- [ ] T13.5.4 Unit test: below-target context not pruned | Start: --- | End: ---
- [ ] T13.5.5 Unit test: tool call+result pruned as pair | Start: --- | End: ---
- [ ] T13.5.6 Integration test: 200-message conversation pruned to ~target tokens | Start: --- | End: ---

---

# Phase Dependencies

```
Phase 0 (T1-T3)
    │
    ▼
Phase 1 (T4-T7)  ← requires T1-T3
    │
    ▼
Phase 2 (T8-T10) ← requires T4-T7
    │
    ├──────────────┐
    ▼              ▼
Phase 3 (T11)   Phase 4 (T12-T13)
                  ▲
                  │
          Phase 3 (T11) ← T12 hemisphere thinking integration
```

Parallel work possible: Phase 3 and Phase 4 can start simultaneously after Phase 2.

---

# Milestones

| Milestone | Contents | Verification |
|-----------|----------|-------------|
| **M0: Ping** | T1-T2 | `cargo run` → successfully calls DeepSeek API with "hello" → prints response |
| **M1: Agent** | T3-T5 | Full turn loop works: user input → API call → tool execution → response → repeat. CLI mode. |
| **M2: Harness** | T6-T8 | Skills, hooks, memory files loaded. TUI renders. Piggybacking on CC files works. |
| **M3: Reset** | T9-T10 | Session reset works from tool call. Memory files survive reset. |
| **M4: Dual** | T11 | Two hemispheres running. Right sees compressed context. Clarifier tool works. |
| **M5: Smart** | T12-T13 | Thinking interleave active. Context prunes to target. |
| **M6: Polish** | All | All tests pass. No stubs. Docs updated. Demo-ready. |

---

# Open Design Decisions (resolve during implementation)

1. **OpenAI vs Anthropic API format:** Start with OpenAI format (simpler SDK, broader compatibility). Revisit Anthropic format only if content block streaming benefits prove necessary for thinking interleave.

2. **Right hemisphere compression algorithm:** Start with simple truncation + summarization. Experiment with different compression ratios after M4.

3. **Relevance scoring weights:** Tune 0.85 decay / 0.2 threshold / 0.9 age multiplier during M5 based on observed behavior.

4. **Thinking display UX:** Start with hidden-by-default + toggle. Gather feedback on whether inline or side-panel is better.

5. **Tool expansion beyond T4 set:** Add tools on demand. Each new tool is a T4-equivalent PR. Candidates: Grep, Glob, Edit, WebFetch.
