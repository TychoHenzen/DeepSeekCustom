## 1. Prove the defect first

- [ ] 1.1 Add a test in `crates/deepseek-custom-tests/tests/it/config_settings.rs` that loads a `settings.json` carrying a real Claude Code hooks block, in the nested matcher-and-hooks shape, and asserts the declared hook is available with its timeout. Run it and confirm it fails against the current code before changing anything.
  <!-- covers: agent/hook-execution :: Hooks are declared in the format Claude Code writes :: A real Claude Code hooks block loads -->
- [ ] 1.2 Add a test asserting a settings file holding one unreadable block still applies its readable blocks. Confirm it fails against the current code, where `load_file` discards the whole file.
  <!-- covers: config/settings-loading :: An unreadable settings file is reported, never silently dropped :: A file with one bad block does not void its other settings -->

## 2. Settings loading

- [ ] 2.1 Rewrite `Settings::load_file` in `crates/deepseek-custom/src/config/settings.rs` to parse the file into a generic JSON value first, then deserialize each known block independently, so a failing block is skipped rather than voiding the file.
- [ ] 2.2 Report every skipped block and every whole-file parse failure at `warn`, naming the file path and the parse error, replacing the silent `.ok()?`.
  <!-- covers: config/settings-loading :: An unreadable settings file is reported, never silently dropped :: A malformed file names itself -->
- [ ] 2.3 Confirm the harness still starts, on defaults, when every file in the chain fails to parse.
  <!-- covers: config/settings-loading :: An unreadable settings file is reported, never silently dropped :: Startup survives an unreadable file -->
- [ ] 2.4 Add coverage for the precedence chain and for a missing file: a later file overrides an earlier one field by field, and an absent file is not an error.
  <!-- covers: config/settings-loading :: Settings load from a fixed precedence chain :: A later file overrides an earlier one -->
  <!-- covers: config/settings-loading :: Settings load from a fixed precedence chain :: A missing file is not an error -->
- [ ] 2.5 Add coverage that an unrecognized field loads and is ignored, so one file can serve both this harness and Claude Code.
  <!-- covers: config/settings-loading :: An unknown field does not fail a load :: A foreign field is ignored -->

## 3. The hooks config shape

- [ ] 3.1 Replace `HooksConfig` and `HookDef` with Claude Code's shape: a per-event list of matcher groups, each carrying a `matcher` and a list of command hooks with `type`, `command`, optional `timeout`, and optional `shell`. Confirm task 1.1's test now passes.
- [ ] 3.2 Add every event to the config: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SessionStart`, `SessionEnd`, `SessionReset`, `UserPromptSubmit`, and `Stop`, each declarable on its own.
  <!-- covers: agent/hook-execution :: Hooks fire at named points in a turn :: A session reset is declarable -->
- [ ] 3.3 Add a `run_global` setting under the hooks block, defaulting to off, and a resolver that concatenates project-declared hooks with globally declared ones only when it is on. Hooks concatenate rather than override, unlike every other settings block; see design.md.
  <!-- covers: agent/hook-execution :: Project hooks run, global hooks are opt-in :: A project hook runs by default -->
  <!-- covers: agent/hook-execution :: Project hooks run, global hooks are opt-in :: A global hook stays silent by default -->
  <!-- covers: agent/hook-execution :: Project hooks run, global hooks are opt-in :: The opt-in reaches global hooks -->
- [ ] 3.4 Implement matcher selection: an exact tool name matches that tool, and an empty matcher or `*` matches every tool. A matcher that does not match skips the hook without spawning anything.
  <!-- covers: agent/hook-execution :: Hooks are declared in the format Claude Code writes :: A matcher selects which tool calls fire a hook -->
  <!-- covers: agent/hook-execution :: Hooks are declared in the format Claude Code writes :: An empty matcher matches everything -->

## 4. The wire format

- [ ] 4.1 Change `HookEvent`'s serde tag to `hook_event_name` and stop lowercasing the variant names, so the payload names events the way Claude Code does.
- [ ] 4.2 Rename the tool payload fields to `tool_name` and `tool_input`, and carry the tool's output and error flag on the result events.
  <!-- covers: agent/hook-execution :: A hook receives the event on stdin in Claude Code's field names :: A tool event names the tool the way a Claude Code hook expects -->
- [ ] 4.3 Add `session_id`, `cwd`, and `transcript_path` to every event's payload, reading `cwd` from the shared working directory and `transcript_path` from the current session's own file.
  <!-- covers: agent/hook-execution :: A hook receives the event on stdin in Claude Code's field names :: Every event carries its session context -->
- [ ] 4.4 Add coverage that the serialized payload holds exactly these field names, since a wrong name is the one defect that makes every real hook script silently do nothing.

## 5. Fix the runner

- [ ] 5.1 Adopt each hook child into the Windows job object through `process_group::adopt` in `run_one`, alongside the `kill_on_drop` it already sets, matching every other spawn path in the harness.
  <!-- covers: agent/hook-execution :: A hook's child process cannot outlive the harness :: A hook subprocess dies with the harness -->
- [ ] 5.2 Replace the plainly quoted `cmd /C` arguments in `run_one` with the `raw_arg` form and the outer quote pair `crates/deepseek-custom/src/tools/bash.rs` already uses, so a hook command naming a quoted path under `C:\Program Files` runs.
- [ ] 5.3 Honor a hook's `shell` field, running a `bash` hook through bash and leaving a hook that declares no shell on the existing `cmd` path.
- [ ] 5.4 Change `HookRunner::run` to return a decision carrying whether to proceed and an optional replacement input, rather than a bare `bool`, so `modified_input` reaches its caller instead of being discarded.
- [ ] 5.5 Treat exit code 2 as a block and every other non-zero exit as a reported non-block, replacing the current behavior where any non-zero exit reads as approval.
  <!-- covers: agent/hook-execution :: A hook can block the call it observes :: Exit code 2 stops the tool -->
  <!-- covers: agent/hook-execution :: A hook can block the call it observes :: Any other non-zero exit does not stop the tool -->
- [ ] 5.6 Confirm every containment path: a command that does not exist, a hook that hangs past its timeout, and output that is not valid JSON each get reported and let execution proceed.
  <!-- covers: agent/hook-execution :: A failing hook never takes the turn down :: A missing hook command is contained -->
  <!-- covers: agent/hook-execution :: A failing hook never takes the turn down :: A hanging hook is cut off -->
  <!-- covers: agent/hook-execution :: A failing hook never takes the turn down :: Unparseable hook output is contained -->

## 6. Wire the call sites

- [ ] 6.1 Carry the resolved hook configuration from `crates/deepseek-custom/src/backend/build_api.rs` onto the `AgentLoop` that will run it.
- [ ] 6.2 Fire `PreToolUse` in `AgentLoop::execute_tool` before `tool.execute`, and stop the call when the decision withholds approval, returning a result that tells the agent the call was blocked and by what.
  <!-- covers: agent/hook-execution :: A hook can block the call it observes :: A withheld approval stops the tool -->
- [ ] 6.3 Apply a returned replacement input to the call, falling back to the original arguments and reporting the refusal when the tool cannot accept it.
  <!-- covers: agent/hook-execution :: A hook can rewrite a tool call's arguments :: Modified arguments reach the tool -->
  <!-- covers: agent/hook-execution :: A hook can rewrite a tool call's arguments :: An unusable modification is refused -->
- [ ] 6.4 Fire `PostToolUse` after a tool returns without error and `PostToolUseFailure` after it returns an error, each receiving that tool's output.
  <!-- covers: agent/hook-execution :: Hooks fire at named points in a turn :: A tool call fires before and after -->
  <!-- covers: agent/hook-execution :: Hooks fire at named points in a turn :: A failing tool call fires the failure event -->
- [ ] 6.5 Fire `SessionReset` from the `SessionReset` branch of `AgentLoop::execute_tool`.
- [ ] 6.6 Fire `UserPromptSubmit` before `run_turn` and `Stop` after it returns, both in `AgentLoop::run_with_image`. If the hemisphere change has landed, `Stop` fires after the advisor pass, so it sees a finished turn.
- [ ] 6.7 Fire `SessionStart` once the backend is built in `crates/deepseek-custom/src/main.rs`, and `SessionEnd` on shutdown.
- [ ] 6.8 Confirm no hook fires on the `ClaudeCli` path, which runs its own hooks inside its child process.
  <!-- covers: agent/hook-execution :: Hooks apply to the in-process backend only :: The child-process backend runs no harness hooks -->

## 7. Documentation and verification

- [ ] 7.1 Correct CLAUDE.md's piggybacking section: it lists five hook events where there are now eight, and claims drop-in compatibility for a shape that did not match. State the real shape, the project-only default, and the `run_global` opt-in. Make the same edit to `AGENTS.md`.
- [ ] 7.2 Update CLAUDE.md's `**Next:**` line, which names hook execution integration as unstarted, and the Hooks entry under "Done". Update the per-module test counts for `hooks/mod.rs` and `config/settings.rs` and the stated total, in both files.
- [ ] 7.3 Run `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo test --workspace`, and `cargo clippy --workspace -- -D warnings`, and report the test count.
- [ ] 7.4 Confirm with a real run that `~/.claude/settings.json` now loads: check the log for the settings load and confirm no parse failure is reported for that file, which is the observable proof the silent discard is gone.
- [ ] 7.5 Run one real session with a project-declared `PreToolUse` hook that blocks one named tool, and confirm on screen that the tool call is refused and the agent is told why.
