# deepseek-custom/verifier-gated-patch-application Specification

## Purpose
Verify a previewed patch in isolation and promote it only when deterministic project gates prove it against the current workspace state.
## Requirements
### Requirement: Verification requires its named approved localization report
The system SHALL load the explicitly named localization run report and require `Approved`, the selected change and task, current fingerprints, and a preview derived from that same run. It SHALL reject `Pending`, `Rejected`, `LegacyUnreviewed`, missing, stale, or mismatched reports before snapshot creation, patch application, model work, or verifier command execution.

#### Scenario: Approved matching report enters verification
- **WHEN** the named localization report is approved, current, and matches the selected change, task, and preview
- **THEN** verification setup may begin

#### Scenario: Untrusted localization input stops verification
- **WHEN** the named localization report is pending, rejected, legacy-unreviewed, missing, stale, or mismatched
- **THEN** the system reports the exact state or mismatch and performs no patch, model, snapshot, or verifier work

### Requirement: Apply requires configured verifier gates
The system SHALL require at least one configured verification command and SHALL show the ordered command list before a patch can be applied.

#### Scenario: No verifier is configured
- **WHEN** a valid preview exists but the verifier command list is empty
- **THEN** Apply is unavailable and the Procedure view explains what is missing

#### Scenario: Verifier list is configured
- **WHEN** a valid preview exists with one or more verifier commands
- **THEN** Apply is available and shows the commands in execution order

### Requirement: Verification uses a disposable current-state snapshot
The system SHALL verify inside a disposable workspace built from the current working tree. It MUST exclude repository metadata, build outputs, and procedure run data.

#### Scenario: Uncommitted source is included
- **WHEN** the current working tree contains an uncommitted source change before Apply
- **THEN** the verification workspace contains that current source content

#### Scenario: Excluded data is not copied
- **WHEN** the verification workspace is created
- **THEN** repository metadata, build outputs, and procedure reports are absent from it

### Requirement: Deterministic gates decide success
The system SHALL apply and parse the patch in isolation, then run each configured format, compile or type, lint, and test command in order. Only exit status and deterministic command results SHALL decide whether a gate passed.

#### Scenario: Every gate passes
- **WHEN** patch application and every configured command exit successfully
- **THEN** the candidate becomes eligible for promotion

#### Scenario: A gate fails
- **WHEN** any patch or command gate fails
- **THEN** later gates do not run and the candidate is not eligible for promotion

### Requirement: Failed verification cannot change the real workspace
The system SHALL leave the real workspace unchanged after a failed gate, interrupted run, or verification setup error.

#### Scenario: Test command fails
- **WHEN** the test gate exits non-zero in the disposable workspace
- **THEN** the real workspace retains its pre-run file hashes

#### Scenario: Verification is interrupted
- **WHEN** the user interrupts a running gate
- **THEN** the child process stops, the candidate is rejected, and the real workspace is unchanged

### Requirement: Promotion checks for concurrent edits
The system SHALL compare every target file to its preview baseline immediately before promotion. Any mismatch MUST stop promotion.

#### Scenario: Baseline still matches
- **WHEN** all target file hashes match the preview baseline after verification
- **THEN** promotion may begin

#### Scenario: A target changed during verification
- **WHEN** any target file hash differs from the preview baseline
- **THEN** promotion is refused and the changed paths are listed

### Requirement: Promotion is all or nothing
The system SHALL place all verified file results into the real workspace as one recoverable transaction. A promotion error MUST restore every target to its pre-promotion content.

#### Scenario: Promotion succeeds
- **WHEN** every verified target can be installed
- **THEN** all target files match the verified workspace result

#### Scenario: Promotion fails partway
- **WHEN** any target cannot be installed after another target was installed
- **THEN** all targets are restored and the run reports promotion failure

### Requirement: Gate evidence is retained
The system SHALL record the patch result and each command's text, exit code, bounded output, duration, and final disposition.

#### Scenario: User inspects a failed run
- **WHEN** a verifier command fails
- **THEN** the Procedure view and saved report show the failing command and bounded diagnostic output

