<h1 align="center">CodeSeeX</h1>

<p align="center">
  <img alt="Version 0.5.0" src="https://img.shields.io/badge/version-0.5.0-1f6feb">
  <img alt="Platform Windows macOS Linux" src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-2ea043">
  <img alt="License AGPL-3.0-only" src="https://img.shields.io/badge/license-AGPL--3.0--only-bd561d">
</p>

<p align="center">
  Run DeepSeek V4 in Codex with 1M context, Codex tool compatibility, Web Search, and a configurable Vision module.
</p>

<p align="center">
  Unofficial and unaffiliated. Use your own credentials and follow the applicable Codex, OpenAI, DeepSeek, and search-provider terms.
</p>

CodeSeeX is a local Codex-compatible bridge for using DeepSeek-compatible upstreams from Codex. The 0.5 line is the Rust/Tauri architecture release: faster startup, stronger desktop residency, cleaner runtime data, richer tool compatibility, and a more complete release installer flow.

Current version: `0.5.0`

```text
Codex Desktop  ->  CodeSeeX local API  ->  DeepSeek-compatible upstream
                       ^
                       |
                 desktop manager
```

## What You Get

- DeepSeek V4 models for Codex: `deepseek-v4-pro` and `deepseek-v4-flash`.
- Generated Codex `config.toml` with `model_catalog_json` and local `base_url`.
- Embedded model catalog seed for first-run machines without a native Codex catalog.
- 1M context metadata with a 95% effective context window for Flash and Pro.
- Codex-native Apply Patch and MCP boundaries: CodeSeeX passes native client tools back to Codex instead of executing them itself.
- CodeSeeX-hosted Web Search and read-only workspace tools with bounded execution and local/private target protection.
- Configurable Vision module for image understanding and image generation through OpenAI-compatible endpoints.
- High-fidelity Responses-to-Chat context compilation with verified tool facts, compact summaries, and binary/data URL redaction.
- Compact user logs and runtime usage summaries without duplicating Codex conversation transcripts.
- Tauri desktop manager with tray controls, autostart, update checks, logs, usage, balance, and settings.
- Community tool discovery under `~/.codeseex/extension/tools/<tool>/manifest.json`, disabled by default and executed only through explicit command manifests.

## Quick Start

1. Start CodeSeeX.
2. Open `Settings -> Proxy` and confirm the local service is running on the default port `8787`.
3. Copy the generated Codex TOML from the CodeSeeX adapter card.
4. Put that TOML into the Codex configuration you use for DeepSeek.
5. Restart Codex after changing TOML.
6. Select `deepseek-v4-pro` or `deepseek-v4-flash` in Codex.

Prefer the generated TOML because the catalog path and local port are machine-specific.

```toml
model_provider = "custom"
model = "deepseek-v4-pro"
disable_response_storage = true
model_context_window = 1000000
model_auto_compact_token_limit = 950000
model_reasoning_effort = "xhigh"
model_catalog_json = "C:\\Users\\you\\.codeseex\\model-catalog.json"

[model_providers.custom]
name = "DeepSeek"
wire_api = "responses"
requires_openai_auth = true
base_url = "http://127.0.0.1:8787/v1"
```

## Install And Update

On Windows, use the NSIS `CodeSeeX_*_setup.exe` installer for normal desktop installs and updates. It supports installer language selection, current-user or all-users install mode, and migration from the earlier Electron build by uninstalling the legacy app before installing the Tauri build.

## Vision Module

The Vision module is optional and configurable from the desktop Tools settings. Configure full request URLs, model names, and an API key for the endpoints you want to use:

- Analyze endpoints: OpenAI-compatible `/responses` or `/chat/completions`.
- Generate endpoints: OpenAI-compatible `/responses` with image generation support or `/images/generations`.
- Image inputs: current Codex `input_image` attachments, HTTP(S) URL, `data:image` URL, `file://` URL, workspace path, or permitted local absolute path.

CodeSeeX does not rewrite Vision endpoint URLs. The request URL you configure is the request URL that will be used. When a local image is analyzed through a remote endpoint, the image pixels are sent to that configured service.

## Credential Boundary

CodeSeeX manager settings do not store upstream API keys. Balance checks read the direct Codex auth source or a cached request `Authorization: Bearer ...` header. A legacy `DEEPSEEK_API_KEY` environment value can still act as a fallback for direct upstream requests, but it is not the balance credential source.

## Runtime Data

CodeSeeX uses the normal release data directory:

```text
~/.codeseex/
  config.toml
  model-catalog.json
  logs/
  extension/tools/
  secrets/
```

Codex owns the conversation transcript. CodeSeeX keeps only current-process bridge state, bounded logs, and explicit compact payload material needed for the proxy boundary.

User-facing logs stay compact by default. Diagnostic events are not persisted unless diagnostic logging is explicitly enabled for development.

## Development

Rust is required for the core workspace.

```sh
cargo run -p codeseex-proxy
cargo test --workspace
```

On Windows, helper scripts load MSVC Build Tools when available, import `.env`, and keep Cargo caches under a configurable local dev directory by default:

```powershell
.\scripts\check-windows.ps1
.\scripts\start-desktop-windows.ps1
```

The desktop UI is served from `apps/ui/public` through Tauri's custom protocol; there is no Vite dev server in the normal workflow.

## Documentation

- [CHANGELOG.md](CHANGELOG.md) for release notes.
- [CHANGELOG.zh-CN.md](CHANGELOG.zh-CN.md) for Chinese release notes.
- [docs/installer-migration.md](docs/installer-migration.md) for installer and legacy migration behavior.
- [docs/electron-parity-checklist.md](docs/electron-parity-checklist.md) for migration parity gates.
- [docs/state-contract.md](docs/state-contract.md) for runtime/log state boundaries.
- [docs/community-tools.md](docs/community-tools.md) for community tool manifests and execution rules.

## License

CodeSeeX is licensed under AGPL-3.0-only. See [LICENSE](LICENSE).
