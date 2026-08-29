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

The checked-in manifest currently names
`placeholder-until-vite-workspace-exists` as its generator. It keeps the Rust
server independently buildable before task 3.1 creates the Vite workspace. The
Vite production build must replace that generator value with `vite`, stage its
hashed output in the same asset directory, and write matching digests.

The server exposes the application shell, its embedded files, and
`/api/health` from one reported loopback origin. Unknown non-API routes return
`index.html` so browser-side routes can load directly.
