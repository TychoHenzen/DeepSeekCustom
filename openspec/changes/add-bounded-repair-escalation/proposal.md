## Why

The verification gate can reject a weak-model patch, but a rejected run currently has no bounded recovery path. This milestone adds the retry and escalation ladder from `docs/IntelligenceProcedure.md` without allowing an unbounded agent loop.

## What Changes

- Require each repair request to name an `Approved` localization report for the selected change and task. Reject pending, rejected, legacy-unreviewed, missing, stale, or mismatched reports before parser retry, patch work, verifier execution, or local or frontier dispatch.
- On grammar or patch-parse rejection, retry the local model once with the exact parser error.
- On compile, lint, or test failure, send the exact failing command, exit code, bounded output, spec slice, target files, and typed scratchpad back to the local model.
- Limit local verifier-driven repairs to three attempts by default. Rebuild each attempt from a fresh verification workspace.
- Escalate the same task to the configured frontier backend after the local repair budget is exhausted.
- Give the frontier backend the accumulated deterministic errors and the same short context. Allow at most two frontier attempts by default.
- Promote only a candidate that passes every deterministic gate. If the frontier budget is exhausted, stop with a blocked report and leave the workspace unchanged.
- Log the trigger, attempt number, backend, model, error category, and disposition for every retry and escalation.
- Make interruption stop the current child process and prevent any later retry or promotion.

## Capabilities

### New Capabilities

- `deepseek-custom/bounded-repair-escalation`: A verifier-driven local repair budget followed by bounded frontier escalation and a clear human handoff.

### Modified Capabilities

None.

## Impact

Production work will add an attempt state machine, compact error formatting, fresh-attempt workspace reset, escalation prompts, and retry settings. It will reuse backend resolution and procedure reports. External tests will use scripted stub backends and verifier commands to cover local recovery, budget exhaustion, frontier recovery, frontier exhaustion, parser retry, and interruption.

This is milestone 4 of 5 and depends on milestones 1 through 3. A practical test deliberately gives the local model a failing patch, observes three verifier-driven repairs, and confirms that the same task then reaches the configured frontier backend.
