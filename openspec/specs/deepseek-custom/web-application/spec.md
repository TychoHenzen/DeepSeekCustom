## Purpose

Provide the complete DeepSeekCustom user interface through a responsive local web application while preserving the existing agent, session, settings, search, procedure, attachment, and voice behavior.

## Requirements

### Requirement: The application starts as a local web service
The system SHALL serve its production frontend and API from one Rust process bound only to a loopback interface. The production binary MUST NOT require a separate frontend development server.

#### Scenario: Normal production startup
- **WHEN** the user starts the production binary
- **THEN** the system binds an available loopback address
- **AND** reports the complete browser URL
- **AND** serves the application shell and API from that origin

#### Scenario: Preferred port is unavailable
- **WHEN** the configured or preferred loopback port cannot be bound
- **THEN** the system either binds another available loopback port and reports it or exits with the exact bind error
- **AND** it does not listen on a non-loopback interface

### Requirement: The web frontend is responsive and accessible
The system SHALL expose every primary workspace through semantic browser controls. Layouts MUST remain usable at viewport widths from 360 CSS pixels through desktop widths without hiding actions or requiring horizontal page scrolling.

#### Scenario: Desktop navigation
- **WHEN** the frontend is shown in a desktop viewport
- **THEN** Chat, Autopilot, Cascade, Evolve, Procedure, Sessions, Tests, and Settings are directly reachable
- **AND** the active workspace is programmatically identifiable

#### Scenario: Narrow navigation
- **WHEN** the frontend is shown at 360 CSS pixels wide
- **THEN** every primary workspace and its actions remain reachable
- **AND** content does not overlap, clip, or create horizontal page scrolling

#### Scenario: Keyboard and assistive navigation
- **WHEN** a user navigates interactive controls by keyboard or accessible role and name
- **THEN** focus order follows the visible workflow
- **AND** controls expose their name, role, state, errors, and disabled reason without relying only on colour

### Requirement: Browser state reflects one authoritative application state
The Rust process SHALL own application state independently of browser connections. Each client SHALL receive a complete initial snapshot followed by ordered state changes, and a reconnect MUST NOT restart or duplicate an active operation.

#### Scenario: Browser connects during an idle session
- **WHEN** a browser loads or reloads the application
- **THEN** it receives the current transcript, session, settings, selected workspace, and operation states

#### Scenario: Browser reconnects during active work
- **WHEN** a browser connection drops while a turn, search, procedure, Autopilot run, voice action, or test run is active
- **THEN** the Rust operation continues
- **AND** the reconnecting browser receives a consistent current snapshot before later state changes
- **AND** already-applied state changes are not applied twice

#### Scenario: A stale client sends a command
- **WHEN** a command targets an application revision that is no longer current
- **THEN** the system rejects the command with the current revision and a recoverable conflict result
- **AND** it does not partially apply the command

### Requirement: Chat and saved sessions preserve their lifecycle
The web application SHALL support text and image turns, streaming transcript blocks, interruption, new sessions, saved-session loading, and deletion. A session change requested during a turn MUST remain deferred until that turn ends.

#### Scenario: User sends a chat turn
- **WHEN** the user submits text or an accepted image attachment
- **THEN** the transcript shows the user content
- **AND** streamed reasoning, text, tool, notice, error, image, and terminal events appear in order
- **AND** active and terminal status remain visible

#### Scenario: User interrupts a turn
- **WHEN** the user activates Stop while a turn is active
- **THEN** the running backend receives the existing interrupt signal
- **AND** the transcript reaches an interrupted terminal state

#### Scenario: User changes sessions during a turn
- **WHEN** the user requests a new or saved session while a turn is active
- **THEN** the request remains pending until the turn reaches a terminal event
- **AND** the complete outgoing turn is saved under the outgoing session

### Requirement: Existing operational workspaces remain available
The web application SHALL expose the existing Autopilot, Cascade, Evolve, and Procedure commands, progress, review decisions, results, and stop controls without changing their backend contracts.

#### Scenario: User runs an operational workflow
- **WHEN** the user starts an Autopilot, Cascade, Evolve, or Procedure operation with valid inputs
- **THEN** the web application submits the corresponding existing command
- **AND** shows live progress and the terminal outcome
- **AND** disables only actions that would conflict with the active operation

#### Scenario: Procedure waits for review
- **WHEN** a Procedure run reaches a user review state
- **THEN** the web application shows the complete review evidence and available decisions
- **AND** sends the selected decision for the current run only

### Requirement: Settings preserve runtime and persistence boundaries
The web application SHALL expose existing backend, model, effort, context, style, voice, Procedure, output, and working-directory settings. It MUST preserve the existing `settings.json` schema and the distinction between fixed `project_root` and mutable `working_dir`.

#### Scenario: User changes an existing setting
- **WHEN** the user changes a valid setting
- **THEN** the active runtime receives the same setting effect as the native frontend
- **AND** the setting persists through the existing settings path and schema
- **AND** secret values are never returned to the browser

#### Scenario: User requests a working-directory folder
- **WHEN** the user activates the web working-directory control
- **THEN** the Rust process opens a native folder picker on the local machine
- **AND** a confirmed folder changes only `working_dir`
- **AND** cancellation leaves runtime and persisted settings unchanged

### Requirement: Attachments and voice controls remain usable
The web application SHALL accept supported pasted, dropped, and selected image attachments. It SHALL expose the existing voice state and focused push-to-talk controls without moving model execution into the browser.

#### Scenario: User attaches an image
- **WHEN** the user pastes, drops, or selects a supported image within documented size and format limits
- **THEN** the application previews the image and applies the existing backend-specific attachment behavior on submission

#### Scenario: User uses push to talk
- **WHEN** voice is available and the focused web application receives the configured push-to-talk press and release
- **THEN** the existing Rust voice service starts and stops capture
- **AND** transcription, playback, errors, and readiness are shown in the web application

### Requirement: Local web commands are protected from other origins
The system SHALL reject state-changing browser requests unless they come from the served origin and carry the current process-scoped request token. It MUST NOT enable cross-origin API access or expose credentials through frontend state.

#### Scenario: Same-origin command is valid
- **WHEN** the served frontend sends a state-changing request with the current process token and current revision
- **THEN** the system evaluates the command against normal application rules

#### Scenario: Another origin attempts a command
- **WHEN** a request has a missing or foreign origin, missing or invalid process token, or a cross-origin preflight
- **THEN** the system rejects it before command dispatch
- **AND** returns no secret settings or application data
