# Web production assets

The Rust web server embeds production files from
`crates/deepseek-custom/src/web/assets`. It does not need a frontend development
server at runtime.

`asset-manifest.json` is the build boundary. It records the contract version,
the generator, the production build command, and the SHA-256 digest of every
served application file. The crate build script validates this manifest before Rust
compilation. A missing file or mismatched digest stops the build with this
action:

```powershell
npm --prefix web run build
```

The `web/` workspace owns production asset generation. Its Vite build empties and
recreates the embedded asset directory, then writes a sorted manifest with SHA-256
digests. Use the locked dependency graph for a clean build:

```powershell
npm --prefix web ci
npm --prefix web run build
```

For local frontend development, `npm --prefix web run dev` proxies `/api` to
`DEEPSEEK_SERVER_URL`. The default target is `http://127.0.0.1:3000`. Production
assets do not use this proxy or require Node after the Rust binary is built.

The server exposes the application shell, its embedded files, and
`/api/health` from one reported loopback origin. Unknown non-API routes return
`index.html` so browser-side routes can load directly.

## Shell milestone practice

Build the production files before starting the practice server:

```powershell
npm --prefix web run build
cargo run -p deepseek-custom-tests --bin web_shell_probe
```

The probe prints its ephemeral loopback URL. It serves the same embedded files
as the Rust production server. Press Ctrl+C to stop it.

At the printed URL, verify these points without a mouse:

1. Use Tab to reach the skip link, then all eight workspace controls.
2. Continue to the current workspace action.
3. Confirm the active workspace has visible `Active` text and an announced current state.
4. Repeat at a desktop width and at 360 CSS pixels.
5. Confirm the page does not scroll horizontally. Long workspace content may scroll inside its own container.

The deterministic server check confirms that the HTML references the built script
and stylesheet, and that Rust serves both files:

```powershell
cargo test -p deepseek-custom-tests --test it production_shell_references_assets_that_the_rust_server_serves
```

This automated check does not replace browser observation. Record the browser,
viewport sizes, focus order, overflow result, and any visual gap with the run.
