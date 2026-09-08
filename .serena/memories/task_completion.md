# Task completion
- Run a focused test after each coherent behavior change.
- For Rust changes: `rtk cargo fmt --all -- --check`, `rtk cargo clippy --workspace -- -D warnings`, then relevant focused integration filters.
- Before repository-wide success: `rtk cargo test --workspace -- --test-threads=1`.
- For frontend changes: `rtk npm --prefix web run typecheck`, `lint`, `test`, and `build`.
- A frontend production change must regenerate `crates/deepseek-custom/src/web/assets` and its SHA-256 manifest through the web build.
- Browser-facing changes need the relevant deterministic browser practice from `docs/browser-testing.md`.
- Do not claim success from stale output, test names, commit messages, or coverage syntax. Report fresh command output and remaining unobserved behavior.