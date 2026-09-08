# Suggested commands
- Agent shell commands on this workstation use the `rtk` prefix. Use `rtk powershell -NoProfile -Command ...` for PowerShell cmdlets.
- Start production harness: `rtk powershell -NoProfile -File .\\run.ps1`.
- Rust check/build: `rtk cargo check --workspace`; `rtk cargo build -p deepseek-custom`.
- Full Rust tests: `rtk cargo test --workspace -- --test-threads=1`.
- External integration target: `rtk cargo test -p deepseek-custom-tests --test it -- --test-threads=1`.
- Focused integration filter: `rtk cargo test -p deepseek-custom-tests --test it <filter> -- --test-threads=1`.
- Rust quality: `rtk cargo fmt --all -- --check`; `rtk cargo clippy --workspace -- -D warnings`.
- Frontend install/check: `rtk npm --prefix web ci`; then `typecheck`, `lint`, `test`, or `build` via `rtk npm --prefix web run <script>`.
- Serena validation: `rtk proxy cmd /c "set PYTHONUTF8=1&& serena project health-check ."`; memory links: `rtk serena memories check`.