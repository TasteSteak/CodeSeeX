# CodeSeeX 0.9.0 Release Checklist

## Release identity

- Application version: `0.9.0`
- Catalog revision: `0.8.3.1`
- Release date: `2026-10-21`

## Required catalog seed

The reviewed prompt seed is private and must be supplied through
`CODESEEX_MODEL_CATALOG_SEED` or the `CODESEEX_MODEL_CATALOG_SEED_GZIP_BASE64`
release secret.

- Local seed: `.private/model-catalog.pending-2026-10-21-v2.seed.json`
- SHA-256: `78396b8554405ffe3367bf9622ab601d5e87b45a9770b0ff25a1a286c824fd57`

The seed is intentionally excluded from Git. A public catalog update cannot
replace `base_instructions` or `model_messages`.

## Validation

```powershell
cargo fmt --all --check
cargo test -p codeseex-core --lib
cargo test -p codeseex-proxy --lib
node --check apps/ui/public/app.js
```

## Windows package

```powershell
$env:CODESEEX_MODEL_CATALOG_SEED='D:\桌面文件\codeseex-next\.private\model-catalog.pending-2026-10-21-v2.seed.json'
cargo tauri build --ci --features tauri/custom-protocol --bundles nsis
```

The release workflow must provide the updater signing key separately. Local
test builds may disable `bundle.createUpdaterArtifacts`.

Local validation artifact:

- `dist/CodeSeeX_0.9.0_x64-setup.exe`
- SHA-256: `ac78d01585c9db7901779cf00bf034e535c8c81a5e16b505b927a45e2d29bb1d`

## Proactive input component

`request_user_input` is a Codex client capability. For Default-mode testing,
enable the client feature:

```powershell
codex features enable default_mode_request_user_input
```

Restart Codex and create a new thread after changing the feature or installing a
new catalog. Existing threads keep their original prompt and tool state.

## Catalog publication

Publish `catalog/model-catalog.json` at revision `0.8.3.1`. Older numeric remote
revisions are ignored by CodeSeeX 0.9.0, so an outdated mirror cannot remove the
new web-search billing entry.
