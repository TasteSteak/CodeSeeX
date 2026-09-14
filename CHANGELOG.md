# Changelog

## 0.8.1 - 2026-09-14

This release rebuilds CodeSeeX local Web Search from the request down to the evidence: the pipeline becomes one contract shared by the model view and the client view, and the problems that made search invisible, unusable on CJK queries, wasteful with page text, or able to break a continuation are fixed.

### Local search

- The pipeline is layered as request → sources → rank → fetch → extract → outcome, and the model view and the client view read the same result contract instead of each defining its own near-copies of the same fields.
- A search CodeSeeX runs itself now reaches the client as a standard `web_search_call` item, so the user sees the query and the step instead of only a final answer.
- The presented item carries the field shape the upstream accepts on replay and is withheld from the replayed payload, so a continuation can no longer be rejected for it.
- A mixed tool group no longer fails the whole turn: CodeSeeX executes its own calls, hands the client-owned calls back, and replays the round as one group.
- Tooling: `POST /api/web-search/probe` runs one search or page open locally for diagnostics, with no model in the loop.

### Search quality

- Requests carry the query and nothing else — no market, language, or locale parameter — and the source set stays region-neutral.
- Sources are fused with reciprocal-rank fusion instead of keeping whichever response arrived first, so the ordering no longer depends on a race and a URL found by several sources ranks higher.
- Scripts without spaces are matched by bigram, so a query like a Chinese phrase matches documents that contain its parts instead of only an exact whole-string hit.
- Candidates pass a relevance gate: a result page whose entries are unrelated to the query is dropped rather than handed to the model.
- Source health distinguishes unreachable from reachable-but-empty, cools a source down only after repeated failures, paces requests per source, caches a query's candidates for ten minutes, and probes every source with a query it must actually parse.

### Extraction and evidence

- Pages are parsed as a real DOM into scored blocks, with content ratio, link ratio, and a confidence verdict instead of a flat string of text.
- Code keeps its line breaks and indentation, a table row reads as one row of columns, and a block's link targets are resolved and reported.
- A long page is sampled by budget — opening blocks, highest scoring blocks, and query matches — with the omitted parts accounted for, instead of truncating from the top.
- Blocks repeated across a page are kept once, so a template does not spend the budget on the same sentence.
- Resource payloads never reach the model: whole `data:` URLs and opaque runs over a size limit are replaced with a bounded marker, and images and media are kept as references only. The rule is size and markup based, never per site or per language.
- The evidence budget is the model-facing budget, so what the pipeline selects is what the model receives.

### Session and logs

- A continuation is no longer rejected because a replayed search item was missing a field the provider requires.
- Client-owned tool calls are never executed by the proxy, and the client's own tool output is unaffected by the search-item handling.
- Page fetching keeps address pinning, per-hop redirect validation, and the private-network block, with additional reserved ranges covered.
- The web search log line is a user-level summary, and the context cost hint is written only when a size threshold is actually crossed instead of on every turn.

## 0.8.0 - 2026-09-13

This release reworks a large amount of underlying behaviour: the upstream address, model list, pricing and tool ownership collapse into a single source of truth, and the transport, logging and feedback are re-tuned for a resident client. The configuration, model-picking, usage-viewing and troubleshooting experience is improved, and the issues left over from earlier versions — the silently falling-back upstream, broken usage chains, rejected native tool declarations, apply_patch compatibility and silent unpriced billing — are fixed.

### Models and pricing

- The model list, aliases, capabilities and prices are one versioned catalog document resolved in layers (user overrides → remote manifest → cache → built-in), so models and prices update without shipping a client.
- The remote manifest is rechecked at startup, every six hours and on a manual refresh; any failure keeps the previous layer and never blocks the proxy.
- The published manifest moved to `catalog/model-catalog.json` and now carries the real DeepSeek models and official CNY prices.
- Catalog `kind` (`chat` | `billing_only`) and a semantic `role` (`chat` | `vision`): a billing-only entry is still priced and still resolvable for a replay, but never offered for chat, pinning or the tray.
- Pricing resolves exact slug → rate group → explicit unpriced; peak/valley windows and multiplier have a single implementation shared by the store and the UI.

### Upstream and transport

- The upstream address lives in Codex's `config.toml` as `[codeseex] upstream_base_url`; the settings field edits that file directly, and the key is only written when the user actually set one.
- Native Responses is the default and only implicit transport; the `auto` mode is gone.
- The upstream credential source is explicit (`auto` / `request` / `env` / `codex_auth` / `secret`), client identity headers are passed through unchanged, and relays stop rejecting requests for a missing Codex identity.
- Management endpoints for the catalog layer: `GET /api/catalog`, `POST /api/catalog/refresh`, `GET /api/upstream/probe`, `POST /api/upstream/test`, `POST /api/upstream/credential`.

### Proxy runtime

- apply_patch input is normalized on both transports: a legacy unified range header becomes a bare `@@`, a whitespace-only context line is repaired, and the repair is reported as an `apply_patch_input_micro_repair_diagnostic` event.
- Native Responses continuations no longer fail on grouped tool declarations, on a repeated namespace after a Codex App restart, or on a tool schema built only from a top-level union; the repairs are recorded in the event log.
- The log page defaults to lifecycle and failures only; a `User` / `Debug` control brings the per-round bookkeeping back, and the events endpoint defaults to the user-facing set.
- The chat-compatibility stream's usage scan is bounded like the native relay, so an upstream that never delimits a frame can no longer grow the buffer without limit.

### Tools

- Tool ownership has one authoritative source: the built-in tool list, alias normalization and the "who executes this" decision are defined once instead of being copied per transport.
- An upstream-native `web_search` declaration is executed inside the native transport instead of switching to Chat compatibility, so tool ownership never changes silently.
- Workspace scope and the full-access flag are only trusted from Codex-injected context, never from tool output.

### Usage and logs

- One user turn is one usage session: native tool handoffs are marked so intermediate replies stay inside their turn, and a failed round stays a failed row inside its turn.
- Usage rows come from every retained request — only the outgoing history is trimmed — so long turns keep their earlier rounds; groups are built in one pass and the session revision is a small hash.
- Dashboard throughput cards: RPM and TPM over a rolling 60-second window, plus average latency.

### Client UI

- Settings were regrouped into Client (personalization, other), Proxy (connection, model behaviour) and Tools, and the web-search backend moved into the web-search tool card.
- A toast manager for transient feedback: top-centre, whole row clickable to dismiss, at most three visible with the rest queued, one row per key with a `×N` counter, and an overflow summary once the queue is full.
- Model pinning from the model card, and a model list that caps at three rows with scrolling.
- Thinking-chain mirror modes: `none`, `smart`, `fixed`, `full`, configurable without changing what the upstream model receives.

### Codex App integration

- Launch Codex with or without injection: the no-injection mode never touches the renderer, and an installed injection can be removed at any time without restarting Codex.
- A tray model menu that mirrors the active catalog instead of the built-in slugs.

### Migration and compatibility

- The CodeSeeX-side `[upstream] base_url` is gone. On first load, a value found there is written into Codex's `config.toml` as `[codeseex] upstream_base_url`; after that the Codex file is authoritative. `DEEPSEEK_BASE_URL` still overrides both.
- `transport = "auto"` in an existing file still loads (as native Responses) but is never written back; an unset transport is the default.
- The removed `[experimental] reasoning_summary` boolean is ignored rather than translated; the mode key is authoritative and the default is `smart`.
- `[catalog] mode` is ignored, and `[model] override = "flash" | "pro"` and the `BILLING_*` keys remain readable.

## 0.7.1 - 2026-09-10

CodeSeeX 0.7.1 turns the model list and pricing into versioned, remotely updatable data with a complete offline fallback, and restores compatibility with upstreams that require a Codex client identity.

### Highlights

- The model catalog is now a versioned data document resolved in layers — user overrides, remote manifest, local cache, and the built-in document — so models, aliases, windows, and prices can change without shipping a new client.
- The remote manifest is re-checked on startup, every six hours, and on demand from the settings page; any failure keeps the previous layer and never blocks the proxy.
- Upstream `GET /v1/models` is used only as an availability probe. It never contributes windows, capabilities, or prices, and it never removes a model.
- Pricing is keyed by exact model slug with an explicit group fallback and an explicit unpriced state; unknown models are never silently billed at another model's price.
- Peak/off-peak billing has a single implementation with `[from, to)` window boundaries shared by the store and the UI.
- Some upstream addresses require the client to present a Codex identity. The previously dropped `originator`, `user-agent`, `session_id`, and `conversation_id` are now passed through natively, and the client Authorization is forwarded as-is on non-official endpoints.

### Added

- Added `crates/core/src/pricing.rs`: a data-driven pricing table with currency, unit, per-model rates, explicit rate groups, and peak/valley windows and multipliers.
- Added catalog documents in `crates/core/src/catalog.rs`: schema, size, duplicate-slug, and minimum-app-version validation, field-level merging, atomic cache writes, and user overrides.
- Added the built-in catalog `crates/core/assets/catalog.default.json` and the published manifest `docs/catalog/model-catalog.json`.
- Added remote catalog refresh with ETag revalidation, a fetch throttle, a short timeout, failure recording, and a built-in fallback, mirroring the release-notes fetch pipeline.
- Added `GET /api/catalog`, `POST /api/catalog/refresh`, `GET /api/upstream/probe`, `POST /api/upstream/test` (also `/manager/upstream/test`), and `POST /api/upstream/credential`.
- Added an explicit upstream credential source: `auto`, `request`, `env`, `codex_auth`, or `secret`, with `x-codeseex-credential-source` attached to upstream requests for diagnostics.

### Changed

- Model aliases and outbound slug rewrites are data-driven: `aliases`, `alias_patterns`, and `upstream_slug` come from the catalog instead of hardcoded `gpt-5*` handling.
- The settings page renders one editable price row per catalog model plus timezone, peak windows, and a peak multiplier, instead of three fixed cards.
- Usage cost estimates read the active pricing document and label unpriced models explicitly.
- `GET /v1/models` and `/api/models` now advertise the catalog document that is actually active.
- `model-catalog.json` keeps its on-disk contract; only its generator moved to the merged catalog document.
- Client identity headers are passed through unchanged: CodeSeeX routes a request, it does not rewrite who sent it.

### Fixed

- Fixed requests to some upstream addresses being rejected for a missing Codex client identity. The client's `originator`, `user-agent`, `session_id`, and `conversation_id` are now passed through natively instead of being dropped, and the client Authorization is no longer discarded for Codex-App-shaped payloads on custom endpoints — only the official endpoint keeps credential isolation.
- Fixed `GET /v1/models` advertising the built-in catalog instead of the currently active document.
- Fixed a fetched-but-unchanged remote manifest discarding its ETag, which made every later refresh re-download the full document.
- Fixed the settings UI silently pricing unknown models at the Pro rate.
- Fixed the peak/valley window and multiplier being implemented twice in Rust and JavaScript; both now come from the pricing document.
- Fixed provider-native grouped tool declarations (`namespace`, `tool_search`) being rejected as untranslatable. CodeSeeX now validates and forwards those declarations verbatim so the endpoint keeps the tool grouping and namespace it owns, while unknown or malformed shapes still fail closed instead of being silently dropped.
- Fixed the default configuration being unusable for any tool request: with the native Responses transport and the CodeSeeX local web-search backend, the provider-native `web_search` declaration Codex always advertises was rejected outright. CodeSeeX now runs that hosted search inside the native transport instead of borrowing the Chat API compatibility path, so tool ownership still never changes silently.
- Fixed a native Responses turn failing with `mixed tool group` whenever the provider asked Codex for one of its own tools, for example `read_thread`. A provider turn whose calls are all Codex-owned is now forwarded to Codex unchanged and retained in RAM for the continuation check, and only a turn that really mixes hosted and client-owned calls keeps failing closed.
- Fixed a native Responses replay being rejected with `Duplicate namespace name 'codex_app'` after the Codex App restarted. A repeated grouped declaration is merged into its first occurrence instead of being forwarded as a duplicate, and the repair is recorded in the event log.
- Fixed native Responses compatibility failures being logged without their `issue`, `selected_web_search_backend`, and `fallback` fields, which made an incompatible request impossible to diagnose from the log.
- Fixed a native Responses replay being rejected with `Invalid schema for function 'codex_app::automation_update'`. Codex's deferred app tools can declare a parameter schema built only from a top-level `oneOf` union with no `type`, which the endpoint refuses outright; CodeSeeX now declares such a union as an object schema without changing the union itself, and records the repair in the event log.
- Fixed a native tool continuation being rejected with `The native tool continuation did not retain the provider tool group visible to Codex`. Codex drops item ids that are not prefix-qualified and provider fields its own item shapes cannot carry before it replays history, so the continuation check now compares the protocol unit — the same item kinds in order and the same tool call identity — while the upstream request still uses CodeSeeX's stored copy of the group.
- Fixed the hosted tool loop dropping the hosted rounds it executed itself from the continuation it sends upstream. When a provider turn that Codex fully owns follows a CodeSeeX-executed search, the retained group now replays that round at its original offset inside the client-visible anchor, so the provider keeps seeing the exact context it produced the group in.

### Compatibility Notes

- `BILLING_*` configuration keys from 0.7.0 remain readable and are migrated onto `[billing]`; new writes use the structured keys only.
- The default remote manifest points at the repository `main` branch and takes effect once it is published there; until then the cache or the built-in document is used. Set `CODESEEX_CATALOG_URL` for a mirror, or `off` to disable remote refresh.
- User overrides and the private prompt overlay always win over the remote manifest; a remote document can never rewrite `base_instructions`/`model_messages`.
- Per-request rate snapshots for historical usage are not part of 0.7.1; billing buckets carry the pricing revision and the resolved rates so estimates stay labelled.
## 0.7.0 - 2026-08-25

CodeSeeX 0.7.0 is the DeepSeek Responses API release. Official DeepSeek endpoints now use the native Responses transport by default, while Chat API compatibility remains available as an explicit experimental fallback.

### Highlights

- Added native DeepSeek Responses support for the official endpoint across supported DeepSeek models, including native SSE and Responses tool items.
- Preserved CodeSeeX's local and DeepSeek official Web Search backends as separate, explicit choices.
- Fixed DeepSeek thinking-mode context continuity by replaying `reasoning_content` with assistant messages.
- Kept Codex full replay authoritative and retained atomic client-tool groups across native continuations.
- Split image understanding and image generation into independent tools, with DeepSeek Vision support and separate credentials.

### Added

- Added an experimental Chat API compatibility option for upstream compatibility and controlled troubleshooting.
- Added native Responses handling for full replay, response identity mapping, function/custom tool calls, cancellation, terminal status, and final usage.
- Added explicit Web Search backend selection: CodeSeeX local search or DeepSeek official server-side search.
- Added bounded reasoning replay coverage for ordinary assistant turns, tool turns, full-context storage, and compatibility budget processing.
- Added separate image understanding and image generation tools, including DeepSeek Vision support, independent credentials, and dedicated Usage accounting.
- Added a dedicated image-capabilities guide covering providers, limits, privacy, credentials, usage, and billing.
- Completed the built-in language-pack key schema; untranslated newer strings explicitly use the English fallback instead of exposing raw keys.

### Changed

- Official `https://api.deepseek.com` requests using `auto` now select `/responses` for all configured DeepSeek models; custom endpoints remain on the conservative Chat compatibility path.
- Chat compatibility is never selected silently after a native upstream failure. Users can choose it explicitly from Experimental settings.
- Native Responses preserves provider event order and sequence numbers and does not synthesize Chat-style `[DONE]` frames.
- DeepSeek thinking `reasoning_content` is retained in non-streaming and streaming assistant turns, including legacy response reconstruction and bounded full-context runtime storage.
- Web Search backend selection never double-dispatches or silently replaces the user's selected ownership model.
- Image understanding and image generation no longer share a tool switch or credential; DeepSeek Vision stays outside the main Codex model catalog.
- Image understanding is enabled by default for new and upgraded configurations through a one-time capability-schema migration; later user changes remain authoritative, while image generation stays independent and disabled by default.
- Vision usage is recorded as a separate session phase with its own token totals, latency, model, and peak/off-peak cost estimate.
- Default Vision diagnostics keep provider, model, image count, detail mode, duration, and normalized usage without storing original images, base64, full prompts, or provider responses.

### Fixed

- Fixed the 0.6.0-reported thinking-mode continuity issue where assistant `reasoning_content` was dropped before the next Chat API request.
- Fixed native tool continuation settlement so failed or incomplete upstream responses leave pending atomic tool groups available for a valid retry.
- Fixed unknown `previous_response_id` values bypassing a matching pending native tool-group anchor.
- Fixed bounded unterminated native SSE frames so provider response IDs are still mapped to the local Codex-facing response ID.
- Fixed legacy Chat history reconstruction and budget processing to retain bounded reasoning content.
- Fixed legacy Vision URL, model, and `VISION_API_KEY` values being lost when migrating from the old combined configuration.
- Fixed legacy explicit tool lists leaving the new image-understanding capability disabled after upgrade.
- Fixed image generation being able to reuse the new image-understanding credential or become implicitly enabled by legacy fields.
- Fixed secret-field autosave so empty password inputs keep existing secrets while explicit replacement and clear actions still save.

### Compatibility Notes

- Official DeepSeek endpoints default to Responses. Select `Chat API compatibility` in Experimental settings for upstream recovery or when a request requires a CodeSeeX-owned local tool executor.
- CodeSeeX local Web Search remains available and remains the default backend. DeepSeek official Web Search is provider-owned and may use additional tokens or provider-side search calls.
- Custom OpenAI-compatible endpoints remain on Chat compatibility under `auto`; forcing native Responses for a custom endpoint is an advanced TOML/environment option and fails closed when unsupported.
- CodeSeeX does not read or write Codex JSONL transcripts and does not add Codex App-specific injection to the native Responses path.
- DeepSeek Vision requires an explicit `DEEPSEEK_API_KEY` source. Custom image understanding and image generation use separate secrets; platforms without secure credential storage fail closed instead of writing plaintext TOML.

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
