# Test core
- All Rust tests live in `crates/deepseek-custom-tests`; production has no inline test modules.
- `autotests = false`; `tests/it/main.rs` is the single integration target. Put new test modules under `tests/it` and register them in `main.rs`.
- Fake Claude, fake Codex, fake MCP, process probes, fixtures, and test-only Stub provide deterministic external boundaries.
- Use the integration target before the filter: `cargo test -p deepseek-custom-tests --test it <filter>`.
- Serialize Windows tests with `-- --test-threads=1`.
- Browser tests use isolated loopback servers and deterministic substitutes. They do not read checkout `settings.json` or call external models.
- Normal tests exclude real voice models. Opt in with `deepseek-custom-tests/voice-models` only when local model files exist.