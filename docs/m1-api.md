# M1 API Surface

CodeSeeX starts with a deliberately small local API.

## Management

- `GET /health`: basic liveness.
- `GET /api/status`: current data directory, proxy base URL, catalog path, model list, and upstream target.
- `GET /api/models`: app-server-shaped model catalog for host UIs. Supports `limit`, `cursor`, and `includeHidden`.
- `POST /api/app-server`: minimal JSON-RPC compatibility endpoint. Currently supports `model/list` for host model pickers.
- `GET|POST /codex-model-catalog`: Codex App renderer bridge catalog used by the model picker injection path.
- `GET /codeseex/renderer-inject.js`: generated renderer patch script with the current CodeSeeX model catalog embedded.
- `POST /api/codex-app/inject`: inject the renderer patch into an already debug-enabled Codex App.
- `POST /api/codex-app/launch`: launch Codex with remote debugging and inject the renderer patch.
- `GET|POST /api/codex-adapter/generate`: generate catalog and return TOML snippet.

## Codex-Compatible

- `GET /v1/models`: OpenAI-style model list.
- `POST /v1/chat/completions`: forwards OpenAI-compatible chat requests to DeepSeek/custom upstream.
- `POST /v1/responses`: accepts basic Responses input and maps text output to Responses-shaped data.

## M1 Limitations

- Tools are intentionally unsupported.
- Streaming Responses currently maps text deltas only.
- Reasoning display is not yet a first-class UI feature.
- Previous response chain reconstruction waits for the M3 context compiler.

These limitations describe the original M1 boundary. Later milestones add context reconstruction, compact handling, Apply Patch, MCP passthrough, built-in tools, and community-tool discovery.
