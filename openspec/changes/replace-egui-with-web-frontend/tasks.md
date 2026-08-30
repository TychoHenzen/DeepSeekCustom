## 1. Characterize Behavior and Extract Application State

- [x] 1.1 Inventory every `DeepSeekGui` field, command path, event path, persisted value, native interaction, and existing external GUI test. Record its destination in an actor, domain service, web component, or deletion list.
<!-- status: completed -->
- [x] 1.2 Add characterization tests for transcript projection, active-operation exclusion, settings effects, pending session switches, and Procedure review before moving ownership.
<!-- status: completed -->
- [x] 1.3 Define serializable application DTOs for snapshots, changes, revisions, command requests, command results, visible settings, operation state, and errors without secret fields.
<!-- status: completed -->
- [x] 1.4 Implement one application actor that serializes browser commands and domain events, owns the presentation-neutral state, and publishes ordered revisions through bounded replay.
<!-- status: completed -->
- [x] 1.5 Move transcript projection and current-session state behind the actor while keeping native startup operational for this migration milestone.
<!-- status: completed -->
- [x] 1.6 Move Autopilot, search, Procedure, voice, backend selection, shared flags, and settings effects behind typed actor service ports without changing their domain contracts.
<!-- status: completed -->
- [x] 1.7 Verify the actor-backed native milestone with focused integration tests and a practical chat, session switch, settings save, and Procedure state run.
<!-- status: completed -->

## 2. Serve the Local Web Application

- [x] 2.1 Add Axum, static-asset embedding, multipart, token generation, and required serialization dependencies with production defaults and test-support seams.
<!-- status: completed -->
- [x] 2.2 Implement the loopback server lifecycle, graceful shutdown, reported URL, health response, embedded fallback route, and browser-open integration.
<!-- status: completed -->
- [x] 2.3 Add the Vite production asset contract and a build failure that explains how to create missing or stale embedded assets.
<!-- status: completed -->
- [x] 2.4 Serve the embedded application and API from one reported loopback origin without a frontend development server.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: The application starts as a local web service :: Normal production startup -->
- [x] 2.5 Implement preferred-port fallback and exact bind-error reporting while proving no non-loopback listener is created.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: The application starts as a local web service :: Preferred port is unavailable -->
- [x] 2.6 Implement bootstrap and snapshot endpoints that return current visible state and revision for a newly connected or reloaded browser.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Browser state reflects one authoritative application state :: Browser connects during an idle session -->
- [x] 2.7 Implement revision-addressed Server-Sent Events, bounded replay, reset snapshots, and reconnect tests during each active operation class.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Browser state reflects one authoritative application state :: Browser reconnects during active work -->
- [x] 2.8 Require base revisions on commands and return an atomic HTTP 409 conflict with the current revision for stale requests.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Browser state reflects one authoritative application state :: A stale client sends a command -->
- [x] 2.9 Generate a process token and accept same-origin JSON commands with the token and current revision through typed routes.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Local web commands are protected from other origins :: Same-origin command is valid -->
- [x] 2.10 Reject foreign or missing Origin values, invalid tokens, cross-origin preflights, secret serialization, framing, and unsafe content sources before dispatch.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Local web commands are protected from other origins :: Another origin attempts a command -->
- [x] 2.11 Run the web-server milestone in practice, reload during a deterministic operation, force replay reset, and verify process shutdown reaps owned children.
<!-- status: completed -->

## 3. Build the Responsive Frontend Shell

- [x] 3.1 Create the `web/` React and strict-TypeScript workspace with locked dependencies, Vite build, type checking, linting, and a Rust-server development proxy.
<!-- status: completed -->
- [x] 3.2 Implement the bootstrap, snapshot, command, revision-conflict, event-replay, reconnect, fatal-error, and offline client state paths.
<!-- status: completed -->
- [x] 3.3 Build desktop semantic navigation for Chat, Autopilot, Cascade, Evolve, Procedure, Sessions, Tests, and Settings with an identifiable active workspace.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: The web frontend is responsive and accessible :: Desktop navigation -->
- [x] 3.4 Build the 360-pixel responsive navigation and workspace layout with reachable actions, contained long content, and no horizontal page overflow.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: The web frontend is responsive and accessible :: Narrow navigation -->
- [x] 3.5 Implement visible focus, logical focus order, labelled controls, live status regions, non-colour state cues, and disabled reasons.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: The web frontend is responsive and accessible :: Keyboard and assistive navigation -->
- [x] 3.6 Add frontend error boundaries and recoverable conflict handling that refreshes a snapshot without repeating the rejected command.
<!-- status: completed -->
- [x] 3.7 Verify the shell milestone from Rust-served assets at desktop and narrow viewports with keyboard-only navigation.
<!-- status: completed -->

## 4. Move Chat, Sessions, Settings, Attachments, and Voice

- [x] 4.1 Build reusable transcript components for user, reasoning, text, tool, notice, error, image, and terminal blocks with bounded long content and follow-output behavior.
<!-- status: completed -->
- [x] 4.2 Connect chat submission and ordered streaming projection for accepted text and image turns, including visible running and terminal states.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Chat and saved sessions preserve their lifecycle :: User sends a chat turn -->
- [x] 4.3 Connect Stop to the existing backend interrupt boundary and render the interrupted terminal event.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Chat and saved sessions preserve their lifecycle :: User interrupts a turn -->
- [x] 4.4 Implement new, list, load, delete, autosave, and deferred mid-turn session switching through the actor and existing session store.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Chat and saved sessions preserve their lifecycle :: User changes sessions during a turn -->
- [x] 4.5 Implement visible settings DTOs and web controls for backend, model, effort, context, style, output, voice, Procedure, and persistence without returning secrets.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Settings preserve runtime and persistence boundaries :: User changes an existing setting -->
- [x] 4.6 Move folder selection behind one `spawn_blocking` native picker request and preserve confirmation, cancellation, failure, `working_dir`, and fixed `project_root` behavior.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Settings preserve runtime and persistence boundaries :: User requests a working-directory folder -->
- [x] 4.7 Add bounded multipart image upload plus paste, drop, select, preview, clear, and existing backend-specific attachment handling.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Attachments and voice controls remain usable :: User attaches an image -->
- [x] 4.8 Connect focused push-to-talk press and release, voice toggles, readiness, transcription, playback, and errors to the existing Rust voice service.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Attachments and voice controls remain usable :: User uses push to talk -->
- [x] 4.9 Verify this milestone in practice with deterministic chat streaming, reconnect, deferred session switch, persisted settings, confirmed and cancelled folders, image input, and voice state.
<!-- status: completed -->

## 5. Move Operational Workspaces

- [x] 5.1 Build shared operation forms, validation messages, progress timelines, bounded logs, result summaries, stop actions, and active-operation exclusion behavior.
<!-- status: completed -->
- [x] 5.2 Implement the Autopilot workspace over the existing repeat command, iteration progress, stop flag, and backend-independent completion behavior.
<!-- status: completed -->
- [x] 5.3 Implement Cascade and Evolve workspaces over existing parameter validation, commands, counters, progress, results, and shared search stop flag.
<!-- status: completed -->
- [x] 5.4 Implement the Procedure workspace for change selection, run modes, progress, route and patch evidence, diffs, reports, failures, interruption, and terminal outcomes.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Existing operational workspaces remain available :: User runs an operational workflow -->
- [x] 5.5 Implement run-scoped Procedure approval and rejection controls with complete review evidence and protection against stale decisions.
<!-- status: completed -->
<!-- covers: deepseek-custom/web-application :: Existing operational workspaces remain available :: Procedure waits for review -->
- [x] 5.6 Recreate the maintained Procedure visual states from deterministic reports at desktop and narrow viewports.
<!-- status: completed -->
- [x] 5.7 Verify the operational milestone with deterministic Autopilot, Cascade, Evolve, and Procedure success, stop, failure, review, and terminal runs.
<!-- status: completed -->

## 6. Add Test Suite Discovery and Control

- [x] 6.1 Define test catalogue, run request, output chunk, counts, outcome, retained result, and active-slot types plus deterministic executor and clock seams.
<!-- status: completed -->
- [x] 6.2 Discover the actual integration target, parse exact test names, group module prefixes, and expose the approved full-workspace run.
<!-- status: completed -->
<!-- covers: deepseek-custom/test-suite-control :: The test catalogue reflects the repository test target :: Test discovery succeeds -->
- [x] 6.3 Preserve a stale successful catalogue and show exact command, exit code, and bounded diagnostics when discovery fails.
<!-- status: completed -->
<!-- covers: deepseek-custom/test-suite-control :: The test catalogue reflects the repository test target :: Test discovery fails -->
- [ ] 6.4 Map the full-suite identity to `cargo test --workspace -j 1 -- --test-threads=1` from fixed `project_root` and record its argument vector.
<!-- covers: deepseek-custom/test-suite-control :: Test execution is constrained to approved suite shapes :: User runs the full suite -->
- [ ] 6.5 Map discovered module and exact-test identities to server-owned focused Cargo argument vectors without invoking a shell.
<!-- covers: deepseek-custom/test-suite-control :: Test execution is constrained to approved suite shapes :: User runs one module or test -->
- [ ] 6.6 Reject unknown identities, client paths, commands, arguments, environments, and stale catalogue revisions before process spawn.
<!-- covers: deepseek-custom/test-suite-control :: Test execution is constrained to approved suite shapes :: Client submits an unknown test identity -->
- [ ] 6.7 Add the single active-run slot and stream identity, command, start time, elapsed time, and ordered output into actor state.
<!-- covers: deepseek-custom/test-suite-control :: Test runs are serialized and observable :: A test run starts -->
- [ ] 6.8 Reject a concurrent request with the current active run identity and prove no second executor starts.
<!-- covers: deepseek-custom/test-suite-control :: Test runs are serialized and observable :: Another run is requested concurrently -->
- [ ] 6.9 Parse reported counts and failed names, retain exit code and duration, and classify passed, failed, cancelled, and infrastructure-error terminal outcomes.
<!-- covers: deepseek-custom/test-suite-control :: Test runs are serialized and observable :: A test run finishes -->
- [ ] 6.10 Adopt the Cargo process tree, implement cancellation and reaping, record cancelled outcome, and release the run slot.
<!-- covers: deepseek-custom/test-suite-control :: Test cancellation reaps the complete process tree :: User cancels an active test run -->
- [ ] 6.11 Keep test execution independent of browser connections and restore current test state from a reconnect snapshot.
<!-- covers: deepseek-custom/test-suite-control :: Test cancellation reaps the complete process tree :: Browser disconnects during a test run -->
- [ ] 6.12 Atomically store terminal results under `.deepseek/test-runs` and render up to 20 newest identities, outcomes, counts, durations, commands, and outputs.
<!-- covers: deepseek-custom/test-suite-control :: Test results are retained with explicit limits :: User revisits recent results -->
- [ ] 6.13 Prune the oldest terminal record on the twenty-first retained result without touching active or newer records.
<!-- covers: deepseek-custom/test-suite-control :: Test results are retained with explicit limits :: Retention limit is exceeded -->
- [ ] 6.14 Implement the 4 MiB head-and-tail output buffer, streamed truncation state, omitted-byte count, and explicit marker.
<!-- covers: deepseek-custom/test-suite-control :: Test results are retained with explicit limits :: Output limit is exceeded -->
- [ ] 6.15 Label each result with only its selected scope and prevent focused success from producing repository-wide completion language.
<!-- covers: deepseek-custom/test-suite-control :: Test results do not imply repository readiness :: Selected tests pass -->
- [ ] 6.16 Build the responsive Tests workspace for catalogue refresh, filtering, full, module, and exact runs, progress, cancellation, history, failures, and output inspection.
- [ ] 6.17 Verify the test milestone with actual discovery and one exact passing test, plus deterministic discovery failure, test failure, concurrency, cancellation, truncation, retention, and reconnect cases.

## 7. Add Playwright-RS Browser Automation

- [ ] 7.1 Add locked `playwright-rs` test dependencies, a version-matched browser installer, focused command, artifact paths, and direct missing-runtime guidance.
- [ ] 7.2 Build the isolated browser harness with ephemeral loopback server, temporary project root, scripted backend, and deterministic dialog, voice, clock, and test-executor services.
- [ ] 7.3 Convert primary browser locators to accessible role and name, with documented test identifiers only where no semantic identity exists.
<!-- covers: deepseek-custom/web-frontend-automation :: Browser controls have stable semantic identities :: Automation locates a primary action -->
- [ ] 7.4 Assert disabled state and its visible reason for unavailable or conflicting actions.
<!-- covers: deepseek-custom/web-frontend-automation :: Browser controls have stable semantic identities :: A control is unavailable -->
- [ ] 7.5 Prove each browser test receives unique temporary state, token, URL, and deterministic success, failure, review, interruption, and reconnect events.
<!-- covers: deepseek-custom/web-frontend-automation :: End-to-end tests run against isolated deterministic services :: Browser test environment starts -->
- [ ] 7.6 Reap each test server, browser context, browser process, and child process tree on pass, failure, timeout, and cancellation while preserving real checkout bytes.
<!-- covers: deepseek-custom/web-frontend-automation :: End-to-end tests run against isolated deterministic services :: Browser test environment stops -->
- [ ] 7.7 Add browser coverage for startup, navigation, chat, stop, sessions, settings, folders, attachments, voice, Autopilot, Cascade, Evolve, Procedure, Tests, cancellation, retention, and reconnect.
<!-- covers: deepseek-custom/web-frontend-automation :: Browser tests cover critical workflows :: Critical workflow contract is changed -->
- [ ] 7.8 Add assertions that compare visible browser state with the server snapshot and command result for success and conflict paths.
<!-- covers: deepseek-custom/web-frontend-automation :: Browser tests cover critical workflows :: Browser and service disagree -->
- [ ] 7.9 Run every primary workspace at 360 by 800, 768 by 1024, and 1440 by 900 while checking reachable actions, page overflow, clipping, overlap, and focus reachability.
<!-- covers: deepseek-custom/web-frontend-automation :: Responsive layouts have automated visual evidence :: Responsive matrix passes -->
- [ ] 7.10 Capture a screenshot, Playwright trace, browser console, and server log for a deliberately failing responsive assertion and for ordinary suite failures.
<!-- covers: deepseek-custom/web-frontend-automation :: Responsive layouts have automated visual evidence :: Responsive check fails -->
- [ ] 7.11 Make missing or incompatible browser startup fail with the exact version-matched installation command instead of skipping.
<!-- covers: deepseek-custom/web-frontend-automation :: Browser prerequisites and failures are explicit :: Browser runtime is missing -->
- [ ] 7.12 Make the focused browser command build or locate production frontend assets, start the isolated Rust server, and finish without external model calls.
<!-- covers: deepseek-custom/web-frontend-automation :: Browser prerequisites and failures are explicit :: Focused browser suite runs -->
- [ ] 7.13 Run the browser milestone from a clean frontend dependency install and inspect generated artifacts for one controlled failure.

## 8. Cut Over and Remove the Native Frontend

- [ ] 8.1 Audit every native GUI test against the parity inventory and move each contract to actor, HTTP, frontend build, or Playwright coverage before deletion.
- [ ] 8.2 Make local web startup the only production entrypoint and update `run.ps1` to report or open the served URL.
- [ ] 8.3 Delete `crates/deepseek-custom/src/gui`, native paint and clipboard seams, temporary dual-start wiring, and obsolete GUI-only tests.
- [ ] 8.4 Remove `egui`, `eframe`, `egui_commonmark`, `egui_extras`, and UI-only transitive support dependencies after proving no production or test references remain.
- [ ] 8.5 Update `AGENTS.md`, `docs/agent-project-context.md`, build and run documentation, voice and folder-picker notes, test prerequisites, and maintained Procedure visual evidence.
- [ ] 8.6 Run the exact frontend install, type, lint, and production-build gates and confirm the binary serves only embedded production assets.
- [ ] 8.7 Run focused actor, HTTP, test-service, and `playwright-rs` tests, then `cargo fmt --all -- --check`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace -j 1 -- --test-threads=1`.
- [ ] 8.8 Run strict OpenSpec validation and `dod-guard cover replace-egui-with-web-frontend`; require every scenario bound with no regressions.
- [ ] 8.9 Perform the final practice run from the production binary without Vite: exercise every workspace, reload during active work, run and cancel tests, inspect retained results, and confirm the checkout has no unexpected changes.
