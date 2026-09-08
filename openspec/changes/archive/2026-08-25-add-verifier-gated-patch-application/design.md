## Context

Milestone 2 already creates a disposable current-state snapshot for drafting and records SHA-256 target baselines. This change turns that seam into the only place where patch application and project commands run. The real working tree may contain user changes, so a Git worktree based only on HEAD would verify the wrong state.

## Goals / Non-Goals

**Goals:**

- Prove a patch against the exact current workspace state.
- Keep failures and interruptions away from real files.
- Promote verified results without overwriting a concurrent user edit.

**Non-Goals:**

- Inferring project commands from the model.
- Running multiple candidates.
- Repairing failed patches.

## Decisions

### Gate verification on one named approved localization report

The Apply request carries the localization run ID in addition to its preview. Load that exact report through the approved-report guard before copying the workspace, applying a patch, or spawning a verifier. Require `Approved`, the selected change and task, current fingerprints, and a preview derived from the same run. Reject `Pending`, `Rejected`, `LegacyUnreviewed`, missing, stale, and mismatched reports. Do not substitute another report or begin any verification work after rejection.

### Snapshot the current tree, not HEAD

Generalize the snapshot helper to copy the current working directory with `walkdir`. Skip `.git`, `target`, `.deepseek`, symlink or reparse-point traversal, and configurable extra exclusions. Preserve relative paths and file bytes, including uncommitted changes and untracked source.

Alternative considered: `git worktree add`. Rejected because it omits current uncommitted and untracked input.

### Treat verifier commands as user-owned configuration

Add an ordered `procedure.verifier_commands` array. Apply stays disabled when it is empty. This repository's practice configuration uses the four commands named in the proposal. The runner resolves the executable through the same Windows PATH and PATHEXT logic used elsewhere, spawns it in the snapshot, captures bounded stdout and stderr, and adopts the process into the existing Windows job object.

The sequence is `git apply --check`, `git apply`, then configured commands in order. Stop at the first failure. Do not ask a model whether output is acceptable.

### Promote file results with baselines and rollback

Immediately before promotion, hash every old target in the real workspace and compare it to the preview baseline. Include create, delete, and rename endpoints in the target set.

Stage verified bytes in sibling temporary files. Save recoverable backups for every existing target. Install all results. If any install fails, restore existing targets and remove newly created targets. Delete backups only after the final state matches the verified hashes.

This is recoverable all-or-nothing behavior, not a claim that Windows offers a multi-file atomic rename.

### Keep verification evidence bounded

Record command text, exit code, duration, and the first and last 4 KiB of combined output. Mark truncation explicitly. Full compiler logs remain in the disposable run directory only while the run is active and are deleted with it.

### Interruption owns child cleanup

The Procedure tab uses the existing shared interrupt flag. The active verifier process is in the job object, so interruption kills descendants. The state machine then deletes the disposable workspace and returns an interrupted disposition without entering promotion.

## Risks / Trade-offs

- [Copying a large workspace is expensive] -> Exclude generated trees, show copy progress, and fail before model work if a configured size cap is exceeded.
- [Verifier commands can have external side effects] -> Run them in the snapshot, show the exact commands, and document that network or external-service effects remain outside file isolation.
- [Rollback can itself fail] -> Keep backups, report every affected path, and never delete recovery data after an incomplete rollback.
- [Concurrent edits arrive after the hash check] -> Use sibling staging and the shortest possible promotion section. Recheck each target before its install.

## Migration Plan

The new verifier list is optional on load but required for Apply. Preview-only behavior remains available with no commands. Rollback disables Apply and leaves existing reports and workspace content intact.
