## Context

See `proposal.md` for motivation and `specs/deepseek-custom/controlled-development-mode/spec.md` for the behavior contract.

The application actor currently owns top-level commands, visible operation state, Stop, and session switching. `BackendFactory` already builds API, Claude CLI, Codex CLI, and Stub backends against a replaceable working-directory handle. API backends receive a harness-owned tool registry. CLI backends build their own tool surface.

Procedure already owns current-state disposable snapshots, bounded verifier commands, path fingerprints, stale-baseline checks, process interruption, and recoverable transactional promotion. These seams operate independently of OpenSpec after their inputs are validated, so Controlled Development can extend and reuse them without changing Procedure behavior.

Normal API tools are not confined to `project_root`, and normal API runs expose shell, directory-change, MCP, and subagent tools. Controlled Development therefore needs a narrower backend construction profile. A working-directory prompt alone is not an enforcement boundary.

## Goals / Non-Goals

**Goals:**

- Make the phase and card the single source of authority for one top-level session.
- Keep all agent writes outside the real workspace until deterministic promotion.
- Reuse Procedure-owned snapshot, verifier, fingerprint, interrupt, and promotion code.
- Preserve complete backend and verifier evidence while deriving bounded visible summaries.
- Keep restored sessions safe without resuming work automatically.

**Non-Goals:**

- Generalize Controlled Development into a policy language or automatic packet queue.
- Add shell access to harness-owned controlled runs.
- Change normal chat, Procedure contracts, global backend settings, or the process-group design.
- Add a dependency, database, network service, frontend framework, or alternate patch engine.
- Guarantee that an approved proof command has no side effects outside the disposable root. The command text is user-approved, and the harness controls its current directory and interrupt path. This limitation remains visible.

## Decisions

### 1. Add one session-owned Controlled Development coordinator

Create a `controlled_development` domain module. Its coordinator owns the state machine, current card, packet identity, disposable workspace pair, raw event log, proof evidence, changed paths, blocker, and compact summary. The application actor exposes typed commands and projects the coordinator state into `AppSnapshot`.

The coordinator runs as an application service like Procedure. It receives a selected backend name and model from the owning session. It never mutates the normal chat backend or its conversation identity.

Alternative considered: embed the workflow directly in `ApplicationCommandDispatcher`. Rejected because slow backend, filesystem, verification, and promotion work would violate actor responsiveness and mix domain state with command arbitration.

### 2. Model cards and transitions as closed Rust data

`WorkCard` uses `serde` with unknown fields denied. A validator checks exact field inventory, one-to-three proof commands, three-or-fewer production paths, normalized repository-relative paths, duplicate paths, overlap between path lists, and nonempty bounded text fields. Planning accepts only the complete structured final response. It does not search markdown fences or extract a JSON substring.

Every command includes the current packet or card id. Approve moves only the current `AwaitingApproval` card to `Executing`. Reject clears approval without execution. Stop sets the shared interrupt before any later transition. Terminal transitions consume approval. A new packet receives a new id and cannot inherit the prior card.

Alternative considered: infer a card from ordinary assistant prose. Rejected because it creates an unreviewed interpretation path.

### 3. Build fresh controlled backend runs with fixed profiles

Extend `BackendFactory` with a narrow controlled-run profile. This is a construction option, not a global policy engine.

- API planning exposes only rooted read, image-read, glob, and grep tools. API execution adds rooted write and edit tools. Both profiles omit Bash, shell stdin, `cd`, Reset, Ask, Skill, MCP, Task, subagent SendMessage, and CloseSession.
- Rooted file tools reject absolute paths, traversal, and canonical paths outside the disposable root. The fixed root cannot be changed during a packet.
- Codex planning forces `--sandbox read-only`, `--ephemeral`, `--ignore-user-config`, `--ignore-rules`, and `--output-schema`. Execution forces `--sandbox workspace-write`, the same isolation flags, and a fresh thread.
- Claude planning forces `--safe-mode`, `--no-session-persistence`, `--permission-mode plan`, a read-only tool allowlist, and `--json-schema`. Execution uses `acceptEdits` with only rooted file read, search, write, and edit tools. It does not resume a Claude session.
- Stub follows the same phase interface for deterministic tests.

Planning and execution use fresh backend instances. Execution receives the original request plus the approved card. Raw routed events flow into the coordinator's evidence log.

Alternative considered: change the normal session's shared working directory temporarily. Rejected because late events or tool calls could observe the wrong root, and normal history could transfer authority.

### 4. Extend the existing disposable snapshot seam with an inventory and baseline fork

Create one current-state snapshot through `DisposableDraftWorkspace`. Before backend execution, fork it through the same copy and exclusion implementation into an immutable baseline and a writable execution root. This avoids two independent reads of a changing real workspace.

Extend that module's existing traversal to produce a deterministic inventory of regular repository files. Comparing the baseline inventory to the execution inventory yields created, modified, deleted, and renamed endpoints. Rename detection is optional for display. Promotion treats matching delete and create endpoints correctly even if displayed separately.

Generate the retained diagnostic diff with the repository's existing Git executable in no-index mode against the baseline and execution roots. Path authorization comes from the inventory comparison, not from parsing this display diff. This adds no patch parser or promoter.

Alternative considered: compare the execution root to the live workspace. Rejected because concurrent unrelated edits would corrupt the diagnostic result and path set.

### 5. Adapt Procedure verification and promotion inputs

The coordinator captures a `PromotionBaseline` for every approved path plus reserved `PROJECT_STATE.md` at execution start. After execution it performs these gates in order:

1. Compare baseline and execution inventories.
2. Require each agent-changed path in the card lists.
3. Count changed production paths and reject counts above three.
4. Check recognized dependency manifest and lockfile changes against named complexity exceptions.
5. Run each card proof command through `VerifierCommandRunner` in the execution root.
6. Recheck the real target fingerprints.
7. Generate bounded `PROJECT_STATE.md` in the execution root.
8. Promote the changed card paths and `PROJECT_STATE.md` through `promote_verified_workspace` as one recoverable transaction.

The manifest classifier is a small fixed repository concern for Cargo and the existing web package manager. It does not parse dependency semantics or become a rule language. A manifest or lockfile requires an approved path and at least one matching named dependency-system exception. The exact exception and changed dependency name are recorded for review.

`PROJECT_STATE.md` is a reserved harness-generated target. It is not an agent-produced Work Card path. Its exact changed-path section includes itself and every promoted packet path. Including it in the same baseline and transaction prevents overwriting a concurrent user edit and avoids a successful code promotion with stale project state.

Alternative considered: copy files directly after verification. Rejected because Procedure promotion already supplies endpoint rechecks, backups, rollback, and final fingerprint evidence.

### 6. Retain failed workspaces as session evidence

Ordinary success cleans the baseline and execution roots after promotion evidence is captured. A packet with unexpected changes, failed proof, interruption, or promotion conflict transfers both roots to retained-workspace ownership. The session record stores their paths and packet id.

`DIFF` reads the retained pair. Starting another packet or an explicit discard closes the old pair before creating a new one. Reset and deletion also close it. Cleanup validates that both resolved roots are owned temporary directories created by this feature before recursive removal.

Alternative considered: store only a text diff. Rejected because raw files are needed when binary, encoding, or generated-file changes explain a failure.

### 7. Persist safe session state, not executable work

Add a backward-compatible `controlled_development` field to `SessionRecord` with `serde(default)`. Persist the enabled flag, phase, current card, packet id, compact evidence, raw details, retained roots, and approval identity.

The load path normalizes `Planning`, `AwaitingApproval`, and `Executing` to `Interrupted` and clears approval before projection. A destination session installs only its own state. Session reset and deletion call coordinator cleanup before changing the session record.

Alternative considered: persist a resumable task closure or backend thread id. Rejected because restart must never resume execution or promotion.

### 8. Intercept control inputs before normal chat dispatch

When a session has Controlled Development enabled, command arbitration recognizes only exact `STATUS`, `MAP`, `DIFF`, `STOP`, and `WHY <nonempty item>` forms as control inputs. It reads coordinator state or sends Stop without appending a backend turn. Other text starts planning only from an eligible non-running phase.

`WHY` looks up a named decision in recorded harness decisions. It does not ask the model to invent a new explanation. `MAP` uses the bounded current system map also written to `PROJECT_STATE.md`.

Alternative considered: teach the model to answer these commands. Rejected because model output would not be authoritative state.

### 9. Add one compact panel to the existing Chat workspace

Add controlled state and commands to the existing client contracts. `ChatWorkspace` renders the toggle, phase, card, actions, changed paths, proof results, blocker or limitation, and a native collapsed details element. No route or state library is added.

The Rust coordinator builds progress and terminal summaries from typed fields. A shared word counter validates the 80-word and 200-word caps. Raw events are never shortened to satisfy these caps and remain in the details element.

Alternative considered: reuse transcript text as the summary. Rejected because arbitrary model text can hide a failure when truncated.

### 10. Test domain behavior before production browser practice

External Rust integration tests use Stub for phase, approval, isolation, gate, restart, session, summary, and raw-evidence cases. A fake Codex executable asserts forced sandbox, ephemeral, configuration-isolation, schema, and disposable working-directory arguments. Frontend component and Playwright tests assert semantic controls and state rendering.

After automated gates, run the production binary against a disposable settings file that selects local Ollama. The Playwright-RS practice records ordered screenshots and a trace for planning, approval, proof, promotion, and Stop. Assertions use lifecycle state, paths, proof results, and workspace bytes, not model wording.

## Risks / Trade-offs

- [CLI behavior changes across installed versions] -> Pin tests to argument contracts and fail closed when required flags are unavailable or rejected.
- [A user-approved proof command can address absolute paths] -> Show the exact commands on the card, run them only after approval, retain output, and state this remaining limitation in the UI.
- [Snapshot copy size or binary files make comparison expensive] -> Reuse existing exclusions and byte cap, hash streams, and stop before backend dispatch when the cap is exceeded.
- [A process exits before retained cleanup] -> Persist only validated owned temporary roots, mark in-flight work interrupted, and clean them on packet replacement, reset, or deletion.
- [A dependency diff cannot be understood without package semantics] -> Fail closed unless the approved exception names the changed dependency or dependency system. Do not infer permission from a nonempty unrelated exception.
- [Generated web assets change during proof commands] -> Determine agent-changed paths before proof commands and promote only the pre-proof validated target set plus `PROJECT_STATE.md`.

## Migration Plan

1. Add backward-compatible DTO and session fields with disabled defaults.
2. Add the coordinator, strict card decoder, controlled backend profiles, and snapshot inventory extensions behind the disabled mode.
3. Wire commands and projection into the application actor.
4. Add the existing-page frontend panel and generated assets.
5. Run focused and workspace verification, then production Ollama browser practice.

Rollback removes the optional field and UI wiring. Older session files remain readable because the field defaults when absent. No data migration, dependency change, commit, archive, or release is required.
