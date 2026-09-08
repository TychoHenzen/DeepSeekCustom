# Production core
- `main.rs` loads settings, creates backend/service ports, starts the application actor, binds an OS-selected `127.0.0.1` port, and opens the browser.
- `application/` owns authoritative visible state, command arbitration, revisioned snapshots, anti-forgery tokens, and stale-result rejection.
- `backend/` contains API, Claude CLI, Codex CLI, and test-only Stub adapters. `BackendFactory` owns construction.
- `agent/` runs turns and child-session dispatch. Open child sessions are bounded and explicitly closed.
- `mcp/`, CLI adapters, and Procedure children use the Windows process-group boundary in `process_group.rs`.
- `procedure/`, `search/`, and `autopilot/` run bounded operations behind application ports.
- `web/` serves embedded assets, bootstrap state, SSE revisions, typed commands, uploads, and health from one loopback origin.
- Detailed maintained architecture: `docs/agent-project-context.md`.