# deepseek-custom/routed-patch-preview Specification

## Purpose
Route a localized coding step to a suitable model and produce an auditable patch preview without changing the user's workspace.
## Requirements
### Requirement: Patch preview requires a current localization report
The system SHALL load the explicitly named localization run report and require an `Approved` disposition, the selected change and task, and OpenSpec and target-file fingerprints that still match the current workspace. It SHALL reject `Pending`, `Rejected`, `LegacyUnreviewed`, missing, stale, or mismatched reports before route evaluation, patch construction, or model dispatch.

#### Scenario: Current report is accepted
- **WHEN** the named localization report is approved, belongs to the selected change and task, and its fingerprints still match
- **THEN** the system evaluates the edit route

#### Scenario: Unapproved or missing report is rejected
- **WHEN** the named report is pending, rejected, legacy-unreviewed, missing, or belongs to another change or task
- **THEN** the system refuses the preview before route evaluation or drafting-model dispatch and names the report state or mismatch

#### Scenario: Localization report is stale
- **WHEN** the selected OpenSpec artifact or target file changed after localization
- **THEN** the system refuses to draft a patch and requests a new localization run

### Requirement: Route decisions use deterministic difficulty signals
The system SHALL route only explicitly mechanical, single-file work to the local backend. It SHALL route multi-file, architectural, cross-cutting, subtle bug, and substantive logic work to the frontier backend. Raw token probability MUST NOT affect the route.

#### Scenario: Mechanical single-file step routes locally
- **WHEN** the spec slice requests a mechanical edit and localization selects one file with no higher-risk marker
- **THEN** the route selects the configured local backend

#### Scenario: Higher-risk step routes to frontier
- **WHEN** any frontier signal is present
- **THEN** the route selects the configured frontier backend and records the triggering signal

### Requirement: A user can override the automatic route
The system SHALL provide local and frontier route overrides for a preview run and SHALL record the automatic route beside the override.

#### Scenario: Local override is selected
- **WHEN** the user forces a local preview for a task that automatically routes to frontier
- **THEN** the local backend is used and the report marks the run as overridden

### Requirement: Patch output has one validated envelope
The system SHALL require a patch envelope containing the route metadata, target list, rationale, and unified diff. Local output SHALL be schema constrained. Every backend's output SHALL pass the same deterministic envelope and diff parser.

#### Scenario: Valid local envelope
- **WHEN** the local backend returns a schema-conforming envelope with a valid unified diff
- **THEN** the system accepts it as a preview candidate

#### Scenario: Frontier output is malformed
- **WHEN** the frontier backend returns content that the patch parser cannot accept
- **THEN** the system reports the parser error and does not present the content as a valid patch

### Requirement: A patch stays inside the localization boundary
The system SHALL reject a patch that creates, changes, renames, or deletes a path outside the validated localization allowlist.

#### Scenario: Patch touches only localized files
- **WHEN** every diff path is in the localization allowlist
- **THEN** the patch is eligible for preview

#### Scenario: Patch reaches an unlocalized file
- **WHEN** any diff path is outside the allowlist
- **THEN** the whole patch is rejected and the unexpected paths are listed

### Requirement: Preview exposes the route and does not edit
The system SHALL show and save the automatic route, applied override, all route signals, backend, model, target files, rationale, and diff. Preview generation MUST NOT change workspace files.

#### Scenario: User inspects a preview
- **WHEN** a valid patch preview is produced
- **THEN** the Procedure view exposes the route evidence and complete diff before any apply action exists

#### Scenario: Preview run leaves no workspace change
- **WHEN** preview generation succeeds, fails, or is interrupted
- **THEN** workspace file hashes are unchanged

