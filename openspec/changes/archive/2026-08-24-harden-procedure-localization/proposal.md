## Why

The first localization milestone is structurally safe, but it can still mark a
semantically wrong target set as succeeded. Its audit also left ambiguous
task-to-capability selection, assumed index defaults, unverified GUI layout,
stale smoke evidence, and twelve unwired specification scenarios.

## What Changes

- Distinguish schema and repository-index validity from semantic target
  approval. A structurally valid localization enters review instead of
  succeeding automatically.
- Let the user accept or reject the proposed targets in the Procedure tab.
  Only an accepted target set becomes a completed localization report that a
  later patch stage may consume.
- Require an explicit `covers` binding when an unchecked task belongs to a
  change with multiple capability deltas. Keep the deterministic pre-dispatch
  error and cover it with a regression test.
- Make the 10,000-file and 64 MiB repository-index defaults part of the public
  procedure contract. Keep both values configurable and test their defaults,
  overrides, and limit errors.
- Keep conservative ASCII Rust symbol extraction as an explicit supported
  subset. Preserve path-only localization for valid files whose symbols are
  outside that subset, and add a Unicode-identifier regression fixture.
- Add a repeatable visual QA checklist and saved evidence for the Procedure
  tab's controls, progress, target review, error, and interruption states.
- Replace the stale Ollama observation with a maintained smoke record that
  distinguishes transport success, structural validity, semantic review, and
  unchanged workspace hashes.
- Bind every `deepseek-custom/procedure-localization` scenario to the exact
  external Rust test that proves it, then require the coverage report to show
  all procedure-localization scenarios bound with no regression.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `deepseek-custom/procedure-localization`: Add explicit semantic review,
  deterministic ambiguity handling, documented index and symbol boundaries,
  inspectable GUI evidence, maintained smoke evidence, and scenario
  traceability.

## Impact

Production changes affect the procedure input, index, run state, report, and
GUI modules. External integration tests remain in
`crates/deepseek-custom-tests/tests/it/`. The saved report schema gains review
state, so readers of completed localization reports must require an approved
disposition. The active downstream procedure changes must consume only
approved localization reports. No patch generation or workspace mutation is
added by this change.
