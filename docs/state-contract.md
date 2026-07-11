# State Contract

CodeSeeX state is a runtime/log boundary, not a second Codex transcript or context store.

Codex owns conversation context and decides what to send to third-party providers on every request. In the common Codex full-context mode, CodeSeeX receives the complete current `input` from Codex and must not persist that payload as a long-term copy of the session. CodeSeeX keeps only the current-process bridge state needed to finish active requests.

## Responsibilities

- Keep active request lifecycle, recent usage, `previous_response_id` bridge data, and a bounded Canonical Session Core of anonymous session/message fingerprints in memory for the current process.
- Clear recent request state on restart; stale `previous_response_id` references must not be silently reconstructed from old local data.
- Write user-visible events to `logs/YYYY-MM-DD.jsonl` with compact, redacted details.
- Keep CodeSeeX-owned tool facts only inside the current request/process bridge or inside explicit compact response items.
- Store user configuration in `config.toml`, generated Codex catalog in `model-catalog.json`, extensions under `extension/`, language overrides under `lang/`, and local compact key material under `secrets/`.

## Non-Responsibilities

CodeSeeX must not store:

- Full Codex jsonl sessions or a second conversation transcript.
- Full Codex `input`, system instructions, tools schema, prompt cache body, or canonical replay text.
- Complete assistant final text as durable state.
- Raw webpage archives, screenshots, `data:` URLs, binary payloads, or unbounded tool stdout.
- API keys, Authorization headers, proxy credentials, cookies, or tokens in logs.
- A global durable tool-fact pool that can leak facts across topics.

## Runtime And Logs

- `/api/status` and `/api/usage` report current-process runtime state.
- Restarting the proxy clears active requests, last turn, and recent usage history.
- `/api/events` reads compact user JSONL logs from `logs/`, not SQLite.
- Diagnostic events are not persisted by default; development diagnostics require explicit opt-in logging.
- Log maintenance deletes old log files according to `log_retention_days`.
- Legacy `codeseex.db` files are ignored and not automatically deleted.

## History Reconstruction

- Current-process `previous_response_id` chains may be replayed from memory only.
- After restart or LRU expiry, missing `previous_response_id` state must be treated as unavailable; the client should send full context.
- Codex full-context requests are used directly for the current upstream call and are not duplicated into CodeSeeX state. The Canonical Session Core stores only bounded SHA-256 fingerprints for alignment diagnostics; it never restores a transcript after restart.
- Compact records are explicit response items; they do not depend on a hidden local context database.
