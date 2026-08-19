## Why

`crates/deepseek-custom/src/hemisphere/mod.rs` has held 92 lines of unwired code since Phase 3 was first sketched. It exports `HemisphereConfig`, two system prompt templates, `compress_for_right`, and `fits_context_window`. No production code calls any of them. The only caller is `crates/deepseek-custom-tests/tests/it/hemisphere.rs`, which tests four functions nothing runs. A clean-house pass found the module and proposed deleting it.

The owner chose to build it instead. The standing rule that decides this is the project's own: backend logic with no visible control is not done. So the module either gains a control and a caller, or it goes. This change gives it both.

The value is a second opinion the primary agent cannot skip. A background advisor watches the session on a cheap or local model, sees a compressed view of the conversation, and asks the primary agent one short question when something looks wrong. The primary agent reads that question at the top of its next turn.

## What Changes

- The `Api` backend gains an optional advisor pass. While hemisphere mode is on, one advisor dispatch runs after each primary turn finishes. The `ClaudeCli` backend is unaffected, since Claude Code owns its own turn loop.
- The advisor runs as an ordinary subagent dispatch through `run_subagent`, so it draws as a `Subagent` block in the transcript, routes its events through `RoutedEvent` like every other dispatch, and answers to the same interrupt flag.
- The advisor's target is a named entry in the `backends` map, so it can be a local Ollama model while the primary agent runs on DeepSeek.
- An advisor question reaches the primary agent as a `Role::User` message reading `Right hemisphere asks: {question}`, pushed into `MessageHistory` before the next turn builds its request.
- An advisor pass that has nothing to say pushes nothing. Silence is the common case and must cost the primary agent no tokens.
- **BREAKING** `HemisphereConfig`'s `left_model` and `right_model` fields go. They predate the `backends` map and name bare model strings, which cannot express "run the advisor on a different provider". The config moves into `settings.json` as a `hemisphere` block alongside every other settings block. No released consumer exists: nothing outside the test crate reads these fields today.
- The settings sidebar gains a hemisphere control in its Experimental section, next to the context budget slider and the plain-language gate.
- `compress_for_right` gets two defects fixed. It truncates with `&content[..500]`, a byte slice that panics when byte 500 falls inside a multi-byte character. Its summary line is a fixed sentence claiming the assistant "has been reading files, executing commands, and writing code" whether or not any of that happened.

## Capabilities

### New Capabilities
- `agent/hemisphere-advisor`: A background advisor that observes the primary agent's conversation on its own backend, sees a compressed view of it rather than the whole history, and injects at most one short question per primary turn into the primary agent's history. Covers when a pass runs, what the advisor sees, how its question reaches the primary agent, what silence means, how the pass is bounded in cost, and how the advisor's state ends.

### Modified Capabilities

None. No existing capability under `openspec/specs/` describes the agent turn loop, the settings blocks, or the subagent machinery, so this change adds requirements rather than changing any.

## Impact

Affected production code, all under `crates/deepseek-custom/src/`:

- `hemisphere/mod.rs`: the config type changes shape, the two compression defects get fixed, and the module gains the pass itself or hands that to a caller.
- `agent/agent_run.rs`: `run_with_image` already wraps `run_turn` and then closes subagent sessions. The advisor pass belongs at that same seam, since it is the one place that knows a primary turn has ended.
- `agent/agent_loop.rs`: holds whatever advisor state the pass needs, and clears it in `clear_history` so an autopilot iteration starts clean.
- `agent/history.rs`: `MessageHistory::push` already takes a `Message`, so the injection needs no new method.
- `backend/build_api.rs` and `backend/factory.rs`: the pass needs a way to reach the factory to build its advisor backend.
- `config/settings.rs`: a new `hemisphere` block, with `Settings::hemisphere_mut` for the sidebar writer, matching how `voice_mut` and `cascade_mut` already work.
- `gui/settings_panel.rs` and `gui/agent_handles.rs`: the control and the shared handle it writes.

Affected tests: `crates/deepseek-custom-tests/tests/it/hemisphere.rs` gains coverage for the pass and loses the two tests that pin the old config fields. `config_settings.rs` gains round-trip coverage for the new block.

Cost impact, stated plainly because it is the main risk: hemisphere mode on adds one model call per primary turn. On a cheap or local advisor backend that is small. On the primary model it is not, which is why the advisor's backend is configured separately rather than inherited.

Documentation: CLAUDE.md and `AGENTS.md` both carry a per-module test table listing `hemisphere/mod.rs` at 4 tests inside a stated total. Both files change together, as CLAUDE.md requires. CLAUDE.md's `**Next:**` line names Phase 3 and needs updating once this lands.
