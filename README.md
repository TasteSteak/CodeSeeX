<h1 align="center">CodeSeeX</h1>

<p align="center">
  <img alt="Version 0.5.4" src="https://img.shields.io/badge/version-0.5.4-1f6feb">
  <img alt="Platform Windows macOS Linux" src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-2ea043">
  <img alt="License AGPL-3.0-only" src="https://img.shields.io/badge/license-AGPL--3.0--only-bd561d">
</p>

<p align="center">
  <a href="https://tastesteak.github.io/CodeSeeX/">Official website</a>
  ·
  <a href="README_CN.md">简体中文</a>
</p>

<p align="center">
  A local Codex-native runtime for DeepSeek V4, built for real agent work rather than plain API forwarding.
</p>

<p align="center">
  <img alt="CodeSeeX desktop manager dashboard" src="docs/img/release-dashboard.png" width="860">
</p>

<p align="center">
  Unofficial and unaffiliated. Use your own credentials and follow the applicable Codex, OpenAI, DeepSeek, Vision, and search-provider terms.
</p>

CodeSeeX connects Codex Desktop to DeepSeek-compatible upstreams through a local `/v1` adapter. Its role is not just to translate one HTTP API into another. CodeSeeX sits at the agent boundary where Codex requests, tool calls, context replay, reasoning behavior, web search, local file operations, usage accounting, and desktop management all meet.

The project targets a specific gap in the current AI tooling market:

- Generic API gateways are good at routing models, keys, and protocols.
- Simple proxy scripts are good at making one model answer through another endpoint.
- CodeSeeX is designed for Codex-style agent sessions, where tool lifecycle, context hygiene, request classification, and cost visibility decide whether the agent is actually usable.

Current version: `0.5.4`

```text
Codex Desktop  ->  CodeSeeX local agent runtime  ->  DeepSeek-compatible upstream
                         |
                         +-> Codex tools, web search, usage, diagnostics, desktop manager
```

## Why CodeSeeX Exists

Running a non-native upstream behind Codex is easy to make work once. It is much harder to make it behave like a stable Codex agent over long sessions, tool loops, large repositories, background service requests, and repeated file edits.

CodeSeeX focuses on that hard part:

- Preserve Codex-native semantics instead of treating every request as a normal chat message.
- Keep tool execution observable, bounded, and replayable.
- Prevent tool results, visible thinking, service requests, and full-context payloads from polluting later turns.
- Show usage as user tasks and agent phases, not just a flat list of upstream API calls.
- Provide a local desktop control plane for logs, settings, model catalog, balance checks, tools, and runtime state.

## Not A Plain Relay

Direct relay tools usually forward request bodies, map model names, and pass responses back. That is useful, but it leaves important Codex agent behavior unresolved.

CodeSeeX adds a runtime layer around the relay:

| Area | Direct API relay | CodeSeeX |
|---|---|---|
| Request semantics | Mostly forwards chat/responses payloads | Classifies Codex user turns, service requests, compact/replay state, and client tool handoffs |
| Tools | Often passes tool schemas through | Owns CodeSeeX tools, preserves Codex-native client tools, bridges deferred tool discovery, and records tool lifecycle |
| Context | Sends whatever the client sends | Compiles context, keeps verified tool facts, avoids duplicating full Codex transcripts, and bounds replay data |
| DeepSeek behavior | Treats output as model text | Adapts DeepSeek-specific thinking/tool protocol behavior at the provider boundary |
| Usage | Flat upstream request log | Groups billable requests into user sessions, service requests, handoff phases, and tool-loop segments |
| Desktop UX | Usually external config files | Tauri manager with status, logs, usage, settings, adapter TOML, update checks, balance, and tray controls |
| Safety | Depends on upstream/client | Adds local/private target protection, bounded tool output, diagnostic redaction, and explicit community-tool trust boundaries |

The result is a tool for people who want DeepSeek inside Codex without giving up the operational behavior that makes Codex useful as an agent.

## What You Get

- DeepSeek V4 models exposed to Codex as `deepseek-v4-pro` and `deepseek-v4-flash`.
- Generated Codex TOML with machine-specific `model_catalog_json` and local `base_url`.
- Embedded model catalog for first-run machines without a native Codex catalog.
- 1M context metadata with a 95% effective context window for Flash and Pro.
- Codex-native Apply Patch handling and client-tool handoff behavior.
- CodeSeeX-hosted Web Search with bounded execution, source diagnostics, automatic evidence opening, and local/private target protection.
- Read-only workspace tools for file and repository inspection.
- Optional Vision module for OpenAI-compatible image understanding and image generation endpoints.
- Context compilation with verified tool facts, compact summaries, binary/data URL redaction, and bounded tool-result replay.
- Usage sessions that separate normal user turns, service requests, model iterations, tool phases, handoffs, cache hits, cache misses, output tokens, and estimated cost.
- Desktop manager with tray controls, autostart, update checks, logs, usage, balance, settings, tools, and adapter setup.
- Community tool discovery under `~/.codeseex/extension/tools/<tool>/manifest.json`, disabled by default and executed only through explicit command manifests.

## Screenshots

The gallery below uses English UI sample data and real CodeSeeX/Codex screens.

### Observability

<table>
  <tr>
    <td width="50%">
      <strong>Usage Sessions</strong><br>
      Conversation-level cost, latency, cache hit rate, service requests, and expandable stages.<br><br>
      <img alt="CodeSeeX usage sessions with cache hit details" src="docs/img/release-usage.png" width="100%">
    </td>
    <td width="50%">
      <strong>Safe Diagnostic Logs</strong><br>
      Request, tool, context, protocol, and network events without exposing prompt payloads by default.<br><br>
      <img alt="CodeSeeX safe diagnostic logs timeline" src="docs/img/release-logs.png" width="100%">
    </td>
  </tr>
</table>

### Agent Setup

<table>
  <tr>
    <td width="50%">
      <strong>Tool Settings</strong><br>
      Built-in workspace tools, Web Search, Vision endpoints, and tool-specific credentials.<br><br>
      <img alt="CodeSeeX tool settings for hosted tools and Vision" src="docs/img/release-settings-tools.png" width="100%">
    </td>
    <td width="50%">
      <strong>Generated Codex TOML</strong><br>
      Machine-specific `model_catalog_json`, local `/v1` endpoint, and DeepSeek model settings.<br><br>
      <img alt="CodeSeeX generated Codex TOML configuration" src="docs/img/release-dashboard-toml.png" width="100%">
    </td>
  </tr>
</table>

### Codex Experience

<table>
  <tr>
    <td width="50%">
      <strong>Codex Session</strong><br>
      DeepSeek-powered Codex session with thinking, tool-capable workflow, and local CodeSeeX routing.<br><br>
      <img alt="Codex session using CodeSeeX and DeepSeek" src="docs/img/release-codex.png" width="100%">
    </td>
    <td width="50%">
      <strong>Vision Example</strong><br>
      Optional Vision module used from Codex through the CodeSeeX tool runtime.<br><br>
      <img alt="CodeSeeX Vision example in Codex" src="docs/img/release-codex-vision.png" width="100%">
    </td>
  </tr>
</table>

## Quick Start

1. Download the latest build for your platform from [GitHub Releases](https://github.com/TasteSteak/CodeSeeX/releases).
2. Start CodeSeeX.
3. Open `Settings -> Proxy` and confirm the local service is running on the default port `8787`.
4. Copy the generated Codex TOML from the CodeSeeX adapter card.
5. Put that TOML into the Codex configuration you use for DeepSeek.
6. Restart Codex after changing TOML.
7. Select `deepseek-v4-pro` or `deepseek-v4-flash` in Codex.

Prefer the generated TOML because the catalog path and local port are machine-specific.

```toml
model_provider = "custom"
model = "deepseek-v4-pro"
disable_response_storage = true
model_reasoning_effort = "xhigh"
# CodeSeeX adds a machine-specific model_catalog_json path in the generated TOML.

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

## Desktop Manager

The desktop app is the control plane for the local runtime:

- Dashboard: proxy status, current port, balance, update status, and troubleshooting hints.
- Usage: user-task-level records with model phases, tool phases, cache hit/miss, output, latency, and cost.
- Logs: compact operational events and safe diagnostics.
- Settings: upstream URL, model behavior, proxy mode, UI options, billing rates, and tools.
- Adapter: generated Codex TOML and model catalog status.
- Tools: built-in tool enablement, Web Search, Vision settings, and community tool discovery.

The proxy is still the core service. Once running, the desktop UI should not be required for Codex requests to continue flowing.

## Tools And Agent Runtime

CodeSeeX treats tools as part of the agent runtime, not as incidental function calls.

- Codex client tools such as native patch application are handed back to Codex in the shape Codex expects.
- CodeSeeX base tools can be exposed directly to the model when enabled.
- Deferred/native Codex tools can still be discovered through the tool-search bridge.
- Web Search is bounded, source-aware, and protected against localhost/private-network targets.
- Tool results are compacted before replay to reduce token pollution.
- Repeated failures and repeated tool signatures are tracked to prevent dead loops without blocking normal complex tool chains.

Community tools are local command executors. They are disabled by default. Enabling one means you trust the command declared by its manifest.

## Context And Usage

Codex owns the conversation transcript. CodeSeeX keeps only the bridge state needed to complete current requests and explain runtime behavior.

CodeSeeX is careful about:

- full-context Codex requests,
- compact summaries,
- service requests such as titles and ambient suggestions,
- client tool handoffs,
- tool result replay,
- visible thinking display,
- cache hit and miss accounting,
- per-user-turn usage grouping.

This matters because a direct relay can appear to work while silently resending large context, duplicating tool output, misclassifying background service calls, or making one small user task look like many unrelated billable API calls.

## Upstream And Models

CodeSeeX exposes `deepseek-v4-pro` and `deepseek-v4-flash` to Codex through its generated catalog. Leave the upstream URL blank to use the default DeepSeek-compatible upstream, or set a custom OpenAI-compatible upstream URL in `Settings -> Proxy`.

The local Codex endpoint remains under `http://127.0.0.1:8787/v1` by default. If you change the listen port, copy the generated TOML again and restart Codex.

## Vision Module

The Vision module is optional and configurable from the desktop Tools settings. Configure full request URLs, model names, and an API key for the endpoints you want to use:

- Analyze endpoints: OpenAI-compatible `/responses` or `/chat/completions`.
- Generate endpoints: OpenAI-compatible `/responses` with image generation support or `/images/generations`.
- Image inputs: current Codex `input_image` attachments, HTTP(S) URL, `data:image` URL, `file://` URL, workspace path, or permitted local absolute path.
- Image generation results are returned as display-ready Markdown and local files; generated base64 payloads are saved to disk instead of being sent back inline.

CodeSeeX does not rewrite Vision endpoint URLs. The request URL you configure is the request URL that will be used. When a local image is analyzed through a remote endpoint, the image pixels are sent to that configured service.

## Install And Update

On Windows, use the NSIS `Windows-CodeSeeX.Setup.*.exe` installer for normal desktop installs and updates. It supports installer language selection, current-user or all-users install mode, and migration from the earlier Electron build by uninstalling the legacy app before installing the Tauri build.

## Credential Boundary

CodeSeeX manager settings are not intended to be upstream credential storage. Balance checks read the direct Codex auth source or a cached request `Authorization: Bearer ...` header. A legacy `DEEPSEEK_API_KEY` environment value can still act as a fallback for direct upstream requests, but it is not the balance credential source.

Tool-specific credentials, such as Vision credentials, belong to the configured tool endpoint and should be treated as local secrets. Do not enable community tools unless you trust their command manifests.

## Privacy Notes

CodeSeeX is a local bridge, but model requests are forwarded to the configured upstream service. Vision analysis sends image pixels to the configured Vision endpoint. Web Search may request search-result pages or regular web pages from third-party websites. Those services may apply their own terms, retention policies, rate limits, and anti-abuse rules.

Default logs are compact and redacted. Development diagnostics can expose more request-shape information and should only be enabled when needed for debugging.

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

The store keeps current-process bridge state, bounded logs, explicit compact payload material, usage summaries, and diagnostics. It is not a replacement for Codex's own transcript storage.

## Troubleshooting

### Balance Query Fails

- Make sure Codex auth is configured for the same user account.
- Confirm the machine can reach the configured DeepSeek-compatible upstream.
- If a system proxy or VPN is required, enable the system proxy mode in CodeSeeX.

### Codex Cannot See DeepSeek Models

- Confirm `model_catalog_json` points to an existing `~/.codeseex/model-catalog.json`.
- Copy the generated TOML from CodeSeeX instead of typing the path manually.
- Restart Codex after changing TOML.
- GPT/OpenAI TOML files do not need `model_catalog_json` and are not affected by CodeSeeX.

### Conversation Requests Fail

- Check the CodeSeeX logs page for the upstream error.
- Confirm Codex `base_url` points to CodeSeeX, for example `http://127.0.0.1:8787/v1`.
- If you use a custom upstream, confirm the URL is reachable and OpenAI-compatible.
- Make sure no other process is using the configured CodeSeeX port.

### Tools Behave Unexpectedly

- Confirm the tool is enabled in CodeSeeX settings.
- For Codex-native tools, make sure Codex itself supports and exposes that tool in the current session.
- For community tools, inspect the manifest and command before enabling it.
- For Web Search, check source diagnostics and network/proxy settings.

## Development

Rust is required for the core workspace.

```sh
cargo run -p codeseex-proxy
cargo test --workspace
```

Source builds require a model catalog seed at build time. Set `CODESEEX_MODEL_CATALOG_SEED` to a local seed file, or place `model-catalog.seed.json` under `.private/`.

On Windows, helper scripts load MSVC Build Tools when available, import `.env`, and keep Cargo caches under a configurable local dev directory by default:

```powershell
.\scripts\check-windows.ps1
.\scripts\start-desktop-windows.ps1
```

The desktop UI is served from `apps/ui/public` through Tauri's custom protocol; there is no Vite dev server in the normal workflow.

## Documentation

- [CHANGELOG.md](CHANGELOG.md) tracks release notes; packaged builds are published on [GitHub Releases](https://github.com/TasteSteak/CodeSeeX/releases).
- [docs/architecture.md](docs/architecture.md) for the runtime architecture.
- [docs/installer-migration.md](docs/installer-migration.md) for installer and legacy migration behavior.
- [docs/state-contract.md](docs/state-contract.md) for runtime/log state boundaries.
- [docs/community-tools.md](docs/community-tools.md) for community tool manifests and execution rules.

## License

CodeSeeX is licensed under AGPL-3.0-only. See [LICENSE](LICENSE).
