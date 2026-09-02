## 1. Closed contracts and lifecycle

- [x] 1.1 Add the session-scoped Controlled Development phase and visible-state contracts with a disabled default, then prove normal chat remains unchanged while the mode is off.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Controlled Development has an explicit lifecycle :: Mode is off -->
- [x] 1.2 Add toggle and packet-start transitions that clear all prior approval before planning dispatch.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Controlled Development has an explicit lifecycle :: Mode starts planning -->
- [x] 1.3 Add the strict `WorkCard` serde contract and bounded validator for fields, commands, paths, exclusions, and complexity exceptions.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: A Work Card is strict and bounded :: Valid Work Card awaits approval -->
- [x] 1.4 Add deterministic malformed-card integration cases that reject unknown fields and every bound violation without prose extraction.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: A Work Card is strict and bounded :: Malformed Work Card is rejected -->

## 2. Existing snapshot and backend boundaries

- [x] 2.1 Extend `DisposableDraftWorkspace` with one-read baseline forking, deterministic file inventories, changed-path comparison, retained ownership, and safe cleanup without changing Procedure behavior.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Execution uses one isolated current-state workspace :: Passing packet starts from current dirty bytes -->
- [x] 2.2 Add a controlled `BackendFactory` profile for API planning and execution with fixed rooted file tools and no shell, directory change, MCP, skill, reset, question, or subagent tools.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Planning cannot change the real workspace :: Planning cannot change the real workspace -->
- [x] 2.3 Add forced fresh Codex and Claude planning profiles with read-only access, strict structured output, isolated configuration, and no session persistence.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: A Work Card is strict and bounded :: Valid Work Card awaits approval -->
- [x] 2.4 Add forced fresh Codex and Claude execution profiles with only their isolated workspace writable and no subagent or external tool configuration.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Execution uses one isolated current-state workspace :: Codex CLI cannot bypass the outer workspace boundary -->
- [x] 2.5 Add focused fake CLI tests for exact Codex sandbox, schema, ephemeral, configuration-isolation, fresh-thread, and disposable working-directory arguments.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Execution uses one isolated current-state workspace :: Codex CLI cannot bypass the outer workspace boundary -->

## 3. Coordinator, approval, and promotion gates

- [x] 3.1 Add the Controlled Development coordinator and typed service commands for planning, approval, rejection, execution, evidence projection, and terminal transitions.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Approval belongs to one card :: Approval applies to one card only -->
- [x] 3.2 Reject a current card without backend execution and clear its approval identity.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Approval belongs to one card :: User rejects a card -->
- [x] 3.3 Compute all isolated changes before proof commands and block any path outside the approved production and supporting lists.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Changed paths must match the approved card :: Changed path outside approved lists blocks promotion -->
- [x] 3.4 Enforce the three-changed-production-path limit independently of the number of listed supporting paths.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Changed paths must match the approved card :: More than three production paths blocks promotion -->
- [x] 3.5 Add the fixed Cargo and web-package manifest classifier and require matching named complexity exceptions for their changed dependency entries.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Complexity exceptions gate dependency files :: Unapproved manifest or lockfile change blocks promotion -->
- [x] 3.6 Reuse `VerifierCommandRunner` to run one to three approved proof commands in order and stop on spawn failure, interruption, or nonzero exit.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Every proof command must pass in isolation :: Failing proof command blocks promotion -->
- [x] 3.7 Reuse `PromotionBaseline` and `promote_verified_workspace` to promote only validated changed paths plus reserved project state after every gate passes.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Every proof command must pass in isolation :: Passing packet promotes only approved paths -->
- [x] 3.8 Add a concurrent-overlap integration case that preserves the real user's bytes and rolls back every packet target.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Promotion rejects overlapping concurrent edits :: Overlapping real-workspace changes block promotion without data loss -->
- [x] 3.9 Retain failed baseline and execution roots, expose their diagnostic diff, and clean them only on explicit discard, packet replacement, reset, or deletion.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Failed packets retain diagnostic evidence :: Blocked packet remains inspectable -->
- [x] 3.10 Route text and UI Stop through the existing backend, verifier, and process interruption paths, then block every later gate and promotion transition.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Stop prevents promotion :: STOP interrupts the backend and prevents promotion -->

## 4. Session recovery, control commands, and project state

- [x] 4.1 Add backward-compatible Controlled Development state to saved session records and install only the selected session's state during load or creation.
<!-- status: completed -->
<!-- covers: deepseek-custom/controlled-development-mode :: Controlled state is session-scoped and restart-safe :: Session change does not transfer approval -->
- [ ] 4.2 Normalize restored `Planning`, `AwaitingApproval`, and `Executing` state to `Interrupted`, clear approval, and prove no automatic backend, verifier, or promotion work starts.
<!-- covers: deepseek-custom/controlled-development-mode :: Controlled state is session-scoped and restart-safe :: Reloaded in-flight sessions become Interrupted -->
- [ ] 4.3 Intercept exact `STATUS`, `MAP`, `DIFF`, `WHY <item>`, and `STOP` inputs before normal chat dispatch and return only the requested harness-owned state.
<!-- covers: deepseek-custom/controlled-development-mode :: Control inputs are handled by the harness :: User requests bounded control information -->
- [ ] 4.4 Build `PROJECT_STATE.md` from typed completed-packet state, enforce its allowed sections, ten-component map, and 40-nonblank-line cap, then include it in transactional promotion.
<!-- covers: deepseek-custom/controlled-development-mode :: Successful promotion updates the project snapshot :: PROJECT_STATE remains within 40 nonblank lines -->

## 5. Application and frontend surface

- [ ] 5.1 Wire controlled commands, service events, session state, and projections through the application actor without blocking normal command arbitration.
<!-- covers: deepseek-custom/controlled-development-mode :: Controlled Development has an explicit lifecycle :: Mode starts planning -->
- [ ] 5.2 Extend the existing client contracts and Chat workspace with the toggle, phase, complete Work Card, Approve, Reject, Stop, changed paths, proof results, limitation, and semantic disabled reasons.
<!-- covers: deepseek-custom/controlled-development-mode :: The existing web application exposes Controlled Development :: User reviews and approves a Work Card -->
- [ ] 5.3 Add typed progress and completion summary builders with deterministic whitespace word counts and focused Rust and frontend boundary tests.
<!-- covers: deepseek-custom/controlled-development-mode :: Compact output is bounded without hiding diagnostics :: Visible progress and completion summaries respect their limits -->
- [ ] 5.4 Add the collapsed raw-details surface and prove full reasoning, tool, assistant, verifier, and failure evidence remains available beyond compact limits.
<!-- covers: deepseek-custom/controlled-development-mode :: Compact output is bounded without hiding diagnostics :: Raw diagnostic output remains available -->
- [ ] 5.5 Update generated production web assets through the existing build path and extend responsive Playwright-RS coverage for the complete controlled panel.
<!-- covers: deepseek-custom/controlled-development-mode :: The existing web application exposes Controlled Development :: User reviews and approves a Work Card -->

## 6. Required verification and production practice

- [ ] 6.1 Run the focused Controlled Development Rust integration target and frontend tests, and record the exact observed counts and results.
- [ ] 6.2 Run `cargo fmt --all -- --check`.
- [ ] 6.3 Run `cargo check --workspace`.
- [ ] 6.4 Run `cargo clippy --workspace -- -D warnings`.
- [ ] 6.5 Run `cargo test --workspace` and record the exact observed result.
- [ ] 6.6 Run the real production binary with a disposable Ollama configuration through Playwright-RS. Exercise planning, approval, isolated diff, proof evidence, approved promotion, and Stop. Retain ordered screenshots and the browser trace, and compare real workspace bytes at each boundary.
