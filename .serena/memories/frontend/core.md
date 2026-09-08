# Frontend core
- Source lives in `web/src`; production assets are generated into `crates/deepseek-custom/src/web/assets`.
- Vite is build-time only. Production Rust uses `rust-embed`; startup never invokes Node, npm, or Vite.
- Eight workspaces: Chat, Sessions, Settings, Autopilot, Cascade, Evolve, Procedure, Tests.
- State bootstraps once, then reduces ordered SSE revisions. A revision gap forces a fresh snapshot.
- Commands remain disabled while state is stale or an incompatible operation owns the application.
- The asset build writes a SHA-256 manifest. Rust build verification rejects missing or stale embedded assets.
- Development Vite proxies `/api` to `DEEPSEEK_SERVER_URL`, defaulting to `http://127.0.0.1:3000`.