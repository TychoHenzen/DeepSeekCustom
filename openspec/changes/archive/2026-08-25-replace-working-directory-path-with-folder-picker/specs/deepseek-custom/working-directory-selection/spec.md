## Purpose

This capability lets users select the agent working directory without typing or validating a raw filesystem path.

## ADDED Requirements

### Requirement: Settings provide folder-based working-directory selection
The settings panel SHALL show the current working directory and SHALL provide a folder picker instead of an editable path field.

#### Scenario: User opens the folder picker
- **WHEN** the user activates the working-directory selection control
- **THEN** the system opens a folder picker initialized to the current working directory when that directory is available

#### Scenario: User selects a folder
- **WHEN** the user confirms an existing folder in the picker
- **THEN** the system updates the active agent working directory to that folder
- **AND** the settings panel displays the selected folder
- **AND** the system persists the selected folder in the existing settings format

#### Scenario: User cancels folder selection
- **WHEN** the user closes or cancels the folder picker without selecting a folder
- **THEN** the active working directory remains unchanged
- **AND** no working-directory setting is persisted for the cancelled selection

### Requirement: Folder selection preserves working-directory boundaries
Changing the selected working directory SHALL affect the shared agent working-directory state without changing the fixed project root used to locate `settings.json`.

#### Scenario: Selected directory differs from the project root
- **WHEN** the user selects a folder outside the fixed project root
- **THEN** agent file and shell operations use the selected folder
- **AND** settings persistence continues to use the fixed project root
