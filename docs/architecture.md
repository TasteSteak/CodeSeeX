# Architecture Notes

CodeSeeX separates the Codex-facing proxy pipeline from the desktop management surface.

```text
Codex App -> 127.0.0.1:8787/v1 -> proxy server -> DeepSeek/custom upstream
                                     |
                                     +-> protocol conversion
                                     +-> context compiler
                                     +-> tool ownership and execution
                                     +-> in-memory runtime state
                                     +-> logs/*.jsonl diagnostics
                                     +-> generated model-catalog.json

Tauri tray/window -> desktop_manager_request command -> manager runtime
                                                   |
                                                   +-> TOML config
                                                   +-> runtime usage/log file queries
                                                   +-> embedded proxy lifecycle

HTTP /api/* -> thin compatibility/debug adapter -> manager runtime
```

The proxy is the product core. The desktop UI is a management surface and must not be required for requests to continue flowing once the service is running. In the Tauri desktop runtime, UI management requests must use commands instead of calling `127.0.0.1` directly; this prevents UI startup from depending on the proxy port and keeps port conflicts visible without killing the window.

The only public compatibility contract for Codex is `/v1/*`. The `/api/*` routes are retained for browser/debug access and should remain thin adapters, not a second source of business logic.

## Runtime Boundaries

- `core`: protocol types, config, model/catalog definitions, URL normalization, and context-safe helpers.
- `store`: in-memory request lifecycle/usage, JSONL log IO, and short-lived current-process bridge state.
- `proxy`: HTTP `/v1/*`, upstream conversion, context compilation, tool ownership, hosted tool execution, and the shared manager runtime.
- `desktop`: Tauri window/tray/autostart/single-instance shell, embedded proxy lifecycle, and command bridge to the manager runtime.
- `ui`: static WebView assets only; no Node/Vite runtime is required for normal desktop use.

## Data Directory

User data lives under `~/.codeseex`:

- `config.toml`: readable CodeSeeX config.
- `model-catalog.json`: generated Codex model catalog.
- `logs/YYYY-MM-DD.jsonl`: durable local logs for UI and diagnostics.
- `runtime/`: transient process files such as locks or status snapshots.
- `cache/`: deletable generated/cache data.
- `secrets/compact.key`: local key material for CodeSeeX-readable compact payloads.
- `lang/`: user or third-party language overrides.
- `extension/tools/<tool>/manifest.json`: optional community tool metadata and explicit command execution declarations.

CodeSeeX uses the normal CodeSeeX data location. Any migration work should be framed as an in-app upgrade path rather than a separate product line.

## State Boundary

Codex owns the raw session transcript files and conversation context. CodeSeeX does not parse those files as protocol state and does not duplicate Codex request context into a database. When Codex sends a full-context request, the proxy forwards that replay as the authority for the current upstream call. A bounded process-local Canonical Session Core retains only anonymous session keys and ordered message fingerprints to diagnose append, divergence, and Codex compaction checkpoints; it never persists the replay body or rewrites it into a tail-only continuation.

The store keeps only current-process facts needed at the adapter boundary: request lifecycle, short-lived `previous_response_id` bridge data, usage, and diagnostics. Tool facts are evidence for CodeSeeX-owned tool execution during the current request/process, not a durable replacement for Codex's own tool/event transcript.

Logs are sanitized before file writes. Legacy `codeseex.db` files are ignored rather than used as protocol state. See [state-contract.md](state-contract.md) for the runtime/log state contract.
