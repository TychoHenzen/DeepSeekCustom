# Diversity.md implementation checklist, 2026-08-09

Derived from `2026-08-09-diversity-implementation-plan.md`. That document
holds the reasoning, including what is already done and what is not being
built. This one holds the work items, in build order.

## Phase A: reliability-floor prompt work

- [x] `A1` Add a tool-first-arithmetic instruction to the system prompt
      (`crates/deepseek-custom/src/agent/prompt.rs`). Unconditional, not
      gated on a flag.
- [x] `A2` Write one line per autopilot step (task text plus final reply,
      newlines flattened) to `.autopilot/decisions.log`. Do this at the end
      of `run_repeat`'s step loop.

## Phase B: the Cascade tool

- [x] `B1` `crates/deepseek-custom/src/tools/cascade.rs`: schema (`prompt`,
      `backend`, `n`, `vote_k`, `check_cmd`, `diversity_hints`,
      `escalate_backend`, `effort`). Register it. Depth-gate it with
      `may_dispatch`, like `Task`.
- [x] `B2` Dispatch `n` attempts at once through `run_subagent`. Give each
      its own `SubagentId`. Round-robin the diversity hints into each
      attempt's prompt.
- [x] `B3` `check_cmd` red-flag step: run it once per candidate against
      `working_dir`, through the same path `BashTool` uses. Drop any
      candidate whose command exits non-zero, before voting.
- [x] `B4` Voting: group by exact text match by default. When `check_cmd`
      is set, vote only among candidates that passed it. Require the
      `vote_k` lead margin to accept a winner.
- [x] `B5` Tool result names the winning attempt id and the vote count. A
      losing attempt still shows up in the transcript as its own
      `Subagent` block.
- [x] `B6` Every-attempt-failed and no-candidate-passed cases both come
      back as a tool error. Neither should ever panic.
- [x] `B7` `tools_cascade.rs` in `crates/deepseek-custom-tests/tests/it/`:
      a `StubBackend` case with a `check_cmd` that rejects some candidates.
      Assert the correct winner comes back.

## Phase C: escalation and visibility

- [ ] `C1` `escalate_backend`: on no `vote_k` win, make one more call to
      `escalate_backend`. Send it the original prompt plus every rejected
      candidate and why it was cut. Its answer becomes the tool output,
      marked as escalated.
- [ ] `C2` Without `escalate_backend`, the no-consensus case stays a tool
      error. No change from Phase B here.
- [ ] `C3` `SharedFlags` gains `cascade_total` and `cascade_escalated`
      (`Arc<AtomicUsize>`). Bump the first on every Cascade call. Bump the
      second on every escalation.
- [ ] `C4` Status bar readout: the escalated share, shown once
      `cascade_total` is above zero.
- [ ] `C5` Extend `tools_cascade.rs`: an all-disagree `StubBackend` case
      with `escalate_backend` set returns the escalated answer. Check the
      counters update too.

## Phase D: plain-language gate

- [ ] `D1` `crates/deepseek-custom/src/style/mod.rs`: pure
      `flesch_kincaid_grade(text: &str) -> f32`. No new dependency.
- [ ] `D2` `style` block in `settings.json`
      (`plain_language_enabled`, `target_grade`, `grade_tolerance`,
      `max_revise_attempts`, `critic_backend`). Default
      `plain_language_enabled` to `false`.
- [ ] `D3` Gate in `AgentLoop::run`, right before `TurnEnd`: compute the
      grade on replies past a minimum length. Skip the check entirely when
      the setting is off.
- [ ] `D4` Critique-and-revise loop against `critic_backend` (falls back
      to the replying backend). Cap it at `max_revise_attempts`. Stop early
      once the grade is within `grade_tolerance`.
- [ ] `D5` `Notice` block (`Info`) recording that a reply was revised, and
      how many tries it took. No notice when the original already passed.
- [ ] `D6` `style.rs` in `crates/deepseek-custom-tests/tests/it/`: unit
      tests for `flesch_kincaid_grade` against sentences with a known
      grade. Add a `StubBackend` test for the revise loop too.

## Phase E: programmatic evolutionary search

- [ ] `E1` `crates/deepseek-custom/src/evolution/mod.rs`: `Candidate`,
      `MapElitesArchive`, `Island`. Plain data, no tool-trait code, no
      model call anywhere in this file.
- [ ] `E2` `MapElitesArchive::insert`: fixed-width bucketing of a feature
      vector into a grid cell, keep the higher-fitness `Candidate` per
      cell. Without a feature vector, fall back to plain top-`k` fitness
      elitism.
- [ ] `E3` `select_parent(&Island) -> &Candidate`: deterministic,
      round-robins across occupied cells rather than always picking the
      single best.
- [ ] `E4` `migrate(&mut [Island])`: rank islands by best fitness, reset
      the bottom half, reseed from the single best candidate across every
      island. Runs every `migration_interval` rounds.
- [ ] `E5` `evolution.rs` in `crates/deepseek-custom-tests/tests/it/`:
      direct unit tests for `insert`, `select_parent`, and `migrate`
      against hand-built fitness numbers. No `StubBackend` needed here.
- [ ] `E6` `crates/deepseek-custom/src/tools/evolve.rs`: schema (`prompt`,
      `backend`, `generations`, `population`, `fitness_cmd`,
      `feature_cmd`, `islands`, `migration_interval`, `mutation_hints`,
      `effort`). Register it. Depth-gate it with `may_dispatch`.
- [ ] `E7` One round: `select_parent` picks a parent, build a prompt from
      the seed task plus the parent plus the next mutation hint, dispatch
      through `run_subagent`. Score the result with `fitness_cmd` and, if
      set, `feature_cmd`. Insert into the archive.
- [ ] `E8` Errors: a failed generation drops that candidate for the round
      without stopping the run. A `fitness_cmd` or `feature_cmd` that does
      not print a parseable number is a tool error naming the command.
- [ ] `E9` Tool result: the best candidate across every island, its
      fitness, its feature coordinates if any, and a small table of the
      full archive.
- [ ] `E10` `tools_evolve.rs` in `crates/deepseek-custom-tests/tests/it/`:
      a `StubBackend` generator run through several rounds against
      synthetic `fitness_cmd`/`feature_cmd` scripts. Assert the final best
      candidate is the one the synthetic fitness function actually favors.

## Already done, no checklist items

- Keeping reasoning apart from formatting (Diversity.md #3): no forced
  grammar exists to remove.
- Context pruning, tool-result elision, sub-agent isolation (Diversity.md
  #4, three of four levers): already built. See the long-term roadmap.
- Executable checks for this repository's own changes (Diversity.md #2's
  general form): `cargo test`, `cargo clippy`, and the
  `dod-guard`/`quality-guard` MCP tools already cover it.

## Not being built

- A learned or rule-based router apart from the calling model (Diversity.md
  #8's router half): the calling model is the router. Only the escalation
  path and counters (`C1` through `C5`) are new.
- Vale and proselint integration (Diversity.md #9's linter half): named as
  a possible follow-up in the plan. Not scheduled here.
