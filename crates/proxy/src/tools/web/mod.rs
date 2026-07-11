mod browser;
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
use std::time::Duration;

const MAX_BYTES: u64 = 524_288;
const MAX_TEXT_CHARS: usize = 12_000;
const MAX_RESULTS: usize = 8;
const MAX_QUERIES: usize = 3;
const MAX_OPEN_TARGETS: usize = 6;
const AUTO_OPEN_EVIDENCE_TARGETS: usize = 2;
const MAX_EVIDENCE_EXCERPT_CHARS: usize = 1_500;
const AUTO_OPEN_EVIDENCE_TIMEOUT_SECS: u64 = 6;
const WEB_SEARCH_TOTAL_TIMEOUT_SECS: u64 = 15;

pub(crate) async fn warm_search_sources(proxy_mode: NetworkProxyMode) -> Value {
    let web_client = net::web_client(proxy_mode);
    search::warm_sources(&web_client, proxy_mode).await
}

pub(crate) async fn execute(
    _client: &reqwest::Client,
    proxy_mode: NetworkProxyMode,
    arguments: &Value,
    messages: &[Value],
) -> Value {
    let action_mode = action_mode(arguments);
    match tokio::time::timeout(
        Duration::from_secs(WEB_SEARCH_TOTAL_TIMEOUT_SECS),
        execute_inner(proxy_mode, arguments, messages),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => json!({
            "ok": false,
            "stage": action_mode,
            "mode": action_mode,
            "error": "web_search_timeout",
            "message": "web_search exceeded the bounded execution time. Try a narrower query, fewer URLs, or retry when network connectivity is stable.",
            "timeout_seconds": WEB_SEARCH_TOTAL_TIMEOUT_SECS
        }),
    }
}

async fn execute_inner(
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
    let mut source_diagnostics = Vec::new();
    let mut source_order = Vec::new();
    let mut sources_deprioritized = Vec::new();
    let mut source_health = Value::Array(Vec::new());
    let mut has_low_confidence_fallback_candidates = false;
    let web_client = net::web_client(proxy_mode);
    let query_results = join_all(
        queries
            .iter()
            .take(MAX_QUERIES)
            .map(|query| search::query(&web_client, proxy_mode, query, max_results)),
    )
    .await;
    let mut sources_attempted = Vec::new();
    for result in query_results {
        if let Some(results) = result.get("results").and_then(Value::as_array) {
            all_results.extend(results.iter().cloned());
        }
        if let Some(errors) = result.get("fallback_errors").and_then(Value::as_array) {
            fallback_errors.extend(errors.iter().cloned());
        }
        if let Some(diagnostics) = result.get("source_diagnostics").and_then(Value::as_array) {
            source_diagnostics.extend(diagnostics.iter().cloned());
        }
        if let Some(sources) = result.get("sources_attempted").and_then(Value::as_array) {
            for source in sources.iter().filter_map(Value::as_str) {
                if !sources_attempted.iter().any(|value| value == source) {
                    sources_attempted.push(source.to_owned());
                }
            }
        }
        if source_order.is_empty() {
            if let Some(sources) = result.get("source_order").and_then(Value::as_array) {
                source_order.extend(sources.iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
        if let Some(sources) = result
            .get("sources_deprioritized")
            .and_then(Value::as_array)
        {
            for source in sources.iter().filter_map(Value::as_str) {
                if !sources_deprioritized.iter().any(|value| value == source) {
                    sources_deprioritized.push(source.to_owned());
                }
            }
        }
        if source_health.as_array().is_some_and(Vec::is_empty) {
            source_health = result
                .get("source_health")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
        }
        has_low_confidence_fallback_candidates |= result
            .get("low_confidence_fallback")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        per_query.push(query_diagnostic(&result));
    }
    let (all_results, low_confidence_fallback) = finalize_search_results(
        all_results,
        max_results,
        has_low_confidence_fallback_candidates,
    );
    let quality = candidates::average_score(&all_results);
    let ok = !all_results.is_empty();
    let evidence_targets = evidence_targets(&all_results, AUTO_OPEN_EVIDENCE_TARGETS);
    let evidence_open = auto_open_evidence(proxy_mode, &evidence_targets).await;
    let evidence = compact_evidence(&evidence_open);
    let auto_opened_count = evidence.len();
    let auto_open_failed_count = evidence_open
        .get("failure_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let auto_open_failed_results = evidence_open
        .get("failed_results")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));

    json!({
        "ok": ok,
        "stage": "search",
        "mode": "search",
        "queries": queries,
        "source": if ok { "multi_source_html" } else { "none" },
        "sources_attempted": sources_attempted,
        "source_order": source_order,
        "sources_deprioritized": sources_deprioritized,
        "source_health": source_health,
        "quality": quality,
        "low_confidence": !ok || quality < 0.24,
        "low_confidence_fallback": low_confidence_fallback,
        "results": all_results.clone(),
        "candidates": all_results.clone(),
        "candidate_count": all_results.len(),
        "evidence": evidence,
        "evidence_count": auto_opened_count,
        "auto_opened": !evidence_targets.is_empty(),
        "auto_open_targets": evidence_targets,
        "auto_opened_count": auto_opened_count,
        "auto_open_failed_count": auto_open_failed_count,
        "auto_open_failed_results": auto_open_failed_results,
        "per_query": per_query,
        "fallback_errors": fallback_errors,
        "source_diagnostics": source_diagnostics,
        "next_action": if auto_opened_count > 0 { "Answer from evidence when sufficient. Do not repeat search for the same question. If a specific candidate needs more content, call web_search with mode=open and selected open_urls or open_ids." } else { "No evidence was opened automatically. If page content is needed, call web_search with mode=open and selected open_urls or open_ids." },
        "truncated": queries.len() > MAX_QUERIES
    })
}

async fn auto_open_evidence(proxy_mode: NetworkProxyMode, targets: &[String]) -> Value {
    if targets.is_empty() {
        return Value::Null;
    }
    match tokio::time::timeout(
        Duration::from_secs(AUTO_OPEN_EVIDENCE_TIMEOUT_SECS),
        open::many(proxy_mode, targets, &[], &[]),
    )
    .await
    {
        Ok(value) => value,
        Err(_) => json!({
            "ok": false,
            "stage": "open",
            "mode": "open",
            "error": "auto_open_timeout",
            "message": "Automatic evidence opening timed out; search candidates are still available.",
            "open_urls": targets,
            "opened_count": 0,
            "failure_count": targets.len(),
            "failed_results": targets.iter().map(|url| json!({
                "url": url,
                "error": "auto_open_timeout"
            })).collect::<Vec<_>>()
        }),
    }
}

fn evidence_targets(results: &[Value], max_targets: usize) -> Vec<String> {
    let mut targets = Vec::new();
    for item in results {
        if targets.len() >= max_targets {
            break;
        }
        let Some(url) = item.get("url").and_then(Value::as_str) else {
            continue;
        };
        if targets.iter().any(|value| value == url) {
            continue;
        }
        targets.push(url.to_owned());
    }
    targets
}

fn compact_evidence(open_result: &Value) -> Vec<Value> {
    open_result
        .get("results")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(AUTO_OPEN_EVIDENCE_TARGETS)
                .map(compact_evidence_item)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn compact_evidence_item(item: &Value) -> Value {
    let content = item
        .get("content")
        .or_else(|| item.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    json!({
        "id": item.get("id").cloned().unwrap_or(Value::Null),
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "url": item.get("url").cloned().unwrap_or(Value::Null),
        "status": item.get("status").cloned().unwrap_or(Value::Null),
        "snippet": item.get("snippet").cloned().unwrap_or(Value::Null),
        "content_excerpt": extract::truncate_chars(content, MAX_EVIDENCE_EXCERPT_CHARS),
        "content_chars": content.chars().count(),
        "source": "auto_open",
        "opened": true,
        "truncated": item.get("truncated").cloned().unwrap_or(Value::Null)
    })
}

fn finalize_search_results(
    raw_results: Vec<Value>,
    max_results: usize,
    has_low_confidence_fallback_candidates: bool,
) -> (Vec<Value>, bool) {
    let fallback_source = raw_results.clone();
    let mut results = candidates::dedupe_results(raw_results);
    candidates::retain_usable_results(&mut results);
    results.sort_by(|left, right| {
        let right_score = right.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let left_score = left.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(max_results);
    if !results.is_empty() || !has_low_confidence_fallback_candidates {
        return (results, false);
    }
    let fallback = search::low_confidence_fallback_results(fallback_source, max_results);
    let used_fallback = !fallback.is_empty();
    (fallback, used_fallback)
}

fn action_mode(arguments: &Value) -> &'static str {
    let mode = input::mode(arguments);
    if mode == "open"
        || !input::open_targets(arguments).is_empty()
        || !input::open_ids(arguments).is_empty()
    {
        "open"
    } else {
        "search"
    }
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
    let source_diagnostic_count = result
        .get("source_diagnostics")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    json!({
        "ok": result.get("ok").cloned().unwrap_or(Value::Bool(false)),
        "query": result.get("query").cloned().unwrap_or(Value::Null),
        "source": result.get("source").cloned().unwrap_or(Value::Null),
        "candidate_count": result_count,
        "evidence_count": result.get("evidence_count").cloned().unwrap_or(Value::Null),
        "quality": result.get("quality").cloned().unwrap_or(Value::Null),
        "low_confidence": result.get("low_confidence").cloned().unwrap_or(Value::Null),
        "fallback_error_count": fallback_error_count,
        "source_diagnostic_count": source_diagnostic_count
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
            "fallback_errors": [{ "source": "bing_html", "error": "empty_results" }],
            "source_diagnostics": [{ "source": "bing_html", "result_count": 0 }]
        }));

        assert_eq!(diagnostic["candidate_count"], 1);
        assert_eq!(diagnostic["fallback_error_count"], 1);
        assert_eq!(diagnostic["source_diagnostic_count"], 1);
        assert!(diagnostic.get("results").is_none());
        assert!(diagnostic.get("fallback_errors").is_none());
        assert!(diagnostic.get("source_diagnostics").is_none());
    }

    #[test]
    fn low_confidence_fallback_survives_outer_aggregation() {
        let (all_results, low_confidence_fallback) = finalize_search_results(
            vec![json!({
                "url": "https://weak.example.com/noise",
                "title": "Noise",
                "snippet": "",
                "query": "today weather in Zhongshan China",
                "source": "bing_html",
                "score": 0.12
            })],
            5,
            true,
        );

        assert_eq!(all_results.len(), 1);
        assert!(low_confidence_fallback);
        assert_eq!(all_results[0]["url"], "https://weak.example.com/noise");
    }

    #[test]
    fn low_confidence_fallback_flag_requires_fallback_results_to_be_used() {
        let (all_results, low_confidence_fallback) = finalize_search_results(
            vec![
                json!({
                    "url": "https://weather.example.com/zhongshan",
                    "title": "Zhongshan weather today",
                    "snippet": "Zhongshan weather details",
                    "query": "today weather in Zhongshan China",
                    "source": "brave_html",
                    "score": 0.988
                }),
                json!({
                    "url": "https://noise.example.com",
                    "title": "Noise",
                    "snippet": "",
                    "query": "today weather in Zhongshan China",
                    "source": "bing_html",
                    "score": 0.08
                }),
            ],
            5,
            true,
        );

        assert_eq!(all_results.len(), 1);
        assert_eq!(all_results[0]["source"], "brave_html");
        assert!(!low_confidence_fallback);
    }

    #[test]
    fn evidence_targets_selects_top_unique_result_urls() {
        let targets = evidence_targets(
            &[
                json!({ "url": "https://example.com/a", "score": 0.9 }),
                json!({ "url": "https://example.com/a", "score": 0.8 }),
                json!({ "url": "https://example.com/b", "score": 0.7 }),
                json!({ "url": "https://example.com/c", "score": 0.6 }),
            ],
            2,
        );

        assert_eq!(
            targets,
            vec![
                "https://example.com/a".to_owned(),
                "https://example.com/b".to_owned()
            ]
        );
    }

    #[test]
    fn compact_evidence_keeps_excerpt_not_full_page_text() {
        let content = "A".repeat(MAX_EVIDENCE_EXCERPT_CHARS + 100);
        let result = compact_evidence(&json!({
            "results": [{
                "id": "cand_a",
                "title": "Evidence",
                "url": "https://example.com/a",
                "status": 200,
                "snippet": "short",
                "content": content,
                "truncated": false
            }]
        }));

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["id"], "cand_a");
        assert!(result[0]["content_excerpt"]
            .as_str()
            .unwrap_or_default()
            .contains("[truncated chars="));
        assert_eq!(
            result[0]["content_chars"].as_u64(),
            Some((MAX_EVIDENCE_EXCERPT_CHARS + 100) as u64)
        );
    }

    #[test]
    fn web_search_total_timeout_stays_bounded() {
        const { assert!(WEB_SEARCH_TOTAL_TIMEOUT_SECS <= 15) }
    }

    #[test]
    fn action_mode_treats_urls_as_open_requests() {
        assert_eq!(
            action_mode(&json!({ "query": "https://example.com" })),
            "open"
        );
        assert_eq!(action_mode(&json!({ "query": "weather today" })), "search");
        assert_eq!(action_mode(&json!({ "open_ids": ["cand_123"] })), "open");
    }
}
