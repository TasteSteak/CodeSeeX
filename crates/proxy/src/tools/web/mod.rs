mod candidates;
mod extract;
mod input;
mod net;
mod open;
mod safety;
mod search;

use codeseex_core::NetworkProxyMode;
use futures_util::future::join_all;
use serde_json::{json, Value};

const MAX_BYTES: u64 = 524_288;
const MAX_TEXT_CHARS: usize = 12_000;
const MAX_RESULTS: usize = 8;
const MAX_QUERIES: usize = 3;
const MAX_OPEN_TARGETS: usize = 6;

pub(crate) async fn execute(
    _client: &reqwest::Client,
    proxy_mode: NetworkProxyMode,
    arguments: &Value,
    messages: &[Value],
) -> Value {
    let mode = input::mode(arguments);
    let urls = input::open_targets(arguments);
    let open_ids = input::open_ids(arguments);
    if mode == "open" || !urls.is_empty() || !open_ids.is_empty() {
        let mut targets = urls;
        let lookup = candidates::lookup_from_messages(messages);
        let unresolved_ids = candidates::resolve_open_ids(&open_ids, &lookup, &mut targets);
        return open::many(proxy_mode, &targets, &open_ids, &unresolved_ids).await;
    }

    let queries = input::queries(arguments);
    if queries.is_empty() {
        return json!({
            "ok": false,
            "error": "missing_query",
            "message": "web_search requires query/search_query/queries for mode=search or url/urls/open_urls/open_ids for mode=open."
        });
    }

    let max_results = usize_arg(arguments, "max_results", 5, 1, MAX_RESULTS);
    let mut per_query = Vec::new();
    let mut all_results = Vec::new();
    let mut fallback_errors = Vec::new();
    let web_client = net::web_client(proxy_mode);
    let query_results = join_all(
        queries
            .iter()
            .take(MAX_QUERIES)
            .map(|query| search::query(&web_client, query, max_results)),
    )
    .await;
    for result in query_results {
        if let Some(results) = result.get("results").and_then(Value::as_array) {
            all_results.extend(results.iter().cloned());
        }
        if let Some(errors) = result.get("fallback_errors").and_then(Value::as_array) {
            fallback_errors.extend(errors.iter().cloned());
        }
        per_query.push(query_diagnostic(&result));
    }
    all_results = candidates::dedupe_results(all_results);
    candidates::retain_usable_results(&mut all_results);
    all_results.sort_by(|left, right| {
        let right_score = right.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let left_score = left.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(max_results);
    let quality = candidates::average_score(&all_results);
    let ok = !all_results.is_empty();

    json!({
        "ok": ok,
        "stage": "search",
        "mode": "search",
        "queries": queries,
        "source": if ok { "multi_source_html" } else { "none" },
        "sources_attempted": ["bing_html", "brave_html", "duckduckgo_lite", "duckduckgo_html", "duckduckgo_instant_answer"],
        "quality": quality,
        "low_confidence": !ok || quality < 0.24,
        "results": all_results.clone(),
        "candidates": all_results.clone(),
        "candidate_count": all_results.len(),
        "per_query": per_query,
        "fallback_errors": fallback_errors,
        "next_action": "When page content is needed, call web_search again with mode=open and selected open_urls. open_ids are also accepted for candidate lookup.",
        "truncated": queries.len() > MAX_QUERIES
    })
}

fn query_diagnostic(result: &Value) -> Value {
    let result_count = result
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let fallback_error_count = result
        .get("fallback_errors")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    json!({
        "ok": result.get("ok").cloned().unwrap_or(Value::Bool(false)),
        "query": result.get("query").cloned().unwrap_or(Value::Null),
        "source": result.get("source").cloned().unwrap_or(Value::Null),
        "candidate_count": result_count,
        "quality": result.get("quality").cloned().unwrap_or(Value::Null),
        "low_confidence": result.get("low_confidence").cloned().unwrap_or(Value::Null),
        "fallback_error_count": fallback_error_count
    })
}

fn usize_arg(args: &Value, key: &str, fallback: usize, min: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(fallback)
        .clamp(min, max)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn query_diagnostic_omits_duplicate_result_bodies() {
        let diagnostic = query_diagnostic(&json!({
            "ok": true,
            "query": "Rust README",
            "source": "multi_source_html",
            "quality": 0.8,
            "low_confidence": false,
            "results": [{
                "id": "cand_1",
                "title": "Rust",
                "url": "https://example.com",
                "snippet": "large snippet should not be duplicated here"
            }],
            "fallback_errors": [{ "source": "bing_html", "error": "empty_results" }]
        }));

        assert_eq!(diagnostic["candidate_count"], 1);
        assert_eq!(diagnostic["fallback_error_count"], 1);
        assert!(diagnostic.get("results").is_none());
        assert!(diagnostic.get("fallback_errors").is_none());
    }
}
