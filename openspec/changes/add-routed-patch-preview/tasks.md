## 1. Preview state and fingerprints

- [x] 1.1 Add SHA-256 target and OpenSpec fingerprints to localization reports, including create, delete, and rename path identities.
  <!-- status: completed -->
- [x] 1.2 Load the explicitly named run through the approved-report guard. Reject pending, rejected, legacy-unreviewed, missing, or change- or task-mismatched reports before route evaluation, workspace creation, or model dispatch.
  <!-- covers: deepseek-custom/routed-patch-preview :: Patch preview requires a current localization report :: Current report is accepted -->
  <!-- status: completed -->
- [x] 1.3 Add disposition and run-mismatch fixtures that assert the complete pre-dispatch diagnostic.
  <!-- covers: deepseek-custom/routed-patch-preview :: Patch preview requires a current localization report :: Unapproved or missing report is rejected -->
  <!-- status: completed -->
- [x] 1.4 Show the stale input paths and require a new localization run without dispatching a drafting model.
  <!-- covers: deepseek-custom/routed-patch-preview :: Patch preview requires a current localization report :: Localization report is stale -->
  <!-- status: completed -->

## 2. Deterministic difficulty router

- [x] 2.1 Define route tiers and an ordered route-signal enum for mechanical verbs, target count, architecture, cross-cutting behavior, concurrency, security, migration, public API, subtle bugs, and substantive logic.
  <!-- status: completed -->
- [x] 2.2 Implement the pure conservative rule table and cover a one-file mechanical local route.
  <!-- covers: deepseek-custom/routed-patch-preview :: Route decisions use deterministic difficulty signals :: Mechanical single-file step routes locally -->
  <!-- status: completed -->
- [x] 2.3 Cover every frontier signal, conflicting signals, and unknown wording, including an assertion that token confidence is not an input.
  <!-- covers: deepseek-custom/routed-patch-preview :: Route decisions use deterministic difficulty signals :: Higher-risk step routes to frontier -->
  <!-- status: completed -->
- [x] 2.4 Add Automatic, Force local, and Force frontier run overrides while retaining the automatic decision in the report.
  <!-- covers: deepseek-custom/routed-patch-preview :: A user can override the automatic route :: Local override is selected -->
  <!-- status: completed -->

## 3. Patch envelope and validation

- [x] 3.1 Define the patch-envelope schema and typed decoder for targets, rationale, route metadata, and unified diff.
  <!-- status: completed -->
- [x] 3.2 Use structured output for local drafting and pass valid output through the shared envelope and diff parser.
  <!-- covers: deepseek-custom/routed-patch-preview :: Patch output has one validated envelope :: Valid local envelope -->
  <!-- status: completed -->
- [x] 3.3 Require exactly one JSON envelope from frontier output and report deterministic parse errors without extracting guessed patches.
  <!-- covers: deepseek-custom/routed-patch-preview :: Patch output has one validated envelope :: Frontier output is malformed -->
  <!-- status: completed -->
- [ ] 3.4 Normalize diff paths and accept create, update, delete, and rename hunks only when every endpoint is localized.
  <!-- covers: deepseek-custom/routed-patch-preview :: A patch stays inside the localization boundary :: Patch touches only localized files -->
- [ ] 3.5 Reject absolute paths, traversal, malformed headers, and any path outside the localization allowlist, listing all violations.
  <!-- covers: deepseek-custom/routed-patch-preview :: A patch stays inside the localization boundary :: Patch reaches an unlocalized file -->

## 4. Isolated drafting

- [ ] 4.1 Add the disposable draft-workspace helper that copies current source while excluding `.git`, `target`, `.deepseek`, binary outputs, and reparse-point traversal.
- [ ] 4.2 Run Claude CLI and Codex CLI drafting through `run_subagent` with the disposable directory as their working directory, then discard all workspace edits they made.
- [ ] 4.3 Run `git apply --check` against the disposable workspace to validate patch hunks without applying them to real files.
- [ ] 4.4 Add fake CLI tests that deliberately write beside the target and prove the real workspace remains unchanged.
  <!-- covers: deepseek-custom/routed-patch-preview :: Preview exposes the route and does not edit :: Preview run leaves no workspace change -->

## 5. Preview interface and verification

- [ ] 5.1 Extend procedure settings with local and frontier backend names and round-trip them without narrowing either backend's visible model list.
- [ ] 5.2 Add Preview controls and render the automatic route, override, signals, backend, model, targets, rationale, and complete diff.
  <!-- covers: deepseek-custom/routed-patch-preview :: Preview exposes the route and does not edit :: User inspects a preview -->
- [ ] 5.3 Add end-to-end tests for a local mechanical preview and a frontier architectural preview, asserting unchanged workspace hashes.
- [ ] 5.4 Run the two practical preview cases through the real configured backends and record their routes and non-mutation evidence.
- [ ] 5.5 Run `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace`.
