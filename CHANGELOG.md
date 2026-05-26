# Changelog

## 0.4.0 - 2026-05-26

This release focuses on high-fidelity agent context, safer request state persistence, first-run catalog reliability, upstream compatibility, and improved client configuration.

### Added

- Added a high-fidelity context compiler that preserves long response chains, verified tool facts, compacted context, and legal Chat tool protocol history.
- Added request lifecycle checkpointing in `proxy-state.json` so in-progress, completed, failed, and interrupted turns keep their verified inputs and tool facts.
- Added deterministic tool fact ledgers and compact diagnostics to reduce context loss after long conversations, tool loops, and manual or automatic compaction.
- Added a full packaged catalog seed for users who do not have a native Codex catalog available on first run.
- Added a reproducible packaged catalog seed so catalog generation no longer depends on short-lived Codex cache files.
- Added custom DeepSeek official path compatibility control in `Settings -> Experimental`.
- Added sampling temperature presets in client settings and tray shortcuts.
- Added Flash and Pro billing rate categories with separate cached-input, cache-miss-input, and output rates.
- Added structured handling for Codex typed image/tool content so screenshot payloads keep stable metadata without injecting base64 into model context.
- Added proxy-hosted community tool execution support through an explicit `executeProxyTool()` hook.
- Added direct fidelity and agent-state test coverage for compaction, context retention, interrupted turns, catalog seed generation, desktop port isolation, and native tool flows.

### Changed

- Default context handling now uses the model catalog context budget instead of a fixed short `60 messages / 120 KB` history window.
- Assistant self-descriptions are treated as lower-priority evidence than user messages, tool calls, tool results, file facts, MCP facts, and compacted verified facts.
- Interrupted or failed responses no longer contribute incomplete assistant final text to future context.
- Official DeepSeek requests use `/v1/chat/completions` compatibility routing by default, while custom upstream URLs remain unaffected.
- The generated catalog now carries more complete Codex model metadata for stable model-list rendering.
- Runtime state reading is stricter: damaged `proxy-state.json` is reported as a diagnostic error instead of being silently replaced with an empty state.
- Tool result conversion now has one model-visible path for native tool output, hosted proxy output, typed content arrays, JSON objects, and binary payload summaries.
- The proxy settings layout was reorganized around connection, model behavior, usage display, Codex adapter, and experimental configuration.
- Billing settings were redesigned into separate Flash and Pro cards while keeping existing persisted configuration keys.
- GitHub Actions release assets are narrowed to the main installable package for each platform and renamed with `Windows-`, `MacOS-`, or `Linux-` prefixes.

### Fixed

- Fixed high-risk context truncation that could drop real tool calls or tool results and let assistant self-descriptions distort later turns.
- Fixed compacted conversations losing important tool facts during long agent workflows.
- Fixed stream interruption and upstream failure paths that could otherwise lose verified current-turn state.
- Fixed first-run catalog issues on machines where Codex does not expose a native bundled catalog.
- Fixed localized Codex App model-list rendering by hardening DeepSeek display metadata and plan visibility in generated model metadata.
- Fixed startup conflicts where the desktop UI could fail when the proxy listen port was already occupied.
- Fixed official DeepSeek endpoint compatibility regressions affecting users whose network or deployment expected `/v1/chat/completions`.
- Fixed screenshot and image tool results being stringified into large `data:image/...;base64` text blocks, which could destroy prompt-cache continuity and inflate request cost.
- Fixed unknown proxy-hosted tools falling through to Web Search execution; unsupported hosted tools now return an explicit protocol error.
- Fixed Codex checkpoint compaction logging so `/compact`-style requests appear as context compaction start/completion events instead of ordinary conversation requests.
- Fixed client settings ordering, sampling temperature styling, and related tray localization gaps.

## 0.3.3 - 2026-05-24

This release focuses on native Apply Patch fidelity, MCP edge-case compatibility, and release documentation polish.

### Added

- Added native Apply Patch bridge tests for freeform patch mapping, history replay, failure output guidance, and complex patch payloads.
- Added MCP passthrough test coverage for nested MCP tool declarations shaped as `{ tool: ... }`.
- Added README usage imagery.

### Changed

- Changed `apply_patch` handling to return Codex native `custom_tool_call` items instead of shell-style wrappers.
- Removed the old apply-patch proxy wrapper path and related lifecycle noise.
- Removed the experimental proxy-tool-to-`mcp_call` UI mapping after testing showed Codex Desktop does not persist or render those response items as native MCP calls.
- Kept ordinary CodeSeeX hosted tools on the stable proxy tool path while preserving native MCP passthrough for Codex-configured MCP servers.

### Fixed

- Fixed nested MCP tool declarations shaped as `{ tool: ... }` being skipped.
- Fixed Apply Patch schema and prompt mismatch issues that could cause malformed patch calls or shell fallback behavior.
- Added model-facing guidance after patch failures so retries re-read exact file content instead of relying on stale context.

### Notes

- `list_mcp_resources` returning an empty array does not necessarily mean MCP tools are unavailable. Some MCP servers expose tools without resources or resource templates; verify by calling an actual MCP tool and checking for Codex `mcp_tool_call_end` events.

## 0.3.2 - 2026-05-23

This release focuses on self-hosted upstream configuration, README imagery, and native MCP passthrough so Codex MCP tools stay on the Codex app tool layer.

### Added

- Added an `MCP Server` system built-in tool card in the desktop Tools page.
- Added native MCP passthrough for Codex-provided MCP tool declarations so Codex can execute and display MCP calls itself.
- Added MCP passthrough tests covering namespace tool mapping, `type=mcp` declarations, collision handling, history replay, and non-hosted execution.
- Added a custom DeepSeek upstream URL field in `Settings -> Proxy` for self-hosted OpenAI-compatible endpoints.
- Added README dashboard imagery and updated setup notes for custom upstream usage.

### Changed

- Changed MCP handling from CodeSeeX proxy-hosted execution to native passthrough.
- DeepSeek upstream configuration now leaves the UI field blank for the official default and uses `https://api.deepseek.com/` at runtime.
- Legacy official upstream values such as `https://api.deepseek.com/v1` are treated as the default and no longer appear as custom values in the UI.
- Updated `proxy.env.example` so the upstream URL is blank by default.

### Fixed

- Fixed MCP calls appearing as CodeSeeX proxy custom tools instead of Codex native MCP tool usage.
- Fixed MCP tools not being exposed to DeepSeek when CodeSeeX was used between Codex and DeepSeek.
- Fixed MCP tool name mapping and history replay so child tool names are restored without polluting model-visible history.
- Fixed MCP calls being treated as CodeSeeX proxy-hosted tools instead of returning to the Codex native MCP layer.

## 0.3.0 - 2026-05-21

This release focuses on release readiness and client experience: configuration flow, startup reliability, tool management, update indicators, and cross-platform runtime data handling.

### Added

- Added a dedicated proxy settings page for listen port, billing rates, catalog mode, upstream model override, and generated `config.toml`.
- Added a read-only `config.toml` card with copy support for Codex `base_url` and `model_catalog_json` setup.
- Added catalog modes: default, auto, and builtin.
- Added upstream model override options: default, Flash, and Pro.
- Added login auto-start configuration in the client settings page.
- Added tray shortcuts for model override and thinking mode.
- Added silent update checks with red-dot indicators.
- Added `start-desktop.cmd` for quick local desktop testing.
- Added the community tool directory convention: `~/.codeseex/extension/tools/<tool>/`.

### Changed

- Switched default runtime data storage to the user's `~/.codeseex` directory to avoid install-directory permission issues.
- Added Electron single-instance locking to reduce duplicate startup, port conflicts, and intermittent EACCES errors.
- Improved startup flow so the client appears sooner and less work blocks first paint.
- Improved dark-mode contrast for cards, switches, usage details, and settings panels.
- Refined the tools page so system tools are fixed while other tools use individual enable switches.
- Simplified tool toggle storage to `ENABLED_TOOLS=[...]`.
- Grouped consecutive proxy tool display messages into a single "used N tools" UI-only summary.
- Changed update checks to report status inside the About page instead of opening the releases page directly.
- Added update result states for latest version, available update, and temporary access failure.
- Added per-version read state for update red dots.
- Improved system-language detection and completed language key coverage.
- Updated README, tool authoring docs, and release notes for 0.3.0.

### Fixed

- Fixed tray setting changes not syncing back to the main window.
- Fixed first-run language display issues for some users.
- Fixed a macOS title-bar right-click crash path.
- Fixed catalog generation for users without an existing native Codex catalog.
- Fixed several configuration save and status refresh edge cases.
- Fixed built-in/community tool labels and toggle behavior.
- Fixed autosave writing an empty `ENABLED_TOOLS` list before the tools page was loaded.
- Tightened About page update-link rendering so only the controlled version link uses HTML.

### Notes

- Changing the listen port still requires restarting CodeSeeX and updating the Codex TOML `base_url`.
- CodeSeeX does not store DeepSeek API keys in `proxy.env`; balance checks and chat requests use the user's Codex auth configuration.

## 0.2.1 - 2026-05-19

### Changed

- Improved cross-platform packaged runtime directory handling: Windows keeps portable folders next to the executable, while macOS and Linux use the app user-data directory.
- Added public `dist:mac` and `dist:linux` build scripts and basic Electron Builder targets.
- Updated documentation to describe current platform support and runtime folder behavior more accurately.

### Fixed

- Added macOS/Linux stale process and listen-port cleanup so old CodeSeeX processes are not left blocking startup.
- Fixed proxy shell command normalization so non-Windows platforms use `sh -lc` instead of Windows PowerShell.
- Removed the Windows-specific browser user-agent fingerprint from built-in web search requests.

## 0.2.0 - 2026-05-19

### Added

- Added a packaged compressed catalog seed so new users without a native Codex catalog can still generate `~/.codeseex/model-catalog.json`.
- Added `deepseek-v4-flash` and `deepseek-v4-pro` catalog generation with `1M` context metadata and `90%` effective context window.
- Added `build:catalog-seed` for private release packaging.
- Added safer built-in workspace tool path handling for workspace, full-access, and explicit root modes.

### Changed

- Unified the app around one configurable local listen port shared by the desktop manager and Codex-compatible `/v1` API.
- Improved user-level logs, log retention, and client-side log rendering performance.
- Improved streaming order and UI-only reasoning display handling for DeepSeek responses.
- Updated API key handling so balance checks and model requests use the Codex auth source consistently.

### Fixed

- Fixed release-blocking startup failures when `model_catalog_json` pointed to a missing catalog file.
- Fixed packaged dependency issues for `iconv-lite` and `undici`.
- Fixed model alias handling for Codex auxiliary requests such as title generation.
- Fixed several UI theme, tray, language loading, and proxy settings display issues.

## 0.1.0 - 2026-05-17

### Added

- Initial desktop manager and local Codex-compatible DeepSeek proxy.
- Initial tool bridge, usage view, logs view, balance card, and adapter TOML helper.
