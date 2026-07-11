# Changelog

## 0.6.0 - 2026-07-11

CodeSeeX 0.6.0 is a context-runtime correctness release. It makes Codex HTTP full replay authoritative, preserves valid tool protocol groups, keeps workspace inspection bounded, and adds an in-app release-notes view with a safe offline fallback.

### Highlights

- Added a Canonical Session Core that aligns active HTTP replay in memory using anonymous fingerprints without reading Codex transcripts or creating a durable conversation store.
- Codex full replay is now forwarded as the authoritative context instead of being rewritten into a local tail-only continuation.
- Tool-call batches and their matching results are handled as atomic protocol groups, including mixed internal and client-tool batches.
- Workspace inspection tools now return bounded, paginated output for large files and directories while keeping broad repository search available.
- Added an in-app Release notes view below the official website action in About.

### Added

- Added structured, version-pinned release notes in English and Simplified Chinese, with all other release-note locales falling back to English.
- Added a local release-notes API with GitHub tag lookup, ETag revalidation, a short network timeout, in-memory retry backoff, and bundled offline content.
- Added a local fake upstream smoke example for zero-cost checks of replay shape, cache-prefix continuity, tool pairing, and redacted outbound traces.
- Added paginated `list_directory`, bounded `read_file_range` continuation for long lines, and source-first `workspace_search` coverage with explicit truncation diagnostics.

### Changed

- Removed the proxy-specific 96k full-replay budget. Context is now limited only by the configured upstream model window and required output/tool reserves.
- Full replay divergence or Codex compaction now rebuilds the active in-memory alignment from the new Codex input; it never silently retains only a local tail.
- Tool outputs, binary/data URLs, credentials, search snippets, and diagnostics are bounded and redacted before they can expand replay cost.
- The Apply Patch compatibility path only repairs the narrowly identified malformed blank context-line shape; already valid and ambiguous patches are left unchanged.
- Release assets and website cache versions now follow the desktop version consistently.

### Fixed

- Fixed cache-hit resets, oscillating context size, and semantic context loss caused by proxy-side tail continuation or replay truncation.
- Fixed malformed mixed tool histories being sent upstream as incomplete assistant tool-call groups.
- Fixed repeated long file reads, directory listings, and broad searches being able to produce oversized tool results.
- Fixed manual fake-upstream traces defaulting to repository-root files; they now default to the ignored `.private` directory.

### Compatibility Notes

- CodeSeeX still does not read, modify, or restore Codex jsonl transcripts. Canonical alignment exists only in RAM and expires with the proxy process or session TTL.
- If the authoritative Codex replay cannot fit the real upstream context window, CodeSeeX returns a controlled context-limit diagnostic instead of silently summarizing or dropping history.
- Release notes use the matching GitHub tag when available. If GitHub is unavailable or the tag has not been published yet, the bundled release notes remain available offline.

## 0.5.4 - 2026-07-08

CodeSeeX 0.5.4 is a focused agent-stability hotfix. It keeps client tool failures visible to the model, prevents stale handoff guards from blocking follow-up turns, and improves upstream decode diagnostics.

### Highlights

- Repeated client tool failures no longer become terminal proxy errors by default.
- Follow-up requests such as "continue" no longer inherit a stale client tool handoff stop.
- Upstream response body decode failures now include safer diagnostics for network/proxy/upstream troubleshooting.

### Changed

- Client tool failure tracking now uses the tool name, arguments hash, and compact failure-summary hash instead of only the tool name.
- Repeated failure protection is now diagnostic-only, so the agent can inspect the failed tool result and choose a different recovery path.
- Handoff preflight no longer short-circuits later requests based on a previous repeated-failure state.

### Fixed

- Fixed legitimate troubleshooting workflows being interrupted after several failing `shell_command` handoffs.
- Fixed repeated apply-patch or shell failures being able to poison later continuation requests.
- Fixed sparse `error decoding response body` logs by adding status, safe upstream response headers, and reqwest error-kind flags without logging raw upstream bodies.

### Compatibility Notes

- This release does not change model catalog behavior, pricing estimates, updater signing, or Web Search policy.
- Existing diagnostics that look for client handoff guard stops should now treat repeated-failure records as warnings unless a separate terminal error is present.

## 0.5.3 - 2026-07-06

CodeSeeX 0.5.3 is a release-readiness and desktop updater stability update. It focuses on safer Codex App integration, clearer catalog diagnostics, and a more reliable in-app update path.

### Highlights

- Added an in-app update flow with update checking, download progress, cancellation, and passive installation on supported desktop builds.
- Improved Codex App model-switch continuity by protecting full-context replay and prompt-cache session anchoring.
- Added Codex runtime catalog diagnostics so users can see whether Codex is actually reading the CodeSeeX model catalog.
- Hardened release packaging for signed updater manifests across Windows, macOS, and Linux targets.

### Added

- Added desktop updater commands and progress events for the update dialog.
- Added an experimental Codex App model-list injection setting. It is enabled by default, persisted in user config, and can be turned off if Codex App compatibility changes.
- Added troubleshooting diagnostics for catalog path mismatches, missing catalog models, and startup-only catalog behavior.

### Changed

- Codex App launch only attempts renderer model-list injection when the experimental setting is enabled.
- Full-context replay now favors the client replay payload for Codex App model switches instead of trimming away short user history.
- Update notice dots are dismissed for the current app run only, so update checks remain visible without permanently hiding future notices.
- Release manifests now include both installer-specific and base updater targets where applicable.

### Fixed

- Fixed a cache/context continuity risk when Codex App switches models without sending `previous_response_id`.
- Fixed update installation UX so downloads run in the background with visible progress instead of sending users directly to a release page.
- Fixed catalog troubleshooting UI stability so validation does not collapse the expanded panel.
- Fixed release workflow gaps that could omit updater-compatible platform entries.

### Compatibility Notes

- CodeSeeX still recommends copying TOML directly from the desktop app when catalog accuracy matters. Some CCS import flows may not preserve the Codex model catalog.
- The in-app updater requires signed updater artifacts from the GitHub release manifest.
- Codex App model-list injection remains experimental and can be disabled without affecting normal CodeSeeX proxy operation.

## 0.5.2 - 2026-07-02

CodeSeeX 0.5.2 is a small stability and billing-display update. It improves long-running agent tasks, adapts cost estimates for DeepSeek peak/off-peak pricing, and polishes several desktop settings interactions.

### Highlights

- Improved long-running agent stability by preventing repeated client tool handoffs from prematurely interrupting active tasks.
- Added DeepSeek peak/off-peak billing estimates, enabled by default in settings.
- Improved Vision tool configuration layout with wider endpoint/API key fields and compact model fields.
- Scoped right-click "Select all" to the current page or active input.

### Added

- Added a `BILLING_PEAK_VALLEY_ENABLED` setting for peak/off-peak cost estimates.
- Added usage billing buckets split by model and billing period for more accurate cost display.
- Added optional tool config field width metadata for built-in and community tools.

### Changed

- Usage cost estimates now apply Beijing-time peak pricing for 09:00-12:00 and 14:00-18:00 when peak/off-peak billing is enabled.
- The billing setting UI now includes the peak/off-peak toggle with the same divider and switch styling as other settings.
- Tool config inputs now use shared width rules for URL, endpoint, API key, token, secret, and model fields.
- Update notice dots are now dismissed only for the current app run instead of being permanently hidden for the version.

### Fixed

- Fixed long-running tasks being interrupted when the same client tool handoff signature appeared repeatedly.
- Fixed the peak/off-peak billing switch not rendering as a visible toggle.
- Fixed missing divider spacing between peak/off-peak billing and billing rate settings.
- Fixed password-style tool config inputs being visually shortened by nested width constraints.
- Fixed right-click "Select all" selecting hidden pages or the whole workspace.

### Compatibility Notes

- Peak/off-peak billing only affects CodeSeeX cost estimates. It does not change upstream billing behavior.
- Existing billing rate values are preserved. New installs and unset configs enable peak/off-peak estimates by default.
- Community tools remain compatible; width metadata is optional.

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
