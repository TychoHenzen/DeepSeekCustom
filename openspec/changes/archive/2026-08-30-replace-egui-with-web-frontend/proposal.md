## Why

The native `egui` frontend cannot be exercised through browser automation and ties application state, process control, and rendering into one `DeepSeekGui` object. Replacing it with a responsive web application will make every user workflow automatable with `playwright-rs` and will add direct visibility and control for the repository test suite.

## What Changes

- **BREAKING** Replace the native `eframe` window with a loopback-only Rust HTTP service and a responsive TypeScript web frontend.
- Preserve all current user workflows: Chat, Autopilot, Cascade, Evolve, Procedure, Sessions, settings, backend and model selection, attachments, voice controls, interruption, and folder selection.
- Separate application state and commands from presentation so browser clients can reconnect without losing an active turn or procedure run.
- Add a test dashboard that discovers the repository's Rust tests, runs approved focused or full-suite commands, streams bounded output, supports cancellation, and stores bounded run results under `.deepseek/test-runs`.
- Add a `playwright-rs` browser test harness with semantic locators, deterministic test seams, responsive viewport coverage, and failure artifacts.
- Remove `egui`, `eframe`, their UI-only dependencies, native paint code, and obsolete GUI-only test seams after web parity is proven.
- Keep the existing `settings.json`, session records, backend behavior, process-group cleanup, and fixed `project_root` versus mutable `working_dir` boundary compatible.

## Capabilities

### New Capabilities

- `deepseek-custom/web-application`: Loopback web serving, responsive feature-complete frontend behavior, state synchronization, reconnect behavior, local security, and packaged startup.
- `deepseek-custom/test-suite-control`: Test discovery, constrained execution, live progress, cancellation, bounded output, and retained results.
- `deepseek-custom/web-frontend-automation`: Stable browser contracts and `playwright-rs` end-to-end coverage for user workflows and responsive layouts.

### Modified Capabilities

None. Existing behavioral capabilities remain in force and move behind the new web presentation boundary without changing their contracts.

## Impact

- Production entrypoint and runtime wiring in `crates/deepseek-custom/src/main.rs`.
- Native UI modules under `crates/deepseek-custom/src/gui` will be replaced by presentation-neutral application services and a web adapter.
- A new TypeScript frontend workspace and build pipeline will produce static assets served by the Rust binary.
- New Rust HTTP, streaming, serialization, test-runner, and static-asset dependencies will replace `eframe`, `egui`, `egui_commonmark`, and other native UI-only dependencies.
- Integration tests in `crates/deepseek-custom-tests` will move from GUI state and paint seams to service-level tests and `playwright-rs` browser tests.
- `run.ps1`, build documentation, visual evidence, and maintained architecture documentation will change from native-window startup to local web startup.
- Browser installation for end-to-end tests becomes an explicit development and CI prerequisite. Production use will not require Playwright.
