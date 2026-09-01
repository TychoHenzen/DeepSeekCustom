import type { AppChange, AppSnapshot } from '../contracts.ts';

export function applyChange(
  snapshot: AppSnapshot,
  change: Exclude<AppChange, { type: 'reset' }>,
): AppSnapshot {
  switch (change.type) {
    case 'workspace_selected':
      return { ...snapshot, revision: change.revision, workspace: change.value };
    case 'transcript_appended':
      return {
        ...snapshot,
        revision: change.revision,
        transcript: [...snapshot.transcript, change.value],
      };
    case 'session_changed':
      return { ...snapshot, revision: change.revision, session: change.value };
    case 'saved_sessions_changed':
      return { ...snapshot, revision: change.revision, saved_sessions: change.value };
    case 'pending_session_switch_changed':
      return {
        ...snapshot,
        revision: change.revision,
        pending_session_switch: change.value,
      };
    case 'settings_changed':
      return { ...snapshot, revision: change.revision, settings: change.value };
    case 'operation_changed': {
      const index = snapshot.operations.findIndex(
        (operation) => operation.kind === change.value.kind,
      );
      const operations = index < 0
        ? [...snapshot.operations, change.value]
        : snapshot.operations.map((operation, operationIndex) => (
          operationIndex === index ? change.value : operation
        ));
      return { ...snapshot, revision: change.revision, operations };
    }
    case 'tests_changed':
      return { ...snapshot, revision: change.revision, tests: change.value };
    case 'error':
      return { ...snapshot, revision: change.revision };
  }
}
