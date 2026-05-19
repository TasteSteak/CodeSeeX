# Changelog

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
