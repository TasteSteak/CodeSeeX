# Milestones

See [Electron Parity Checklist](electron-parity-checklist.md) for the release gate that must be satisfied before the Tauri/Rust implementation replaces the current Electron line.

## M1 Proxy Loop

The first milestone proves the lightweight architecture without carrying over the old tool layer.

Acceptance criteria:

- `codeseex-proxy` starts on `127.0.0.1:8787`.
- `/api/codex-adapter/generate` writes `~/.codeseex/model-catalog.json` and returns a copyable TOML snippet.
- `/v1/models` lists `deepseek-v4-flash` and `deepseek-v4-pro`.
- `/v1/chat/completions` forwards JSON and streaming requests to the configured upstream.
- `/v1/responses` accepts basic Codex Responses input and maps non-tool output back to Responses-shaped results.
- Request lifecycle and usage are tracked in current-process runtime state; notable events are written to `logs/*.jsonl`.

Explicitly out of scope:

- Apply Patch bridge.
- MCP passthrough.
- Web Search.
- Community tools.
- Migration from `~/.codeseex`.

## M2 Desktop Manager

Add the Tauri management surface after M1 is stable.

Acceptance criteria:

- The proxy can run without opening the window.
- Tray state reflects proxy state.
- Settings are persisted to TOML and applied through the proxy core.
- UI reads status, catalog path, TOML snippet, usage, and logs through management APIs.

Current progress:

- Embedded proxy starts from the Tauri desktop setup path.
- Desktop start/stop/restart now controls the embedded proxy through native Tauri commands and graceful shutdown, rather than fake HTTP no-op actions.
- The Tauri UI reads `/api/*` management data through `desktop_manager_request`; direct HTTP `/api/*` remains only a compatibility/debug adapter.
- Normal launch shows the main window; `--autostart` launch stays in the tray.
- Native tray can persist model override, thinking mode, and sampling temperature.
- Official Tauri autostart and single-instance plugins are wired.
- Update checks query GitHub Releases silently and return red-dot-ready status data.
- Port conflicts no longer prevent the desktop window from opening; the embedded proxy failure is surfaced through runtime status.

## M3 High-Fidelity Context

Rebuild the context compiler in Rust using the lessons from the Electron version.

Acceptance criteria:

- Long `previous_response_id` chains are reconstructed deterministically.
- Compact records are explicit, inspectable, and never override verified tool/request facts.
- Interrupted requests preserve user input and verified facts but do not reuse partial assistant text as final context.

Current progress:

- Responses input now flows through a deterministic context compiler instead of the old plain text-only conversion path.
- Function/tool/MCP-like request items are retained as verified facts when they cannot be represented as legal chat protocol messages.
- Known tool call outputs are replayed as legal Chat `assistant.tool_calls` / `tool` pairs when the previous response contains the matching native call.
- Inline `data:` URLs in tool facts are redacted to deterministic size/hash markers to protect prompt caching from binary payloads.
- Context diagnostics are written to bounded logs, including current message count and verified fact count.
- `/v1/responses/compact` returns a readable summary plus a CodeSeeX-owned opaque `encrypted_content` payload, not fake OpenAI server state.

## M4 Tool System

Add tools only after the base proxy and context state are proven.

Acceptance criteria:

- Apply Patch behaves like Codex native freeform patching; CodeSeeX passes the native call to Codex and replays the later output instead of applying the patch itself.
- MCP remains Codex-native and is not converted into fake CodeSeeX hosted tools.
- Web Search avoids base64/image payloads entering model-visible text context and blocks local/private targets by default.
- Community tools are disabled by default and isolated by design.

Current progress:

- `/api/tools` now exposes system tools and built-in tool metadata for the desktop Tools page.
- `ENABLED_TOOLS` is persisted as an enabled id array in TOML.
- Apply Patch, Web Search, and MCP Server are represented as non-configurable system tools with built-in source labels; Web Search has CodeSeeX execution while Apply Patch and MCP remain Codex-native at the client boundary.
- `apply_patch` and `web_search` are always exposed as system tools; `apply_patch` is passed through as a native custom tool call and `web_search` returns compact text evidence from CodeSeeX.
- Codex-native MCP/external tool declarations from Responses `tools` are normalized into upstream Chat function tools, then mapped back to native Responses `function_call` items for Codex execution.
- MCP/external `function_call_output` turns replay as legal Chat tool pairs only when the referenced previous response contains the matching function call; otherwise they remain verified facts.
- `/v1/responses` now supports tool loops in non-streaming and streaming mode: system `web_search`, plus configurable built-ins `list_directory`, `read_file_range`, and `workspace_search`.
- Streaming regular built-in/community tool calls are emitted as display-only/proxy diagnostic output, then CodeSeeX executes the bounded tool, keeps current-request facts, and continues the upstream stream. Apply Patch uses native `custom_tool_call` events and is executed by Codex.
- Built-in tool calls write separate call/result events to `logs/*.jsonl`; verified tool facts stay scoped to the current request/process bridge.
- Configurable built-in and community execution is gated by the persisted enabled-tool list; system tools are always enabled. Read-only tools revalidate workspace boundaries before touching files, and Web Search returns compact text-only evidence.
- Community tool manifests are discovered from `~/.codeseex/extension/tools/<tool>/manifest.json` for the desktop Tools page.
- Community tools default to disabled and can persist safe UI config fields into the existing TOML `[tools.settings]` table.
- Enabled community tools are advertised only when their manifest declares `execution.type = "command"`; execution runs as a child process with no shell, minimal environment variables, timeout handling, and bounded output capture.

Still staged:

- Community tool authoring docs, parity fixtures, and broader platform executor validation.
