## 1. Config and types

- [x] 1.1 Add `CodexCli` variant to `BackendConfig` in `crates/deepseek-custom/src/config/settings.rs` with fields `model`, `sandbox`, `env`, and `models`. Extend `BackendConfig::model()` to cover it.
<!-- covers: deepseek-custom/codex-backend :: A settings.json entry with kind "codex_cli" produces a Codex backend :: A codex_cli entry builds and runs -->
<!-- status: completed -->

- [x] 1.2 Add `Effort` mapping for Codex in `crates/deepseek-custom/src/effort.rs`: a `codex_cli_effort(&self) -> Option<String>` method returning the `-c reasoning.effort=<value>` string, `None` for `Effort::None`.
<!-- covers: deepseek-custom/codex-backend :: Effort maps to Codex reasoning_effort values :: Effort::High passes the correct config override -->
<!-- covers: deepseek-custom/codex-backend :: Effort maps to Codex reasoning_effort values :: Effort::None omits the flag -->
<!-- status: completed -->

## 2. JSONL event parsing

- [x] 2.1 Create `crates/deepseek-custom/src/backend/codex_cli/events.rs`: define Rust types for Codex JSONL events (`ThreadStarted`, `TurnStarted`, `ItemStarted`, `ItemUpdated`, `ItemCompleted`, `TurnCompleted`, `TurnFailed`) and a `parse_event(line: &str) -> Option<CodexEvent>` function that returns `None` with a `warn` log for unparseable lines.
<!-- covers: deepseek-custom/codex-backend :: JSONL events map to the existing StreamEvent enum :: Malformed lines are skipped -->
<!-- status: completed -->

- [x] 2.2 Create `crates/deepseek-custom/src/backend/codex_cli/map.rs`: an `EventMapper` that maps `CodexEvent` values to `StreamEvent` values. Map `agent_message` to `Text`, `reasoning` to `Reasoning`, `command_execution`/`file_change`/`mcp_tool_call` to `ToolCallStart`/`ToolCallEnd`, `turn.completed` to `TurnEnd`, and `turn.failed` to `Error`.
<!-- covers: deepseek-custom/codex-backend :: JSONL events map to the existing StreamEvent enum :: An agent_message item becomes Text -->
<!-- covers: deepseek-custom/codex-backend :: JSONL events map to the existing StreamEvent enum :: A command_execution item becomes a tool call pair -->
<!-- covers: deepseek-custom/codex-backend :: JSONL events map to the existing StreamEvent enum :: A turn.failed event becomes an Error -->
<!-- status: completed -->

## 3. Process management

- [x] 3.1 Create `crates/deepseek-custom/src/backend/codex_cli/spawn.rs`: build the argument list for `codex exec --json`. Handle `--dangerously-bypass-approvals-and-sandbox` vs `--sandbox <value>`, model via `-m`, effort via `-c reasoning.effort=<value>`, prompt as positional arg, and the `resume <thread_id>` subcommand form. Resolve `codex` on PATH using the same `PATHEXT` logic `mcp/spawn.rs` uses. Call `process_group::adopt` on the child.
<!-- covers: deepseek-custom/codex-backend :: The backend spawns codex exec with JSONL output :: A single turn spawns one child and collects its reply -->
<!-- covers: deepseek-custom/codex-backend :: The backend spawns codex exec with JSONL output :: The child joins the job object -->
<!-- covers: deepseek-custom/codex-backend :: A settings.json entry with kind "codex_cli" produces a Codex backend :: An explicit sandbox value reaches the child -->
<!-- status: completed -->

- [x] 3.2 Create `crates/deepseek-custom/src/backend/codex_cli/mod.rs`: the `CodexCliDriver` struct holding `thread_id: Option<String>`, `working_dir: Arc<Mutex<PathBuf>>`, `tx_events: UnboundedSender<RoutedEvent>`, effort/interrupt/voice_mode/model flags, and the six `SharedFlags` adoption methods. Implement `send` and `send_with_image` (image is dropped with a transcript notice, matching the DeepSeek path). Each `send` spawns a child, reads its stdout line by line, maps events, captures `thread_id`, and waits for `turn.completed` or `turn.failed`.
<!-- covers: deepseek-custom/codex-backend :: Session resume uses the thread_id from thread.started :: A second turn resumes the first turn's session -->
<!-- covers: deepseek-custom/codex-backend :: Session resume uses the thread_id from thread.started :: A session reset clears the thread_id -->
<!-- covers: deepseek-custom/codex-backend :: Working directory reaches the child :: A cd changes where the next Codex turn runs -->
<!-- covers: deepseek-custom/codex-backend :: Interrupt kills the child process :: Escape during a running turn kills the child -->
<!-- status: completed -->

- [x] 3.3 Create `crates/deepseek-custom/src/backend/codex_cli/repeat.rs`: implement `RepeatTarget` for `CodexCliDriver`. Reset clears `thread_id` so each iteration starts fresh.
<!-- covers: deepseek-custom/codex-backend :: Autopilot repeat works on the CodexCli backend :: An autopilot run of 3 iterations completes -->
<!-- status: completed -->

## 4. Backend wiring

- [x] 4.1 Add `CodexCli(Box<CodexCliDriver>)` variant to `Backend` enum in `crates/deepseek-custom/src/backend/mod.rs`. Add arms to `run_with_image`, `run_repeat`, `adopt_flags`, `shutdown`, `interrupt_flag`, `effort_flag`, `voice_mode_flag`, `context_budget_flag`, `model_flag`, and `repeat_interrupt_flag`.
<!-- status: completed -->

- [x] 4.2 Add the `CodexCli` build path in `crates/deepseek-custom/src/backend/factory.rs`: `BackendFactory::build` matches on `BackendConfig::CodexCli` and constructs a `CodexCliDriver`, the same way it constructs a `ClaudeCliDriver` for the `ClaudeCli` variant.
<!-- status: completed -->

- [x] 4.3 Add `CodexCli` to `resolve_named_backend` in `crates/deepseek-custom/src/backend/resolved.rs` so the `Task` tool can dispatch onto a `codex_cli` entry.
<!-- covers: deepseek-custom/codex-backend :: Subagent dispatch works on the CodexCli backend :: A one-shot subagent dispatch returns the reply -->
<!-- covers: deepseek-custom/codex-backend :: Subagent dispatch works on the CodexCli backend :: A kept-open session accepts a follow-up via SendMessage -->
<!-- status: completed -->

- [x] 4.4 Add model discovery for `CodexCli` in `crates/deepseek-custom/src/api/models.rs`: return the explicit `models` array if set, otherwise a static fallback of `["o3", "o4-mini"]`.
<!-- covers: deepseek-custom/codex-backend :: Model discovery queries the Codex CLI or uses an explicit list :: No explicit models returns the static fallback -->
<!-- covers: deepseek-custom/codex-backend :: Model discovery queries the Codex CLI or uses an explicit list :: An explicit models array overrides discovery -->
<!-- status: completed -->

## 5. Tests

- [x] 5.1 Create `crates/deepseek-custom-tests/tests/it/backend_codex_cli_events.rs`: tests for `parse_event` covering every event type, malformed lines, and unknown types. Add `mod backend_codex_cli_events;` to `main.rs`.
<!-- status: completed -->

- [x] 5.2 Create `crates/deepseek-custom-tests/tests/it/backend_codex_cli_map.rs`: tests for `EventMapper` covering every `CodexEvent`-to-`StreamEvent` mapping. Add `mod backend_codex_cli_map;` to `main.rs`.
<!-- status: completed -->

- [x] 5.3 Create `crates/deepseek-custom-tests/tests/it/backend_codex_cli_spawn.rs` (or add to `backend_codex_cli_process.rs`): tests for argument assembly covering sandbox modes, effort levels, model override, resume vs fresh, and working directory.
<!-- status: completed -->

- [x] 5.4 Add `CodexCli` variant coverage to existing tests in `backend_factory.rs` (build path, depth gating) and `config_settings.rs` (deserialization of a `codex_cli` entry).
<!-- status: completed -->

- [ ] 5.5 Create `crates/deepseek-custom-tests/src/bin/fake_codex.rs`: a fake Codex binary that emits canned JSONL events (thread.started, turn.started, item.completed with agent_message, turn.completed) and supports the `resume` subcommand. Use it in a lifecycle test covering a two-turn conversation and an interrupt.

## 6. Documentation

- [ ] 6.1 Update CLAUDE.md: add the `CodexCli` variant to the Backend, Config, and Effort sections. Add the new module paths to the Architecture section and the test table.
