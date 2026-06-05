# State Contract

CodeSeeX state is an adapter ledger, not a second Codex transcript or context store.

Codex owns conversation context and decides what to send to third-party providers on every request. In the common Codex full-context mode, CodeSeeX receives the complete current `input` from Codex and must not persist that payload as a long-term copy of the session. The proxy keeps only the bounded adapter state needed to finish the current request, bridge short-lived protocol gaps, report usage, and recover from interrupted tool loops.

## Responsibilities

The SQLite store may persist only data needed by the adapter boundary:

- Request lifecycle: `in_progress`, `completed`, `failed`, and `interrupted`.
- The `response_id` / `previous_response_id` chain only when Codex actually uses server-side chaining.
- A bounded request envelope: model, prompt cache key, current delta input, or a small tail slice for Codex full-context requests.
- Replayable `turn_messages` only for short-lived bridge/tool-loop recovery, not as a full transcript.
- Bounded verified tool facts for in-progress recovery, compact diagnostics, or short-lived tool result disambiguation.
- Usage, visible logs, and diagnostics needed by the local manager UI.

The store must stay useful after a crash or stream disconnect. A request is checkpointed at start, upgraded as tools complete, and finalized only when the turn really completes.

## Non-Responsibilities

The SQLite store must not become:

- A full copy of Codex jsonl sessions.
- A durable replacement for Codex's own context management.
- A raw browser cache or webpage archive.
- A long-term dump of screenshots, `data:` URLs, binary payloads, or complete tool stdout.
- A secret store for API keys, Authorization headers, proxy credentials, or tokens.
- A source of model behavior that overrides Codex-native tool execution or MCP ownership.

If a fact is too large for durable adapter replay, persist a deterministic marker with size and hash instead of the full payload.

## Maintenance Rules

State maintenance must preserve schema and conversation identity while bounding risk:

- SQLite is opened with WAL journaling, normal synchronous mode, foreign keys enabled, and a busy timeout for local concurrent reads/writes.
- New writes are sanitized before they reach SQLite.
- Codex full-context request payloads are stored as bounded adapter slices, not complete transcripts.
- Existing oversized request payloads are sanitized in place on maintenance; request rows are not deleted blindly.
- Maintenance may reclaim SQLite file space after large in-place compaction.
- Maintenance may process multiple bounded batches per run, and reports when the batch limit is reached.
- Visible/debug event logs are retention-bound by `log_retention_days`.
- Large inline `data:` URLs are replaced by size/hash markers.
- Sensitive keys are redacted recursively.
- Long strings are truncated with size/hash markers.

This prevents the old single-file JSON failure mode and avoids turning SQLite into an ever-growing mirror of Codex's own session files.

## History Reconstruction

When rebuilding context:

- Completed parents may contribute final response data and replayable turn messages.
- Failed or interrupted parents may contribute user input and verified tool facts, but not partial assistant final text.
- Tool facts have higher evidence priority than assistant self-descriptions.
- Compact records are client summaries and must not override verified tool/request facts.

The result should be deterministic, bounded, and protocol-valid without reading Codex's private session files. If Codex already provides the full current context, CodeSeeX should use that request directly instead of reconstructing an alternative history from SQLite.
