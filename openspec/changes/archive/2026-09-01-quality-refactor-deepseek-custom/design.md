# Quality refactor design

## Context

The scan covers 326 non-generated source files across the Rust production crate,
the external integration-test crate, examples, and `web/src`. The first scan
found 7,045 violations: 1,545 errors and 5,500 warnings. Generated embedded
assets under `crates/deepseek-custom/src/web/assets` are excluded from the
refactor scope.

The baseline behavior checks passed before planning:

- `cargo build --workspace`
- `cargo test --workspace`: 1,256 passed, 0 failed, 2 ignored
- `npm --prefix web run typecheck`
- `npm --prefix web run lint`
- `npm --prefix web test`: 43 passed across 11 files
- `npm --prefix web run build`

The responsibility discovery record is stored in the ignored
`.quality/responsibility-discovery.json`. It validates against the bundled
responsibility-map validator. The scanner units remain evidence only.

## First cluster

The first dependency-connected cluster has ten outcomes. It starts with dead
surface removal, then shared transaction extraction, then application and web
boundary splits, followed by Procedure repair and run orchestration. The plan
keeps each outcome with its callers, tests, and public boundary.

1. Remove unreachable application, settings, and web exports.
2. Extract shared Procedure promotion and report preparation functions.
3. Move application command arbitration into `ApplicationCommandDispatcher`.
4. Move application event projection into `ApplicationEventProjector`.
5. Move test execution state and process ownership into `TestRunCoordinator`.
6. Move Procedure report persistence into `ProcedureReportRepository`.
7. Move browser wire contracts and stream lifecycle into `BrowserEventClient`.
8. Move operation workspace rendering into `OperationWorkspaceView`.
9. Simplify Procedure repair transitions behind `ProcedureRepairCoordinator`.
10. Move Procedure run orchestration behind `ProcedureRunCoordinator` and its
    parameter object.

The remaining violation-bearing files form later clusters. They are not repair
tasks in this wave because their responsibility boundaries depend on the first
cluster's settled application and Procedure contracts.

## Decisions

- Preserve the existing `ApplicationActor`, Procedure, web, and test contracts.
- Keep Claude and Codex adapters separate.
- Keep production code separate from the external integration-test crate.
- Split by responsibility and dependency direction, not by line count.
- Use `cargo test --workspace` and `npm --prefix web test` for behavior checks.
- Use the ignored `.quality/baseline.json` only as a local ratchet. Do not edit
  any tracked quality baseline.
- Exclude generated embedded assets from source quality planning.

## Risks and controls

- Application event moves can lose terminal or transcript ordering. Existing
  actor, session, and web-server lifecycle tests stay with the move.
- Procedure extraction can change error or recovery sequencing. Promotion,
  repair, apply, and sandbox end-to-end tests stay with their owners.
- TypeScript contract extraction can change wire names or SSE handling. Client,
  App, and operation workspace tests remain required.
- A temporary metric increase is allowed only inside one ordered structural
  task. The final step must pass the ratchet without a regression.
