# deepseek-custom/controlled-development-mode Specification

## Purpose
Provide an optional, session-scoped development workflow that keeps write-capable agent work outside the real workspace until one bounded Work Card passes deterministic checks and receives one explicit approval.
## Requirements
### Requirement: Controlled Development has an explicit lifecycle
The system SHALL represent Controlled Development with the phases `Off`, `Planning`, `AwaitingApproval`, `Executing`, `Completed`, `Blocked`, and `Interrupted`. The mode SHALL be optional, SHALL apply only to the owning top-level session, and SHALL leave normal chat behavior unchanged while `Off`.

#### Scenario: Mode is off
- **WHEN** Controlled Development is disabled for a session
- **THEN** top-level messages use the existing chat lifecycle without a Work Card gate

#### Scenario: Mode starts planning
- **WHEN** the user enables Controlled Development and submits a development request
- **THEN** that session enters `Planning` before backend dispatch
- **AND** no approval from an earlier packet is available

### Requirement: A Work Card is strict and bounded
A proposed Work Card SHALL contain only `id`, `outcome`, `proof_commands`, `production_paths`, `supporting_paths`, `excluded`, and `complexity_exceptions`. `outcome` SHALL name one observable result. `proof_commands` SHALL contain one to three exact nonempty commands. `production_paths` SHALL contain at most three exact repository-relative paths. `supporting_paths` SHALL contain exact repository-relative test, fixture, documentation, or generated-state paths. Paths MUST reject globs, absolute paths, traversal, non-normalized separators, and duplicate entries. `excluded` SHALL state what the packet must not change. `complexity_exceptions` SHALL name every approved new dependency, framework, service, storage layer, global state item, generated-code system, or generic abstraction. An empty `complexity_exceptions` list SHALL prohibit every such addition.

#### Scenario: Valid Work Card awaits approval
- **WHEN** planning returns exactly the required fields with all values inside their bounds
- **THEN** the system stores the card and enters `AwaitingApproval`

#### Scenario: Malformed Work Card is rejected
- **WHEN** planning returns missing fields, unknown fields, invalid value types, invalid paths, a non-observable or empty outcome, zero or more than three proof commands, or more than three production paths
- **THEN** the system enters `Blocked` with the structural errors
- **AND** it does not recover a card by parsing prose heuristically

### Requirement: Planning cannot change the real workspace
Planning SHALL run the selected backend against read-only access to the current workspace bytes. Where the harness owns tools, it MUST expose only read-capable tools rooted at the planning workspace. Planning MUST NOT expose or accept `Task`, subagent `SendMessage`, `CloseSession`, or any other subagent-dispatch path. Backend reasoning, tool events, and assistant text SHALL remain available as raw details.

#### Scenario: Planning cannot change the real workspace
- **WHEN** a planning backend attempts a write or subagent dispatch
- **THEN** the action is unavailable or confined outside the real workspace
- **AND** every real workspace byte remains unchanged

### Requirement: Approval belongs to one card
Approval SHALL identify the current Work Card by its `id` and SHALL authorize execution, verification, and promotion only for that card. Approval MUST become invalid after rejection, interruption, completion, session change, card replacement, or process restart. A valid approval SHALL not require a second routine approval before promotion.

#### Scenario: Approval applies to one card only
- **WHEN** an approved card completes, fails, is interrupted, or is replaced
- **THEN** the approval cannot authorize any later packet

#### Scenario: User rejects a card
- **WHEN** the user selects Reject for the current card
- **THEN** execution does not start
- **AND** the card approval is cleared

### Requirement: Execution uses one isolated current-state workspace
Execution SHALL create one session-owned disposable workspace from the exact current workspace bytes available at execution start, including unrelated dirty files needed by the task. Repository metadata, build outputs, and harness run data SHALL remain excluded by the existing snapshot rules. Every backend SHALL receive the disposable root through its working-directory boundary. Harness-owned execution tools SHALL be rooted there, and subagent dispatch SHALL remain unavailable.

#### Scenario: Passing packet starts from current dirty bytes
- **WHEN** the real workspace contains an unrelated dirty file before execution
- **THEN** the disposable workspace starts with the same bytes for that file
- **AND** later promotion does not overwrite it unless its exact path is an approved changed path

#### Scenario: Codex CLI cannot bypass the outer workspace boundary
- **WHEN** Codex CLI executes an approved card and writes through its child tools
- **THEN** its process working directory and writable workspace boundary are the disposable root
- **AND** no pre-promotion write reaches the real workspace

### Requirement: Changed paths must match the approved card
After the agent run and before proof commands, the system SHALL compare the disposable workspace to its execution-start inventory and list every created, modified, deleted, or renamed repository-relative path. Every changed path MUST appear in `production_paths` or `supporting_paths`. No more than three changed paths may come from `production_paths`.

#### Scenario: Changed path outside approved lists blocks promotion
- **WHEN** the disposable workspace contains a changed path absent from both approved path lists
- **THEN** the system enters `Blocked` and does not promote any path

#### Scenario: More than three production paths blocks promotion
- **WHEN** the disposable workspace contains changes to more than three approved production paths
- **THEN** the system enters `Blocked` and does not promote any path

### Requirement: Complexity exceptions gate dependency files
A changed dependency manifest or lockfile SHALL block promotion unless the path is approved and `complexity_exceptions` explicitly names the dependency or dependency-system change represented by that file change. Approval of another exception MUST NOT authorize an unrelated dependency change.

#### Scenario: Unapproved manifest or lockfile change blocks promotion
- **WHEN** an isolated change modifies a dependency manifest or lockfile without a matching named complexity exception
- **THEN** the system enters `Blocked` and leaves the real workspace unchanged

### Requirement: Every proof command must pass in isolation
The system SHALL run every approved `proof_commands` entry in card order from the disposable root after path and complexity checks. A command spawn failure, interruption, or nonzero exit SHALL fail verification. Later commands SHALL not run after the first failure.

#### Scenario: Failing proof command blocks promotion
- **WHEN** any approved proof command cannot start, is interrupted, or exits unsuccessfully
- **THEN** the system records that command and result, enters `Blocked` or `Interrupted` as applicable, and leaves the real workspace unchanged

#### Scenario: Passing packet promotes only approved paths
- **WHEN** every changed path is approved, every complexity rule passes, every proof command succeeds, and the promotion baseline remains current
- **THEN** the system promotes only the validated changed paths
- **AND** the phase becomes `Completed`

### Requirement: Promotion rejects overlapping concurrent edits
The system SHALL fingerprint every promotion target at execution start and compare it again immediately before and during transactional promotion. A changed real-workspace target MUST stop or roll back promotion. Concurrent changes to non-target paths SHALL remain untouched and SHALL not block promotion.

#### Scenario: Overlapping real-workspace changes block promotion without data loss
- **WHEN** a real-workspace promotion target changes after execution starts
- **THEN** promotion is refused or rolled back
- **AND** the concurrent real-workspace bytes remain intact
- **AND** no other packet path is promoted

### Requirement: Failed packets retain diagnostic evidence
Unexpected isolated changes, structural failures, proof failures, promotion conflicts, and interruptions SHALL leave the real workspace unchanged. The system SHALL retain the isolated diff and raw diagnostic output until the user starts another packet for that session or explicitly discards the retained evidence. It MUST NOT silently delete or revert unexpected isolated changes.

#### Scenario: Blocked packet remains inspectable
- **WHEN** a packet is blocked after it has an isolated workspace
- **THEN** `DIFF` and the UI expose the retained isolated changes
- **AND** starting another packet or explicit discard safely removes the old disposable workspace

### Requirement: Stop prevents promotion
`STOP` and the visible Stop action SHALL use the active backend and verifier interruption paths. A stop during planning, execution, or verification SHALL prevent all later dispatch, proof, and promotion work for that packet and SHALL end in `Interrupted`.

#### Scenario: STOP interrupts the backend and prevents promotion
- **WHEN** the user enters `STOP` or selects Stop during active Controlled Development work
- **THEN** the active child or in-process backend receives the interrupt
- **AND** no packet path is promoted
- **AND** the phase becomes `Interrupted`

### Requirement: Control inputs are handled by the harness
While Controlled Development is active, the exact input `STATUS` SHALL show the phase, current or approved card, and blocker. `MAP` SHALL show the current bounded system map. `DIFF` SHALL show the retained isolated or last promoted diff. `WHY <item>` SHALL explain only the named decision. `STOP` SHALL follow the interruption contract. These inputs SHALL be handled without starting a normal backend turn.

#### Scenario: User requests bounded control information
- **WHEN** the user enters `STATUS`, `MAP`, `DIFF`, or `WHY <item>` while Controlled Development is active
- **THEN** the system returns only the requested harness-owned state or explanation
- **AND** it does not consume approval or dispatch the backend

### Requirement: Controlled state is session-scoped and restart-safe
The system SHALL persist the current card, phase, compact evidence, and retained-workspace reference with the owning saved session. Loading another session MUST NOT transfer approval. Resetting or deleting a session SHALL interrupt its work and safely clean up its disposable workspace. A process restart MUST NOT resume execution, verification, or promotion automatically. A restored `Planning`, `AwaitingApproval`, or `Executing` packet SHALL become `Interrupted` with no valid approval.

#### Scenario: Reloaded in-flight sessions become Interrupted
- **WHEN** a saved session with an in-flight Controlled Development phase is loaded after process restart
- **THEN** its phase is `Interrupted`
- **AND** execution and promotion remain stopped
- **AND** its prior approval is invalid

#### Scenario: Session change does not transfer approval
- **WHEN** the user loads or creates another top-level session
- **THEN** the destination session has only its own Controlled Development state

### Requirement: Successful promotion updates the project snapshot
After successful promotion, the harness SHALL rewrite `PROJECT_STATE.md` at the repository root. The file SHALL contain at most 40 nonblank lines and only these sections: Current outcome, System map with at most ten named components, Last completed Work Card, Exact changed paths, Last proof commands and results, and Known blocker when one exists. The file MUST NOT contain a backlog, journal, architecture essay, or second OpenSpec plan. `PROJECT_STATE.md` is a harness-generated result and SHALL not count as an agent-produced card path.

#### Scenario: PROJECT_STATE remains within 40 nonblank lines
- **WHEN** a packet is promoted successfully
- **THEN** `PROJECT_STATE.md` is rewritten from current harness-owned state
- **AND** it has no more than 40 nonblank lines and no unapproved section

### Requirement: The existing web application exposes Controlled Development
The existing web application SHALL add a session-level Controlled Development toggle, Work Card view, Approve, Reject, and Stop actions, current phase, changed paths, proof results, compact result, and a collapsed raw-details view. It MUST NOT add a page, route hierarchy, frontend framework, or state library. Controls SHALL expose semantic names, states, errors, and disabled reasons.

#### Scenario: User reviews and approves a Work Card
- **WHEN** a valid card reaches `AwaitingApproval`
- **THEN** the existing application page shows the complete card and current phase
- **AND** Approve, Reject, and Stop target only that card and session

### Requirement: Compact output is bounded without hiding diagnostics
Each visible progress notice SHALL contain at most 80 whitespace-delimited words. Each visible completion summary SHALL contain at most 200 whitespace-delimited words and SHALL be built from phase, Work Card, changed paths, proof results, and remaining limitation. The system MUST NOT meet these limits by arbitrarily truncating backend text or failure output. Complete reasoning, tool events, assistant text, and diagnostics SHALL remain available in raw details.

#### Scenario: Visible progress and completion summaries respect their limits
- **WHEN** Controlled Development renders progress or a terminal result
- **THEN** progress notices contain at most 80 words and completion summaries contain at most 200 words
- **AND** failures are represented by harness-owned fields rather than hidden truncation

#### Scenario: Raw diagnostic output remains available
- **WHEN** backend or verifier output exceeds the compact summary limits
- **THEN** the compact view stays within its limit
- **AND** the complete retained raw details remain available through the collapsed details control
