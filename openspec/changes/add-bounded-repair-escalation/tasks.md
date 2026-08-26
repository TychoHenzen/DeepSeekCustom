## 0. Approved localization input

- [x] 0.1 Require each repair request to name an approved, current localization report matching the selected change, task, fingerprints, and patch state before creating the attempt state machine.
<!-- status: completed -->
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Repair requires its named approved localization report :: Approved matching report enters the repair ladder -->
- [x] 0.2 Reject pending, rejected, legacy-unreviewed, missing, stale, and mismatched reports with exact diagnostics. Assert that no parser retry, patch, verifier, local-model, or frontier-model action occurs.
<!-- status: completed -->
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Repair requires its named approved localization report :: Untrusted localization input stops repair -->

## 1. Attempt policy and state machine

- [x] 1.1 Add validated procedure settings for local structural retries, total local verifier attempts, frontier backend, and total frontier attempts, with defaults and hard caps from design.md.
<!-- status: completed -->
- [x] 1.2 Implement the explicit attempt transition table and reject any dispatch from a terminal state.
<!-- status: completed -->
- [x] 1.3 Add table-driven tests for every local, frontier, blocked, promoted, failed, and interrupted transition.
<!-- status: completed -->

## 2. Structural retry

- [x] 2.1 Classify schema, envelope, allowlist, and patch-parse failures as structural failures with exact deterministic diagnostics.
<!-- status: completed -->
- [x] 2.2 Retry one local structural failure and continue when the second candidate parses.
<!-- status: completed -->
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Structural failures receive one local retry :: Parser retry succeeds -->
- [x] 2.3 Stop structural retry after a second invalid result and prove no third structural request occurs.
<!-- status: completed -->
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Structural failures receive one local retry :: Parser retry fails again -->

## 3. Local verifier-driven repair

- [x] 3.1 Build bounded `FailureDigest` values from command, exit code, error category, and the newest detailed verifier output.
<!-- status: completed -->
- [x] 3.2 Reconstruct each repair prompt from the spec slice, targets, scratchpad, and failure digests without prior prompts or chat history.
<!-- status: completed -->
- [x] 3.3 Promote a local candidate that passes on attempt three or earlier and make no frontier call.
<!-- status: completed -->
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Verifier failures have a bounded local repair budget :: Local repair passes within budget -->
- [ ] 3.4 Exhaust the default local budget after three failed candidates and assert that no fourth local candidate is requested.
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Verifier failures have a bounded local repair budget :: Local budget is exhausted -->
- [ ] 3.5 Delete each failed verification workspace and rebuild the next attempt from the original current-state snapshot.
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Every repair starts from a fresh verification workspace :: Prior failed files cannot leak -->

## 4. Frontier escalation

- [ ] 4.1 Build the frontier request from the same task, spec slice, targets, scratchpad, and accumulated failure digests, then dispatch it in isolation.
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Exhausted local work escalates the same task :: Frontier receives accumulated evidence -->
- [ ] 4.2 Pass frontier envelopes through the same allowlist, parser, verifier, baseline check, and promotion transaction, and cover a successful frontier repair.
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Frontier repair is also bounded :: Frontier candidate passes -->
- [ ] 4.3 Stop after two failed frontier candidates, return a blocked report, and prove the real workspace is unchanged.
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Frontier repair is also bounded :: Frontier budget is exhausted -->

## 5. Cancellation and evidence

- [ ] 5.1 Cancel the active model or verifier child on interruption, delete disposable state, and prevent later retries or promotion.
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Interruption cancels the ladder :: User interrupts during repair -->
- [ ] 5.2 Persist and render attempt number, tier, backend, model, trigger, error category, gate result, and disposition for every transition.
  <!-- covers: deepseek-custom/bounded-repair-escalation :: Retry and escalation decisions are visible :: User inspects the ladder -->
- [ ] 5.3 Add scripted stub and fake-command end-to-end tests for parser recovery, local recovery, local exhaustion, frontier recovery, frontier exhaustion, and interruption.

## 6. Practical escalation and verification

- [ ] 6.1 Add the repair and escalation progress ladder to the Procedure tab with exact attempt caps visible before a run.
- [ ] 6.2 Run a controlled task whose local candidates fail verification, confirm the frontier receives the accumulated errors, and record the final verified or blocked disposition.
- [ ] 6.3 Confirm the practical run made no real workspace change before a candidate passed every gate.
- [ ] 6.4 Run `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace`.
