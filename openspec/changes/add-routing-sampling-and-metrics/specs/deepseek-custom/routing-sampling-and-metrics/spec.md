## Purpose

Use bounded agreement and candidate sampling to improve local-model routing while retaining auditable metrics and privacy-limited tuning data.

## ADDED Requirements

### Requirement: Sampling requires its named approved localization report
The system SHALL load the explicitly named baseline localization run report and require `Approved`, the selected change and task, current fingerprints, and downstream state derived from that same run. It SHALL reject `Pending`, `Rejected`, `LegacyUnreviewed`, missing, stale, or mismatched reports before agreement sampling, candidate generation, patch work, verification, or model dispatch.

#### Scenario: Approved matching report enters sampling
- **WHEN** the named baseline localization report is approved, current, and matches the selected change, task, and downstream state
- **THEN** bounded agreement sampling may start

#### Scenario: Untrusted localization input stops sampling
- **WHEN** the named baseline localization report is pending, rejected, legacy-unreviewed, missing, stale, or mismatched
- **THEN** the system reports the exact state or mismatch and performs no sampling, candidate, patch, verifier, or model work

### Requirement: Localization uses bounded agreement sampling
The system SHALL run a configured 3 to 5 local localization samples and normalize each result to ordered repository path and symbol identities.

#### Scenario: Sample settings are inside bounds
- **WHEN** a run requests between 3 and 5 localization samples
- **THEN** exactly that many bounded local dispatches are attempted unless interrupted

#### Scenario: Sample settings are outside bounds
- **WHEN** configuration requests fewer than 3 or more than 5 samples
- **THEN** the system rejects the configuration before dispatch

### Requirement: Localization agreement controls escalation
The system SHALL compare normalized target sets and SHALL continue with the local result only when one exact set reaches the configured quorum. Otherwise it SHALL escalate localization to the frontier backend.

#### Scenario: Local samples reach quorum
- **WHEN** one normalized target set reaches the configured quorum
- **THEN** that set becomes the localization result and patch routing continues

#### Scenario: Local samples disagree
- **WHEN** no normalized target set reaches quorum
- **THEN** the frontier backend performs localization and the disagreement trigger is recorded

### Requirement: Local mechanical edits use bounded best-of-N
The system SHALL generate a configured 3 to 5 local patch candidates for work routed locally and SHALL test each candidate through the existing isolated verifier.

#### Scenario: Local candidates are generated
- **WHEN** a mechanical edit remains on the local route
- **THEN** the configured bounded candidate count is generated and each completed candidate receives a verifier result

#### Scenario: No local candidate passes
- **WHEN** every local candidate fails verification
- **THEN** the existing bounded repair and frontier escalation policy begins without increasing its budgets

### Requirement: Passing candidates are selected deterministically
The system SHALL select a passing candidate with the fewest changed lines. It SHALL use the lowest candidate index to break a tie.

#### Scenario: Two candidates pass
- **WHEN** two verified candidates pass and one changes fewer lines
- **THEN** the smaller passing patch is selected

#### Scenario: Passing patches have equal size
- **WHEN** multiple verified candidates change the same number of lines
- **THEN** the candidate with the lowest index is selected

### Requirement: Routing metrics are durable and inspectable
The system SHALL persist stage, route signals, backend, model, attempt counts, schema rejections, verifier outcomes, escalation triggers, available token usage, duration, and final disposition for every procedure run.

#### Scenario: Completed run updates metrics
- **WHEN** a procedure run reaches any terminal disposition
- **THEN** its metrics survive restart and appear in the Procedure view

### Requirement: Threshold warnings do not rewrite policy
The system SHALL calculate local mechanical success and frontier escalation rates. It SHALL show configured threshold warnings without silently changing routing settings.

#### Scenario: Escalation rate exceeds threshold
- **WHEN** the measured escalation rate exceeds its configured review threshold
- **THEN** the Procedure view shows the warning and leaves the route configuration unchanged

### Requirement: Exported localization traces protect workspace content
The system SHALL export normalized targets, route labels, outcomes, and numeric metrics for tuning. It MUST exclude prompts, source contents, credentials, and raw command output.

#### Scenario: Trace export is inspected
- **WHEN** the user exports localization traces
- **THEN** the export contains only the allowed structured fields and no excluded content

### Requirement: The completed procedure remains bounded end to end
The system SHALL execute OpenSpec validation, agreement localization, difficulty routing, bounded candidate generation, deterministic verification, bounded repair, and bounded frontier escalation as one observable run.

#### Scenario: Local end-to-end success
- **WHEN** localization reaches quorum and a local candidate passes verification
- **THEN** the candidate is promoted without a frontier call and all stages are recorded

#### Scenario: End-to-end escalation
- **WHEN** localization disagreement or exhausted local patch work triggers frontier use
- **THEN** the frontier path remains bounded and ends with either a verified promotion or a blocked report
