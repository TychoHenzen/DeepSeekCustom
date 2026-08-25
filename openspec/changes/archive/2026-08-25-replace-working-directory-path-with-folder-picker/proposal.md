## Why

The settings panel requires users to type an exact working-directory path. A native folder picker makes directory selection discoverable and prevents invalid path input.

## What Changes

- Replace the editable working-directory path field with a folder-selection control.
- Start the picker at the current working directory when possible.
- Apply and persist a selected directory through the existing shared working-directory state.
- Leave the current directory unchanged when the picker is cancelled.

## Capabilities

### New Capabilities

- `deepseek-custom/working-directory-selection`: Select and persist the agent working directory through the settings UI.

### Modified Capabilities

None.

## Impact

- Affects the egui settings panel and its external integration tests.
- Adds a production dependency for a native cross-platform folder dialog.
- Keeps the existing settings format and backend working-directory behavior unchanged.
