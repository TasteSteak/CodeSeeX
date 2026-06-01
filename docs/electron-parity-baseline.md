# CodeSeeX Electron Verified Logic Baseline

This document records the verified behavior of the existing Electron/Node implementation before fixing the Tauri/Rust rewrite. It is the migration baseline: CodeSeeX Next must match these behaviors before adding new abstractions.

## Why This Exists

The latest failing conversation shows a low-level Responses compatibility regression:

- Conversation file: `C:\Users\Administrator\.codex\sessions\2026\05\28\rollout-2026-05-28T15-56-38-019e6d95-de35-7b72-bc75-ede1e3d6a876.jsonl`
- Initial failures at `15:56` were upstream `400`:
  - `Tool names must be unique.`
  - `gpt-5.4-mini` was forwarded to DeepSeek instead of being mapped to a supported DeepSeek model.
- After that, one user turn produced multiple assistant messages in the same Codex turn.
- The repeated assistant outputs had `phase: null`, and one hallucinated `"我是 Claude"`.

This means the rewrite did not fully preserve the old verified Responses streaming contract. Fixes must be made against the baseline below, not by guessing.

## Core Contract

CodeSeeX is a local Codex-compatible Responses proxy backed by DeepSeek Chat Completions. The proxy must preserve Codex's native behavior at the app boundary while adapting only the upstream DeepSeek call.

The boundary rules are:

- Codex speaks `/v1/responses`, `/v1/chat/completions`, `/v1/models`, and related Responses endpoints to CodeSeeX.
- CodeSeeX sends OpenAI-compatible Chat Completions to DeepSeek or a custom upstream.
- Codex-native tools, especially MCP/external tools, remain visible to Codex as native Responses `function_call` items.
- CodeSeeX-hosted tools may be executed internally, but their visible output must still be legal Responses output.
- The app must see a completed turn exactly once unless the model intentionally emits a native tool call and waits for Codex to execute it.

## Model Mapping

Old behavior:

- `deepseek-v4-flash` stays `deepseek-v4-flash`.
- `deepseek-v4-pro` stays `deepseek-v4-pro`.
- Unknown Codex-requested models are not forwarded to DeepSeek raw.
- In default mode, unsupported requested models fall back to `deepseek-v4-pro`.
- Explicit override modes force Flash or Pro regardless of requested model.

Regression guard:

- A request with `model: "gpt-5.4-mini"` must not reach DeepSeek as `gpt-5.4-mini`.
- Logs should record both `requested_model` and actual upstream `model`.

## Account Balance

Old behavior:

- The balance panel reads the API key from Codex `auth.json`, not from a separately typed CodeSeeX secret.
- Auth lookup supports `CODEX_AUTH_JSON` / `CODEX_AUTH_FILE`, `CODEX_HOME/auth.json`, `%USERPROFILE%\.codex\auth.json`, and `%APPDATA%\codex\auth.json`.
- The balance URL is derived from the configured upstream base URL by stripping a trailing `/v1` and appending `/user/balance`.
- Official DeepSeek therefore uses `https://api.deepseek.com/user/balance`, while custom/self-hosted upstream keys are not accidentally sent to the official host.
- The manager API returns a stable JSON body for success and failure so the UI can render a normal unavailable state.

Regression guard:

- A configured custom base URL like `http://127.0.0.1:9000/v1` must query `http://127.0.0.1:9000/user/balance`.
- The Authorization header for balance checks must come from Codex `auth.json` when available.
- Balance failures should return `{ ok: false, code, message }` rather than surfacing as a UI transport crash.

## Tool Declaration Rules

Old behavior:

- CodeSeeX registers its known hosted/internal tools first.
- `apply_patch` is special and must not be duplicated if Codex also provides a native declaration.
- `apply_patch`, `web_search`, and `mcp_server` are system tools in the client registry: always enabled, non-configurable, and shown without client-side switches.
- System tools may still have a built-in source label; "system" describes configurability and trust boundary, not whether the implementation lives in CodeSeeX.
- Known CodeSeeX tools have priority over same-name external declarations.
- External/MCP tools are normalized into upstream Chat function tools only so DeepSeek can choose them.
- If DeepSeek selects an external/MCP tool, CodeSeeX maps it back to a native Responses `function_call` and stops; Codex executes/displays it natively.
- CodeSeeX must not execute MCP tools itself and must not fake MCP servers.

Regression guard:

- Upstream `tools[*].function.name` must be unique.
- Duplicate `apply_patch` from Codex input plus CodeSeeX injection must produce one upstream tool.
- MCP calls must return as native Responses `function_call` with the original response name/namespace.

## Responses Streaming Contract

Old Electron streaming is not just raw SSE passthrough. It emits a Codex-compatible Responses stream with these invariants:

- Emit `response.created`.
- Emit `response.in_progress`.
- Emit `response.output_item.added` before text/tool/reasoning deltas.
- Emit content deltas in order.
- Emit `response.output_text.done`.
- Emit `response.content_part.done`.
- Emit `response.output_item.done`.
- Emit `response.completed`.
- Emit `data: [DONE]`.

Important old details:

- Message output items include `phase`.
- Text before tool calls is `phase: "commentary"`.
- Final answer text is `phase: "final_answer"`.
- The final `response.completed.response.output` reuses the same streamed output item IDs.
- The final response must not create a second message item with a new id for text already streamed.
- Function-call argument streams emit both deltas and done events.
- Streaming payloads request usage with `stream_options: { include_usage: true }`.

Regression guard:

- A single user request that produces a final answer must create exactly one final assistant message in the Codex jsonl.
- The jsonl `event_msg.agent_message.phase` should be `final_answer` for final answers, not `null`.
- Codex must not keep re-issuing the same turn with accumulated assistant messages.
- Token usage should be available when upstream returns usage.

## Reasoning / Thinking Display

Old behavior:

- DeepSeek `reasoning_content` is represented as legal Responses reasoning output.
- Visible thinking display is a separate display-only message when enabled.
- Display-only thinking uses metadata/markers so it does not pollute later model context.
- Reasoning associated with tool calls is preserved where the DeepSeek protocol needs it.
- Reasoning without tool-call value is not allowed to endlessly accumulate into long-term context.

Regression guard:

- Thinking display must not become ordinary assistant final text.
- Hidden reasoning must not replace user-visible final answer.
- Reasoning output must not break tool-call replay.

## Tool Loop Semantics

Old behavior:

- DeepSeek may return text, reasoning, and tool calls.
- Internal hosted tools are executed by CodeSeeX, appended as legal Chat `tool` messages, and DeepSeek is called again.
- External/MCP tool calls are returned to Codex as native Responses function calls and CodeSeeX stops the current turn.
- Hosted tool loops have a bounded max iteration count.
- If a hosted tool loop cannot produce a final answer, CodeSeeX returns a deterministic fallback rather than spinning forever.

Regression guard:

- Internal tools must not leak as fake MCP calls.
- MCP/native calls must not be swallowed into internal execution.
- A tool-call turn should be `commentary`; a final answer should be `final_answer`.

## Context Compiler And State

Old behavior:

- `previous_response_id` chains are reconstructed from the persisted response state.
- Current input and previous history are de-duplicated where Codex already sends overlapping messages.
- Evidence priority is deterministic:
  - user/developer instructions
  - verified tool call/result facts
  - file/apply_patch facts
  - MCP/web_search facts
  - compaction summaries
  - assistant final text
  - assistant self-description/reasoning
- Failed/interrupted records do not replay partial assistant final text.
- Tool facts survive interruption and can be replayed safely.
- Large data URLs and binary-like payloads are redacted before entering durable context.

Regression guard:

- Tool facts cannot be overwritten by assistant self-description.
- Interrupted output cannot become final historical truth.
- Inline `data:` URLs must not be replayed into long context.

## Error Handling And Logs

Old behavior:

- Runtime logs distinguish conversation requests from context compaction.
- Upstream failure details are preserved enough for diagnosis.
- User-facing logs should not collapse all upstream failures into only `status: 400`.
- State checkpointing happens before and during tool loops, not only after final completion.

Regression guard:

- UI/logs should show messages like `Tool names must be unique` or unsupported model errors.
- A failed request should be persisted as failed, not silently turned into an empty completed turn.

## Compatibility Checklist Before Fixing Next

Before changing Rust code, verify each item against old behavior:

- Model mapping protects DeepSeek from unsupported Codex model names.
- Upstream tool definitions are unique.
- CodeSeeX known tools take priority over external same-name tools.
- MCP/external tools return as native Responses function calls.
- Final answer messages carry `phase: "final_answer"`.
- Commentary/tool-intent messages carry `phase: "commentary"`.
- Streamed message item IDs are reused in `response.completed`.
- Streaming payloads include usage request options.
- No empty output item is emitted unless the protocol truly requires a placeholder.
- Failed upstream status is visible in state and manager logs.
- Context replay does not inject failed partial assistant text.
- Compact and tool facts are logged and replayed deterministically.

## Immediate Regression Hypothesis From The Latest Trace

The latest trace strongly points to three concrete Next regressions:

1. Missing Responses message `phase` caused Codex to treat generated text as non-final/ambiguous, then reissue the same turn with previous assistant outputs included in `input`.
2. Tool declarations initially duplicated `apply_patch`, causing DeepSeek `400 Tool names must be unique`.
3. Unknown requested models initially passed through to DeepSeek, causing DeepSeek `400 unsupported model`.

The first fix pass should restore the old Responses streaming item shape and add a regression test that parses the generated SSE stream as Codex would:

- One request with plain text answer.
- Exactly one `response.completed`.
- Completed response contains one `message` item with `phase: "final_answer"`.
- The `response.completed.output[0].id` equals the earlier streamed `response.output_item.done.item.id`.
- No duplicate assistant message can be reconstructed from the stream.
