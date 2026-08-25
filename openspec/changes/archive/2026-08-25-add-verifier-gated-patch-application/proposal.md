## Why

A routed patch preview is useful for inspection, but it does not prove that the patch parses, builds, lints, or passes tests. This milestone turns the preview into a safe coding run whose only success condition is deterministic verification.

## What Changes

- Require the Apply request to name an `Approved` localization report for the selected change and task. Reject pending, rejected, legacy-unreviewed, missing, stale, or mismatched reports before snapshot creation, patch application, model work, or verifier commands.
- Materialize a disposable verification workspace from the current working tree while excluding repository metadata, build outputs, and procedure run data.
- Apply the previewed patch only inside that workspace first.
- Run mandatory gates in order: patch parse and apply, configured format check, compile or type check, lint, and tests.
- Require a non-empty verifier command set before Apply is enabled. Persist the project-specific commands in procedure settings.
- Treat command exit status and output as the source of truth. A model cannot mark a gate as passed.
- Promote the verified file contents to the real workspace only when every gate passes and the original file hashes still match the preview baseline.
- Leave the real workspace unchanged after any failed gate, interruption, stale baseline, or promotion error.
- Record each command, exit code, bounded output, duration, and final disposition in the procedure report.

## Capabilities

### New Capabilities

- `deepseek-custom/verifier-gated-patch-application`: Isolated patch verification and conflict-checked promotion into the current workspace.

### Modified Capabilities

None.

## Impact

Production work will add verification workspace management, command sequencing, file hashing, promotion transactions, and gate events under `procedure`. It will reuse the existing Windows command resolution rules and process-group cleanup. Settings and the Procedure tab will gain verifier command controls. External tests will use temporary repositories and fake commands to prove pass, fail, interrupt, stale-baseline, and partial-promotion behavior.

This is milestone 3 of 5 and depends on milestones 1 and 2. A practical test is a small one-file mechanical change with `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace` configured. The real file changes only after all four commands pass.
