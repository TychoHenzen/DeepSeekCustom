# Continuous workflows

`deepseek_custom::workflow` owns durable, provider-neutral workflow runs. It
does not own backend process handles, approval authority, merges, or branch
deletion.

## Durable state

Runs are stored atomically under `.deepseek/workflows`. A record keeps the
repository and project identity, issue or pull request, project root,
working directory, branch and revision, session and context ids, current
route step, evidence, and waiting feedback. `WorkflowStore::load_after_restart`
turns an in-flight claim into `Interrupted`; an explicit resume is required
before work can be claimed again.

## Route

The guarded route is `Capture` → `Refine` → `Implement` → `DraftPullRequest` →
`Review` → `FixFindings` → `CompletionGate`. A step cannot advance without a
recorded outcome. Blocked and question outcomes publish the exact question,
evidence, and requested action through `WorkflowFeedbackPort`; only a matching
run and feedback id can resume the run. Completion still requires an explicit
approval decision.

## Inspection

The local server exposes `GET /api/workflows` for operator inspection. The
endpoint returns the persisted run records. `POST /api/workflows/<run>/select`,
`.../resume`, `.../approve`, `.../reject`, and `.../feedback` are authenticated
run-scoped controls with stale-transition checks.

Set `DEEPSEEK_WORKFLOW_OWNER`, `DEEPSEEK_WORKFLOW_PROJECT`, and
`DEEPSEEK_WORKFLOW_REPOSITORY` to start the optional `gh`-backed worker. It
uses the authenticated GitHub CLI to list `Todo` issues, claim them by
assignment, publish issue comments, and observe closed issues. Queue, Git
feedback, and terminal-state adapters are supplied to `WorkflowScheduler`; it
claims one eligible item at a time and selects the next item only after the
current run is observed merged or closed. Configured step commands must finish
within five minutes and print `WORKFLOW_STEP_COMPLETED`; otherwise the run
stops in a durable decision or retry state rather than inventing evidence.
