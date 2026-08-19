## Context

See proposal.md for motivation and for the two defects that hide each other. What matters for the approach is what already exists and what shape the outside world actually writes.

`HookRunner::run` already loops a hook list, spawns each command, writes an event as JSON to its stdin, reads a `HookResult` back, and returns whether execution may proceed. Its logic is close to right. What it lacks is a caller, the correct wire format, and three fixes named below.

`HookResult` already parses `approved`, `modified_input`, and `message`. `run` uses two of the three and drops `modified_input` on the floor.

The real Claude Code shape was read off `~/.claude/settings.json` on this machine rather than assumed:

```json
"PreToolUse": [
  { "matcher": "", "hooks": [ { "type": "command", "command": "python \"$HOME/.claude/hooks/force-sync-agents.py\"", "timeout": 5 } ] }
]
```

The real payload shape was observed the same way. A `SessionStart` hook running in a live Claude Code session on this machine received:

```json
{"session_id":"...","transcript_path":"...jsonl","cwd":"C:\\Users\\siriu\\RustroverProjects\\DeepSeekCustom","hook_event_name":"SessionStart","source":"clear"}
```

That is a direct observation, not a reading of documentation. It is what makes the field-name decision below a fact rather than a guess.

## Goals / Non-Goals

**Goals:**

- A hook script written for Claude Code runs under this harness unchanged, reading the same stdin fields.
- One unreadable block in a settings file costs only that block.
- A hook cannot take a turn down, and cannot leave a process behind.

**Non-Goals:**

- Running the user's globally declared hooks by default. That stays opt-in, and the opt-in is the whole safety story for a feature that executes arbitrary commands.
- Reproducing Claude Code's transcript format. The `transcript_path` field is supplied and points at this harness's own session file, whose format differs. A hook that parses the transcript is out of scope, and the spec asks for no such thing.
- Hooks on the `ClaudeCli` path. That backend runs its own hooks inside its child process and always did.
- Matcher regular expressions. A matcher is compared as an exact tool name, with an empty matcher and `*` both meaning every tool. See Open Questions.

## Decisions

### The payload uses Claude Code's field names

The stdin object becomes `hook_event_name`, `tool_name`, `tool_input`, `session_id`, `cwd`, and `transcript_path`, replacing today's `event`, `tool`, and `input`.

This is the decision that makes the whole feature worth having. A hook written for Claude Code reads `tool_name`. Fed today's payload it reads nothing, finds no field, and either crashes or silently does nothing. Since the point of the format is that one script serves both harnesses, the field names are not an implementation detail. They are the contract.

`HookEvent`'s serde attributes change accordingly: the tag becomes `hook_event_name`, and the variant names stop being lowercased, since Claude Code writes `PreToolUse` rather than `pretooluse`.

Alternative considered: keep the harness's own field names and document the difference. Rejected because it makes every existing hook script useless here, which leaves the feature with no scripts to run.

### Settings parse block by block, not whole file

`load_file` stops deserializing the file straight into `Settings`. It parses the file into a generic JSON value first, then deserializes each known block independently. A block that fails is reported and skipped. Everything else in the file takes effect.

This is what the "one bad block does not void its other settings" requirement needs, and it is a general fix rather than a hooks-shaped one. Today's failure mode is not "hooks are ignored", it is "the file vanishes", and a change that only taught `HooksConfig` the new shape would leave the next unreadable block free to do the same thing again.

Alternative considered: give every block a custom deserializer that swallows its own errors. Rejected because it spreads the same logic across every block type and gets forgotten on the next one added. Parsing once at the file level handles blocks that do not exist yet.

### Hook resolution is a merge of two sources with different defaults

Hooks resolve from the project settings file always, and from the global Claude Code settings only when a new `hooks.run_global` setting is on. The setting defaults to off.

This deliberately breaks the field-by-field override rule the rest of the settings chain follows. Under that rule a global hooks block would replace a project one. For hooks that is wrong twice over: it would let a global file silence a project's own hooks, and it would run commands the repository owner never chose. So hooks concatenate rather than override, and the global source is gated.

### A hook decision is a value, not a boolean

`HookRunner::run` returns a decision type carrying whether to proceed and an optional replacement input, rather than a bare `bool`. That is the smallest change that lets `modified_input` reach a caller that can act on it.

A `PreToolUse` hook's replacement input is validated by the tool's own deserialization, which already runs on every call. An input the tool rejects falls back to the original arguments rather than failing the call, since a hook breaking every call it touches is worse than a hook whose modification is ignored.

### Exit code 2 blocks, every other non-zero does not

Claude Code treats exit code 2 as a block. Today `run_one` treats every non-zero exit as approval, so a hook that crashes looks exactly like a hook that said yes.

Splitting the two matters because both readings are wrong in one direction. Treating every non-zero exit as a block would let a broken hook wedge the agent. Treating code 2 as approval would silently ignore the one signal Claude Code hooks actually use to say no.

### Three fixes to `run_one`, all from failures already recorded in this repository

1. The child joins the Windows job object through `process_group::adopt`. Every other spawn path in this harness does. CLAUDE.md records what skipping it cost: a real orphan ran for two and a half hours, spawned 33 processes, and rewrote a repository. `kill_on_drop` alone does not fix that, and `run_one` sets only `kill_on_drop` today.
2. The command line stops using plainly quoted arguments to `cmd /C`. CLAUDE.md records this failure too, on a quoted `node.exe` path under `C:\Program Files`, which is precisely the shape of a hook command. The fix is the `raw_arg` form and the outer quote pair the Bash tool already uses.
3. A hook's `shell` field is honored, since the real config uses it: one hook on this machine declares `"shell": "bash"`. A hook declaring no shell keeps today's `cmd` path on Windows.

### Where each event fires

| Event | Call site |
|---|---|
| `UserPromptSubmit` | `AgentLoop::run_with_image`, before `run_turn` |
| `PreToolUse` | `AgentLoop::execute_tool`, before `tool.execute` |
| `PostToolUse` | `AgentLoop::execute_tool`, after a tool returns without error |
| `PostToolUseFailure` | `AgentLoop::execute_tool`, after a tool returns an error |
| `Stop` | `AgentLoop::run_with_image`, after `run_turn` returns |
| `SessionReset` | the `SessionReset` branch of `AgentLoop::execute_tool` |
| `SessionStart` | `main.rs`, once the backend is built |
| `SessionEnd` | `main.rs`, on shutdown |

`execute_tool` takes `&self`, so nothing here needs it to become `&mut self`. A replacement input is a local value passed to `tool.execute`.

`Stop` and the hemisphere advisor pass both attach to `run_with_image`. If both changes land, the order is: `run_turn`, then the advisor pass, then `Stop`, so a `Stop` hook sees a turn that is genuinely finished.

## Risks / Trade-offs

**A hook is an arbitrary shell command that this change lets block and rewrite tool calls.** -> Global hooks are off by default. The default source is the project's own `settings.json`, a file the repository owner controls and git tracks. Nothing here reads a hook from a location the owner does not already control.

**A rewritten tool call is hard to see.** A hook that quietly changes a `write` path changes what happened without changing what the model asked for. -> Every applied modification is reported where the user can see it, and a modification the tool rejects falls back to the original. Making the rewrite visible is the mitigation; refusing rewrites entirely was considered and rejected, since `modified_input` is the field that makes a `PreToolUse` hook more than a logger.

**Eight call sites is eight chances to fire a hook at the wrong moment.** -> Each one gets a test asserting it fires once, and the table above is the single record of where each belongs.

**A per-tool-call hook runs a process per tool call.** On a turn with many tool calls that is many spawns. -> A hook with a matcher runs only for the tools it names, and no hook declared means no spawn at all. The cost is opt-in per event and per tool.

**Changing the payload field names breaks any hook written against the current shape.** -> No such hook can exist. The runner has never had a caller, so no hook has ever received the old payload.

## Migration Plan

The old flat `HooksConfig` shape stops being read. No file on this machine declares hooks in it, and no test asserts it beyond two `hooks: None` struct literals, so there is nothing to migrate. A file still using the old shape now reports an unreadable hooks block rather than voiding the file, which is strictly better than today.

The first task is a test, not a change: a `settings.json` carrying a real Claude Code hooks block must fail to load before this change and load after. That test is the proof the defect is real, and it must be seen failing first.

Rollback is declaring no hooks, which is the state of every settings file in the repository today. The settings-loading fix has no rollback and needs none: reporting a parse error instead of hiding it cannot break a file that already parses.

## Open Questions

- **Should a matcher support a regular expression?** Claude Code's own matchers on this machine are `""` and `"*"` only, so exact-name matching with those two wildcards covers every real case observed. Adding regular expressions later widens what a matcher accepts without changing any requirement in the spec.
- **Should `SessionEnd` be guaranteed to run?** A hook on shutdown cannot fire if the process is killed outright, and this harness deliberately kills children on exit. The spec asks only that the event be declarable and fire on an ordinary shutdown. Whether to attempt more is a separate question about shutdown, not about hooks.
- **Should a blocked tool call count against the turn's tool budget?** Either reading is defensible, and nothing in the spec depends on it. Worth deciding when the block path is built and its behavior can be observed.
