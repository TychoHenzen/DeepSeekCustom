## 1. DELETE

- [x] 1.1 Remove Dead Code from unreachable application services, settings, and web exports, including unused locals, while retaining intentional external test-support seams and migrating or deleting only tests for removed symbols.
<!-- status: completed -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

## 2. DEDUPE

- [x] 2.1 Extract Function and Move Function for repeated Procedure promotion, report-preparation, and gate-evidence blocks in `crates/deepseek-custom/src/procedure/promotion.rs`, `crates/deepseek-custom/src/procedure/apply.rs`, and `crates/deepseek-custom/src/procedure/patch_apply_check.rs`, preserving transaction order and recovery evidence.
<!-- status: completed -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

## 3. SPLIT

- [x] 3.1 Extract Class `ApplicationCommandDispatcher` from `ApplicationActor::submit`, migrate the web command call sites, and preserve command validation, stale-revision conflicts, typed-port dispatch, and actor integration coverage.
<!-- status: completed -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

- [x] 3.2 Extract Class `ApplicationEventProjector` from actor and transcript event handling, migrate routed-event callers, and preserve ordered transcript text, tool, notice, error, and terminal projections.
<!-- status: completed -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

- [x] 3.3 Extract Class `TestRunCoordinator` from `crates/deepseek-custom/src/application/test_control.rs`, migrate `ApplicationActor` and web-server consumers, and preserve active identity, cancellation, polling, terminal results, and process cleanup.
<!-- status: completed -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

- [x] 3.4 Extract Class `ProcedureReportRepository` from `crates/deepseek-custom/src/procedure/report.rs`, separating document encoding from review and metrics operations, and migrate Procedure runners, preview input, apply, and report tests without changing JSON shapes.
<!-- status: completed -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

- [x] 3.5 Extract Class `BrowserEventClient` from `web/src/client/contracts.ts` and `web/src/client/client.ts`, migrate App and workspace consumers, and preserve bootstrap, SSE, command, conflict, reconnect, and terminal-result wire contracts.
<!-- status: completed -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

- [x] 3.6 Extract Class `OperationWorkspaceView` from `web/src/app/OperationWorkspace.tsx`, keeping operation-specific controls, accessible names, progress rows, disabled reasons, and visual fixture coverage unchanged.
<!-- status: completed -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

## 4. SIMPLIFY

- [x] 4.1 Replace Nested Conditional with Guard Clauses and Extract Function across `BoundedRepairCoordinator`, `LocalRepairRunner`, and `FrontierRepairDispatcher`, keeping local, structural, frontier, interruption, and exhaustion transitions bounded and ordered.
<!-- status: completed -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

## 5. SIGNATURES

- [ ] 5.1 Extract Class and Introduce Parameter Object for `ProcedureRunCoordinator` from `completed.rs` and `runner.rs`, migrate sampled and whole-change callers, and preserve request, outcome, progress, report, and sandbox end-to-end contracts.
<!-- status: pending -->
<!-- verify_cmd: cargo test --workspace && npm --prefix web test && node "C:\Users\siriu\.codex\plugins\cache\dod-guard-monorepo\quality-guard\0.5.8\skills\quality-refactor\scripts\quality-scan.mjs" crates web/src --root=. --exclude=crates/deepseek-custom/src/web/assets --test-path=crates/deepseek-custom-tests --baseline=.quality/baseline.json --fail-on=regression -->
<!-- verify_surface: structural -->
<!-- manual_required: false -->

## 6. COSMETIC

No cosmetic task is planned in this cluster. Line length and comment cleanup
will follow structural moves in later clusters.
