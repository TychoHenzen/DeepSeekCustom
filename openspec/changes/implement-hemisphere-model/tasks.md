## 1. Configuration

- [ ] 1.1 Add a `HemisphereSettings` block to `crates/deepseek-custom/src/config/settings.rs` with `enabled`, `advisor_backend`, `view_budget_chars`, `keep_verbatim_turns`, and `max_reply_tokens`, every field optional with `skip_serializing_if = "Option::is_none"`, and the defaults from design.md's table.
- [ ] 1.2 Add the `hemisphere` field to `Settings`, a `Settings::hemisphere()` reader returning the resolved values, and a `Settings::hemisphere_mut()` that creates the block with defaults when absent, matching `voice_mut` and `cascade_mut`.
- [ ] 1.3 Add round-trip coverage in `crates/deepseek-custom-tests/tests/it/config_settings.rs`: a `settings.json` with no `hemisphere` block loads and reports the mode off, a written block reads back with every field intact, and an unknown field inside the block is dropped rather than failing the load.
  <!-- covers: agent/hemisphere-advisor :: The advisor is visible and controllable from the interface :: The mode survives a restart -->

## 2. Rewrite HemisphereConfig and fix the compression defects

- [ ] 2.1 Replace `HemisphereConfig`'s `left_model` and `right_model` fields with `advisor_backend: Option<String>`, keeping `enabled`, and rename the two window fields to match the settings block. Add a constructor that builds it from `Settings`.
- [ ] 2.2 Fix the byte-slice truncation in `compress_for_right`: cut on a character boundary the way `truncate_for_speech` in `crates/deepseek-custom/src/voice/mod.rs` already does, so a multi-byte character at the cut point cannot panic.
  <!-- covers: agent/hemisphere-advisor :: The advisor sees a compressed view, never the whole conversation :: A multi-byte message truncates safely -->
- [ ] 2.3 Replace the fixed summary sentence in `compress_for_right` with a statement of how many messages were elided and nothing else, so the rendering asserts no activity it cannot see.
  <!-- covers: agent/hemisphere-advisor :: The advisor sees a compressed view, never the whole conversation :: A summary claims nothing it cannot see -->
- [ ] 2.4 Make `compress_for_right` enforce the configured character budget on its whole output, not only per message, so a long conversation cannot exceed the budget through many short messages.
  <!-- covers: agent/hemisphere-advisor :: The advisor sees a compressed view, never the whole conversation :: The view stays inside its budget -->
- [ ] 2.5 Update `crates/deepseek-custom-tests/tests/it/hemisphere.rs`: drop the two tests pinning the removed model fields, and add coverage for the character-boundary cut, the elision count, and the whole-output budget.

## 3. The advisor pass

- [ ] 3.1 Add an advisor pass function that takes the primary agent's history, the resolved hemisphere config, and the backend factory, builds a `SubagentRequest` with `keep_open: false` at depth 1, and dispatches it through `run_subagent`. Return the advisor's reply text, or nothing when the pass did not produce one.
- [ ] 3.2 Give the pass a registry carrying no `Task`, `SendMessage`, or `CloseSession` tool, independent of `subagent_max_depth`, so an advisor cannot dispatch a subagent of its own. This is stricter than `may_dispatch` and is deliberate; see design.md.
- [ ] 3.3 Reduce an advisor reply holding more than one question down to exactly one before it leaves the pass.
  <!-- covers: agent/hemisphere-advisor :: An advisor question reaches the primary agent before its next turn :: One question per pass at most -->
- [ ] 3.4 Return no question for an empty reply, or for a reply that holds no question, so a silent pass injects nothing.
  <!-- covers: agent/hemisphere-advisor :: Silence is free :: An empty reply injects nothing -->
- [ ] 3.5 Make every pass failure a reported non-failure: an unreachable provider, a provider error, or a malformed reply logs and reports where the user can see it, and returns no question, leaving the caller's conversation untouched.
  <!-- covers: agent/hemisphere-advisor :: An advisor pass never breaks the primary turn :: A dead advisor backend leaves the session running -->
- [ ] 3.6 Report an unknown `advisor_backend` once, naming both the requested entry and the entries that exist, through the same path `resolve_named_backend` already uses for an unknown `Task` backend, and run no pass.
  <!-- covers: agent/hemisphere-advisor :: The advisor runs on its own configured backend :: An unnamed or unknown advisor backend disables the pass -->
- [ ] 3.7 Have the pass observe the shared interrupt flag, so an interrupt mid-pass abandons it without injecting anything, and without consuming the flag in a way that leaves a later turn unstoppable.
  <!-- covers: agent/hemisphere-advisor :: An advisor pass never breaks the primary turn :: An interrupt reaches a running pass -->
- [ ] 3.8 Cover the pass in `crates/deepseek-custom-tests/tests/it/hemisphere.rs` against `StubBackend`: a scripted question comes back, a scripted empty reply comes back as no question, a multi-question reply reduces to one, and an unknown backend name reports and yields nothing.

## 4. Wire the pass into the turn loop

- [ ] 4.1 Call the pass from `AgentLoop::run_with_image` in `crates/deepseek-custom/src/agent/agent_run.rs`, after `run_turn` returns and while hemisphere mode is on, so exactly one pass follows one whole turn rather than one per internal round.
  <!-- covers: agent/hemisphere-advisor :: An advisor pass runs once per primary turn while the mode is on :: One pass follows one turn -->
- [ ] 4.2 Skip the pass when `run_turn` returned an error or an interrupt, and confirm the next successful turn still gets one.
  <!-- covers: agent/hemisphere-advisor :: An advisor pass runs once per primary turn while the mode is on :: A turn that fails still ends the pass cycle -->
- [ ] 4.3 Skip the pass entirely while the mode is off, building no advisor backend and issuing no request.
  <!-- covers: agent/hemisphere-advisor :: An advisor pass runs once per primary turn while the mode is on :: The mode off costs nothing -->
- [ ] 4.4 Push a returned question into `MessageHistory` as a `Role::User` message prefixed `Right hemisphere asks: `, matching the prefix `LEFT_EXTRA_PROMPT` already tells the primary agent to expect, so the next request carries it.
  <!-- covers: agent/hemisphere-advisor :: An advisor question reaches the primary agent before its next turn :: A question lands before the next turn -->
  <!-- covers: agent/hemisphere-advisor :: An advisor question reaches the primary agent before its next turn :: The question is attributable -->
- [ ] 4.5 Append `LEFT_EXTRA_PROMPT` to the primary agent's system prompt while the mode is on, and remove it while off, through the same suffix mechanism voice reply mode already uses.
- [ ] 4.6 Confirm a turn running many internal rounds and many tool calls still produces exactly one advisor request.
  <!-- covers: agent/hemisphere-advisor :: The pass is bounded in cost per turn :: One request per turn, whatever the turn did -->
- [ ] 4.7 Confirm the mode injects nothing on the `ClaudeCli` path, since that backend owns its own turn loop and never reaches `run_with_image`.
  <!-- covers: agent/hemisphere-advisor :: The mode applies to the in-process backend only :: The child-process backend ignores the mode -->
- [ ] 4.8 Confirm a request built with the mode on and a silent pass is identical to the request the same conversation builds with the mode off.
  <!-- covers: agent/hemisphere-advisor :: Silence is free :: An empty reply injects nothing -->

## 5. State lifetime

- [ ] 5.1 Confirm the pass holds no state between calls, so a session reset leaves nothing carried over and no question from the old conversation can reach the primary agent.
  <!-- covers: agent/hemisphere-advisor :: Advisor state ends when the conversation does :: A reset clears the advisor -->
- [ ] 5.2 Confirm an autopilot iteration boundary carries nothing across, given `clear_history` rebuilds the history and the pass reads that history fresh.
  <!-- covers: agent/hemisphere-advisor :: Advisor state ends when the conversation does :: An autopilot iteration starts clean -->

## 6. Interface

- [ ] 6.1 Add a hemisphere checkbox to the Experimental section of `crates/deepseek-custom/src/gui/settings_panel.rs`, beside the context budget slider and the plain-language gate, writing a shared flag on `AgentHandles` the way those two already do.
  <!-- covers: agent/hemisphere-advisor :: The advisor is visible and controllable from the interface :: The mode is reachable without editing a file -->
- [ ] 6.2 Add an advisor backend picker that appears once the checkbox is on, listing the entries from the `backends` map the same way the main backend picker does.
- [ ] 6.3 Persist both controls through an `apply_hemisphere` writer, and seed both from `Settings` at startup, so the mode and its advisor backend survive a restart.
  <!-- covers: agent/hemisphere-advisor :: The advisor is visible and controllable from the interface :: The mode survives a restart -->
- [ ] 6.4 Show a caption saying the mode does not apply while a `claude_cli` backend is active, so enabling it there does not appear to work.
  <!-- covers: agent/hemisphere-advisor :: The mode applies to the in-process backend only :: The child-process backend ignores the mode -->
- [ ] 6.5 Confirm a pass draws as a collapsed `Subagent` block naming its backend and model, and that it does not expand by itself, which follows from routing the pass through `run_subagent` and needs no new rendering code.
  <!-- covers: agent/hemisphere-advisor :: The advisor is visible and controllable from the interface :: A pass draws as its own block -->
  <!-- covers: agent/hemisphere-advisor :: The advisor runs on its own configured backend :: The advisor runs on a different provider -->
- [ ] 6.6 Confirm a silent pass still leaves its block in the transcript, so a silent advisor is distinguishable from an advisor that never ran.
  <!-- covers: agent/hemisphere-advisor :: Silence is free :: A silent pass is still visible -->

## 7. Documentation and verification

- [ ] 7.1 Add a Hemisphere section to CLAUDE.md describing the pass, its seam, its settings block with defaults, and the `Api`-only rule, and make the same edit to `AGENTS.md`. Update the per-module test count for `hemisphere/mod.rs` and the stated total in both files.
- [ ] 7.2 Update CLAUDE.md's `**Next:**` line, which currently names Phase 3 as unstarted work, and the Hemisphere entry under "Done", which currently reads "Stub for Phase 3 dual-model".
- [ ] 7.3 Run `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo test --workspace`, and `cargo clippy --workspace -- -D warnings`, and report the test count.
- [ ] 7.4 Run one real session with the mode on and a local Ollama advisor backend, and confirm on screen that a pass draws its own block, that a question reaches the next turn prefixed, and that turning the mode off stops the passes.
