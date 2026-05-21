<h1 align="center">CodeSeeX</h1>

<p align="center">
  <img alt="Version 0.3.0" src="https://img.shields.io/badge/version-0.3.0-1f6feb">
  <img alt="Platform Windows macOS Linux" src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-2ea043">
  <img alt="License AGPL-3.0-only" src="https://img.shields.io/badge/license-AGPL--3.0--only-bd561d">
</p>

<p align="center">
  A local Codex-compatible bridge that lets Codex use DeepSeek through a desktop manager.
</p>

CodeSeeX exposes a local OpenAI-compatible Responses API for Codex, forwards model requests to DeepSeek, and provides a desktop UI for configuration, adapter catalog generation, logs, usage, and tool visibility.

CodeSeeX is an unofficial, unaffiliated learning and research tool. It is not endorsed by OpenAI, Codex, or DeepSeek. Use it with your own API credentials and follow the applicable Codex, OpenAI, DeepSeek, and search-provider terms.

## Preview

Recommended screenshots to capture for the project page:

- Dashboard: CodeSeeX service status, balance card, and recent logs.
- Proxy settings: listen port, billing fields, thinking display, and model catalog helper.
- Codex TOML setup: `base_url` and `model_catalog_json` filled in.
- Codex model selector: `deepseek-v4-flash` and `deepseek-v4-pro` visible.
- Conversation demo: DeepSeek reasoning/tool display and final answer working in Codex.

## Why CodeSeeX

- Use DeepSeek from Codex through a local Responses API bridge.
- Keep Codex and CodeSeeX independent: start CodeSeeX only when you want the local proxy.
- Generate a DeepSeek V4 adapter catalog so Codex can understand the model metadata, context window, and tool behavior.
- View user-level logs, request status, usage estimates, and tool activity in a local desktop UI.
- Extend tools through `~/.codeseex/extension/tools/<tool>/` without changing built-in source files.

```text
Codex  ->  CodeSeeX local API  ->  DeepSeek API
              ^
              |
        desktop manager
```

## Current Release

Windows installer and portable builds are available from the [GitHub Releases page](https://github.com/TasteSteak/CodeSeeX/releases):

- `CodeSeeX Setup 0.3.0.exe`: Windows installer.
- `CodeSeeX 0.3.0.exe`: Windows portable build.

macOS and Linux build scripts are included, but release artifacts should be treated as pending until they are verified on real devices.

## Quick Start

1. Install or start CodeSeeX.
2. Open the CodeSeeX manager UI.
3. Make sure the local service is running on the configured port. The default is `8787`.
4. Open `Settings -> Proxy` and confirm the catalog status is ready.
5. Copy the generated `config.toml` snippet shown by CodeSeeX into the Codex configuration you use for DeepSeek.
6. Restart CodeSeeX after port changes, then start Codex Desktop with that DeepSeek TOML.
7. In Codex, select `deepseek-v4-pro` or `deepseek-v4-flash` and start a new conversation.

CodeSeeX reads API credentials from the user's Codex auth file. It does not store DeepSeek API keys in `proxy.env`.

## Codex TOML Example

Copy the TOML shown inside CodeSeeX whenever possible, because the catalog path and port are machine-specific.

```toml
model_provider = "custom"
model = "deepseek-v4-pro"
disable_response_storage = true
model_reasoning_effort = "xhigh"
model_catalog_json = 'C:/Users/you/.codeseex/model-catalog.json'

[model_providers.custom]
name = "DeepSeek"
wire_api = "responses"
requires_openai_auth = true
base_url = "http://127.0.0.1:8787/v1"
```

To use the faster model, change:

```toml
model = "deepseek-v4-flash"
```

If you change the CodeSeeX listen port, update `base_url` to the same port and restart CodeSeeX.

## Main Features

- Local Codex-compatible `/v1/responses` bridge.
- DeepSeek V4 model mapping for `deepseek-v4-flash` and `deepseek-v4-pro`.
- Adapter catalog generation at `~/.codeseex/model-catalog.json`.
- Streaming output with reasoning display control.
- Usage estimate and balance display.
- User-level logs with daily log files and retention options.
- Built-in tool bridge for `apply_patch`, `web_search`, `workspace_search`, and `read_file_range`.
- Community tool discovery from `~/.codeseex/extension/tools/<tool>/` by default.
- Single configurable local listen port through `PROXY_PORT`.

## Runtime Files

Most settings can be changed in the desktop manager. Runtime data is stored in the user's `~/.codeseex` directory by default so installed apps can write safely across Windows, macOS, and Linux. Set `PROXY_DATA_DIR` to choose another data directory, or `PORTABLE_EXECUTABLE_DIR` for an explicit portable layout.

Runtime folders include:

- `lang/`: optional language overrides.
- `logs/`: daily user-level logs such as `logs-20260519.jsonl`.
- `debug/`: diagnostic files when debug mode is enabled.
- `extension/tools/`: community tool packages.
- `proxy.env`: non-secret local runtime settings.

Example non-secret `proxy.env` values:

```env
PROXY_HOST=127.0.0.1
PROXY_PORT=8787
DEEPSEEK_THINKING=auto
SHOW_THINKING=true
```

## Adapter Catalog

Codex needs model metadata to understand the DeepSeek V4 models. CodeSeeX maintains this file:

```text
~/.codeseex/model-catalog.json
```

CodeSeeX first tries to derive the adapter catalog from the user's installed Codex catalog. If that is unavailable, release builds use a packaged compressed seed so new users can still start Codex with the DeepSeek V4 models. A minimal emergency fallback is kept only to avoid a missing catalog file.

GPT/OpenAI TOML files do not need `model_catalog_json` and are not affected by CodeSeeX.

## Community Tools

Built-in tools live under `src/tools/<tool>/`. Community tools are discovered from:

```text
~/.codeseex/extension/tools/<tool>/
```

Each tool should provide a `manifest.json`. Fixed icon files can be placed under the tool folder, such as `assets/icon.svg` or `assets/icon.png`.

Community tool code execution is intentionally controlled by configuration. Enabling community tool code means running local code from that tool package, so only use tools you trust.

See `docs/tool-authoring.md` for tool authoring details.

## Troubleshooting

### Balance Query Fails

- Make sure Codex auth is already configured for the same user account.
- Confirm CodeSeeX can reach the DeepSeek API endpoint.
- If the error mentions TLS certificate hostname mismatch, DNS/proxy software may be redirecting `api.deepseek.com`.
- If a local proxy is configured, make sure the proxy process is actually listening.

### Codex Cannot See DeepSeek Models

- Confirm `model_catalog_json` points to an existing `~/.codeseex/model-catalog.json`.
- Copy the TOML snippet from CodeSeeX instead of typing the path manually.
- Restart Codex after changing TOML.
- If Codex Desktop still shows an empty model selector, try starting a new conversation or restarting Codex Desktop.

### Conversation Fails With `fetch failed`

- Check the CodeSeeX logs page for the upstream error.
- Test whether the machine can access `https://api.deepseek.com`.
- Confirm the local `base_url` in Codex points to CodeSeeX, for example `http://127.0.0.1:8787/v1`.
- Make sure no other process is occupying the configured CodeSeeX port.

### Port Already In Use

- Change the CodeSeeX listen port in the proxy settings page.
- Restart CodeSeeX after changing the port.
- Update Codex `base_url` to the same port.

## Development

Install dependencies:

```sh
npm install
```

Start the desktop app:

```sh
npm start
```

Start only the manager/API service:

```sh
npm run start:manager
```

Start the standalone proxy entry for focused debugging:

```sh
npm run start:proxy
```

Run syntax checks:

```sh
npm run check
```

Build Windows release artifacts:

```sh
npm run dist:win
```

Build scripts for macOS/Linux are available, but should be run on the target platform:

```sh
npm run dist:mac
npm run dist:linux
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
|-- docs/                # Documentation
|-- manager.js           # Manager CLI entry
|-- proxy.js             # Proxy CLI entry
|-- proxy.env.example    # Local config template
`-- package.json
```

Runtime files such as `proxy.env`, `runtime.json`, `proxy-state.json`, `logs/`, `debug/`, `dist/`, and `node_modules/` are ignored by Git. By default, app runtime data is written to `~/.codeseex`; development scripts can override this with `PROXY_DATA_DIR`.

## Privacy and Third-Party Services

CodeSeeX is a local proxy, but model requests are forwarded to the configured DeepSeek API endpoint. Do not send code, secrets, personal data, or third-party material unless you have permission to process it with that service.

The built-in `web_search` tool may request pages or search-result pages from third-party websites. Search engines and websites may apply their own terms, rate limits, and anti-abuse rules.

## License

AGPLv3. See `LICENSE`. If you modify and distribute CodeSeeX, or provide a modified version as a network service, you must make the corresponding source code available under the same license.
