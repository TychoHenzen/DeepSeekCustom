## 1. Folder Picker Behavior

- [x] 1.1 Add the native folder-dialog dependency, replace the editable field with a current-path display and seeded selection button, and add external tests for the picker request.
<!-- status: completed -->
  <!-- covers: deepseek-custom/working-directory-selection :: Settings provide folder-based working-directory selection :: User opens the folder picker -->
- [x] 1.2 Apply confirmed folder selections to the display, shared working-directory handle, and persisted settings while treating cancellation as a no-op, preserving `project_root`, and testing each path externally.
<!-- status: completed -->
  <!-- covers: deepseek-custom/working-directory-selection :: Settings provide folder-based working-directory selection :: User selects a folder -->
  <!-- covers: deepseek-custom/working-directory-selection :: Settings provide folder-based working-directory selection :: User cancels folder selection -->
  <!-- covers: deepseek-custom/working-directory-selection :: Folder selection preserves working-directory boundaries :: Selected directory differs from the project root -->

## 2. Verification

- [x] 2.1 Run the focused GUI tests, formatting check, clippy with warnings denied, and full workspace test suite.
<!-- status: completed -->
