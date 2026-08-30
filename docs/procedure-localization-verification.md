# Procedure localization visual verification

This checklist covers the production web Procedure workspace. The browser harness serves the same embedded assets as the production Rust binary.

## Prerequisites

Install the locked frontend dependencies and the version-matched browser:

```powershell
Push-Location web
npm ci
npm run browser:install
Pop-Location
```

The browser runtime is Playwright 1.62.1 through `playwright-rs` 0.17.0. Normal Rust tests do not download it.

## Maintained browser run

Run from the repository root:

```powershell
npm --prefix web run browser:test
```

The harness starts an isolated Rust loopback server. It uses deterministic Procedure data and does not call a model or read the checkout's `settings.json`.

Failures write these files under `target/playwright-artifacts/<test-name>/`:

- `failure.png`
- `trace.zip`
- `browser-console.log`
- `server.log`

## State evidence

The maintained browser scenarios cover each visible terminal or review state:

| State | Required visible contract |
|---|---|
| Running | Progress, backend, model, attempt count, enabled Stop, and disabled conflicting controls. |
| Awaiting review | Run identity, targets, optional symbols, evidence, Approve, and Reject. |
| Approved | Retained evidence and an approved terminal decision without review buttons. |
| Rejected | Retained evidence and a rejected terminal decision without review buttons. |
| Failed | Complete diagnostic and no accepted invalid target. |
| Interrupted | Interrupted terminal state and no review action. |

The responsive browser matrix checks desktop and narrow viewports. Text,
controls, report paths, and evidence must remain readable without overlap,
clipping, or horizontal page overflow.

Review and apply commands must address the displayed run identity. A stale result from an older run must not replace the current Procedure state.

## Focused checks

The Rust browser filter exercises the real server adapter:

```powershell
cargo test -p deepseek-custom-tests --test it web_browser -- --test-threads=1
```

These focused checks prove the maintained scenario only. Repository readiness still requires the complete frontend and Rust gates.
