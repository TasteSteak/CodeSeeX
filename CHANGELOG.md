# Changelog

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
