# deepseek-custom/bounded-repair-escalation Specification

## Purpose
Recover from deterministic patch failures through a bounded local repair budget, then a bounded frontier escalation with a clear final handoff.
## Requirements
### Requirement: Repair requires its named approved localization report
The system SHALL load the explicitly named localization run report and require `Approved`, the selected change and task, current fingerprints, and patch state derived from that same report. It SHALL reject `Pending`, `Rejected`, `LegacyUnreviewed`, missing, stale, or mismatched reports before parser retry, patch work, verifier execution, or local or frontier model dispatch.

#### Scenario: Approved matching report enters the repair ladder
- **WHEN** the named localization report is approved, current, and matches the selected change, task, and patch state
- **THEN** the bounded repair state machine may start

#### Scenario: Untrusted localization input stops repair
- **WHEN** the named localization report is pending, rejected, legacy-unreviewed, missing, stale, or mismatched
- **THEN** the system reports the exact state or mismatch and performs no parser retry, patch, verifier, local-model, or frontier-model work

### Requirement: Structural failures receive one local retry
The system SHALL retry one schema, envelope, or patch-parse failure on the local backend with the exact deterministic parser error. A second structural failure SHALL exhaust this retry path.

#### Scenario: Parser retry succeeds
- **WHEN** the first local candidate has a structural error and the retry is valid
- **THEN** verification continues with the repaired candidate

#### Scenario: Parser retry fails again
- **WHEN** the retry also has a structural error
- **THEN** the system records structural retry exhaustion and proceeds to the configured escalation policy

### Requirement: Verifier failures have a bounded local repair budget
The system SHALL allow at most three local verifier-driven patch attempts by default. Each repair request SHALL include the exact failing command, exit code, bounded output, spec slice, targets, and typed scratchpad.

#### Scenario: Local repair passes within budget
- **WHEN** a local repair candidate passes every gate on attempt three or earlier
- **THEN** it is promoted through the verification gate and no frontier request is made

#### Scenario: Local budget is exhausted
- **WHEN** three local candidates fail deterministic verification
- **THEN** no fourth local candidate is requested

### Requirement: Every repair starts from a fresh verification workspace
The system SHALL discard the failed candidate workspace before testing the next candidate.

#### Scenario: Prior failed files cannot leak
- **WHEN** attempt one changes a file and fails before attempt two
- **THEN** attempt two starts from the original current-state snapshot plus only attempt two's patch

### Requirement: Exhausted local work escalates the same task
The system SHALL send the same OpenSpec slice, targets, scratchpad, and accumulated deterministic failures to the configured frontier backend after local exhaustion.

#### Scenario: Frontier receives accumulated evidence
- **WHEN** local verification exhausts its budget
- **THEN** the frontier request identifies every local attempt and its deterministic failure without including the full transcript

### Requirement: Frontier repair is also bounded
The system SHALL allow at most two frontier patch attempts by default and SHALL verify each attempt through the same gates as a local candidate.

#### Scenario: Frontier candidate passes
- **WHEN** a frontier candidate passes all gates within its budget
- **THEN** it is promoted and the run records a successful escalation

#### Scenario: Frontier budget is exhausted
- **WHEN** both frontier candidates fail verification
- **THEN** the run ends blocked, exposes the accumulated evidence, and leaves the workspace unchanged

### Requirement: Interruption cancels the ladder
The system SHALL stop the active model or verifier process after interruption and MUST NOT start another attempt or promote a candidate.

#### Scenario: User interrupts during repair
- **WHEN** interruption occurs during any local or frontier attempt
- **THEN** the run ends interrupted with no later dispatch and no workspace promotion

### Requirement: Retry and escalation decisions are visible
The system SHALL record each attempt number, backend, model, trigger, error category, verifier result, and disposition.

#### Scenario: User inspects the ladder
- **WHEN** a run uses one or more repairs or an escalation
- **THEN** the Procedure view and saved report show the complete bounded attempt sequence
