# Milestones

See [Electron Parity Checklist](electron-parity-checklist.md) for the release gate that must be satisfied before the Tauri/Rust implementation replaces the current Electron line.

## M1 Proxy Loop

The first milestone proves the lightweight architecture without carrying over the old tool layer.

Acceptance criteria:

- `codeseex-proxy` starts on `127.0.0.1:8787`.
- `/api/codex-adapter/generate` writes `~/.codeseex-next/model-catalog.json` and returns a copyable TOML snippet.
- `/v1/models` lists `deepseek-v4-flash` and `deepseek-v4-pro`.
- `/v1/chat/completions` forwards JSON and streaming requests to the configured upstream.
- `/v1/responses` accepts basic Codex Responses input and maps non-tool output back to Responses-shaped results.
- Request checkpoints, failures, completions, and notable events are written to SQLite.

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
- Normal launch shows the main window; `--autostart` launch stays in the tray.
- Native tray can persist model override, thinking mode, and sampling temperature.
- Official Tauri autostart and single-instance plugins are wired.
- Update checks query GitHub Releases silently and return red-dot-ready status data.
- Inline proxy mode disables old start/stop/restart controls to avoid fake lifecycle actions.

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
- Request checkpoints persist context diagnostics, including current message count and verified fact count.
- `/v1/responses/compact` returns a local readable compaction item without fake `encrypted_content`.
- `npm run smoke:context:windows` starts a fake upstream plus the real proxy and verifies instructions, `previous_response_id` history, verified tool facts, completed parent output, failed parent safe replay, streaming parent persistence, manual compaction replay, native MCP/external tool passthrough and result replay, community command-tool execution, streaming built-in tool execution, and inline image redaction.

## M4 Tool System

Add tools only after the base proxy and context state are proven.

Acceptance criteria:

- Apply Patch behaves like Codex native freeform patching.
- MCP remains Codex-native and is not converted into fake CodeSeeX hosted tools.
- Web Search avoids base64/image payloads entering model-visible text context and blocks local/private targets by default.
- Community tools are disabled by default and isolated by design.

Current progress:

- `/api/tools` now exposes system tools and built-in tool metadata for the desktop Tools page.
- `ENABLED_TOOLS` is persisted as an enabled id array in TOML.
- Apply Patch, Web Search, and MCP Server are represented as non-configurable system tools with built-in source labels; Apply Patch and Web Search have CodeSeeX execution while MCP remains Codex-native and is passed through without proxy execution.
- `apply_patch` and `web_search` are always exposed as system executable tools; `apply_patch` applies Codex-style patches under the configured workspace root and returns stale-context recovery guidance on failure.
- Codex-native MCP/external tool declarations from Responses `tools` are normalized into upstream Chat function tools, then mapped back to native Responses `function_call` items for Codex execution.
- MCP/external `function_call_output` turns replay as legal Chat tool pairs only when the referenced previous response contains the matching function call; otherwise they remain verified facts.
- `/v1/responses` now supports tool loops in non-streaming and streaming mode: system `web_search`, plus configurable built-ins `list_directory`, `read_file_range`, and `workspace_search`.
- Streaming tool calls are emitted as native Responses `function_call` events, then CodeSeeX executes the bounded built-in tool, persists the verified fact, and continues the upstream stream.
- Built-in tool calls write separate call/result events and persist verified tool facts to SQLite for later `previous_response_id` reconstruction.
- Configurable built-in and community execution is gated by the persisted enabled-tool list; system tools are always enabled. Read-only tools revalidate workspace boundaries before touching files, and Web Search returns compact text-only evidence.
- Community tool manifests are discovered from `~/.codeseex-next/extension/tools/<tool>/manifest.json` for the desktop Tools page.
- Community tools default to disabled and can persist safe UI config fields into the existing TOML `[tools.settings]` table.
- Enabled community tools are advertised only when their manifest declares `execution.type = "command"`; execution runs as a child process with no shell, minimal environment variables, timeout handling, and bounded output capture.

Still staged:

- Community tool authoring docs, parity fixtures, and broader platform executor validation.
