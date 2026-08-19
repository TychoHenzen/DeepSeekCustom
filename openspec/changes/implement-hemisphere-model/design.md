## Context

See proposal.md for motivation. What matters for the approach is which seams already exist.

`AgentLoop::run_with_image` in `crates/deepseek-custom/src/agent/agent_run.rs:23` already wraps `run_turn` and then closes every open subagent session. It is the only place that knows a whole primary turn has ended, which is exactly when an advisor pass must run.

`run_subagent` in `crates/deepseek-custom/src/backend/subagent.rs` already does everything a dispatch needs. It builds a backend from a named entry through `BackendFactory`, spawns an event forwarder that prepends a `RouteHop` to every event, and returns a `SubagentOutcome` carrying the reply text, the resolved backend, and the model. Its input is a `SubagentRequest` of seven fields. A `Subagent` transcript block and its state badge come for free from the routing, so an advisor pass draws itself with no new rendering code.

`SubagentRegistry` keeps a dispatch alive past one turn. Its documented lifetime rule is that a session dies with its parent's turn: `close_all` runs at every turn end and drops every session. `session_turn_cap` defaults to 20 turns per session.

`MessageHistory::push` takes a `Message`, so injecting a user-role message needs no new method.

## Goals / Non-Goals

**Goals:**

- One advisor pass per primary turn, on a separately configured backend, drawn in the transcript like any other dispatch.
- No new event type, no new transcript block kind, and no second dispatch path beside `run_subagent`.
- A pass that fails, hangs, or gets interrupted leaves the primary conversation exactly as it was.

**Non-Goals:**

- The advisor does not run concurrently with the primary turn. A pass starts after the turn finishes and the primary agent waits for it. Overlapping the two would need the advisor to read a history the turn is still writing.
- The advisor cannot call tools. It observes and asks. Giving it tools would make it a second agent acting on the repository, which is what the `Task` tool already covers.
- No summarizing model call for the compressed view. Compression stays mechanical this round. See Open Questions.
- The advisor never speaks to the user directly. Its only output path is a question into the primary agent's history.

## Decisions

### The advisor is a stateless pass, not a kept-open session

Each pass is an ordinary `keep_open: false` dispatch through `run_subagent`. The advisor backend is built for the pass, runs one turn, and is dropped.

This refines the mechanism choice made before planning started. That choice was to reuse the subagent machinery rather than build a second `AgentLoop`, and this is that choice at its simplest. A kept-open session would have fought two existing invariants for no gain:

- `SubagentRegistry::close_all` runs at every primary turn end and closes every session. An advisor meant to outlive a turn would need an exemption, and that exemption would weaken a rule whose entire job is stopping runaway cost.
- `session_turn_cap` defaults to 20. A kept-open advisor would silently stop advising after the twentieth turn of a long session, which is a defect that would take a long time to notice.

Nothing is lost, because the advisor's state is not accumulated history. It is the compressed view, and that view is recomputed from the primary conversation on every pass. A pass therefore already sees everything a kept-open advisor would have remembered. This also makes the "advisor state ends when the conversation does" requirement true by construction rather than by cleanup code: there is no advisor state to clear on reset, on a new conversation, or at an autopilot iteration boundary.

Alternative considered: a second `AgentLoop` owned by the first, closer to the original T11 sketch. Rejected because it would duplicate event routing, interrupt propagation, and backend construction, all of which `run_subagent` already does.

### The pass lives at the `run_with_image` seam

`run_with_image` calls `run_turn`, then closes subagent sessions. The advisor pass goes in that same wrapper, after `run_turn` returns.

Placing it inside `run_turn`'s round loop would fire a pass per internal round rather than per turn, which breaks the one-request-per-turn bound in the spec. Placing it in the GUI or in `main.rs` would put it outside the `Api` backend, where it would also have to run for `ClaudeCli`, which must not have it.

The pass runs only when `run_turn` returned a normal reply. An error or an interrupt skips it, which the spec requires and which also avoids asking an advisor to comment on a turn that produced nothing.

### The advisor's question is injected, not returned

The pass pushes a `Role::User` message into `MessageHistory` reading `Right hemisphere asks: {question}`. It does not alter the value `run_with_image` returns, so nothing that calls a turn needs to change shape.

The prefix is load bearing. Without it the primary agent cannot distinguish an advisor question from a human one, and neither can a reader of the saved session. The existing `LEFT_EXTRA_PROMPT` template already tells the primary agent to expect that exact prefix, so the two must stay in step.

A user-role message is the only shape available. An assistant-role message would claim the primary agent said it. A tool-role message needs a matching `tool_call_id`, and no tool call happened.

### Config lands in `settings.json`, replacing the struct's model fields

`HemisphereConfig` loses `left_model` and `right_model`. The left model is whatever the running backend is already using, and naming it twice invites the two to disagree. The right side names a `backends` entry instead of a model string, so it can be a different provider.

The block, with defaults:

| Field | Default |
|---|---|
| `enabled` | `false` |
| `advisor_backend` | none (mode cannot run without it) |
| `view_budget_chars` | `16000` |
| `keep_verbatim_turns` | `6` |
| `max_reply_tokens` | `200` |

`view_budget_chars` is characters, not tokens, because the compressed view is built by truncation and a character count is what the truncation actually enforces. `fits_context_window` already estimates four characters per token, and keeping the budget in the unit the code enforces removes one conversion.

`enabled` defaults false so no existing `settings.json` starts paying for a pass. `advisor_backend` has no default: a wrong guess here sends every pass to the expensive model, so the mode stays off until a person names the entry.

### Two defects in `compress_for_right` get fixed in place

`&content[..500]` is a byte slice on a `&str`. It panics when byte 500 lands inside a multi-byte character, so one emoji or one accented word in a long message crashes the pass. `truncate_for_speech` in `crates/deepseek-custom/src/voice/mod.rs` already solves this by counting characters, and the fix here follows it.

The summary line currently reads that "the assistant has been reading files, executing commands, and writing code" whatever the elided turns hold. That is a fixed sentence dressed as a summary, and it feeds the advisor a claim about work that may never have happened. It becomes a count of what was elided and nothing more.

Neither defect is reachable today, since nothing calls the function. Both become reachable the moment it has a caller, which is why they are in this change rather than a later one.

### Depth and the existing caps

A pass dispatches at depth 1, the same depth a `Task` dispatch from the main session uses. So an advisor at the default `subagent_max_depth` of 2 could itself carry a `Task` tool.

It must not. An advisor is an observer, and one that can dispatch its own subagents turns a per-turn observation into an unbounded tree. The pass therefore requests a registry with no dispatch tools at all, independent of the depth limit. This is a stricter rule than `may_dispatch` applies, and it is deliberate.

## Risks / Trade-offs

**One extra model call per turn, on every turn, forever.** -> The advisor's backend is configured separately, so it can be local. The mode defaults off. The status readout names the advisor backend, so a session running an expensive advisor says so on screen rather than only in the bill.

**The primary agent waits for the pass before its next turn can start.** -> The advisor's reply is capped at roughly 200 tokens on a cheap backend, so the added latency is small. The pass is skipped entirely for a failed or interrupted turn. If the wait proves noticeable in practice, the fix is a timeout that abandons the pass, not concurrency, which would reintroduce the read-while-writing problem.

**The advisor's question consumes primary context every turn it speaks.** -> One question per pass, capped in length. Silence injects nothing. An advisor that speaks every turn is a prompt problem to fix in the advisor's system prompt, not a mechanism to add here.

**The advisor sees a compressed view and may ask about something the elision removed.** -> Accepted. The compressed view is the design, not a limitation of it. A question about elided context is still a question the primary agent can answer or dismiss.

**A pass could fire during an autopilot run, once per iteration turn, unnoticed.** -> The transcript records every pass as its own block, and autopilot draws the same blocks the chat tab does, so a pass is visible in an autopilot record too.

## Migration Plan

No data migration. `HemisphereConfig`'s changed shape has no persisted form today, since nothing writes it to disk.

An existing `settings.json` with no `hemisphere` block loads unchanged and reports `enabled: false`. `serde` drops an unknown field, so a `settings.json` written by a later version and read by an earlier one degrades to the mode being off rather than failing to load.

Rollback is turning `enabled` off. The mode changes no persisted format, so a conversation saved with hemisphere mode on reopens correctly with it off. An advisor question already injected stays in that conversation's history as an ordinary user-role message, which is what it is.

## Open Questions

- **Should the compressed view use a summarizing model call instead of mechanical truncation?** The original plan noted "start with simple truncation + summarization" and left the ratio to experiment. This change ships truncation only. A summarizing call is a strictly additive change later: it replaces the body of one function and changes no requirement in the spec, since the spec constrains what the view must and must not claim, not how it is built.
- **What should the advisor's system prompt actually say?** `RIGHT_SYSTEM_PROMPT` exists and is a reasonable start, but no pass has ever run against it, so nobody knows whether it produces useful questions or constant noise. Tuning it needs a real session and changes no requirement.
- **Should a silent pass be visible as a collapsed block, or as something quieter?** The spec requires a silent pass be distinguishable from no pass. A block satisfies that. Whether a per-turn block is too noisy in practice is a judgment that needs the feature running first.
