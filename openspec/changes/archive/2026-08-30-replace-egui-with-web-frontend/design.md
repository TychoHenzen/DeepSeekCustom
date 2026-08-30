## Context

See `proposal.md` for motivation. The current `DeepSeekGui` combines presentation state, event draining, command submission, settings persistence, session transitions, native attachment handling, and control of Autopilot, search, Procedure, and voice services. `main.rs` constructs the backend and all channels before handing their receivers, senders, and shared flags to the blocking `eframe` loop.

The backend boundary already helps this migration. Backends emit `RoutedEvent` values through channels and receive typed commands. Session records, settings, Procedure reports, process-group adoption, and the fixed `project_root` versus mutable `working_dir` distinction must remain compatible. Production and test code must remain in separate crates.

The new requirements are defined by the three delta specs under `specs/deepseek-custom`. Existing main specs remain binding, including the Procedure visual states and native working-directory picker behavior.

## Goals / Non-Goals

**Goals:**

- Give the Rust process one presentation-neutral application state and command boundary.
- Serve one responsive TypeScript application from the production binary.
- Preserve current backend and persisted-data contracts while replacing every native screen.
- Make browser state recoverable after reloads and transport interruptions.
- Keep all state-changing access local and protected from unrelated browser origins.
- Add bounded, cancellable, observable Rust test execution.
- Make independently runnable milestones available throughout migration.

**Non-Goals:**

- Remote hosting, multi-user authentication, or access from another machine.
- A public network API or support for third-party API clients.
- Arbitrary command execution from the Tests workspace.
- Replacing Rust backend, Procedure, voice, session, or model behavior.
- Preserving `egui` as a second frontend after migration.
- Moving model calls, native audio capture, native folder selection, or test processes into the browser.

## Decisions

### 1. Extract a single application actor before replacing presentation

Add a presentation-neutral application layer that owns the state currently held by `DeepSeekGui`. It will receive typed `AppCommand` values, consume backend and service events, update a serializable `AppSnapshot`, and publish sequenced `AppChange` records. One actor will serialize mutations, so HTTP handlers never edit shared state directly.

The actor will own transcript projection, active workspace, pending session switch, settings projection, operation state, voice state, and test state. Existing backend, search, Procedure, session, and voice modules remain domain services. Browser DTOs will use explicit serializable types rather than exposing channels, atomic flags, or internal service types.

Alternative considered: let each HTTP handler lock and mutate the old GUI data. That would preserve its presentation coupling and introduce ordering races between service events and concurrent requests.

### 2. Use Axum with REST commands and Server-Sent Events

The production process will use Axum and Tokio. Read endpoints will return bootstrap data and snapshots. State-changing endpoints will translate validated JSON bodies into typed application commands. A Server-Sent Events stream will carry sequenced application changes.

Each snapshot and change will carry a monotonic revision. A client will request changes after its snapshot revision. The server will keep a bounded replay buffer. If the requested revision is too old, it will send a reset instruction and a fresh snapshot. Commands will carry the revision they were based on. A stale command will return HTTP 409 with the current revision.

Alternative considered: a bidirectional WebSocket. Commands are discrete request-response operations, while events flow mainly from server to browser. REST plus Server-Sent Events gives normal status codes, automatic stream reconnection, and simpler origin checks.

### 3. Use React, TypeScript, and Vite for the browser frontend

Create a `web/` frontend workspace with TypeScript in strict mode, React, Vite, semantic HTML, and a small explicit client state layer. The layout will use responsive CSS rather than JavaScript viewport branches. Accessible role and name will be the primary automation contract. Explicit `data-testid` values are reserved for content with no stable accessible identity.

Production builds will embed the hashed `web/dist` assets into the Rust binary. Development mode may run Vite with a loopback proxy, but production and browser tests will exercise Rust-served assets. The Cargo build will fail with direct guidance when required production assets are missing or stale.

Alternative considered: Rust/Wasm UI. It would retain Rust as the frontend language and would not create the requested separation from the Rust UI. Alternative considered: a separate Node server. It would duplicate process lifetime, settings, security, and packaging responsibilities already owned by Rust.

### 4. Keep local-only access protected by process-scoped credentials

The server will bind `127.0.0.1` and `::1` only. It will not enable CORS. On startup it will generate a random process token. Same-origin bootstrap data will provide the token to the loaded frontend. Every state-changing request must use JSON, send the token in a custom header, provide the served Origin, and include the current revision. The server will reject missing or foreign origins before parsing the command.

Responses and snapshots will omit API keys, environment values, backend secret maps, and raw credential sources. Static responses will set a restrictive Content Security Policy and disable framing. The design does not add a remote-bind option.

Alternative considered: trust loopback without a token. A malicious page in another origin can still target local HTTP services. The custom header, same-origin read boundary, token, and Origin check close that path without adding user accounts.

### 5. Preserve native-only services behind typed web commands

The Rust process will continue to own voice capture and playback. The frontend will send focused push-to-talk press and release commands and display `VoiceEvent` state. Browser media permission and browser audio codecs are not added.

The working-directory control will call a typed endpoint that uses `spawn_blocking` to open the existing native folder picker. Only one dialog may be open. The response will distinguish confirmed selection, cancellation, and native dialog failure. The selected path will never change `project_root`.

Images will use a dedicated multipart endpoint with the existing accepted formats and decoded-size boundary. The server will normalize them into the existing attachment type before an agent command is created.

### 6. Add a constrained Cargo test service

Add a test service outside the frontend actor's rendering concerns. Discovery will invoke the integration target's list operation from fixed `project_root` and parse exact test identities. Execution will map a server-owned run kind to an argument vector:

- Full: `cargo test --workspace -j 1 -- --test-threads=1`
- Module: `cargo test -p deepseek-custom-tests --test it <module>:: -- --test-threads=1`
- Exact test: `cargo test -p deepseek-custom-tests --test it <name> -- --exact --test-threads=1`

The service will use `tokio::process::Command` directly. It will not invoke a shell or accept a path, command, argument list, or environment map from the browser. One mutex-protected run slot will allow one active process tree. The existing Windows process-group support will adopt Cargo so cancellation and parent shutdown reach descendants.

Stdout and stderr chunks will be timestamped and merged into one ordered stream. A parser will extract libtest totals and failed test names without treating parsing as the source of pass or failure. Exit status remains authoritative. Output will use a 4 MiB head-and-tail buffer with an omitted-byte counter. Terminal JSON results will be written atomically under `.deepseek/test-runs`, and retention will prune to 20 records.

Alternative considered: accept an arbitrary command field. That would duplicate a shell console and create an injection boundary unrelated to test visibility. Alternative considered: run tests in the browser. Cargo, native dependencies, and child-process cleanup require server-side execution.

### 7. Use deterministic service substitutes for browser tests

Add `playwright-rs` to the test crate. A browser harness will start the real web adapter on an ephemeral loopback port with a temporary project root. Test-only dependency injection will provide the existing scripted backend plus deterministic folder-picker, voice, clock where required, and test-executor substitutes. These seams remain behind `test-support` and live outside production behavior.

Tests will use role and accessible-name locators, condition-based waits, and server-observed terminal states. They will not use fixed sleeps. Each test will own its server, browser context, temporary files, and process tree. Failures will retain a screenshot, Playwright trace, browser console, and Rust server log in a test artifact directory.

The required viewport matrix is 360 by 800, 768 by 1024, and 1440 by 900. Tests will inspect page overflow and focus reachability as data, not only compare screenshots. Browser installation will use the version matched to the locked `playwright-rs` driver.

Alternative considered: test only HTTP DTOs. That would not prove responsive layout, semantic locators, event rendering, or the browser command path.

### 8. Remove native UI only after parity gates pass

Migration will temporarily keep a native entrypoint available while the application actor and web workspaces are built. Each milestone will test one running system. The final migration will make web startup the only production path, migrate relevant GUI tests to actor, HTTP, frontend, or Playwright coverage, then delete `crates/deepseek-custom/src/gui` and UI-only dependencies.

No permanent compatibility layer will remain. Pure formatting and transcript projection helpers may move to presentation-neutral modules when both runtime and tests still need their behavior. Native paint constants and paint-only test accessors will be deleted.

## Risks / Trade-offs

- [Large parity surface] -> Migrate by independently runnable workspace groups and keep a parity inventory tied to browser scenarios.
- [Event loss or duplicated state after reconnect] -> Use one actor, revisions, a replay buffer, snapshot reset, and reconnect tests during active operations.
- [Loopback request forgery] -> Require loopback binding, same-origin bootstrap, process token, custom request header, Origin validation, no CORS, and restrictive browser headers.
- [Frontend build adds Node tooling] -> Lock dependencies, use reproducible installs, embed production assets, and document the Rust and frontend gates separately.
- [Browser dependencies increase test setup] -> Keep Playwright out of production, provide a version-matched installer, and fail with direct setup guidance.
- [Cargo output can exhaust memory or disk] -> Stream through fixed bounds, retain only 20 atomic result records, and expose truncation counts.
- [Cancellation can leave compilers or tests running] -> Adopt the Cargo process tree and verify cancellation and parent-exit cleanup on Windows.
- [Native dialogs block an async worker] -> Run one picker on a blocking thread and expose pending, cancelled, and failed states.
- [Focused push to talk differs from a native window] -> Keep capture in Rust and specify that keyboard control applies while the browser app is focused.
- [Temporary dual frontend paths can drift] -> Keep the overlap short, gate each web workspace before moving on, and remove native production code in the final milestone.

## Migration Plan

1. Characterize the current native behaviors and extract the application actor while keeping native startup working. Practice test: run the current binary and drive a chat turn, session switch, settings save, and Procedure event through actor-backed state.
2. Add loopback server startup, security middleware, snapshot and event transport, embedded shell, and a minimal responsive navigation frame. Practice test: start the Rust server, load its reported URL, reload it, and verify revision continuity.
3. Move Chat, Sessions, Settings, backend selection, attachments, folder selection, and voice controls to the web frontend. Practice test: complete a deterministic browser chat, reconnect during streaming, defer a session switch, save a setting, choose and cancel folders, and submit an image.
4. Move Autopilot, Cascade, Evolve, and Procedure to the web frontend. Practice test: run deterministic success, stop, failure, and Procedure review paths from a browser at desktop and narrow widths.
5. Add test discovery, constrained execution, streaming output, cancellation, parsing, and retention. Practice test: discover the actual integration catalogue, run one exact passing test, run a deterministic failing executor case, cancel a long case, and reload its retained results.
6. Complete the `playwright-rs` critical-flow and responsive matrix, add failure artifacts, and package the production assets. Practice test: install the matched browser and run the focused browser command from a clean frontend dependency install.
7. Make web startup the sole production path. Remove `egui`, `eframe`, native GUI modules, native paint tests, and temporary dual-path wiring. Update `run.ps1`, architecture docs, visual evidence, and commands. Practice test: build and start the production binary without a frontend development server, exercise every workspace, then run all Rust, frontend build, formatting, Clippy, strict OpenSpec, coverage, and browser gates.

Rollback before step 7 is selection of the still-present native entrypoint. Rollback after step 7 is a source revert of the final removal milestone. Persisted settings and session formats remain compatible across both paths, so rollback needs no data migration.
