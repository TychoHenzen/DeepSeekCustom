## Why

The localization milestone can identify likely edit targets, but it cannot decide which model should draft the edit. This milestone makes that route explicit and reviewable while keeping the workspace unchanged.

## What Changes

- Consume one explicitly named localization run report from `add-procedure-localization-runner`. Require `Approved` and matching change, task, OpenSpec, and target fingerprints. Reject pending, rejected, legacy-unreviewed, missing, stale, or mismatched reports before route evaluation or drafting-model dispatch.
- Classify the requested step with deterministic signals from the OpenSpec slice and localization result. Signals include target count, cross-cutting or architectural markers, and whether the requested work is mechanical.
- Route only clearly mechanical work to the configured local backend. Route substantive logic, subtle bugs, architecture, and multi-file work to the configured frontier backend.
- Never use raw token probability as a routing signal.
- Ask the selected backend for a patch envelope. Constrain local output with JSON Schema and validate frontier output with the same deterministic parser.
- Reject malformed patches and patches that touch files outside the localization allowlist.
- Show a preview with the route, every route signal, backend, model, target files, and unified diff. Do not apply it.
- Add explicit local and frontier overrides for practice runs. The preview records that an override replaced the automatic route.

## Capabilities

### New Capabilities

- `deepseek-custom/routed-patch-preview`: Deterministic difficulty routing and a validated, non-mutating patch preview over localized repository targets.

### Modified Capabilities

None.

## Impact

Production work will extend the `procedure` module with difficulty signals, route decisions, patch-envelope parsing, and preview state. It will reuse `BackendFactory` and the existing one-shot dispatch paths instead of adding provider-specific routing branches. The Procedure tab and transcript will show the decision and preview. External integration tests will cover both automatic routes, overrides, malformed diffs, allowlist violations, and unchanged workspace hashes.

This is milestone 2 of 5 and depends on milestone 1. A practical test is to preview a one-file rename through Ollama, then preview an architectural task through Codex or Claude. Both runs must leave `git diff` unchanged.
