## 0. Approved localization input

- [x] 0.1 Require the Apply request to name an approved, current localization report matching the selected change, task, fingerprints, and preview before verification setup.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Verification requires its named approved localization report :: Approved matching report enters verification -->
- [x] 0.2 Reject pending, rejected, legacy-unreviewed, missing, stale, and mismatched reports with exact diagnostics. Assert that no snapshot, patch, model, or verifier action occurs.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Verification requires its named approved localization report :: Untrusted localization input stops verification -->

## 1. Verifier configuration

- [x] 1.1 Add ordered `procedure.verifier_commands` settings with load, merge, mutation, save, and round-trip coverage.
<!-- status: completed -->
- [x] 1.2 Disable Apply when the command list is empty and show the missing configuration in the Procedure view.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Apply requires configured verifier gates :: No verifier is configured -->
- [x] 1.3 Show the exact ordered verifier commands and enable Apply for a valid preview with a non-empty list.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Apply requires configured verifier gates :: Verifier list is configured -->

## 2. Current-state verification workspace

- [x] 2.1 Generalize the draft snapshot helper to preserve current tracked, untracked, and uncommitted source bytes in a disposable verification directory.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Verification uses a disposable current-state snapshot :: Uncommitted source is included -->
- [x] 2.2 Exclude `.git`, `target`, `.deepseek`, configured output trees, symlink traversal, and Windows reparse-point traversal, with fixture coverage.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Verification uses a disposable current-state snapshot :: Excluded data is not copied -->
- [x] 2.3 Add snapshot size limits, copy progress, cleanup on drop, and retained recovery data only when rollback cannot complete.
<!-- status: completed -->

## 3. Deterministic gate runner

- [x] 3.1 Run `git apply --check` and `git apply` in the verification workspace before project commands.
<!-- status: completed -->
- [x] 3.2 Implement the ordered command runner with Windows PATH and PATHEXT resolution, process-group adoption, bounded output, durations, and exit codes.
<!-- status: completed -->
- [x] 3.3 Mark a candidate eligible only when patch application and every configured command exit successfully.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Deterministic gates decide success :: Every gate passes -->
- [x] 3.4 Stop at the first failed gate and prove that later commands do not run.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Deterministic gates decide success :: A gate fails -->
- [x] 3.5 Capture command text, exit code, first and last 4 KiB of output, truncation state, duration, and disposition in the report.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Gate evidence is retained :: User inspects a failed run -->

## 4. Failure and interruption isolation

- [x] 4.1 Add a failing-test fixture and assert that real workspace hashes remain unchanged after verification failure.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Failed verification cannot change the real workspace :: Test command fails -->
- [x] 4.2 Wire the shared interrupt flag into the active verifier process, kill descendants, clean the snapshot, and prohibit promotion.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Failed verification cannot change the real workspace :: Verification is interrupted -->

## 5. Conflict-checked promotion

- [x] 5.1 Model create, update, delete, and rename targets and compare every real path to its preview baseline before promotion.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Promotion checks for concurrent edits :: Baseline still matches -->
- [x] 5.2 Refuse promotion after a concurrent target edit and list every stale path.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Promotion checks for concurrent edits :: A target changed during verification -->
- [x] 5.3 Stage verified bytes beside targets, back up existing paths, install all results, verify final hashes, then remove backups.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Promotion is all or nothing :: Promotion succeeds -->
- [x] 5.4 Inject a mid-promotion failure and prove existing files are restored, created files removed, and recovery data retained only if rollback fails.
<!-- status: completed -->
  <!-- covers: deepseek-custom/verifier-gated-patch-application :: Promotion is all or nothing :: Promotion fails partway -->

## 6. Apply interface and verification

- [x] 6.1 Add Apply, gate-progress, command-output, conflict, promotion, and terminal-state rendering to the Procedure tab.
<!-- status: completed -->
- [x] 6.2 Add end-to-end temporary-repository tests for passing, failing, interrupted, stale-baseline, create, delete, rename, and rollback runs.
<!-- status: completed -->
- [x] 6.3 Configure the four Rust workspace gates and run one real small mechanical change through isolated verification and promotion.
<!-- status: completed -->
- [x] 6.4 Confirm unrelated pre-existing working-tree changes remain byte-identical after the practical run.
<!-- status: completed -->
- [ ] 6.5 Run `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace`.
