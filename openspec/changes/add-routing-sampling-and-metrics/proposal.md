## Why

A fixed local-first ladder is testable, but it cannot detect uncertain localization or measure whether its thresholds save work. This final milestone adds agreement sampling, verifier-selected patch candidates, and the evidence needed to tune the router.

## What Changes

- Require the completed-procedure request to name an `Approved` localization report for the selected change and task. Reject pending, rejected, legacy-unreviewed, missing, stale, or mismatched reports before agreement sampling, candidate generation, patch work, verification, or model dispatch.
- Run 3 to 5 constrained localization samples on the local backend and normalize each result to repository file and symbol identities.
- Continue locally only when the configured agreement threshold is met. Escalate localization to the frontier backend when samples disagree.
- For local mechanical edits, generate a bounded best-of-N set of 3 to 5 patch candidates before frontier escalation.
- Verify candidates in isolated workspaces and select a passing candidate deterministically. Prefer fewer touched lines, then lower candidate index, when multiple candidates pass.
- Keep the repair and frontier budgets from milestone 4. Sampling does not create an unbounded retry path.
- Persist per-stage metrics for route signals, backend and model, attempts, schema rejections, verifier results, escalation triggers, token usage when available, and duration.
- Show cumulative local success and frontier escalation rates in the Procedure tab. Flag, but do not silently change, configurable review thresholds such as local mechanical success below 70 percent or escalation above 15 percent.
- Export privacy-limited localization traces for later fine-tuning. Exclude prompts, file contents, credentials, and raw command output.

## Capabilities

### New Capabilities

- `deepseek-custom/routing-sampling-and-metrics`: Agreement-based localization, verifier-selected patch sampling, routing metrics, threshold warnings, and privacy-limited trace export.

### Modified Capabilities

None.

## Impact

Production work will extend `procedure` with bounded concurrent sampling, normalization, deterministic candidate selection, metrics storage, trace export, and GUI summaries. The existing Cascade concurrency and counters are reference implementations, but procedure candidates retain typed stage state and isolated verification. External tests will cover agreement boundaries, deterministic tie-breaking, caps, metrics persistence, redaction, and end-to-end local and escalated runs.

This is milestone 5 of 5 and depends on milestones 1 through 4. A practical test runs the same small OpenSpec task several times, observes localization agreement and candidate verification, and compares the recorded local success and escalation rates. At this point the full Stage 0 through Stage 4 routing system is built.
