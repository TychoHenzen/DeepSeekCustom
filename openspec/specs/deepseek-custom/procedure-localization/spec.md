# deepseek-custom/procedure-localization Specification

## Purpose
Provide a read-only coding procedure that turns a valid OpenSpec change into a bounded, machine-validated repository localization report.

## Requirements
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

### Requirement: Task contract selection is unambiguous
The system SHALL select an unbound task automatically only when its change
contains exactly one capability delta. An unbound task in a change with zero
or multiple capability deltas MUST fail before repository indexing or model
dispatch and MUST identify the task, change, and number of deltas. A `covers`
binding SHALL remain the explicit way to select a requirement and scenario in
a multi-capability change.

#### Scenario: One capability supports an unbound task
- **WHEN** an unchecked task has no `covers` binding and its change contains exactly one capability delta
- **THEN** the system selects that complete capability delta as the task contract

#### Scenario: Multiple capabilities require a binding
- **WHEN** an unchecked task has no `covers` binding and its change contains multiple capability deltas
- **THEN** the system reports that the task needs exactly one capability delta and makes no model request

### Requirement: Repository index boundaries are explicit
The system SHALL default one procedure repository index to at most 10,000
files and 64 MiB of inspected file content. Both limits SHALL be configurable
in procedure settings. A configured limit MUST replace its default, and an
overflow MUST stop localization with the configured limit and first sorted
overflow path in the error.

#### Scenario: Missing index settings use documented defaults
- **WHEN** procedure settings omit repository-index limits
- **THEN** the effective limits are 10,000 files and 64 MiB

#### Scenario: Configured index settings replace defaults
- **WHEN** procedure settings contain valid repository-index limits
- **THEN** the system uses those exact limits for the next localization run

#### Scenario: Repository index exceeds a configured limit
- **WHEN** repository indexing exceeds either configured limit
- **THEN** localization stops before model dispatch and reports the limit and first sorted overflow path

### Requirement: Conservative Rust symbols retain a path-only fallback
The system SHALL index ordinary ASCII Rust item identifiers for structs,
enums, traits, unions, type aliases, constants, statics, modules, functions,
and `macro_rules` declarations. A valid Rust file containing an identifier
outside that supported subset MUST remain selectable as a path-only target.
The system MUST NOT invent or normalize an unsupported identifier.

#### Scenario: Supported ASCII Rust items expose symbols
- **WHEN** an indexed Rust file declares supported items with ASCII identifiers
- **THEN** the repository index exposes their exact identifiers as symbols

#### Scenario: Unicode Rust identifier falls back to its path
- **WHEN** a valid Rust file declares an item whose identifier is outside the supported ASCII subset
- **THEN** the repository index retains the file path without claiming that identifier as a selectable symbol

### Requirement: Localizer output is schema constrained
The system SHALL use a configured local backend that supports
schema-constrained output and SHALL require a final JSON localization result
with target path, optional symbol, and evidence fields. Ollama localization
requests MUST omit provider-native reasoning controls and use the selected
model's default reasoning behavior.

#### Scenario: Ollama receives the localization schema
- **WHEN** an Ollama backend performs localization
- **THEN** the request carries a JSON Schema through the provider's structured-response field

#### Scenario: Backend cannot constrain output
- **WHEN** the configured localization backend cannot enforce the localization schema
- **THEN** the system rejects the configuration before dispatch and names the unsupported backend

#### Scenario: Ollama model lacks native thinking control
- **WHEN** an Ollama model performs localization regardless of the shared effort setting
- **THEN** the request omits native reasoning fields and remains eligible for schema-constrained dispatch

### Requirement: Every localization target exists
The system SHALL mark a localization result structurally valid only when every
repository-relative path is present in the current index and every reported
symbol is present under its reported path. Structural validity MUST NOT imply
semantic correctness. A structurally valid target set SHALL remain pending
review until the user accepts or rejects it.

#### Scenario: All reported targets are valid
- **WHEN** every returned path and symbol matches the repository index
- **THEN** the system records the target set as structurally valid and awaiting review

#### Scenario: A target is invented
- **WHEN** a returned path or symbol does not exist in the repository index
- **THEN** the system rejects the whole result and reports each invalid target

#### Scenario: Structurally valid targets are semantically wrong
- **WHEN** the user reviews a structurally valid target set and rejects it
- **THEN** the run records the rejection and cannot be consumed as a completed localization

#### Scenario: Structurally valid targets are approved
- **WHEN** the user reviews a structurally valid target set and accepts it
- **THEN** the run records the approval and becomes an approved localization report

### Requirement: Invalid localization has one bounded retry
The system SHALL retry one malformed or invalid localization result once with the deterministic validation error. It SHALL stop after the second invalid result.

#### Scenario: Retry repairs invalid output
- **WHEN** the first result is invalid and the retry returns a valid result
- **THEN** the system accepts the retry and records two attempts

#### Scenario: Retry budget is exhausted
- **WHEN** both localization attempts are invalid
- **THEN** the run ends as failed without another model request

### Requirement: Localization is observable and non-mutating
The system SHALL show and save the selected change, targets, evidence,
backend, model, attempts, structural validation, review decision, and final
status. A localization run and either review decision MUST NOT change
workspace source files. The Procedure view SHALL expose distinct running,
review, approved, rejected, failed, and interrupted states.

#### Scenario: Completed report is inspectable
- **WHEN** localization is structurally valid and the user approves its targets
- **THEN** the Procedure view and saved run report expose the approved targets, review decision, and dispatch details

#### Scenario: Workspace remains unchanged
- **WHEN** localization succeeds structurally, fails, is interrupted, or receives either review decision
- **THEN** hashes of workspace files match their values before the run

#### Scenario: Procedure review states are visually inspectable
- **WHEN** the Procedure view is rendered through its documented visual QA procedure
- **THEN** its controls, progress, evidence, review actions, errors, interruption, and terminal states are visible without overlap or clipping
