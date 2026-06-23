# Changelog

## 0.5.1 - 2026-06-23

CodeSeeX 0.5.1 is a stability and experience update for the 0.5 Rust/Tauri line. It focuses on UI polish, optional Codex App model switching, improved Web Search behavior, and refreshed release documentation.

### Highlights

- Refined the desktop UI to improve daily usage, including Usage, Logs, settings, screenshots, and release-facing pages.
- Added optional Codex App model switching for DeepSeek V4 Flash / Pro. Switching models from CodeSeeX remains the recommended path for the most consistent runtime behavior.
- Improved Web Search source probing, evidence opening, fallback behavior, and diagnostics.
- Refreshed README screenshots, website entry points, update prompts, and release documentation.

### Changed

- Improved Usage and Logs layout, scrolling, event presentation, and detail loading without changing billing semantics.
- Updated Codex App integration so Flash / Pro can appear in the Codex App model menu, while keeping CodeSeeX-side model switching as the preferred workflow.
- Improved Web Search network health ordering, evidence collection, and diagnostics while keeping local/private target protections.
- Improved generated setup and release documentation around Codex configuration, screenshots, and official website links.

### Fixed

- Fixed Usage page scrolling, active session refresh, service request labels, and transient intermediate records.
- Fixed Logs entries that were too flat or noisy to explain request, tool, cache, and network behavior.
- Fixed desktop update links so they open in the system browser from the WebView.
- Improved stability around Codex App model switching, while still recommending CodeSeeX as the primary model-switching surface.

### Packaging Notes

- Users upgrading from 0.5.0 should fully restart both CodeSeeX and Codex App after installation if they use Codex App integration.
- Generated Codex TOML and catalog paths remain machine-specific, so copying configuration from the desktop manager is still the recommended setup path.
