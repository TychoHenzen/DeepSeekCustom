## Purpose

Provide a deterministic acceptance fixture that proves a procedure can consume a valid small OpenSpec proposal, promote a verified edit, and explain localization failures without touching the real repository.

## ADDED Requirements

### Requirement: The acceptance fixture uses a valid isolated proposal

The procedure acceptance test SHALL create a disposable project containing a minimal Rust source target, an unrelated source file, and a valid OpenSpec change with one unchecked task and one capability delta. The fixture SHALL validate the proposal before any model or patch dispatch.

#### Scenario: Minimal proposal passes the preflight gate

- **WHEN** the acceptance fixture starts with its generated proposal and task
- **THEN** strict OpenSpec validation succeeds, the task contract is selected, and the repository index contains the target and unrelated files

#### Scenario: Invalid proposal stops before execution

- **WHEN** the fixture removes or corrupts a required OpenSpec artifact
- **THEN** the procedure reports the exact validation failure and performs no localization, patch, verifier, or promotion action

### Requirement: A passing fixture proves the complete procedure lifecycle

The acceptance test SHALL execute validation, repository indexing, localization, review approval, agreement sampling, route selection, patch generation, isolated verification, promotion, and report persistence in that order. The final result SHALL identify a successful terminal disposition and evidence for each executed stage.

#### Scenario: One simple task is promoted end to end

- **WHEN** deterministic fixture dispatchers return a valid indexed target, a valid patch, and passing verifier commands
- **THEN** the procedure approves the localization, verifies the patch in a disposable workspace, promotes only the localized target, and reloads a report showing successful terminal evidence

#### Scenario: Stage order and dispatch boundaries are recorded

- **WHEN** the passing fixture completes
- **THEN** captured events show validation and localization before patch work, isolated verification before promotion, no frontier dispatch for a mechanical local success, and no skipped verifier gate

### Requirement: Localization failures remain diagnosable and bounded

The acceptance test SHALL cover a localization response that names an indexed path with a symbol absent from that path. The procedure SHALL retain the exact structural rejection, apply its configured bounded retry policy, and stop before patch or verifier work when the retry is also invalid.

#### Scenario: Invalid symbol produces the known immediate failure

- **WHEN** both deterministic localization responses report a symbol that is not present under the indexed path
- **THEN** the run ends as failed after the allowed localization attempts, includes `symbol is not present under the indexed path`, and performs no patch, verifier, or promotion dispatch

#### Scenario: One invalid response is repaired by the bounded retry

- **WHEN** the first localization response reports the invalid symbol and the second response reports the indexed path without that symbol
- **THEN** the run records both attempts, reaches review with the repaired target, and proceeds only after explicit approval

### Requirement: The fixture proves workspace and evidence isolation

The acceptance test SHALL compare file content or hashes before and after each run. A successful run SHALL change only the localized sandbox target after promotion. A failed or interrupted run SHALL leave both the sandbox's unrelated file and the real repository unchanged.

#### Scenario: Promotion is limited to the localized target

- **WHEN** the passing fixture promotes its verified patch
- **THEN** the target contains the expected edit, the unrelated sandbox file retains its original bytes, and no path outside the target changes

#### Scenario: Failure does not mutate source files

- **WHEN** localization fails or the run is interrupted before promotion
- **THEN** all sandbox files and the real repository retain their pre-run content, including the existing dirty `settings.json`

### Requirement: The acceptance test is runnable without live model services

The fixture SHALL use deterministic in-process dispatch seams and temporary directories. It SHALL not require network access, Ollama, Codex, a user credential, or mutation of the repository's OpenSpec files.

#### Scenario: The test runs offline

- **WHEN** the acceptance test executes in an environment without live model services
- **THEN** it completes using fixture dispatchers and reports the same stage, evidence, and terminal assertions
