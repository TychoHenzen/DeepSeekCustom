## Context

See `proposal.md` for motivation. The settings panel currently owns an editable path buffer. Losing focus validates and commits that text. The same value also appears in the top status panel. The shared `AgentHandles::working_dir` value remains the runtime source of truth, while `project_root` remains the settings-file anchor.

This change introduces a native folder dialog dependency. Tests must stay in `crates/deepseek-custom-tests` and must not open an interactive dialog.

## Goals / Non-Goals

**Goals:**

- Replace raw path editing with a native folder-selection action.
- Keep the selected path visible in the settings panel and top status panel.
- Keep selection application testable without displaying a native dialog.
- Preserve the current shared-state and settings persistence boundaries.

**Non-Goals:**

- Changing the `settings.json` schema.
- Changing the `Cd` tool or backend process behavior.
- Adding recent-directory history or multi-root workspace support.

## Decisions

### Use a native cross-platform folder dialog

Add the `rfd` crate and call its folder-selection API from the settings control. Seed the dialog with the current working directory. A native dialog matches platform navigation conventions and prevents arbitrary invalid text.

Alternative considered: build an egui directory browser. This would add filesystem navigation state, filtering, and platform-specific path handling inside the application.

### Separate dialog launch from selection application

The rendered button launches the dialog. A separate method accepts the dialog result and applies a selected `PathBuf`. This keeps shared-state mutation and persistence directly testable without opening an interactive operating-system window.

Alternative considered: test only the settings mapping helper. That would not cover cancellation or shared working-directory updates.

### Keep the current path as display state

Retain a string representation of the active working directory for labels and the existing top status panel. Remove editable text behavior and invalid-path feedback. On confirmed selection, update the display string, shared handle, and settings value together.

Alternative considered: format the locked shared path on every frame. The existing display state avoids repeated locking and limits the change to current ownership boundaries.

## Risks / Trade-offs

- [Risk] A synchronous native dialog blocks egui rendering while open. -> The dialog is modal user interaction, so blocking the window is expected and bounded by the user's selection.
- [Risk] The selected directory can disappear between selection and later tool use. -> Preserve existing runtime error handling because no picker can guarantee future filesystem availability.
- [Risk] A new dependency increases platform build surface. -> Use a maintained cross-platform dialog crate and verify the full workspace on Windows.

## Migration Plan

1. Add the folder-dialog dependency and replace the settings control.
2. Update external GUI tests for selection, cancellation, display state, and persistence.
3. Run focused GUI tests, then formatting, clippy, and the full workspace tests.

Rollback removes the dependency and restores the editable field. The persisted settings format needs no migration.
