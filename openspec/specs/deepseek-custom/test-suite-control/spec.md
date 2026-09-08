## Purpose

Let users inspect and run the repository's Rust test suite from the web frontend with live progress, safe command boundaries, cancellation, and retained diagnostic results.

## Requirements

### Requirement: The test catalogue reflects the repository test target
The system SHALL discover tests from the fixed application `project_root` and present the workspace suite, integration modules, and individual tests. Discovery failures MUST remain visible and MUST NOT replace the last successful catalogue with an empty success state.

#### Scenario: Test discovery succeeds
- **WHEN** the user opens or refreshes the Tests workspace
- **THEN** the system discovers the current `deepseek-custom-tests` integration target
- **AND** groups individual test names by their module prefix
- **AND** shows the approved full-workspace run option

#### Scenario: Test discovery fails
- **WHEN** Cargo cannot compile or list the integration target
- **THEN** the Tests workspace shows the command, exit code, and bounded diagnostic output
- **AND** marks any older catalogue as stale

### Requirement: Test execution is constrained to approved suite shapes
The system SHALL run only the full workspace suite, one discovered integration module, or one exact discovered test. It MUST construct arguments from server-owned templates, run from the fixed `project_root`, and reject arbitrary commands or client-supplied paths.

#### Scenario: User runs the full suite
- **WHEN** the user selects the full workspace suite
- **THEN** the system runs the documented serialized workspace test command from `project_root`
- **AND** records the resolved argument vector and working directory

#### Scenario: User runs one module or test
- **WHEN** the user selects a discovered module or exact test
- **THEN** the system constructs the focused integration-target invocation from that catalogue identity
- **AND** no unvalidated shell text from the browser reaches process spawning

#### Scenario: Client submits an unknown test identity
- **WHEN** a client requests a test or module absent from the current catalogue
- **THEN** the system rejects the request without spawning a process

### Requirement: Test runs are serialized and observable
The system SHALL permit at most one active test process tree. It SHALL stream ordered, bounded output and update running totals and elapsed time while retaining the exact terminal exit status.

#### Scenario: A test run starts
- **WHEN** no test run is active and a valid run is requested
- **THEN** the Tests workspace shows its identity, command, start time, elapsed time, output, and running state

#### Scenario: Another run is requested concurrently
- **WHEN** a test run is already active
- **THEN** the system rejects the second request with the active run identity
- **AND** does not spawn a second test process

#### Scenario: A test run finishes
- **WHEN** the test process exits
- **THEN** the result records passed, failed, ignored, and filtered counts when Cargo reports them
- **AND** records the exit code, duration, failed test names, and diagnostic output
- **AND** distinguishes passed, failed, cancelled, and infrastructure-error outcomes

### Requirement: Test cancellation reaps the complete process tree
The system SHALL provide cancellation for an active test run and SHALL apply the repository's existing child-process cleanup boundary to Cargo and every descendant it starts.

#### Scenario: User cancels an active test run
- **WHEN** the user activates Cancel for the current run
- **THEN** the system terminates and reaps the active Cargo process tree
- **AND** records a cancelled terminal result
- **AND** permits a later test run to start

#### Scenario: Browser disconnects during a test run
- **WHEN** every browser disconnects while a test run is active
- **THEN** the test run continues until completion or explicit cancellation
- **AND** its current state is available after reconnect

### Requirement: Test results are retained with explicit limits
The system SHALL retain the 20 most recent terminal test runs under `.deepseek/test-runs` at the fixed `project_root`. Each run SHALL retain at most 4 MiB of combined process output while preserving its beginning and end with an explicit truncation marker.

#### Scenario: User revisits recent results
- **WHEN** the Tests workspace loads after one or more completed runs
- **THEN** it lists up to 20 newest results with identity, outcome, counts, duration, and timestamp
- **AND** the user can inspect the retained command and diagnostic output

#### Scenario: Retention limit is exceeded
- **WHEN** a twenty-first terminal result is stored
- **THEN** the oldest retained result is removed
- **AND** active or newer results remain intact

#### Scenario: Output limit is exceeded
- **WHEN** one run emits more than 4 MiB of combined output
- **THEN** retained and streamed output stay within the limit
- **AND** the visible result identifies the omitted byte count
- **AND** preserves diagnostic content from both the start and end of the run

### Requirement: Test results do not imply repository readiness
The Tests workspace SHALL report only the selected invocation's observed outcome. It MUST NOT label the repository, an OpenSpec change, or a checkpoint complete based only on a passing test run.

#### Scenario: Selected tests pass
- **WHEN** a focused module or individual test exits successfully
- **THEN** the result names that exact scope as passed
- **AND** does not claim that unrun checks passed
