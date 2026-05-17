# CodeSeeX

CodeSeeX is a local bridge between Codex and DeepSeek. It exposes an OpenAI-compatible Responses API endpoint for Codex, forwards requests to DeepSeek Chat Completions, and provides a local desktop manager for configuration, logs, usage, and tool visibility.

CodeSeeX is an unofficial, unaffiliated learning and research tool. It is not endorsed by OpenAI, Codex, or DeepSeek. Use it with your own API credentials and follow the applicable Codex, OpenAI, DeepSeek, and search-provider terms.

## Features

- Local Responses API proxy for Codex.
- DeepSeek upstream integration with streaming, reasoning display control, and token usage tracking.
- Desktop manager built with Electron plus a local web UI.
- Registered tool packages under `src/tools/<tool>/`, with community tools discoverable from `extension/tools/<tool>/`.
- Built-in tool bridge for `apply_patch`, `web_search`, `workspace_search`, and `read_file_range`.
- Single configurable CodeSeeX listen port through `PROXY_PORT`.

```text
Codex  ->  CodeSeeX proxy  ->  DeepSeek API
              ^
              |
        desktop manager
```

## Get Started

Download the latest Windows installer or portable build from the project releases, then start
`CodeSeeX`. The desktop app opens a local manager UI and starts the local Codex-compatible API on
the configured port.

CodeSeeX and Codex are separate apps. Start CodeSeeX when you want the local proxy available, then
start Codex Desktop with a DeepSeek-specific TOML that points to CodeSeeX:

```toml
model_provider = "custom"
model = "deepseek-v4-pro"
model_catalog_json = 'C:/Users/you/.codeseex/model-catalog.json'

[model_providers.custom]
name = "DeepSeek"
wire_api = "responses"
requires_openai_auth = true
base_url = "http://127.0.0.1:8787/v1"
```

Copy the full TOML example from the CodeSeeX About page so the port and catalog path match your
machine. If the listen port is changed in CodeSeeX, restart CodeSeeX and update Codex `base_url` to
the same port.

API credentials are read from the user's Codex auth file, not from `proxy.env`.

## Configuration

Most settings can be changed in the desktop manager. Runtime folders are created next to
`CodeSeeX.exe`: `lang/`, `logs/`, and `extension/tools/`.

For local overrides, CodeSeeX stores a small `proxy.env` file with non-secret runtime settings:

```env
PROXY_HOST=127.0.0.1
PROXY_PORT=8787
DEEPSEEK_THINKING=auto
SHOW_THINKING=true
```

For DeepSeek V4 model metadata, CodeSeeX maintains an adapter catalog under the current user's
`.codeseex/model-catalog.json`. GPT/OpenAI TOML files do not need this setting and are not affected.

The adapter catalog is generated locally from the user's installed Codex catalog. CodeSeeX does not
ship a prebuilt OpenAI/Codex model catalog or bundled official system prompt file.

## Development

Install dependencies from source:

```powershell
npm install
```

Start the desktop app:

```powershell
npm start
```

For CLI-only development:

```powershell
npm run start:manager
```

The manager serves both the desktop UI and Codex API on one port. The standalone proxy entry remains
available for focused proxy debugging:

```powershell
npm run start:proxy
```

## Project Layout

```text
codeseex/
|-- src/
|   |-- electron/        # Electron desktop shell
|   |-- manager/         # Manager API and static UI
|   |-- proxy/           # Responses API proxy
|   |-- shared/          # Shared config and utilities
|   `-- tools/           # Built-in tool packages
|-- extension/tools/     # Community tool packages in installed/runtime directories
|-- manager.js           # Manager CLI entry
|-- proxy.js             # Proxy CLI entry
|-- proxy.env.example    # Local config template
`-- package.json
```

In development, visible runtime folders are created in the project root. Runtime files such as `proxy.env`, `runtime.json`, `proxy-state.json`, `logs/`, `debug/`, `dist/`, and `node_modules/` are ignored by Git.

Run syntax checks before committing:

```powershell
npm run check
```

## Privacy and Third-Party Services

CodeSeeX is a local proxy, but model requests are forwarded to the configured DeepSeek API endpoint.
Do not send code, secrets, personal data, or third-party material unless you have permission to
process it with that service.

The built-in `web_search` tool may request pages or search-result pages from third-party websites.
Search engines and websites may apply their own terms, rate limits, and anti-abuse rules.

## License

AGPLv3. See `LICENSE`. If you modify and distribute CodeSeeX, or provide a modified version as a
network service, you must make the corresponding source code available under the same license.
