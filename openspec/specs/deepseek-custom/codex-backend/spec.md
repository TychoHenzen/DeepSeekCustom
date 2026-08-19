# deepseek-custom/codex-backend Specification

## Purpose
Drives OpenAI models through the Codex CLI's non-interactive exec mode, giving the harness a third subprocess backend alongside `ClaudeCli`.
## Requirements
### Requirement: A settings.json entry with kind "codex_cli" produces a Codex backend

The harness SHALL accept a `BackendConfig` variant with `kind: "codex_cli"`. The entry SHALL carry a `model` field (required) and optional fields `sandbox`, `env`, and `models`. An unknown `kind` value SHALL remain a hard startup error naming both the requested entry and the entries that exist.

#### Scenario: A codex_cli entry builds and runs

- **WHEN** `settings.json` contains a backend entry `{ "kind": "codex_cli", "model": "o3" }` and that entry is selected as `default_backend`
- **THEN** the harness starts with a `CodexCli` backend variant and the status bar reads `Backend: codex (o3)`

#### Scenario: An explicit sandbox value reaches the child

- **WHEN** a `codex_cli` entry sets `sandbox: "read-only"`
- **THEN** the spawned child receives `--sandbox read-only` in its argument list

### Requirement: The backend spawns codex exec with JSONL output

The harness SHALL spawn `codex exec --json` as a child process for each turn. The child SHALL receive `--dangerously-bypass-approvals-and-sandbox` unless a `sandbox` override is set, since the harness has no interactive approval prompt. The child SHALL join the Windows job object through `process_group::adopt`, so closing the harness kills every Codex child the same way it kills `claude -p` children.

#### Scenario: A single turn spawns one child and collects its reply

- **WHEN** the user sends "hello" on a `codex_cli` backend
- **THEN** the harness spawns `codex exec --json` with the prompt as a positional argument, reads JSONL from stdout until `turn.completed` or `turn.failed`, and delivers the agent's text as `StreamEvent::Text`

#### Scenario: The child joins the job object

- **WHEN** a `codex_cli` backend spawns a child
- **THEN** `process_group::adopt` is called on the child before the first read, so the kernel terminates it if the harness exits

### Requirement: JSONL events map to the existing StreamEvent enum

The harness SHALL parse each stdout line as a JSON object with a `type` field and map it to `StreamEvent` values. The mapping SHALL cover these Codex event types:

- `thread.started`: capture `thread_id` for resume, emit no `StreamEvent`.
- `turn.started`: emit no `StreamEvent`.
- `item.started` / `item.updated` / `item.completed` with item type `agent_message`: emit `StreamEvent::Text` with the message text.
- `item.started` / `item.completed` with item type `reasoning`: emit `StreamEvent::Reasoning` with the reasoning text.
- `item.started` / `item.completed` with item type `command_execution`: emit `StreamEvent::ToolCallStart` at start and `StreamEvent::ToolCallEnd` at completion, with the command as the tool name and the aggregated output as the result.
- `item.started` / `item.completed` with item type `file_change`: emit `StreamEvent::ToolCallStart` at start and `StreamEvent::ToolCallEnd` at completion.
- `item.started` / `item.completed` with item type `mcp_tool_call`: emit `StreamEvent::ToolCallStart` at start and `StreamEvent::ToolCallEnd` at completion, with the MCP server and tool name joined.
- `turn.completed`: emit `StreamEvent::TurnEnd` with token counts from the `usage` field.
- `turn.failed`: emit `StreamEvent::Error` with the error message.

A line that does not parse as JSON, or that carries an unrecognised `type`, SHALL be skipped with a `warn` log, never a hard failure.

#### Scenario: An agent_message item becomes Text

- **WHEN** the JSONL stream contains an `item.completed` with item type `agent_message` and text "Hello, world!"
- **THEN** the transcript shows "Hello, world!" as assistant text

#### Scenario: A command_execution item becomes a tool call pair

- **WHEN** the JSONL stream contains an `item.started` with item type `command_execution` and command `ls -la`, followed by an `item.completed` with exit code 0 and aggregated output "total 42\n..."
- **THEN** the transcript shows a tool call block with name "command_execution", arguments containing the command, and output containing the aggregated result

#### Scenario: A turn.failed event becomes an Error

- **WHEN** the JSONL stream contains a `turn.failed` event with error message "rate limit exceeded"
- **THEN** `StreamEvent::Error` is emitted with that message and the turn ends

#### Scenario: Malformed lines are skipped

- **WHEN** the JSONL stream contains a line that is not valid JSON
- **THEN** the harness logs a warning and continues reading the next line

### Requirement: Session resume uses the thread_id from thread.started

The harness SHALL capture the `thread_id` from the `thread.started` JSONL event. On a subsequent turn in the same conversation, the harness SHALL pass `codex exec resume <thread_id>` instead of a fresh `codex exec`, so the Codex CLI restores its own conversation history.

#### Scenario: A second turn resumes the first turn's session

- **WHEN** the user sends a second message on the same conversation
- **THEN** the harness spawns `codex exec resume <thread_id>` using the id captured from the first turn's `thread.started` event

#### Scenario: A session reset clears the thread_id

- **WHEN** the user resets the session
- **THEN** the next turn spawns a fresh `codex exec` with no resume argument

### Requirement: Effort maps to Codex reasoning_effort values

The harness's `Effort` enum SHALL map to Codex values through a config override flag. The mapping:

| Effort | Codex value |
|---|---|
| None | flag omitted |
| Low | `low` |
| Medium | `medium` |
| High | `high` |
| Max | `max` |

The value SHALL be passed as `-c reasoning.effort=<value>` on the spawn command.

#### Scenario: Effort::High passes the correct config override

- **WHEN** the effort control is set to High
- **THEN** the spawned child receives `-c reasoning.effort=high` in its arguments

#### Scenario: Effort::None omits the flag

- **WHEN** the effort control is set to None
- **THEN** the spawned child receives no `-c reasoning.effort=...` argument

### Requirement: Interrupt kills the child process

The harness SHALL kill the Codex child when the user presses Escape, the same way it kills a `claude -p` child. The next turn SHALL spawn a fresh child.

#### Scenario: Escape during a running turn kills the child

- **WHEN** the user presses Escape while a Codex turn is streaming
- **THEN** the child process is killed, `StreamEvent::Interrupted` is emitted, and the next turn spawns a fresh child

### Requirement: Working directory reaches the child

The harness SHALL spawn the Codex child with its working directory set to the shared `working_dir` value, through `Command::current_dir`. The `-C` flag SHALL NOT be used, since `current_dir` already sets the working directory for the child process.

#### Scenario: A cd changes where the next Codex turn runs

- **WHEN** the user runs `cd /tmp` and then sends a prompt on a `codex_cli` backend
- **THEN** the spawned child runs with `/tmp` as its working directory

### Requirement: Model discovery queries the Codex CLI or uses an explicit list

When no explicit `models` array is set on the backend entry, `list_models` SHALL return a static fallback list of known OpenAI models (`o3`, `o4-mini`). When an explicit `models` array is set, that array SHALL be used as-is.

#### Scenario: No explicit models returns the static fallback

- **WHEN** a `codex_cli` entry has no `models` field
- **THEN** `list_models` returns at least `o3` and `o4-mini`

#### Scenario: An explicit models array overrides discovery

- **WHEN** a `codex_cli` entry sets `models: ["gpt-4.1", "o3"]`
- **THEN** `list_models` returns exactly `["gpt-4.1", "o3"]`

### Requirement: Autopilot repeat works on the CodexCli backend

The `CodexCli` backend SHALL implement the `RepeatTarget` trait, the same way `ClaudeCliDriver` does. Each iteration SHALL start a fresh `codex exec` with no resume, so no iteration sees an earlier iteration's conversation.

#### Scenario: An autopilot run of 3 iterations completes

- **WHEN** the user starts an autopilot run with 3 iterations on a `codex_cli` backend
- **THEN** the harness runs 3 separate `codex exec` invocations, each with a fresh prompt and no `resume` argument, and reports completion

### Requirement: Subagent dispatch works on the CodexCli backend

The `Task` tool SHALL accept `"codex"` (or whatever the entry is named) as a `backend` value and dispatch a subagent onto it. A `keep_open: false` dispatch SHALL run `codex exec` once and return the reply text. A `keep_open: true` dispatch SHALL keep the child's `thread_id` so `SendMessage` can resume it.

#### Scenario: A one-shot subagent dispatch returns the reply

- **WHEN** the `Task` tool dispatches onto a `codex_cli` backend with `keep_open: false`
- **THEN** the subagent runs one `codex exec` turn, returns the agent_message text, and the child exits

#### Scenario: A kept-open session accepts a follow-up via SendMessage

- **WHEN** the `Task` tool dispatches onto a `codex_cli` backend with `keep_open: true`, then `SendMessage` sends a follow-up
- **THEN** the follow-up spawns `codex exec resume <thread_id>` and returns the new reply text

