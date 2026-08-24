## 1. Input and repository-index contracts

- [x] 1.1 Expose named 10,000-file and 64 MiB repository-index defaults through procedure settings, and add missing-settings load coverage.
  <!-- covers: deepseek-custom/procedure-localization :: Repository index boundaries are explicit :: Missing index settings use documented defaults -->
  <!-- status: completed -->
- [x] 1.2 Add valid settings overrides with configuration load and round-trip tests that assert the exact effective limits.
  <!-- covers: deepseek-custom/procedure-localization :: Repository index boundaries are explicit :: Configured index settings replace defaults -->
  <!-- status: completed -->
- [x] 1.3 Report the effective index limit and first sorted overflow path, with deterministic file-count and byte-count fixtures.
  <!-- covers: deepseek-custom/procedure-localization :: Repository index boundaries are explicit :: Repository index exceeds a configured limit -->
  <!-- status: completed -->
- [x] 1.4 Retain automatic contract selection for an unbound task in a one-capability change and add a direct input-parser regression test.
  <!-- covers: deepseek-custom/procedure-localization :: Task contract selection is unambiguous :: One capability supports an unbound task -->
  <!-- status: completed -->
- [x] 1.5 Add zero-capability and multi-capability fixtures that assert failure before index or model seams are called and inspect the complete error.
  <!-- covers: deepseek-custom/procedure-localization :: Task contract selection is unambiguous :: Multiple capabilities require a binding -->
  <!-- status: completed -->
- [x] 1.6 Lock the supported ASCII Rust item kinds and exact identifiers with a repository-index fixture.
  <!-- covers: deepseek-custom/procedure-localization :: Conservative Rust symbols retain a path-only fallback :: Supported ASCII Rust items expose symbols -->
  <!-- status: completed -->
- [x] 1.7 Add a valid Unicode Rust identifier fixture that remains path-selectable without an invented or normalized symbol.
  <!-- covers: deepseek-custom/procedure-localization :: Conservative Rust symbols retain a path-only fallback :: Unicode Rust identifier falls back to its path -->
  <!-- status: completed -->

## 2. Structured Ollama dispatch

- [x] 2.1 Add an Ollama request-capture test that asserts the localization JSON Schema is sent through the structured-response field.
  <!-- covers: deepseek-custom/procedure-localization :: Localizer output is schema constrained :: Ollama receives the localization schema -->
  <!-- status: completed -->
- [x] 2.2 Add a configuration test that rejects unsupported localization backends before dispatch and names the selected backend.
  <!-- covers: deepseek-custom/procedure-localization :: Localizer output is schema constrained :: Backend cannot constrain output -->
  <!-- status: completed -->
- [x] 2.3 Lock Ollama request serialization so every shared effort setting omits provider-native reasoning fields while retaining the localization schema.
  <!-- covers: deepseek-custom/procedure-localization :: Localizer output is schema constrained :: Ollama model lacks native thinking control -->
  <!-- status: completed -->

## 3. Structural validation and semantic review

- [ ] 3.1 Add report review types for pending, approved, rejected, and legacy-unreviewed dispositions, with backward-compatible deserialization fixtures.
- [ ] 3.2 Change a schema-valid, index-valid result to save and publish `AwaitingReview` instead of a completed success.
  <!-- covers: deepseek-custom/procedure-localization :: Every localization target exists :: All reported targets are valid -->
- [ ] 3.3 Keep whole-result rejection for invented paths or symbols and add a distinct synchronous binding test for its complete diagnostics.
  <!-- covers: deepseek-custom/procedure-localization :: Every localization target exists :: A target is invented -->
- [ ] 3.4 Add a run-id-scoped rejection command that saves the decision and makes the report fail the approved-report guard.
  <!-- covers: deepseek-custom/procedure-localization :: Every localization target exists :: Structurally valid targets are semantically wrong -->
- [ ] 3.5 Add a run-id-scoped approval command that saves the decision and is the only path through the approved-report guard.
  <!-- covers: deepseek-custom/procedure-localization :: Every localization target exists :: Structurally valid targets are approved -->
- [ ] 3.6 Make repeated matching decisions idempotent, reject decision reversal and stale run identifiers, and prove that none of these paths dispatches the model again.
- [ ] 3.7 Add consumer tests showing that pending, rejected, and legacy-unreviewed reports cannot enter a downstream procedure stage.

## 4. Procedure view and durable evidence

- [ ] 4.1 Render distinct running, awaiting-review, approved, rejected, failed, and interrupted states, with target evidence and run-scoped approve and reject controls.
- [ ] 4.2 Add external GUI-state and report round-trip tests that inspect approved targets, review disposition, backend, model, attempts, and structural validation.
  <!-- covers: deepseek-custom/procedure-localization :: Localization is observable and non-mutating :: Completed report is inspectable -->
- [ ] 4.3 Extend the source-hash harness across structural success, validation failure, interruption, approval, and rejection.
  <!-- covers: deepseek-custom/procedure-localization :: Localization is observable and non-mutating :: Workspace remains unchanged -->
- [ ] 4.4 Add a verification-manifest test that requires the maintained GUI checklist and every named state screenshot to exist.
  <!-- covers: deepseek-custom/procedure-localization :: Localization is observable and non-mutating :: Procedure review states are visually inspectable -->
- [ ] 4.5 Preserve run-id event isolation, reset interruption, and parent-drop interruption while adding review events to the procedure channel.

## 5. Executable scenario bindings

- [ ] 5.1 Add a portable Node Rust-test-file runner and map `rust` to it in `openspec/test-runners.json`; test path validation, module derivation, exit propagation, and paths with spaces.
- [ ] 5.2 Give each unchanged main-spec scenario for change selection, context construction, retry bounds, and interruption its own synchronous external test and direct `// covers:` marker.
- [ ] 5.3 Convert async binding entry points to exact `#[test]` wrappers around shared Tokio helpers, with one marker and one scenario per wrapper.
- [ ] 5.4 Generate every procedure-localization verification command through dod-guard, execute it, and correct any binding whose command does not run its named test.
- [ ] 5.5 Record the derived final scenario count and assert that every final `deepseek-custom/procedure-localization` scenario is bound without weakening the repository coverage ratchet.

## 6. Maintained visual and Ollama verification

- [ ] 6.1 Create a maintained procedure-localization verification document with exact GUI setup, actions, expected states, and screenshot paths.
- [ ] 6.2 Capture the running, review, approved, rejected, failed, and interrupted Procedure views at a readable window size, then inspect each image for overlap, clipping, disabled actions, progress, and target evidence.
- [ ] 6.3 Run the configured Ollama localization smoke case, record the exact model and report identifier, and classify transport, schema, structural, and semantic results separately.
- [ ] 6.4 Hash workspace source files before and after the smoke run and both review decisions, then record the comparison without committing generated run reports.
- [ ] 6.5 Update maintained procedure documentation with index defaults, the ASCII symbol boundary, report review semantics, Ollama reasoning omission, and the relationship to the archived smoke note.
- [ ] 6.6 Update active downstream procedure change artifacts so they require approved localization reports, then strictly validate each affected change.

## 7. Verification gates

- [ ] 7.1 Run focused integration filters for procedure input, index, runner, report, GUI, settings, and Ollama request serialization after their matching implementation groups.
- [ ] 7.2 Run `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace`.
- [ ] 7.3 Run `openspec validate "harden-procedure-localization" --strict --no-interactive` and save the passing output in the maintained verification record.
- [ ] 7.4 Run dod-guard coverage for this change and for `--all`, confirm zero regressions, and record actual bound and unwired counts without treating the ratchet result alone as complete coverage.
