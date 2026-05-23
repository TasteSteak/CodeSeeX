<h1 align="center">CodeSeeX</h1>

<p align="center">
  <img alt="Version 0.3.2" src="https://img.shields.io/badge/version-0.3.2-1f6feb">
  <img alt="Platform Windows macOS Linux" src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-2ea043">
  <img alt="License AGPL-3.0-only" src="https://img.shields.io/badge/license-AGPL--3.0--only-bd561d">
</p>

<p align="center">
  Run DeepSeek V4 in Codex with 1M context, Codex tool compatibility, and built-in Web Search.
</p>

<p align="center">
  Unofficial and unaffiliated. Use your own credentials and follow the applicable Codex, OpenAI, DeepSeek, and search-provider terms.
</p>

<p align="center">
  <img alt="CodeSeeX dashboard showing service status and balance" src="docs/img/dashboard-balance.png">
</p>

CodeSeeX gives Codex a local Responses API endpoint, forwards supported requests to DeepSeek, preserves the Codex tool workflow, and provides a desktop manager for setup, logs, usage, tools, and adapter configuration.

```text
Codex Desktop  ->  CodeSeeX local API  ->  DeepSeek API
                       ^
                       |
                 desktop manager
```

## What You Get

- DeepSeek V4 with 1M context: expose `deepseek-v4-pro` and `deepseek-v4-flash` to Codex with million-token catalog metadata.
- Codex tool compatibility: keep Codex workflows such as Apply Patch, MCP, Skills, Plugins, and native MCP tools available through the bridge.
- Built-in tool layer: use CodeSeeX Web Search, workspace search, file reading, and patch support out of the box.
- Generated setup: copy a ready-to-use `config.toml` from the CodeSeeX proxy settings page.
- Custom upstream support: point CodeSeeX at the official DeepSeek API or a self-hosted OpenAI-compatible endpoint.
- Local visibility: see service state, balance, usage estimates, user-level logs, and tool activity from the desktop UI.

## Quick Start

1. Download the latest build for your platform from [GitHub Releases](https://github.com/TasteSteak/CodeSeeX/releases).
2. Start CodeSeeX and open the desktop manager.
3. Go to `Settings -> Proxy`.
4. Confirm the local service is running. The default listen port is `8787`.
5. Copy the generated `config.toml` shown by CodeSeeX into the Codex configuration you use for DeepSeek.
6. Restart CodeSeeX only if you changed the listen port, then start or restart Codex Desktop.
7. In Codex, choose `deepseek-v4-pro` or `deepseek-v4-flash` and start a new conversation.

Release artifacts may vary by version. Build scripts are included for Windows, macOS, and Linux, and platform builds should be verified on real devices before release.

## Codex config.toml

Prefer copying the TOML generated inside CodeSeeX, because the local port and catalog path are machine-specific.

```toml
model_provider = "custom"
model = "deepseek-v4-pro"
disable_response_storage = true
model_reasoning_effort = "xhigh"
model_catalog_json = '~/.codeseex/model-catalog.json'

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

CodeSeeX reads API credentials from the user's Codex auth configuration. It does not store DeepSeek API keys in `proxy.env`.

If you self-host a DeepSeek-compatible service, set the upstream URL in `Settings -> Proxy`. Leave it blank to use the official DeepSeek API.

<p align="center">
  <img alt="CodeSeeX proxy settings with generated config.toml" src="docs/img/config-toml.png">
</p>

## Features

- Codex-compatible local API for `/v1/responses` and related model calls.
- DeepSeek V4 adapter catalog for `deepseek-v4-flash` and `deepseek-v4-pro` with `1M` context metadata.
- Compatibility with Codex built-in tool flows, including Apply Patch, MCP, Skills, and Plugins.
- Native MCP passthrough so Codex-configured MCP tools remain executed and displayed by the Codex app tool layer.
- Single configurable local port for the desktop manager, `/api/*`, and `/v1/*`.
- Proxy settings for catalog mode, upstream model override, custom upstream URL, billing rates, and generated `config.toml`.
- Streaming answer display with reasoning visibility controls and grouped proxy tool summaries.
- User-level logs, daily log retention, balance checks, usage estimates, tray shortcuts, auto-start, and silent update indicators.
- Built-in tools for Web Search, patching, workspace search, and file reading, plus optional community tools.

## Configuration & Tools

Runtime data is stored in the user's `~/.codeseex` directory by default so installed apps can write safely across Windows, macOS, and Linux.

Important runtime paths:

- `~/.codeseex/model-catalog.json`: adapter catalog referenced by Codex through `model_catalog_json`.
- `~/.codeseex/logs/`: daily user-level logs such as `logs-20260521.jsonl`.
- `~/.codeseex/extension/tools/<tool>/`: optional community tool packages.
- `~/.codeseex/proxy.env`: non-secret local runtime settings.

Community tools are disabled unless enabled in configuration. Enabling community tool code means running local code from that tool package, so only use tools you trust. See [docs/tool-authoring.md](docs/tool-authoring.md) for the tool package format.

MCP servers stay on the user's Codex configuration. CodeSeeX translates Codex-provided MCP tool declarations for DeepSeek, then returns native `function_call` items so Codex can execute and display MCP calls itself.

## Troubleshooting

### Balance Query Fails

- Make sure Codex auth is configured for the same user account.
- Confirm the machine can reach the DeepSeek API endpoint.
- If a local proxy is configured, make sure that proxy process is actually running.

### Codex Cannot See DeepSeek Models

- Confirm `model_catalog_json` points to an existing `~/.codeseex/model-catalog.json`.
- Copy the generated TOML from CodeSeeX instead of typing the path manually.
- Restart Codex after changing TOML.
- GPT/OpenAI TOML files do not need `model_catalog_json` and are not affected by CodeSeeX.

### Conversation Fails With `fetch failed`

- Check the CodeSeeX logs page for the upstream error.
- Confirm Codex `base_url` points to CodeSeeX, for example `http://127.0.0.1:8787/v1`.
- If you use the official upstream, test whether the machine can access `https://api.deepseek.com`.
- If you use a self-hosted upstream, confirm the URL is reachable and OpenAI-compatible.
- Make sure no other process is using the configured CodeSeeX port.

## Development

```sh
npm install
npm start
npm run check
npm run dist:win
```

Additional build scripts are available for target-platform testing:

```sh
npm run dist:mac
npm run dist:linux
```

GitHub Actions can build the desktop artifacts automatically for Linux, macOS, and Windows. See [Desktop Build Action](docs/build-actions.md) for the workflow setup and testing steps.

## Privacy & License

CodeSeeX is a local proxy, but model requests are forwarded to the configured DeepSeek API endpoint. Do not send code, secrets, personal data, or third-party material unless you have permission to process it with that service.

The built-in `web_search` tool may request search-result pages or regular web pages from third-party websites. Those services may apply their own terms, rate limits, and anti-abuse rules.

CodeSeeX is licensed under AGPL-3.0-only. See [LICENSE](LICENSE). If you modify and distribute CodeSeeX, or provide a modified version as a network service, you must make the corresponding source code available under the same license.
