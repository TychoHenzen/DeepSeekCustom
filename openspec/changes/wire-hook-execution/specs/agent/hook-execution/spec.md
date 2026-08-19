## Purpose

Runs the project's own hook commands at named points in a turn, so a repository can observe and gate what the agent does without changing the harness. Hook commands are written in the format Claude Code uses, so a script written for one harness runs under the other unchanged.

## ADDED Requirements

### Requirement: Hooks are declared in the format Claude Code writes

A hooks declaration SHALL be read in the shape Claude Code writes: a map from event name to a list of matcher groups, where each group carries a matcher pattern and a list of command hooks, and each command hook carries a command string, an optional timeout, and an optional shell.

A hook whose matcher does not match the event's subject SHALL NOT run. An empty matcher SHALL match every subject.

#### Scenario: A real Claude Code hooks block loads

- **WHEN** a settings file declares hooks in the nested matcher-and-hooks shape Claude Code writes
- **THEN** the file loads, every declared hook is available to run, and its timeout is carried through

#### Scenario: A matcher selects which tool calls fire a hook

- **WHEN** a `PreToolUse` hook declares a matcher naming one tool, and the agent calls a different tool
- **THEN** that hook does not run

#### Scenario: An empty matcher matches everything

- **WHEN** a hook declares an empty matcher and any qualifying event fires
- **THEN** that hook runs

### Requirement: Project hooks run, global hooks are opt-in

Hooks declared in the project's own settings file SHALL run. Hooks declared in the user's global Claude Code settings SHALL NOT run unless a setting explicitly enables them, and that setting SHALL default to off.

A hook is an arbitrary shell command. A globally declared hook is written against another harness's events and tools, so running one unasked would execute a command the repository owner never chose.

#### Scenario: A project hook runs by default

- **WHEN** the project settings file declares a hook for an event, and that event fires
- **THEN** the hook runs

#### Scenario: A global hook stays silent by default

- **WHEN** only the user's global settings declare a hook for an event, the opt-in setting is off, and that event fires
- **THEN** no hook runs

#### Scenario: The opt-in reaches global hooks

- **WHEN** the opt-in setting is on and both the project and the global settings declare a hook for one event
- **THEN** both hooks run

### Requirement: A hook receives the event on stdin in Claude Code's field names

A hook SHALL receive one JSON object on stdin. That object SHALL name the event in a `hook_event_name` field, and SHALL carry the session identifier and the current working directory.

For a tool event, the object SHALL name the tool in a `tool_name` field and its arguments in a `tool_input` field. For a tool result event, it SHALL also carry the tool's output and whether that output was an error.

The field names SHALL match the ones Claude Code writes, so a hook script written for Claude Code reads them without modification.

#### Scenario: A tool event names the tool the way a Claude Code hook expects

- **WHEN** a `PreToolUse` hook runs for a tool call
- **THEN** its stdin holds a JSON object whose `hook_event_name` is `PreToolUse`, whose `tool_name` is the tool's name, and whose `tool_input` is that call's arguments

#### Scenario: Every event carries its session context

- **WHEN** any hook runs
- **THEN** its stdin object carries the session identifier and the working directory the tools are acting in

### Requirement: Hooks fire at named points in a turn

A hook SHALL be able to fire at each of these points: before a tool call, after a tool call succeeds, after a tool call returns an error, when a user turn is submitted, when a turn finishes, when a session starts, when a session ends, and when a session is reset.

Each of those points SHALL be declarable independently, so enabling one does not enable another.

#### Scenario: A tool call fires before and after

- **WHEN** hooks are declared for both the before-tool-call and after-tool-call events, and the agent calls a tool that succeeds
- **THEN** the before hook runs before the tool executes, and the after hook runs once the tool has returned

#### Scenario: A failing tool call fires the failure event

- **WHEN** a hook is declared for the tool-failure event and a tool returns an error
- **THEN** that hook runs and receives the error output

#### Scenario: A session reset is declarable

- **WHEN** a hook is declared for the session-reset event and the agent resets its session
- **THEN** that hook runs

### Requirement: A hook can block the call it observes

A hook that runs before a tool call SHALL be able to stop that call. It SHALL be able to do so by returning a reply that withholds approval, or by exiting with status code 2.

A blocked tool call SHALL NOT execute. The agent SHALL be told the call was blocked, and by what, so it can choose a different action rather than retrying the same one.

#### Scenario: A withheld approval stops the tool

- **WHEN** a before-tool-call hook returns a reply withholding approval
- **THEN** the tool does not execute, and the agent receives a result saying the call was blocked

#### Scenario: Exit code 2 stops the tool

- **WHEN** a before-tool-call hook exits with status code 2
- **THEN** the tool does not execute, and the agent receives a result saying the call was blocked

#### Scenario: Any other non-zero exit does not stop the tool

- **WHEN** a before-tool-call hook exits with a non-zero status other than 2
- **THEN** the tool executes, and the failure is reported rather than treated as a block

### Requirement: A hook can rewrite a tool call's arguments

A hook that runs before a tool call SHALL be able to replace that call's arguments by returning a modified input. The tool SHALL then execute against the replacement rather than the original.

A returned modified input that is not valid for the tool SHALL be rejected, the original arguments SHALL be used, and the rejection SHALL be reported.

#### Scenario: Modified arguments reach the tool

- **WHEN** a before-tool-call hook returns a modified input for the call
- **THEN** the tool executes against the modified input, not the original

#### Scenario: An unusable modification is refused

- **WHEN** a before-tool-call hook returns a modified input the tool cannot accept
- **THEN** the tool executes against the original arguments and the refusal is reported

### Requirement: A failing hook never takes the turn down

A hook that cannot be spawned, exits non-zero for any reason other than a block, times out, or returns output that is not valid JSON SHALL NOT fail the turn. Each such failure SHALL be reported where a user can see it, and execution SHALL proceed as though that hook had approved.

A hook SHALL be bounded by its declared timeout, and by a default timeout when it declares none.

#### Scenario: A missing hook command is contained

- **WHEN** a declared hook names a command that does not exist
- **THEN** the failure is reported, the turn continues, and the tool call proceeds

#### Scenario: A hanging hook is cut off

- **WHEN** a hook does not exit within its timeout
- **THEN** it is terminated, the timeout is reported, and the turn continues

#### Scenario: Unparseable hook output is contained

- **WHEN** a hook exits zero but writes output that is not valid JSON
- **THEN** the invalid output is reported, and the call proceeds as though the hook had approved

### Requirement: A hook's child process cannot outlive the harness

Every process a hook spawns SHALL be tied to the harness's own lifetime, by the same mechanism every other child process this harness spawns is tied to it. When the harness ends, however it ends, a hook's process and any subtree it started SHALL be terminated.

#### Scenario: A hook subprocess dies with the harness

- **WHEN** a hook has spawned a long-running child and the harness process ends
- **THEN** that child is terminated rather than left running

### Requirement: Hooks apply to the in-process backend only

Hooks SHALL run only for the backend whose turn loop this harness owns. A backend that runs its own turn loop in a child process runs its own hooks, and this harness SHALL NOT run hooks for it.

#### Scenario: The child-process backend runs no harness hooks

- **WHEN** a hook is declared and the active backend runs its turn loop in a child process
- **THEN** this harness runs no hook for that backend's turns
