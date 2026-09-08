## Purpose

Provide deterministic Rust-driven browser automation contracts that verify the web frontend's behavior, accessibility, responsive layouts, and integration with application services.

## Requirements

### Requirement: Browser controls have stable semantic identities
The frontend SHALL expose user-facing controls through accessible roles, names, labels, and state. Automation MUST NOT depend on generated CSS class names, visual coordinates, timing sleeps, or internal component structure.

#### Scenario: Automation locates a primary action
- **WHEN** a `playwright-rs` test locates a workspace, form control, action, status, or result
- **THEN** it can use an accessible role and name or an explicit documented test identifier
- **AND** the locator remains independent of layout width

#### Scenario: A control is unavailable
- **WHEN** an action is disabled by application state
- **THEN** automation can inspect its disabled state and visible reason without attempting the action

### Requirement: End-to-end tests run against isolated deterministic services
The browser harness SHALL start the real Rust web adapter on an ephemeral loopback port with a temporary project root and deterministic substitutes for external models, native dialogs, audio hardware, and test child processes. It MUST NOT read or change the user's settings, sessions, source files, credentials, or running app.

#### Scenario: Browser test environment starts
- **WHEN** an end-to-end test starts
- **THEN** it receives a unique temporary project root, process token, and loopback URL
- **AND** deterministic service events can drive success, failure, review, interruption, and reconnect states

#### Scenario: Browser test environment stops
- **WHEN** an end-to-end test passes, fails, times out, or is cancelled
- **THEN** its server, browser, and child process trees are reaped
- **AND** the real checkout and user configuration remain unchanged

### Requirement: Browser tests cover critical workflows
The `playwright-rs` suite SHALL cover startup, navigation, chat streaming, stop, session switching, settings persistence, folder selection, attachments, voice state, Autopilot, Cascade, Evolve, Procedure review, test discovery, test execution, cancellation, result retention, and reconnect behavior.

#### Scenario: Critical workflow contract is changed
- **WHEN** implementation changes one of the listed critical workflows
- **THEN** a browser test exercises the observable browser behavior and its Rust service boundary

#### Scenario: Browser and service disagree
- **WHEN** the frontend displays state that does not match the server snapshot or command result
- **THEN** the browser test fails with the mismatched observable values

### Requirement: Responsive layouts have automated visual evidence
The browser suite SHALL exercise at least 360 by 800, 768 by 1024, and 1440 by 900 CSS-pixel viewports. It SHALL check for hidden actions, overlap, clipping, horizontal page overflow, and inaccessible focus order.

#### Scenario: Responsive matrix passes
- **WHEN** the responsive suite renders every primary workspace at each required viewport
- **THEN** required actions are visible or reachable through the responsive navigation
- **AND** the document has no horizontal page overflow
- **AND** focus can reach each primary action

#### Scenario: Responsive check fails
- **WHEN** a responsive or end-to-end assertion fails
- **THEN** the harness retains a screenshot, browser trace, console output, and server log for that test

### Requirement: Browser prerequisites and failures are explicit
The project SHALL provide a version-matched browser installation command and a focused browser-test command. A missing or incompatible browser runtime MUST fail with installation guidance rather than silently skipping browser coverage.

#### Scenario: Browser runtime is missing
- **WHEN** the browser suite starts without its required browser runtime
- **THEN** it exits unsuccessfully
- **AND** reports the version-matched installation command

#### Scenario: Focused browser suite runs
- **WHEN** a developer invokes the documented focused command with installed prerequisites
- **THEN** the harness builds or locates the frontend assets
- **AND** starts the isolated Rust server
- **AND** runs the `playwright-rs` tests without external model calls
