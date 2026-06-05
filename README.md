# CodeSeeX Next

CodeSeeX Next is the temporary development workspace for the lighter Rust/Tauri rewrite of CodeSeeX.

During development it uses `~/.codeseex-next` only to keep test data away from the current released app. The final product remains CodeSeeX; this is a technical-stack upgrade, not a separate product line.

## Direction

- Rust core for proxy, protocol conversion, catalog generation, state, and diagnostics.
- Tauri 2 desktop shell with static WebView assets.
- SQLite for the minimal adapter state ledger, usage, logs, and diagnostics.
- TOML for readable user configuration.
- External compatibility with the current CodeSeeX Codex setup: port `8787`, `deepseek-v4-flash`, `deepseek-v4-pro`, generated `config.toml`, and `model_catalog_json`.

## Current Status

M1 proxy loop is in place:

- Generate `~/.codeseex-next/model-catalog.json`.
- Generate copyable Codex `config.toml`.
- Serve `/v1/models`, `/v1/chat/completions`, `/v1/responses`, `/api/status`, and `/api/codex-adapter/generate`.
- Forward to official DeepSeek or a custom OpenAI-compatible upstream.
- Record request lifecycle and events in SQLite.
- Bridge completed `previous_response_id` chains only when Codex actually uses server-side chaining; Codex full-context requests are not duplicated as durable transcripts.

M2 desktop management has started:

- Tauri desktop shell starts the embedded proxy before showing the window.
- The desktop manager reuses the proven CodeSeeX UI shell while the Rust/Tauri internals are migrated.
- In the desktop runtime, UI management calls go through Tauri commands instead of `127.0.0.1`, so the window can open even when the proxy port is occupied.
- Start, stop, and restart control the embedded proxy through graceful shutdown rather than fake inline no-op actions.
- Native tray supports quick model, thinking, and sampling-temperature changes.
- Close-to-tray, start-at-login, single-instance guard, and silent update checks are wired through the desktop layer.
- HTTP `/api/*` routes remain as compatibility/debug adapters; Codex compatibility is provided by `/v1/*`.

M3 context fidelity has started:

- Responses input is compiled through a deterministic context compiler before reaching the upstream model.
- Function/tool/MCP-like request facts that cannot be represented as plain chat messages are preserved as verified facts instead of being silently dropped.
- Inline `data:` URLs from tool facts are redacted to size/hash markers so screenshots or binary payloads do not poison prompt caching.
- Context diagnostics are persisted with each request checkpoint for debugging history reconstruction and compaction behavior.
- SQLite state is an adapter ledger, not a duplicate Codex transcript; large inline data, secrets, and oversized strings are sanitized before durable storage.
- Failed or interrupted parent turns can safely contribute user input and verified facts without replaying partial assistant text.
- `/v1/responses/compact` returns a local, readable compaction item and does not fake OpenAI `encrypted_content`.

M4 tool migration has started:

- `/api/tools` exposes the first system and built-in tool registry for the desktop Tools page.
- Tool enablement is persisted to TOML as an enabled id array; system tools such as Apply Patch and MCP do not expose client switches.
- `apply_patch` is exposed as a system/native capability only; CodeSeeX declares it to the upstream model and replays Codex's later `custom_tool_call_output`, but never applies patches internally.
- Codex-native MCP/external tools are passed through from Responses `tools` to the upstream model without proxy execution; tool calls are returned to Codex as native `function_call` items and later `function_call_output` turns replay as legal Chat tool pairs.
- `/v1/responses` can execute the first built-in tools in both non-streaming and streaming mode: `list_directory`, `read_file_range`, `workspace_search`, and `web_search`.
- Streaming tool calls are surfaced as native Responses `function_call` events before CodeSeeX executes the bounded built-in tool and continues the upstream conversation.
- Built-in tool calls emit separate call/result events and persist verified tool facts into SQLite so later `previous_response_id` turns can reconstruct what happened.
- The executor only runs enabled tools, revalidates workspace boundaries before reading files, and keeps Web Search text-only with local/private targets blocked by default.
- Community tools under `~/.codeseex-next/extension/tools/<tool>/manifest.json` are discovered for the Tools page, default to disabled, can persist safe UI settings, and execute only when the manifest declares an explicit external command.
- Community tool execution runs in a child process with no shell, a minimal environment, timeout handling, and bounded stdout/stderr capture; third-party code is never loaded into the proxy process.

See [docs/electron-parity-checklist.md](docs/electron-parity-checklist.md) for the migration release gate, [docs/state-contract.md](docs/state-contract.md) for the durable state boundary, and [docs/community-tools.md](docs/community-tools.md) for the current manifest and execution contract. The next step for community tools is parity hardening with broader platform executor validation.

## Development

Rust is required for the core workspace.

```sh
cargo run -p codeseex-proxy
cargo test --workspace
```

If Rust is not installed, install it from <https://rustup.rs/> and reopen the terminal so `cargo` is available in `PATH`.

On Windows, use the helper script when working from a normal PowerShell session:

```powershell
.\scripts\check-windows.ps1
.\scripts\context-fidelity-smoke-windows.ps1
.\scripts\start-desktop-windows.ps1
```

The scripts load MSVC Build Tools, import `.env` when present, keep Cargo caches on `D:\DevTools\CodeSeeXNext` when available, and check or launch the Rust/Tauri workspace. The desktop UI is served from `apps/ui/public` through Tauri's custom protocol; there is no Vite dev server in the normal workflow.

For a quick desktop smoke test from Explorer or `cmd.exe`, run:

```cmd
start-desktop.cmd
```
