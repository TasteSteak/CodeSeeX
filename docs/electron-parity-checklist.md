# Electron Parity Checklist

This checklist is the migration guardrail for replacing the Electron implementation with the Rust/Tauri implementation. It captures validated behavior from the current CodeSeeX line so the rewrite does not drift into a similar-looking but incompatible product.

## Release Gate

- Do not call Next a replacement until every P0 item is verified against real Codex App traffic.
- Keep the temporary data directory as `~/.codeseex-next` during development.
- The final product remains CodeSeeX; Next is only the implementation workspace name.
- Any behavior that differs from Electron must be deliberate, documented, and tested.

## P0 Protocol Parity

- `model_catalog_json` points to a generated catalog that Codex App can read with and without language packs loaded.
- `/v1/models` lists `deepseek-v4-flash` and `deepseek-v4-pro`.
- `/v1/chat/completions` supports non-streaming and streaming pass-through.
- `/v1/responses` maps Codex Responses input to legal DeepSeek Chat messages.
- Streaming tool loops preserve DeepSeek `reasoning_content` when an assistant message contains `tool_calls`.
- Non-streaming tool loops preserve DeepSeek `reasoning_content` from upstream assistant tool-call messages.
- Usage maps to Responses-compatible fields: input, cached input, cache miss input, output, reasoning output, and total.
- `response.completed` includes stable Codex metadata: `error`, `incomplete_details`, and `parallel_tool_calls`.
- Upstream URL normalization supports official DeepSeek and custom OpenAI-compatible/local deployments.

## P0 Context And State

- Request lifecycle is durable in SQLite from request start: in progress, completed, failed, and interrupted.
- Long `previous_response_id` chains are reconstructed without low fixed-depth truncation.
- Completed parent turns replay assistant final text or legal tool pairs.
- Failed/interrupted parent turns replay user input and verified facts only, never partial assistant final text.
- Tool facts outrank assistant self-description during replay.
- Inline `data:` URLs are redacted to deterministic size/hash markers before entering model-visible text context.
- Manual compact produces explicit readable compaction items, not fake `encrypted_content`.
- Compact summaries never override verified tool facts.

## P0 Tool Parity

- Apply Patch behaves as a Codex-native freeform editing capability from the model's perspective.
- Apply Patch stays inside the configured workspace root and rejects escapes.
- Apply Patch failure returns stale-context recovery guidance.
- Web Search is a system/built-in capability and does not expose a user switch.
- Web Search returns compact text evidence and blocks local/private targets by default.
- MCP remains Codex-native: CodeSeeX must not require users to move MCP configuration into CodeSeeX.
- MCP/external tool declarations from Codex are passed through to upstream, then returned as native Responses function calls for Codex execution.
- Built-in read-only tools are configurable and default enabled.
- Community tools are disabled by default and only execute explicit command manifests.
- Community tool execution uses no shell, minimal environment, timeout handling, and bounded stdout/stderr.

## P0 Desktop And UI

- The proxy starts before the main window depends on API data.
- The UI is a manager surface only; it does not own the proxy request pipeline.
- Settings changes persist to TOML and refresh tray/menu state.
- Tray supports model override, thinking mode, and sampling temperature.
- Autostart starts to tray without forcing the main window open.
- Single-instance guard prevents duplicate desktop processes.
- The manager exposes Overview, Configuration, Tools, Logs/Usage, and About/Update status.
- `config.toml` remains copy-only; CodeSeeX does not edit the user's Codex config.

## P1 Product Parity

- Language selection supports system, English, and Chinese at minimum.
- Dark/light theme boundaries remain visible.
- Logs distinguish normal conversation requests from context compaction requests.
- Balance query reads Codex auth when available and respects custom upstream limitations.
- Update checks are silent and red-dot-ready, with no automatic download.
- Packaging emits only the intended installer artifacts.

## Verification Matrix

- Real Codex App: model visibility, first response, streaming response, and usage.
- Real Codex App: Apply Patch success, Apply Patch stale-context retry, and multi-file patch.
- Real Codex App: MCP discovery, MCP function call, and MCP result replay.
- Real Codex App: Web Search call, URL open call, and large result redaction.
- Real Codex App: manual compact, automatic compact-like long context replay, and post-compact follow-up.
- Fake upstream: usage mapping, streaming tool loop with `reasoning_content`, upstream 400/500 errors, and interrupted stream.
- Desktop smoke: fresh data dir, configured data dir, port conflict, tray actions, autostart read/write, and update check failure.

