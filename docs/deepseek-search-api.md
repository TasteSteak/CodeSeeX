# DeepSeek Search And Fetch Backend

CodeSeeX's `official` web-search backend uses DeepSeek's raw Search and Fetch
APIs. It is still a CodeSeeX-hosted tool: the provider does not own a native
`web_search` declaration for this path.

## Endpoints

| Operation | Endpoint |
| --- | --- |
| Search | `POST https://api.deepseek.com/v1/tools/search` |
| Fetch | `POST https://api.deepseek.com/v1/tools/fetch` |

The base URL follows the configured upstream. A custom URL keeps its path and
gets `/v1/tools/search` or `/v1/tools/fetch` appended.

## Search

Each CodeSeeX query becomes one Search request:

```json
{
  "query": "DeepSeek model release",
  "count": 5,
  "need_content": false,
  "ori_question": "The triggering user question"
}
```

- `count` is bounded by the CodeSeeX web-search result limit.
- `need_content` stays false. DeepSeek's `summary` field is already sized for a
  model, so the search path avoids a second Fetch request for every result.
- The provider's result order and `rerank_score` are preserved. The first three
  summaries become bounded evidence blocks.
- `open_ids` resolve through the same candidate-id contract as the local
  backend.

## Fetch

Opening one or more pages calls Fetch with `{ "url": "..." }`. The returned
Markdown is parsed by the existing CodeSeeX document extractor, so evidence
budgeting, block selection, links, and low-confidence detection stay shared with
the local backend.

## Credentials

Search and Fetch reuse CodeSeeX's normal upstream credential resolution:

1. Managed CodeSeeX credential.
2. `DEEPSEEK_API_KEY` process environment.
3. Codex auth / DeepSeek provider key.
4. Inbound request authorization when the credential source is explicitly
   `request`.

Codex App request authentication is not forwarded as the official search key.
Missing credentials produce an explicit error.

## Failures And Cost

- HTTP 408, 429, and 5xx requests are retried once after a short delay.
- Search or Fetch failures are returned to the model; CodeSeeX does not fall
  back to local scraping.
- DeepSeek bills Search and Fetch at CNY 5 per 1,000 successful requests.
  `count` changes result volume, not the request price. Calls are currently free
  through 2026-11-01 00:00 Asia/Shanghai.
- Both Native Responses and Chat compatibility use the same CodeSeeX executor
  and differ only in the model transport.
