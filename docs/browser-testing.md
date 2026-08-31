# Browser testing

The browser tests use exactly `playwright-rs` 0.17.0. Its bundled Playwright
driver is 1.62.1. Cargo locks both the Rust crate and driver selection.

Install the matching Chromium build from the repository root:

```powershell
cargo run -p deepseek-custom-tests --example install_playwright_chromium
```

Run the focused production browser milestone:

```powershell
npm --prefix web run browser:test
```

This command builds the production frontend, regenerates its asset manifest,
then starts each isolated Rust server used by the browser suite. The harness
uses scripted model events and deterministic service substitutes. It makes no
external model calls.

The lower-level Rust command remains useful while editing browser assertions:

```powershell
cargo test -p deepseek-custom-tests --test it web_browser -- --test-threads=1
```

Run the real production binary against local Ollama with this opt-in practice:

```powershell
cargo build -p deepseek-custom -j 1
cargo test -p deepseek-custom-tests --test it web_browser::production_binary_uses_only_ollama_across_browser_workflows -j 1 -- --ignored --exact --nocapture --test-threads=1
```

The practice creates a disposable project with only the
`qwen2.5-coder:7b-instruct-q4_K_M` Ollama backend. It does not read or write the
checkout's `settings.json`. The production child receives
`DEEPSEEK_DISABLE_BROWSER=1`, so its Windows browser launcher stays disabled.
Playwright-RS still launches headless Chromium and records the browser state.

Passing and failing live runs retain evidence under
`target/playwright-artifacts/production_binary_ollama_browser_practice/`.
The directory contains numbered screenshots for startup, Settings, chat prompt,
running chat, final chat, reload, Sessions, test discovery, and an exact passing
test. It also contains `trace.zip`, `browser-console.log`, and `server.log`.

For a disposable clean-install check, copy `web` without `node_modules`, run
`npm ci` in the copy, and set `DEEPSEEK_BROWSER_CARGO_ROOT` to this repository
before running its `browser:test` script. The override changes only where Cargo
runs. Vite still builds production assets inside the disposable copy.

Check the installed runtime against the isolated server:

```powershell
cargo test -p deepseek-custom-tests --test it web_browser::installed_chromium_opens_the_isolated_real_server -- --ignored --exact --test-threads=1
```

A missing or incompatible runtime fails that command. The failure prints the
version-matched installer command. It does not skip browser coverage.

Browser failure evidence belongs under `target/playwright-artifacts/<test-name>/`
from the workspace root. The path does not depend on Cargo's process working
directory. Each failing test directory contains these deterministic paths:

- `failure.png` is the full browser screenshot at the failed assertion.
- `trace.zip` is a Playwright trace for `playwright show-trace`.
- `browser-console.log` keeps browser messages in observed order.
- `server.log` records the loopback server, revision, and failure diagnostic.

The responsive matrix routes ordinary browser errors and assertion panics
through this capture path. A passing run removes stale evidence for its test
name. The controlled artifact test retains its evidence under
`target/playwright-artifacts/controlled_responsive_failure/` so the outer
passing test can inspect every file.

The harness gives each test an ephemeral loopback URL and a temporary project
root. It uses deterministic folder-dialog, voice, clock, model-event, and test
executor substitutes. It does not read `settings.json` from the checkout and
does not call an external model.

The independently runnable test-control milestone is:

```powershell
cargo run -p deepseek-custom-tests --example test_control_milestone
```

This command discovers the current integration target through the production
Cargo discovery executor. It then runs one discovered exact test through the
production Cargo test executor. Its output reports the observed discovery
count, exact command, selected scope, result counts, and terminal outcome. It
does not infer repository readiness from that focused result.

Observed on 2026-08-30:

- Discovery found 1,495 exact tests.
- The selected test was
  `application_test_control::passing_result_serializes_only_the_selected_scope_and_observed_outcome`.
- The production executor ran `cargo test -p deepseek-custom-tests --test it application_test_control::passing_result_serializes_only_the_selected_scope_and_observed_outcome -- --exact --test-threads=1`.
- The outcome was `Passed`: 1 passed, 0 failed, 0 ignored, and 1,494 filtered out.

This is focused evidence for the selected test. It is not a repository-wide
test result and must not be presented as repository readiness.
