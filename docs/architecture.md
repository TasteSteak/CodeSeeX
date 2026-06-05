# Architecture Notes

CodeSeeX separates the Codex-facing proxy pipeline from the desktop management surface.

```text
Codex App -> 127.0.0.1:8787/v1 -> proxy server -> DeepSeek/custom upstream
                                     |
                                     +-> protocol conversion
                                     +-> context compiler
                                     +-> tool ownership and execution
                                     +-> SQLite state, logs, usage
                                     +-> generated model-catalog.json

Tauri tray/window -> desktop_manager_request command -> manager runtime
                                                   |
                                                   +-> TOML config
                                                   +-> SQLite usage/log queries
                                                   +-> embedded proxy lifecycle

HTTP /api/* -> thin compatibility/debug adapter -> manager runtime
```

The proxy is the product core. The desktop UI is a management surface and must not be required for requests to continue flowing once the service is running. In the Tauri desktop runtime, UI management requests must use commands instead of calling `127.0.0.1` directly; this prevents UI startup from depending on the proxy port and keeps port conflicts visible without killing the window.

The only public compatibility contract for Codex is `/v1/*`. The `/api/*` routes are retained for browser/debug access and should remain thin adapters, not a second source of business logic.

## Runtime Boundaries

- `core`: protocol types, config, model/catalog definitions, URL normalization, and context-safe helpers.
- `store`: SQLite adapter ledger, request lifecycle, logs, usage, bounded tool facts, and short-lived bridge state.
- `proxy`: HTTP `/v1/*`, upstream conversion, context compilation, tool ownership, hosted tool execution, and the shared manager runtime.
- `desktop`: Tauri window/tray/autostart/single-instance shell, embedded proxy lifecycle, and command bridge to the manager runtime.
- `ui`: static WebView assets only; no Node/Vite runtime is required for normal desktop use.

## Data Directory

Development data lives under `~/.codeseex-next`:

- `config.toml`: readable CodeSeeX config.
- `codeseex.db`: minimal adapter ledger for request lifecycle, short-lived bridge state, bounded tool facts, compact records, usage, logs, and diagnostics.
- `model-catalog.json`: generated Codex model catalog.
- `extension/tools/<tool>/manifest.json`: optional community tool metadata and explicit command execution declarations.

The `-next` data directory is development-only isolation. The final product remains CodeSeeX, so the release plan should use the normal CodeSeeX data location or an explicit in-app upgrade path rather than framing this as a separate product migration.

## State Boundary

Codex owns the raw session transcript files and conversation context. CodeSeeX does not parse those files as protocol state and does not duplicate Codex request context into SQLite. When Codex sends a full-context request, the proxy uses it for the current upstream call and stores only a bounded adapter slice for diagnostics and interrupted-turn recovery.

The store keeps only the facts needed at the adapter boundary: request lifecycle, short-lived `previous_response_id` bridge data when Codex uses it, bounded tool facts, compact records, usage, and diagnostics. Tool facts are evidence for CodeSeeX-owned tool execution, not a durable replacement for Codex's own tool/event transcript.

New state writes are sanitized before persistence, and maintenance sanitizes oversized legacy request payloads in place without deleting request identity. See [state-contract.md](state-contract.md) for the durable state contract.
