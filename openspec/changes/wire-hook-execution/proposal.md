## Why

The hooks subsystem does not work, and it fails in a way that hides itself.

`crates/deepseek-custom/src/hooks/mod.rs` holds 150 lines that can run a hook and interpret its reply. `settings.json` can declare hook lists. Nothing connects the two: `HookRunner::run` has no caller in production, and `Settings.hooks` is read in exactly two places, a debug log at `config/settings.rs:147` that prints whether it is set, and the merge at `465-466`. No turn has ever run a hook.

The shape the harness expects is also not the shape Claude Code writes, which matters because piggybacking on Claude Code's files is this project's stated premise. Claude Code writes each entry as `{"matcher": "...", "hooks": [{"type": "command", "command": "...", "timeout": 10}]}`. `HookDef` requires a top-level `command` field, which that shape has nowhere. `serde` therefore rejects a real hooks block.

That rejection is not contained. `Settings::load_file` at `config/settings.rs:433` ends in `.ok()?`, so any parse failure discards the whole file without a word. On this machine `~/.claude/settings.json` declares hooks in Claude Code's real shape, so that entire file is being silently thrown away right now, taking its `permissions`, its `env`, and its step in the API key resolution chain with it.

So this change has two jobs. Make hooks run, and stop one unreadable block from silently voiding a whole settings file.

## What Changes

- **BREAKING** `HooksConfig` and `HookDef` are replaced by Claude Code's real shape: a per-event list of matcher groups, each holding a list of command hooks with an optional `timeout` and an optional `shell`. An existing `settings.json` written in the old flat shape stops being read. Nothing in this repository uses the old shape, and no `settings.json` on this machine declares hooks in it.
- A malformed or foreign settings file no longer disappears. `load_file` reports the file and the parse error at `warn` instead of returning nothing silently.
- Hooks run from the project `settings.json` only, by default. A new setting opts in to running hooks declared in `~/.claude/settings.json` too. Global hooks are written against Claude Code's own event payloads and its own tools, so running them unasked from this harness would surprise.
- The stdin payload adopts Claude Code's field names, so a hook script written for Claude Code works unchanged: `hook_event_name`, `tool_name`, `tool_input`, `session_id`, and `cwd`. The current payload uses `event`, `tool`, and `input`, which no existing hook script reads.
- The event set grows to match Claude Code's: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SessionStart`, `SessionEnd`, `SessionReset`, `UserPromptSubmit`, and `Stop`. `HookEvent` already names five of those. `HooksConfig` today can declare only four, so a `SessionReset` hook cannot be configured even though CLAUDE.md lists that event as supported.
- A `PreToolUse` hook can block the tool call it observes, and can rewrite that call's arguments by returning `modified_input`. `HookResult` already parses that field and `HookRunner::run` discards it, so this makes an existing field real rather than adding one.
- Exit code 2 blocks, matching Claude Code. Today any non-zero exit is read as approval, so a hook that fails hard is indistinguishable from a hook that approved.
- A hook's child process joins the same Windows job object every other child in this harness joins, through `process_group::adopt`. `run_one` sets `kill_on_drop` and skips the job object, so a hook child can currently outlive the harness.
- `run_one` stops hardcoding `cmd /C` with plainly quoted arguments. That form was proven broken here for a quoted program path under `C:\Program Files`, which is exactly what a hook command looks like. It adopts the `raw_arg` form and the outer quote pair the Bash tool already uses, and honors a hook's `shell` field.
- Hooks stay `Api`-only. The `ClaudeCli` backend runs its own hooks inside the `claude` child process, and always has.

## Capabilities

### New Capabilities
- `agent/hook-execution`: Running project-declared hook commands at named points in a turn, on the backend whose turn loop this harness owns. Covers which events fire, what a hook receives on stdin, what a hook may do to the call it observes, how a failing or hanging hook is contained, and the child process lifetime guarantee.
- `config/settings-loading`: How a settings file is found, parsed, merged, and reported when it cannot be read. Covers the precedence chain, that a missing file is not an error, and that an unreadable file is reported rather than silently dropped.

### Modified Capabilities

None. No existing capability under `openspec/specs/` describes hooks or settings loading.

## Impact

Affected production code, all under `crates/deepseek-custom/src/`:

- `hooks/mod.rs`: the event enum grows, the payload field names change, `run` returns a decision rather than a bare boolean so `modified_input` can reach the caller, `run_one` gains job-object adoption and a correct command line.
- `config/settings.rs`: `HooksConfig` and `HookDef` change shape, `load_file` reports its errors, and a new setting controls whether globally declared hooks run.
- `agent/agent_exec.rs`: `execute_tool` gains the `PreToolUse`, `PostToolUse`, and `PostToolUseFailure` call sites, and the `SessionReset` branch gains its own.
- `agent/agent_run.rs`: `run_with_image` gains the `UserPromptSubmit` and `Stop` call sites.
- `backend/build_api.rs`: the resolved hook configuration reaches the `AgentLoop` that will run it.
- `main.rs`: the `SessionStart` and `SessionEnd` call sites.

Affected tests: `crates/deepseek-custom-tests/tests/it/hooks.rs` grows from 5 tests. `config_settings.rs` gains coverage that a real Claude Code hooks block loads, which is the test that must fail before this change and pass after.

Security note, stated because it is the sharpest edge here: a hook is an arbitrary shell command that this change gives the power to block a tool call and rewrite its arguments. That is why global hooks are opt-in rather than on, and why the default is the project's own `settings.json`, which is a file the repository owner controls.

Documentation: CLAUDE.md's piggybacking section lists the hook events and claims drop-in compatibility. Both statements need correcting, and `AGENTS.md` carries the same text. CLAUDE.md's `**Next:**` line names hook execution integration as unstarted.
