# Architecture Notes

CodeSeeX Next separates long-lived proxy behavior from the desktop window.

```text
Codex App -> 127.0.0.1:8787/v1 -> codeseex-proxy -> DeepSeek/custom upstream
                                      |
                                      +-> SQLite state, logs, usage
                                      +-> generated model-catalog.json
                                      +-> generated config.toml snippet

Tauri tray/window -> local management API -> same proxy state
```

The proxy is the product core. The desktop UI is a management surface and must not be required for requests to continue flowing once the service is running.

## Data Directory

Development data lives under `~/.codeseex-next`:

- `config.toml`: readable CodeSeeX config.
- `codeseex.db`: request state, logs, usage, diagnostics.
- `model-catalog.json`: generated Codex model catalog.
- `extension/tools/<tool>/manifest.json`: optional community tool metadata and explicit command execution declarations.

The `-next` data directory is development-only isolation. The final product remains CodeSeeX, so the release plan should use the normal CodeSeeX data location or an explicit in-app upgrade path rather than framing this as a separate product migration.
