## 0. Approved localization input

- [ ] 0.1 Require the completed-procedure request to name an approved, current baseline localization report matching the selected change, task, fingerprints, and downstream state before sampling starts.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Sampling requires its named approved localization report :: Approved matching report enters sampling -->
- [ ] 0.2 Reject pending, rejected, legacy-unreviewed, missing, stale, and mismatched reports with exact diagnostics. Assert that no sampling, candidate, patch, verifier, or model action occurs.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Sampling requires its named approved localization report :: Untrusted localization input stops sampling -->

## 1. Sampling settings and normalization

- [ ] 1.1 Add bounded settings for localization sample count, agreement quorum, local patch candidate count, metric window, and review thresholds, with load and round-trip coverage.
- [ ] 1.2 Reject localization and candidate counts outside 3 through 5, and reject quorum outside 2 through the localization sample count before dispatch.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Localization uses bounded agreement sampling :: Sample settings are outside bounds -->
- [ ] 1.3 Normalize each accepted localization result into a sorted, deduplicated set of repository path and symbol identities.

## 2. Agreement-based localization

- [ ] 2.1 Launch exactly the configured 3 to 5 constrained local localization samples with bounded concurrency and shared interruption.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Localization uses bounded agreement sampling :: Sample settings are inside bounds -->
- [ ] 2.2 Group exact normalized target sets, select the largest quorum group, and break equal-size ties by first sample index.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Localization agreement controls escalation :: Local samples reach quorum -->
- [ ] 2.3 Dispatch one frontier localization when no group reaches quorum, validate it against the same repository index, and record the disagreement trigger.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Localization agreement controls escalation :: Local samples disagree -->
- [ ] 2.4 Add fixtures for order-only differences, duplicates, invalid samples, quorum boundaries, ties, interruption, and frontier localization failure.

## 3. Verifier-selected local candidates

- [ ] 3.1 Generate the configured 3 to 5 local patch envelopes with stable diversity hints and retain candidate index and generation evidence.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Local mechanical edits use bounded best-of-N :: Local candidates are generated -->
- [ ] 3.2 Verify every completed candidate serially in a fresh isolated workspace and calculate added plus removed line count from the parsed diff.
- [ ] 3.3 Select the passing candidate with the fewest changed lines.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Passing candidates are selected deterministically :: Two candidates pass -->
- [ ] 3.4 Break an equal-size passing tie by the lowest candidate index.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Passing candidates are selected deterministically :: Passing patches have equal size -->
- [ ] 3.5 Enter the existing repair and escalation ladder after every sampled candidate fails without increasing either attempt budget.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Local mechanical edits use bounded best-of-N :: No local candidate passes -->

## 4. Durable metrics and warnings

- [ ] 4.1 Extend run reports with stage timings, route signals, backends, models, attempts, schema failures, candidates, gate outcomes, escalation triggers, available token usage, and terminal disposition.
- [ ] 4.2 Load reports across restart and calculate recent-window local mechanical success and frontier escalation rates with their run counts.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Routing metrics are durable and inspectable :: Completed run updates metrics -->
- [ ] 4.3 Render threshold warnings without mutating route, budget, sample, or backend settings.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Threshold warnings do not rewrite policy :: Escalation rate exceeds threshold -->

## 5. Privacy-limited trace export

- [ ] 5.1 Define a separate allowlisted export record containing normalized targets, route labels, outcomes, and numeric metrics only.
- [ ] 5.2 Export records without serializing procedure reports directly and add secret, prompt, source-content, and raw-output redaction fixtures.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: Exported localization traces protect workspace content :: Trace export is inspected -->

## 6. Completed procedure and verification

- [ ] 6.1 Extend the Procedure tab with sample standings, candidate verifier results, selected candidate, recent metrics, thresholds, and trace export.
- [ ] 6.2 Add a full stub-backed local-success run from OpenSpec validation through verified promotion and assert that no frontier dispatch occurs.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: The completed procedure remains bounded end to end :: Local end-to-end success -->
- [ ] 6.3 Add full disagreement and local-exhaustion runs that end in bounded frontier promotion or a blocked report.
  <!-- covers: deepseek-custom/routing-sampling-and-metrics :: The completed procedure remains bounded end to end :: End-to-end escalation -->
- [ ] 6.4 Run the same small real OpenSpec task several times and record localization agreement, candidate pass rate, selected patch size, and escalation rate.
- [ ] 6.5 Confirm sampling never exceeds configured caps and no workspace change occurs before deterministic verification passes.
- [ ] 6.6 Run `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace`.
