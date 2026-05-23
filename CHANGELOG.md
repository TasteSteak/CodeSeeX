# Changelog

## 0.3.1 - 2026-05-23

This release focuses on MCP compatibility and upstream configuration for users running official or self-hosted DeepSeek-compatible services.

### Added

- Added native MCP bridge discovery for Codex-configured stdio, streamable HTTP, and legacy SSE MCP servers.
- Added MCP smoke tests covering tools, resources, resource templates, prompts, HTTP transport, and legacy SSE transport.
- Added a custom DeepSeek upstream URL field in `Settings -> Proxy` for self-hosted OpenAI-compatible endpoints.
- Added README dashboard imagery and updated setup notes for custom upstream usage.

### Changed

- DeepSeek upstream configuration now leaves the UI field blank for the official default and uses `https://api.deepseek.com/` at runtime.
- Legacy official upstream values such as `https://api.deepseek.com/v1` are treated as the default and no longer appear as custom values in the UI.
- Updated `proxy.env.example` so the upstream URL is blank by default.

### Fixed

- Fixed MCP tools not being discovered or exposed when CodeSeeX was used between Codex and DeepSeek.
- Fixed MCP tool name mapping and history replay so child tool names are restored without polluting model-visible history.
- Fixed hosted MCP tool execution for helper calls and server tool calls across streaming and non-streaming turns.

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
