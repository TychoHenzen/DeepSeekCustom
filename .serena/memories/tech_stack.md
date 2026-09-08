# Tech stack
- Rust edition 2024 workspace with Cargo resolver 3.
- Async/web: Tokio, Axum 0.8, reqwest 0.12, SSE, serde, rust-embed.
- Backends: in-process DeepSeek/Ollama API plus separate Claude and Codex CLI process adapters.
- Frontend: Node >=22.12, TypeScript 6, React 19, Vite 8, Vitest 4, ESLint 10.
- Browser acceptance: `playwright-rs = 0.17.0`, aligned with Playwright 1.62.1. Browser installation is explicit.
- Voice: whisper-rs and Kokoro. Model-dependent tests require the opt-in `voice-models` feature.
- Windows native build config lives in `.cargo/config.toml`; it pins CMake/LLVM paths and disables incremental compilation.
- Exact compatibility pins include `ort = 2.0.0-rc.12`, `windows = 0.58.0`, and `tempfile = 3.27.0`.