## Context

Milestones 1 through 4 provide one localization result, one routed candidate, isolated verification, and bounded escalation. `search/cascade` already demonstrates bounded concurrent dispatch and counters, but its exact-text voting and free-form reports cannot represent repository targets or verifier outcomes.

## Goals / Non-Goals

**Goals:**

- Detect uncertain local localization through exact normalized agreement.
- Use a small local candidate set before paying for frontier work.
- Persist enough evidence to tune explicit thresholds.

**Non-Goals:**

- Automatic online learning or policy mutation.
- Storing source content for fine-tuning.
- Expanding repair or escalation budgets.

## Decisions

### Gate sampling and metrics work on one named approved localization report

The completed-procedure request carries the baseline localization run ID. Load that exact report through the approved-report guard before scheduling agreement samples, generating candidates, or invoking any model, patch, or verifier operation. Require `Approved`, the selected change and task, current fingerprints, and downstream state derived from the same run. Reject `Pending`, `Rejected`, `LegacyUnreviewed`, missing, stale, and mismatched reports. Do not substitute a newer report or record stage metrics for work that never started.

### Normalize before measuring agreement

Convert each accepted localization sample to a sorted, deduplicated set of `(path, symbol)` pairs after repository validation. Defaults are three samples and quorum two. Configuration accepts sample counts 3 through 5 and a quorum from 2 through the sample count.

Group exact normalized sets. Choose the largest group that reaches quorum. Break an equal-size tie by the lowest first sample index. If no group reaches quorum, dispatch one frontier localization and validate it against the same index.

This reuses bounded dispatch ideas from Cascade, not its trimmed-text voting.

### Generate and verify a small candidate set serially

Default to three local patch candidates, configurable from 3 through 5. Generate candidates with distinct fixed hints. Verify them one at a time to avoid several Cargo builds competing for disk and memory. Continue through the full set so selection can prefer the smallest passing patch.

Count changed added and removed lines after diff parsing. Select the passing candidate with the smallest count, then lowest candidate index. If none passes, enter milestone 4 with its normal budgets. Sampling does not add attempts to those budgets.

### Store one report per run and derive aggregates

Extend each run report with stage timings, route signals, backend and model, schema failures, candidates, gate outcomes, escalation triggers, token usage when a backend supplies it, and terminal disposition. Calculate aggregate rates by reading reports under `.deepseek/procedure-runs`; do not maintain a second mutable counter file.

The GUI shows local mechanical success and frontier escalation rates over a selectable recent window. Defaults warn below 70 percent local success and above 15 percent escalation. Warnings are observations only.

### Export an allowlisted trace schema

Build export records field by field from normalized identifiers, route labels, numeric metrics, and outcomes. Do not serialize run reports directly. This makes prompts, source text, credentials, and raw verifier output impossible to include through a forgotten skip annotation.

### Keep concurrency and cancellation bounded

Use `FuturesUnordered` for localization calls with at most five entries. Patch generation and verification remain serial. Interruption cancels pending samples and prevents candidate generation or promotion.

## Risks / Trade-offs

- [Exact agreement treats near matches as disagreement] -> Normalize order and duplicates, then prefer safe frontier escalation over fuzzy path matching.
- [Best-of-N increases local latency] -> Keep N at 3 by default and show candidate progress.
- [Metrics can mislead on small samples] -> Show run count beside each rate and do not change policy automatically.
- [Trace identifiers can reveal repository structure] -> Export is explicit, excludes content, and documents that path names remain included for localization tuning.

## Migration Plan

Add optional sampling, quorum, metric window, and warning settings with bounded defaults. Older procedure reports load with absent sampling fields. Disabling sampling restores the milestone 4 single-result flow without deleting metrics.
