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
