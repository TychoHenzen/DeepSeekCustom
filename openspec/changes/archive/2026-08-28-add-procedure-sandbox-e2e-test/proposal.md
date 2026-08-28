## Why

The procedure system currently has unit and stub-backed coverage, but its real execution path can fail immediately when localization returns symbols that are not in the repository index. A disposable end-to-end fixture with a small, valid OpenSpec proposal will prove whether the complete procedure contract works, isolate failures from the user's checkout, and prevent transport-only or isolated-stage tests from being mistaken for a working system.

## What Changes

- Add a test-only disposable project fixture containing a minimal Rust target and a valid OpenSpec proposal with one executable task.
- Exercise the complete procedure path in that fixture: OpenSpec validation, repository indexing, localization, review approval, route and patch generation, isolated verification, promotion, report persistence, and final assertions.
- Use deterministic fake backend responses for the default passing path, while preserving a separate regression case for invalid localization symbols and its bounded failure result.
- Assert that the promoted change affects only the localized target, leaves the sandbox's unrelated file unchanged, and does not modify the real repository.
- Capture the exact stage reached, backend dispatches, verifier evidence, terminal disposition, and persisted report so an early failure identifies its boundary.

## Capabilities

### New Capabilities

- `deepseek-custom/procedure-sandbox-e2e-test`: A disposable, deterministic end-to-end acceptance fixture for the complete procedure lifecycle and its immediate localization failure path.

### Modified Capabilities

None.

## Impact

The change is test-focused. It will add external integration coverage and reusable sandbox helpers under `crates/deepseek-custom-tests`, with no production API or configuration changes expected. The fixture will use temporary directories and fake dispatchers, so it will not require Ollama, Codex, network access, or changes to the user's worktree.
