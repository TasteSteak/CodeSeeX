# M1 API Surface

CodeSeeX Next starts with a deliberately small local API.

## Management

- `GET /health`: basic liveness.
- `GET /api/status`: current data directory, proxy base URL, catalog path, model list, and upstream target.
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
