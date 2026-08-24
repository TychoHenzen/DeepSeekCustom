## Context

The localization runner currently treats schema validity and repository-index
validity as a successful result. A real Ollama smoke run demonstrated the gap:
the model returned existing paths and symbols that were unrelated to the task,
and the run was recorded as succeeded.

The same subsystem has several narrower audit gaps. Its repository-index limits
exist as implementation defaults rather than a public contract. Its task input
has a deterministic multi-capability error without a regression test. Its Rust
symbol extractor deliberately recognizes an ASCII subset without a boundary
fixture. Its GUI tests exercise state and channels without rendered evidence.
All scenarios in the maintained procedure-localization specification are also
unbound in the coverage report.

This change crosses the input parser, indexer, runner, saved report, GUI, test
infrastructure, and maintained documentation. Active downstream procedure
changes may read localization reports, so the report transition needs an
explicit compatibility rule.

## Goals / Non-Goals

**Goals:**

- Separate structural validation from semantic approval.
- Prevent an unreviewed or rejected target set from becoming downstream input.
- Specify and test ambiguous task selection, index limits, and symbol fallback.
- Make the Procedure tab's states inspectable through repeatable visual evidence.
- Produce maintained Ollama smoke evidence that records semantic disposition.
- Bind every final procedure-localization scenario to a runnable external test.

**Non-Goals:**

- Automatically decide whether a target set is semantically correct.
- Generate, preview, verify, or apply source patches.
- Replace the conservative Rust scanner with a complete Rust parser.
- Add provider-native reasoning controls to Ollama requests.
- Rewrite archived OpenSpec artifacts to change their historical result.

## Decisions

### 1. Structural validity enters a review state

The runner will stop at `AwaitingReview` after schema and repository-index
validation succeeds. The saved report will contain a review disposition with
`Pending`, `Approved`, or `Rejected` state. Approval and rejection commands will
name the run identifier, so a stale GUI event cannot decide a newer run.

The first terminal review decision wins. Repeating the same decision is
idempotent. Attempting to reverse an approved or rejected decision returns a
state error and leaves the report unchanged. Only an approved report satisfies
the completed-localization contract used by downstream procedure stages.

This keeps the existing validation useful while making its actual guarantee
explicit. Automatically treating an index-valid result as correct was rejected
because the smoke run showed that this inference is false.

### 2. Older reports load as legacy unreviewed reports

The report reader will use a serde default for the new review field. A report
written before this change will load as `LegacyUnreviewed`, remain visible for
diagnosis, and fail the approved-report guard. It will not be silently upgraded
to approved.

This preserves access to existing run history without trusting a decision that
was never recorded. Rejecting old reports during deserialization was considered,
but that would hide useful diagnostic evidence from the GUI.

### 3. Ambiguous task contracts fail before expensive work

An unchecked task without `covers` may inherit a capability only when the
change has exactly one delta capability. Zero or multiple delta capabilities
will produce a deterministic input error before indexing and model dispatch.
The error will include the change, task, and discovered capability count.

Requiring `covers` on every task was considered. The one-capability shorthand is
retained because it is unambiguous and already part of the established input
format.

### 4. Index limits and the ASCII symbol subset become public boundaries

Named defaults will remain 10,000 files and 64 MiB. Procedure settings will
allow each value to be overridden. Limit errors will report the effective
limit and first sorted overflow path so the result is deterministic.

The existing lightweight Rust scanner will continue to expose supported ASCII
identifiers. A valid file with an unsupported identifier remains in the index,
but that identifier is not added or normalized. This keeps path-only targeting
available without claiming full Rust syntax support.

Using a full Rust parser was rejected for this change. It would add substantial
dependency and syntax-version scope without addressing the observed semantic
approval failure.

### 5. Coverage bindings use synchronous external test entry points

Each scenario will have one distinct Rust test with one `// covers:` marker
directly above an exact `#[test]` attribute. Async behavior will be exercised by
synchronous wrapper tests that create a Tokio runtime and invoke shared async
helpers. This matches the current dod-guard Rust marker parser, which does not
bind markers above `#[tokio::test]`.

`openspec/test-runners.json` will map Rust files to a portable Node runner. The
runner will validate that the supplied path belongs to the integration-test
tree, derive its module filter, and invoke:

```text
cargo test -p deepseek-custom-tests --test it <module>
```

This gives each binding a runnable verification command on Windows and other
supported development hosts. Binding markers without a working runner were
rejected because they would improve the count without making verification
reproducible.

### 6. Visual and live-model evidence is maintained outside the archive

A maintained verification document will record the GUI checklist, screenshot
paths, Ollama command and model, report identifier, target decision, workspace
hash comparison, and relevant command output. Saved screenshots will cover the
running, review, approved, rejected, failed, and interrupted presentations.

The live smoke procedure will treat transport success, structural validity,
semantic approval, and workspace immutability as separate observations. If the
model proposes the wrong targets, the reviewer will reject them and the run
will count as a successful safety demonstration, not a successful localization.

The archived implementation note will remain unchanged because it records what
was observed at archive time. The maintained document will link that history
to the current evidence.

## Risks / Trade-offs

- **Human review adds a stop before later stages.** This is intentional until a
  separate specification defines an acceptable automatic semantic gate.
- **Legacy reports no longer count as completed input.** They remain readable,
  but downstream work must rerun localization and record approval.
- **Synchronous async-test wrappers add small test boilerplate.** Shared helpers
  keep behavior aligned with existing async tests while satisfying the binding
  parser.
- **Screenshot evidence can drift after GUI changes.** The checklist and an
  evidence-presence test make stale or missing artifacts visible during review.
- **The ASCII scanner omits valid Rust identifiers.** Path-only fallback avoids
  false symbol claims, and the limitation remains explicit.

## Migration Plan

1. Add report review types and backward-compatible deserialization.
2. Add the approved-report guard before any downstream consumer changes.
3. Change successful structural validation to save `AwaitingReview`.
4. Add run-id-scoped approve and reject commands, then expose them in the GUI.
5. Add settings, input, index, report, runner, and GUI regression tests.
6. Add one binding test per final procedure-localization scenario and install
   the Rust file runner configuration.
7. Capture visual and Ollama smoke evidence with workspace hashes.
8. Run focused tests, the full Rust quality suite, strict OpenSpec validation,
   and both change-level and repository-level coverage checks.

Rollback removes the new review commands and restores the old runner state.
The report reader must continue accepting the review field if reports with that
field may remain on disk. A rollback must never reinterpret pending or rejected
reports as approved.

## Open Questions

None. Automatic semantic consensus remains part of a separate downstream
change and must update this contract before it can replace explicit review.
