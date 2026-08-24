## Purpose

Provide a read-only coding procedure that turns a valid OpenSpec change into a bounded, machine-validated repository localization report.

## ADDED Requirements

### Requirement: A procedure run starts from a valid OpenSpec change
The system SHALL require an existing OpenSpec change and SHALL run strict, non-interactive validation before dispatching a localization model.

#### Scenario: Valid change starts localization
- **WHEN** the user starts a procedure run for an existing change that passes strict validation
- **THEN** the system begins Stage 1 localization and records the validation result

#### Scenario: Invalid change stops before model use
- **WHEN** the selected change is missing or fails strict validation
- **THEN** the system reports the exact validation failure and makes no model request

### Requirement: Localization context is short and typed
The system SHALL construct each localization request from the relevant OpenSpec slice, a repository index, and a typed scratchpad. It MUST NOT include the full conversation transcript.

#### Scenario: Captured request contains only stage context
- **WHEN** a localization request is captured during a test run
- **THEN** it contains the selected requirement and scenario, indexed targets, and scratchpad fields without unrelated conversation messages

#### Scenario: Key instruction is not buried
- **WHEN** the localization prompt is built
- **THEN** its target-selection instruction appears at the start or end of the prompt

### Requirement: Localizer output is schema constrained
The system SHALL use a configured local backend that supports schema-constrained output and SHALL require a final JSON localization result with target path, optional symbol, and evidence fields.

#### Scenario: Ollama receives the localization schema
- **WHEN** an Ollama backend performs localization
- **THEN** the request carries a JSON Schema through the provider's structured-response field

#### Scenario: Backend cannot constrain output
- **WHEN** the configured localization backend cannot enforce the localization schema
- **THEN** the system rejects the configuration before dispatch and names the unsupported backend

### Requirement: Every localization target exists
The system SHALL accept only repository-relative paths present in the current index and symbols present under their reported path.

#### Scenario: All reported targets are valid
- **WHEN** every returned path and symbol matches the repository index
- **THEN** the system accepts the localization result

#### Scenario: A target is invented
- **WHEN** a returned path or symbol does not exist in the repository index
- **THEN** the system rejects the whole result and reports each invalid target

### Requirement: Invalid localization has one bounded retry
The system SHALL retry one malformed or invalid localization result once with the deterministic validation error. It SHALL stop after the second invalid result.

#### Scenario: Retry repairs invalid output
- **WHEN** the first result is invalid and the retry returns a valid result
- **THEN** the system accepts the retry and records two attempts

#### Scenario: Retry budget is exhausted
- **WHEN** both localization attempts are invalid
- **THEN** the run ends as failed without another model request

### Requirement: Localization is observable and non-mutating
The system SHALL show and save the selected change, targets, evidence, backend, model, attempts, and final status. A localization run MUST NOT change workspace files.

#### Scenario: Completed report is inspectable
- **WHEN** localization succeeds
- **THEN** the Procedure view and saved run report expose the validated targets and dispatch details

#### Scenario: Workspace remains unchanged
- **WHEN** localization succeeds, fails, or is interrupted
- **THEN** hashes of workspace files match their values before the run
