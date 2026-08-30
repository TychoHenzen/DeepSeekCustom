# Browser testing

The browser tests use exactly `playwright-rs` 0.17.0. Its bundled Playwright
driver is 1.62.1. Cargo locks both the Rust crate and driver selection.

Install the matching Chromium build from the repository root:

```powershell
cargo run -p deepseek-custom-tests --example install_playwright_chromium
```

Run the focused Rust harness:

```powershell
cargo test -p deepseek-custom-tests --test it web_browser -- --test-threads=1
```

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
