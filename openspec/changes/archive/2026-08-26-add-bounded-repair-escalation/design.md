## Context

The verifier from milestone 3 returns a typed gate failure and never changes the real workspace. Backend resolution and model dispatch already support named local and frontier entries. This change connects those seams with bounded attempt state.

## Goals / Non-Goals

**Goals:**

- Recover from likely local-model mistakes without unbounded loops.
- Escalate with deterministic evidence and minimal context.
- Preserve identical verification and promotion rules for both tiers.

**Non-Goals:**

- Model self-critique as a success signal.
- More than one local and one frontier tier.
- Automatic changes to attempt budgets.

## Decisions

### Gate the repair ladder on one named approved localization report

Every ladder request carries the localization run ID. Load that exact report through the approved-report guard before constructing attempt state or dispatching a model or verifier. Require `Approved`, the selected change and task, current fingerprints, and patch state derived from the same report. Reject `Pending`, `Rejected`, `LegacyUnreviewed`, missing, stale, and mismatched reports without starting parser retry, repair, verification, or escalation. The ladder never searches for a replacement report.

### Use an explicit attempt state machine

Add `AttemptState` with tier, attempt index, structural retry count, last candidate, failures, and disposition. Defaults are three total local verifier attempts and two total frontier verifier attempts. A single structural retry occurs before verifier attempts can continue. Configuration validates finite caps and applies a hard upper bound of 4 local and 2 frontier attempts.

The state machine has explicit transitions. It cannot dispatch from a terminal state. Tests enumerate the transition table, including interruption.

### Reconstruct every repair prompt

Build a fresh prompt from the selected spec slice, normalized targets, typed scratchpad, and one `FailureDigest` per prior attempt. A digest contains the command, exit code, error category, and bounded diagnostic. It does not contain the full transcript, prior prompts, or full command logs.

Place the repair instruction at the end of the prompt. Keep the total failure section under a configured character cap by retaining the newest failure and compacting older failures to one line each.

### Reset isolation for every candidate

Delete the failed verification workspace and clone a fresh current-state snapshot before applying the next candidate. Reuse only typed state and error digests. This prevents failed patch residue from becoming invisible input to a later verifier.

### Escalate through the same patch boundary

After local exhaustion, resolve the configured frontier backend and request the same `PatchEnvelope`. Run it in a disposable workspace and pass it through the same allowlist, patch parser, verifier, baseline check, and promotion transaction.

There is no trusted-model bypass. A frontier candidate that fails is simply another typed failure until its two-attempt budget ends.

### Persist an event per transition

Extend procedure reports and progress with attempt start, structural retry, verifier failure, escalation, promotion, blocked, and interrupted events. Each event stores backend and model names but not credentials or source contents.

## Risks / Trade-offs

- [Three local builds can be slow] -> Show attempt and gate progress. Budgets are configurable downward, not unbounded upward.
- [Diagnostics can overflow context] -> Use typed bounded digests and keep only the newest detailed failure.
- [A weak model may regress on feedback] -> The external verifier remains authoritative, and the fixed budget leads to frontier escalation.
- [Frontier CLI tools mutate files] -> Every dispatch and verification remains inside a disposable workspace.

## Migration Plan

Add optional local and frontier attempt caps with defaults. Existing milestone 3 behavior is equivalent to a one-attempt local budget with no escalation, so disabling escalation remains a rollback path.
