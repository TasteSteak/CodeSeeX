use futures_util::stream::{FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

use codeseex_core::NetworkProxyMode;

use super::candidates::{average_score, dedupe_results, retain_usable_results};
use super::extract::{clean_visible_text, truncate_chars};
use super::net::{fetch_text, read_limited_response_bytes, request_error_message, user_agent};
use super::safety::normalize_candidate_url;
mod sources;
use sources::{
    ranked_sources_from_health, refresh_search_source_health, search_plan,
    source_health_diagnostic, SearchSource,
};
#[cfg(test)]
use sources::{SearchHealthSnapshot, SearchPlan, SearchSourceHealth};
mod parsers;
use parsers::{
    collect_duckduckgo_related, parse_bing_results, parse_brave_results,
    parse_duckduckgo_lite_results, parse_duckduckgo_results, web_locale,
};

const SEARCH_SOURCE_RESULT_GRACE_MS: u64 = 1_200;
const SEARCH_SOURCE_PROBE_TIMEOUT_SECS: u64 = super::net::WEB_REQUEST_TIMEOUT_SECS;
const LOW_CONFIDENCE_FALLBACK_MIN_SCORE: f64 = 0.08;
const LOW_CONFIDENCE_FALLBACK_MAX_RESULTS: usize = 3;

pub(super) async fn query(
    client: &reqwest::Client,
    proxy_mode: NetworkProxyMode,
    query: &str,
    max_results: usize,
) -> Value {
    let plan = search_plan(client, proxy_mode).await;
    let mut fallback_errors = Vec::new();
    let mut collected = Vec::new();
    let mut sources_attempted = Vec::new();
    let mut source_diagnostics = Vec::new();
    let primary_sources = plan.primary_sources();
    let fallback_sources = plan.deprioritized_sources();

    run_search_sources_progressive(
        client,
        query,
        max_results,
        &primary_sources,
        &mut collected,
        &mut fallback_errors,
        &mut sources_attempted,
        &mut source_diagnostics,
    )
    .await;
    if usable_result_count(&collected) == 0 && !fallback_sources.is_empty() {
        run_search_sources_progressive(
            client,
            query,
            max_results,
            &fallback_sources,
            &mut collected,
            &mut fallback_errors,
            &mut sources_attempted,
            &mut source_diagnostics,
        )
        .await;
    }

    let raw_collected = collected.clone();
    let mut results = dedupe_results(collected);
    retain_usable_results(&mut results);
    results.sort_by(|left, right| {
        let right_score = right.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let left_score = left.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(max_results);
    let low_confidence_fallback = if results.is_empty() {
        results = low_confidence_fallback_results(raw_collected, max_results);
        !results.is_empty()
    } else {
        false
    };
    let quality = average_score(&results);

    json!({
        "ok": !results.is_empty(),
        "stage": "search",
        "mode": "search",
        "query": query,
        "source": if results.is_empty() { "none" } else { "multi_source_html" },
        "source_plan": plan.plan_source,
        "sources_attempted": sources_attempted,
        "source_order": plan.source_order_names(),
        "sources_deprioritized": plan.deprioritized_source_names(),
        "source_health": plan.health_diagnostic(),
        "source_diagnostics": source_diagnostics,
        "results": results.clone(),
        "candidates": results.clone(),
        "candidate_count": results.len(),
        "quality": quality,
        "low_confidence": results.is_empty() || quality < 0.24,
        "low_confidence_fallback": low_confidence_fallback,
        "fallback_errors": fallback_errors
    })
}

pub(super) async fn warm_sources(client: &reqwest::Client, proxy_mode: NetworkProxyMode) -> Value {
    let snapshot = refresh_search_source_health(
        client,
        &crate::network::proxy_cache_key(proxy_mode),
        SEARCH_SOURCE_PROBE_TIMEOUT_SECS,
    )
    .await;
    json!({
        "ok": true,
        "stage": "search_source_probe",
        "proxy_key": snapshot.cache_key,
        "source_order": ranked_sources_from_health(&snapshot.sources)
            .iter()
            .map(|source| source.name())
            .collect::<Vec<_>>(),
        "source_health": source_health_diagnostic(&snapshot.sources, snapshot.checked_at.elapsed().as_millis() as u64)
    })
}

async fn run_search_sources_progressive(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    sources: &[SearchSource],
    collected: &mut Vec<Value>,
    fallback_errors: &mut Vec<Value>,
    sources_attempted: &mut Vec<String>,
    source_diagnostics: &mut Vec<Value>,
) {
    let mut pending = sources
        .iter()
        .copied()
        .map(|source| source.search(client, query, max_results))
        .collect::<FuturesUnordered<_>>();
    let mut found_usable_results = false;

    loop {
        let next = if found_usable_results {
            match tokio::time::timeout(
                Duration::from_millis(SEARCH_SOURCE_RESULT_GRACE_MS),
                pending.next(),
            )
            .await
            {
                Ok(next) => next,
                Err(_) => break,
            }
        } else {
            pending.next().await
        };
        let Some(result) = next else {
            break;
        };
        collect_source_results(
            vec![result],
            collected,
            fallback_errors,
            sources_attempted,
            source_diagnostics,
        );
        let usable_count = usable_result_count(collected);
        if usable_count == 0 {
            continue;
        }
        found_usable_results = true;
    }
}

impl SearchSource {
    async fn search(self, client: &reqwest::Client, query: &str, max_results: usize) -> Value {
        match self {
            SearchSource::BingHtml => bing_html(client, query, max_results).await,
            SearchSource::BraveHtml => brave_html(client, query, max_results).await,
            SearchSource::DuckDuckGoLite => duckduckgo_lite(client, query, max_results).await,
            SearchSource::DuckDuckGoHtml => duckduckgo_html(client, query, max_results).await,
            SearchSource::DuckDuckGoInstantAnswer => {
                duckduckgo_instant_answer(client, query, max_results).await
            }
        }
    }
}

fn collect_source_results(
    results_by_source: Vec<Value>,
    collected: &mut Vec<Value>,
    fallback_errors: &mut Vec<Value>,
    sources_attempted: &mut Vec<String>,
    source_diagnostics: &mut Vec<Value>,
) {
    for result in results_by_source {
        let source = result
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        push_unique_source(sources_attempted, source);
        let diagnostic = source_result_diagnostic(&result);
        if let Some(results) = result.get("results").and_then(Value::as_array) {
            collected.extend(results.iter().cloned());
        }
        let result_count = result
            .get("results")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let usable_result_count = diagnostic
            .get("usable_result_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        source_diagnostics.push(diagnostic.clone());
        if result.get("ok").and_then(Value::as_bool) != Some(true)
            || result_count == 0
            || usable_result_count == 0
        {
            fallback_errors.push(json!({
                "source": source,
                "error": diagnostic.get("error").and_then(Value::as_str).unwrap_or("empty_results"),
                "status": result.get("status").and_then(Value::as_u64),
                "result_count": result_count,
                "usable_result_count": usable_result_count,
                "max_score": diagnostic.get("max_score").cloned().unwrap_or(Value::Null),
                "message": result.get("message").and_then(Value::as_str).unwrap_or_default()
            }));
        }
    }
}

fn source_result_diagnostic(result: &Value) -> Value {
    let source = result
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let result_count = results.len();
    let max_score = results
        .iter()
        .filter_map(|item| item.get("score").and_then(Value::as_f64))
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let mut usable_results = dedupe_results(results);
    retain_usable_results(&mut usable_results);
    let usable_result_count = usable_results.len();
    let error = if ok && result_count > 0 && usable_result_count == 0 {
        "filtered_low_confidence"
    } else if ok && result_count == 0 {
        "empty_results"
    } else {
        result
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request_failed")
    };
    json!({
        "source": source,
        "ok": ok,
        "status": result.get("status").cloned().unwrap_or(Value::Null),
        "error": error,
        "result_count": result_count,
        "usable_result_count": usable_result_count,
        "max_score": max_score,
        "message": result.get("message")
            .and_then(Value::as_str)
            .map(|value| truncate_chars(value, 240))
            .unwrap_or_default()
    })
}

fn usable_result_count(results: &[Value]) -> usize {
    let mut results = dedupe_results(results.to_vec());
    retain_usable_results(&mut results);
    results.len()
}

pub(super) fn low_confidence_fallback_results(
    results: Vec<Value>,
    max_results: usize,
) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut results = results
        .into_iter()
        .filter_map(|mut item| {
            let score = item.get("score").and_then(Value::as_f64)?;
            if score < LOW_CONFIDENCE_FALLBACK_MIN_SCORE {
                return None;
            }
            let url = item.get("url").and_then(Value::as_str)?;
            let url = normalize_candidate_url(url)?;
            if !seen.insert(url.to_ascii_lowercase()) {
                return None;
            }
            if let Some(object) = item.as_object_mut() {
                object.insert("url".to_owned(), Value::String(url));
            }
            Some(item)
        })
        .collect::<Vec<_>>();
    results.sort_by(|left, right| {
        let right_score = right.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let left_score = left.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(max_results.min(LOW_CONFIDENCE_FALLBACK_MAX_RESULTS));
    results
}

fn push_unique_source(output: &mut Vec<String>, source: &str) {
    if !output.iter().any(|value| value == source) {
        output.push(source.to_owned());
    }
}

async fn bing_html(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    bing_html_at(
        client,
        query,
        max_results,
        "bing_html",
        "https://www.bing.com/search",
    )
    .await
}

async fn bing_html_at(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    source: &'static str,
    endpoint: &'static str,
) -> Value {
    let locale = web_locale(query);
    let Ok(url) = reqwest::Url::parse_with_params(
        endpoint,
        &[
            ("q", query),
            ("setlang", locale.bing_setlang),
            ("mkt", locale.bing_market),
            ("cc", locale.bing_country),
            ("ensearch", locale.bing_english_search),
        ],
    ) else {
        return json!({ "ok": false, "query": query, "source": source, "error": "invalid_search_url" });
    };
    let fetched = fetch_text(client, url, "text/html,application/xhtml+xml").await;
    if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
        return merge_search_error(source, query, fetched);
    }
    let html = fetched
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let results = parse_bing_results(query, html, max_results, source);
    json!({
        "ok": !results.is_empty(),
        "query": query,
        "source": source,
        "results": results,
        "error": if results.is_empty() { "empty_results" } else { "" }
    })
}

async fn brave_html(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://search.brave.com/search",
        &[("q", query), ("source", "web")],
    ) else {
        return json!({ "ok": false, "query": query, "source": "brave_html", "error": "invalid_search_url" });
    };
    let fetched = fetch_text(client, url, "text/html,application/xhtml+xml").await;
    if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
        return merge_search_error("brave_html", query, fetched);
    }
    let html = fetched
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let results = parse_brave_results(query, html, max_results);
    json!({
        "ok": !results.is_empty(),
        "query": query,
        "source": "brave_html",
        "results": results,
        "error": if results.is_empty() { "empty_results" } else { "" }
    })
}

async fn duckduckgo_lite(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    let locale = web_locale(query);
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://lite.duckduckgo.com/lite/",
        &[("q", query), ("kl", locale.duckduckgo_kl)],
    ) else {
        return json!({ "ok": false, "query": query, "source": "duckduckgo_lite", "error": "invalid_search_url" });
    };
    let fetched = fetch_text(client, url, "text/html,application/xhtml+xml").await;
    if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
        return merge_search_error("duckduckgo_lite", query, fetched);
    }
    let html = fetched
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let results = parse_duckduckgo_lite_results(query, html, max_results);
    json!({
        "ok": !results.is_empty(),
        "query": query,
        "source": "duckduckgo_lite",
        "results": results,
        "error": if results.is_empty() { "empty_results" } else { "" }
    })
}

async fn duckduckgo_html(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    let locale = web_locale(query);
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://html.duckduckgo.com/html/",
        &[("q", query), ("kl", locale.duckduckgo_kl)],
    ) else {
        return json!({ "ok": false, "query": query, "source": "duckduckgo_html", "error": "invalid_search_url" });
    };
    let fetched = fetch_text(client, url, "text/html,application/xhtml+xml").await;
    if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
        return merge_search_error("duckduckgo_html", query, fetched);
    }
    let html = fetched
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let results = parse_duckduckgo_results(query, html, max_results);
    json!({
        "ok": !results.is_empty(),
        "query": query,
        "source": "duckduckgo_html",
        "results": results,
        "error": if results.is_empty() { "empty_results" } else { "" }
    })
}

async fn duckduckgo_instant_answer(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Value {
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://api.duckduckgo.com/",
        &[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ],
    ) else {
        return json!({ "ok": false, "query": query, "source": "duckduckgo_instant_answer", "error": "invalid_search_url" });
    };
    let response = match client
        .get(url)
        .header(reqwest::header::USER_AGENT, user_agent())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return json!({ "ok": false, "query": query, "source": "duckduckgo_instant_answer", "error": "request_failed", "message": request_error_message(&error) });
        }
    };
    let status = response.status().as_u16();
    let (bytes, byte_truncated) = read_limited_response_bytes(response).await;
    if byte_truncated {
        return json!({
            "ok": false,
            "query": query,
            "source": "duckduckgo_instant_answer",
            "status": status,
            "error": "search_response_too_large",
            "bytes": bytes.len()
        });
    }
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|_| json!({}));
    let mut results = Vec::new();

    let abstract_text = payload
        .get("AbstractText")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if let Some(text) = abstract_text {
        let text = clean_visible_text(text);
        results.push(json!({
            "title": clean_visible_text(payload.get("Heading").and_then(Value::as_str).unwrap_or(query)),
            "url": payload.get("AbstractURL").and_then(Value::as_str).unwrap_or(""),
            "snippet": truncate_chars(&text, 1200),
            "query": query,
            "source": "abstract"
        }));
    }
    if let Some(answer) = payload
        .get("Answer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let answer = clean_visible_text(answer);
        results.push(json!({
            "title": "Answer",
            "url": payload.get("AnswerType").and_then(Value::as_str).unwrap_or(""),
            "snippet": truncate_chars(&answer, 1200),
            "query": query,
            "source": "answer"
        }));
    }
    collect_duckduckgo_related(
        query,
        payload.get("RelatedTopics"),
        &mut results,
        max_results,
    );
    results.truncate(max_results);

    json!({
        "ok": (200..400).contains(&status),
        "query": query,
        "status": status,
        "source": "duckduckgo_instant_answer",
        "results": results,
        "truncated": false,
        "bytes": bytes.len()
    })
}

fn merge_search_error(source: &str, query: &str, error: Value) -> Value {
    json!({
        "ok": false,
        "query": query,
        "source": source,
        "results": [],
        "error": error.get("error").and_then(Value::as_str).unwrap_or("request_failed"),
        "status": error.get("status").and_then(Value::as_u64),
        "message": error.get("message").and_then(Value::as_str).unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bing_html_results() {
        let html = r#"
            <html><body>
              <li class="b_algo">
                <h2><a href="https://example.com/result">Example &#8212; Result</a></h2>
                <div class="b_caption"><p>Useful&nbsp;snippet &amp;#187; CodeSeeX.</p></div>
              </li>
            </body></html>
        "#;
        let results = parse_bing_results("CodeSeeX", html, 5, "bing_html");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://example.com/result");
        assert_eq!(results[0]["title"], "Example — Result");
        assert_eq!(results[0]["snippet"], "Useful snippet » CodeSeeX.");
        assert!(results[0]["id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("cand_"));
    }

    #[test]
    fn parses_bing_html_results_with_unquoted_data_attributes() {
        let html = r#"
            <ol id="b_results">
              <li class="b_algo" data-id iid=SERP.5159>
                <h2 class=""><a target="_blank" href="https://example.com/current" h="ID=SERP,5102.2">Current Result</a></h2>
                <div class="b_caption"><p class="b_lineclamp2">Current Bing HTML snippet.</p></div>
              </li>
            </ol>
        "#;
        let results = parse_bing_results("Current Bing HTML", html, 5, "bing_html");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://example.com/current");
        assert_eq!(results[0]["title"], "Current Result");
        assert_eq!(results[0]["snippet"], "Current Bing HTML snippet.");
    }

    #[test]
    fn parses_bing_rich_result_blocks() {
        let html = r#"
            <html><body>
              <div class="b_ans">
                <a href="https://example.com/weather/current">Zhongshan weather today</a>
                <span class="b_focusTextLarge">Heavy rain, 29 / 23 C</span>
                <div class="b_secondaryText">Updated at 08:00 with current conditions.</div>
              </div>
            </body></html>
        "#;
        let results = parse_bing_results("Zhongshan weather today", html, 5, "bing_html");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://example.com/weather/current");
        assert_eq!(results[0]["title"], "Zhongshan weather today");
        assert!(results[0]["snippet"]
            .as_str()
            .unwrap_or_default()
            .contains("Heavy rain"));
    }

    #[test]
    fn parses_bing_plain_link_fallbacks() {
        let html = r#"
            <html><body>
              <main>
                <a href="https://example.com/docs/release">Project release status</a>
                <p>The project release status page lists the latest stable version.</p>
              </main>
            </body></html>
        "#;
        let results = parse_bing_results("Project release status latest", html, 5, "bing_html");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://example.com/docs/release");
        assert_eq!(results[0]["title"], "Project release status");
    }

    #[test]
    fn bing_plain_link_fallback_prefers_result_over_serp_chrome() {
        let html = r#"
            <html><body>
              <header>
                <a href="https://www.bing.com/search?q=project+release+status">Search</a>
                <a href="https://www.bing.com/images/search?q=project+release+status">Images</a>
                <a href="https://privacy.microsoft.com/privacystatement">Privacy</a>
              </header>
              <main>
                <a href="https://example.com/docs/release">Project release status</a>
                <p>The latest stable release status is listed with changelog notes.</p>
              </main>
            </body></html>
        "#;
        let results = parse_bing_results("Project release status latest", html, 5, "bing_html");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://example.com/docs/release");
    }

    #[test]
    fn source_ranking_moves_unreachable_sources_after_reachable_sources() {
        let ranked = ranked_sources_from_health(&[
            SearchSourceHealth {
                source: SearchSource::DuckDuckGoLite,
                reachable: false,
                latency_ms: Some(3000),
                status: None,
                error: Some("probe_timeout".to_owned()),
            },
            SearchSourceHealth {
                source: SearchSource::BingHtml,
                reachable: true,
                latency_ms: Some(580),
                status: Some(200),
                error: None,
            },
        ]);

        assert_eq!(ranked[0], SearchSource::BingHtml);
        assert!(
            ranked
                .iter()
                .position(|source| *source == SearchSource::DuckDuckGoLite)
                .unwrap()
                > ranked
                    .iter()
                    .position(|source| *source == SearchSource::BingHtml)
                    .unwrap()
        );
    }

    #[test]
    fn source_ranking_keeps_slow_reachable_sources_before_unreachable_sources() {
        let ranked = ranked_sources_from_health(&[
            SearchSourceHealth {
                source: SearchSource::DuckDuckGoHtml,
                reachable: true,
                latency_ms: Some(9000),
                status: Some(200),
                error: None,
            },
            SearchSourceHealth {
                source: SearchSource::BraveHtml,
                reachable: false,
                latency_ms: Some(3000),
                status: None,
                error: Some("probe_timeout".to_owned()),
            },
        ]);

        assert!(
            ranked
                .iter()
                .position(|source| *source == SearchSource::DuckDuckGoHtml)
                .unwrap()
                < ranked
                    .iter()
                    .position(|source| *source == SearchSource::BraveHtml)
                    .unwrap()
        );
    }

    #[test]
    fn unprobed_plan_uses_all_quality_sources() {
        let plan = SearchPlan::unprobed("system".to_owned());
        assert_eq!(
            plan.source_order_names(),
            vec![
                "bing_html",
                "brave_html",
                "duckduckgo_lite",
                "duckduckgo_html",
                "duckduckgo_instant_answer"
            ]
        );
        assert!(plan.health_diagnostic().is_empty());
    }

    #[test]
    fn plan_splits_primary_and_deprioritized_sources() {
        let plan = SearchPlan::from_snapshot(
            SearchHealthSnapshot {
                cache_key: "none".to_owned(),
                checked_at: Instant::now(),
                sources: vec![
                    SearchSourceHealth {
                        source: SearchSource::DuckDuckGoLite,
                        reachable: false,
                        latency_ms: Some(3000),
                        status: None,
                        error: Some("probe_timeout".to_owned()),
                    },
                    SearchSourceHealth {
                        source: SearchSource::BingHtml,
                        reachable: true,
                        latency_ms: Some(200),
                        status: Some(200),
                        error: None,
                    },
                    SearchSourceHealth {
                        source: SearchSource::BraveHtml,
                        reachable: true,
                        latency_ms: Some(1200),
                        status: Some(200),
                        error: None,
                    },
                    SearchSourceHealth {
                        source: SearchSource::DuckDuckGoHtml,
                        reachable: true,
                        latency_ms: Some(1500),
                        status: Some(200),
                        error: None,
                    },
                    SearchSourceHealth {
                        source: SearchSource::DuckDuckGoInstantAnswer,
                        reachable: true,
                        latency_ms: Some(1600),
                        status: Some(200),
                        error: None,
                    },
                ],
            },
            "cached_probe",
        );

        assert_eq!(
            plan.primary_sources(),
            vec![
                SearchSource::BingHtml,
                SearchSource::BraveHtml,
                SearchSource::DuckDuckGoHtml,
                SearchSource::DuckDuckGoInstantAnswer
            ]
        );
        assert_eq!(
            plan.deprioritized_sources(),
            vec![SearchSource::DuckDuckGoLite]
        );
        assert_eq!(plan.plan_source, "cached_probe");
    }

    #[test]
    fn empty_success_is_kept_as_fallback_diagnostic() {
        let mut collected = Vec::new();
        let mut fallback_errors = Vec::new();
        let mut sources_attempted = Vec::new();
        collect_source_results(
            vec![json!({
                "ok": true,
                "source": "bing_html",
                "results": []
            })],
            &mut collected,
            &mut fallback_errors,
            &mut sources_attempted,
            &mut Vec::new(),
        );

        assert_eq!(sources_attempted, vec!["bing_html"]);
        assert!(collected.is_empty());
        assert_eq!(fallback_errors.len(), 1);
        assert_eq!(fallback_errors[0]["source"], "bing_html");
        assert_eq!(fallback_errors[0]["error"], "empty_results");
    }

    #[test]
    fn filtered_low_confidence_result_is_diagnosed() {
        let mut collected = Vec::new();
        let mut fallback_errors = Vec::new();
        let mut sources_attempted = Vec::new();
        let mut source_diagnostics = Vec::new();
        collect_source_results(
            vec![json!({
                "ok": true,
                "source": "bing_html",
                "results": [{
                    "url": "https://example.com/noise",
                    "title": "Noise",
                    "query": "中山天气",
                    "source": "bing_html",
                    "score": 0.08
                }]
            })],
            &mut collected,
            &mut fallback_errors,
            &mut sources_attempted,
            &mut source_diagnostics,
        );

        assert_eq!(collected.len(), 1);
        assert_eq!(source_diagnostics[0]["error"], "filtered_low_confidence");
        assert_eq!(source_diagnostics[0]["result_count"], 1);
        assert_eq!(source_diagnostics[0]["usable_result_count"], 0);
        assert_eq!(fallback_errors[0]["error"], "filtered_low_confidence");
    }

    #[test]
    fn low_score_results_do_not_stop_fallback() {
        let results = vec![json!({
            "url": "https://example.com/unrelated",
            "title": "Example",
            "snippet": "",
            "query": "中山天气",
            "source": "bing_html",
            "score": 0.08
        })];

        assert_eq!(usable_result_count(&results), 0);
    }

    #[test]
    fn low_confidence_fallback_keeps_bounded_exhausted_results() {
        let results = low_confidence_fallback_results(
            vec![
                json!({
                    "url": "https://weak.example.com/weather",
                    "title": "Weak",
                    "snippet": "",
                    "query": "中山天气",
                    "source": "bing_html",
                    "score": 0.08
                }),
                json!({
                    "url": "https://noise.example.com/weather",
                    "title": "Noise",
                    "snippet": "",
                    "query": "中山天气",
                    "source": "bing_html",
                    "score": 0.03
                }),
                json!({
                    "url": "https://better.example.com/weather",
                    "title": "Better",
                    "snippet": "",
                    "query": "中山天气",
                    "source": "bing_html",
                    "score": 0.12
                }),
            ],
            5,
        );

        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["url"], "https://better.example.com/weather");
        assert_eq!(results[1]["url"], "https://weak.example.com/weather");
    }

    #[test]
    fn search_source_set_stays_quality_only_without_region_specific_fallbacks() {
        let names = SearchSource::ALL
            .iter()
            .map(|source| source.name())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "bing_html",
                "brave_html",
                "duckduckgo_lite",
                "duckduckgo_html",
                "duckduckgo_instant_answer"
            ]
        );
        assert!(!names.iter().any(|name| name.contains("cn_bing")));
        assert!(!names.iter().any(|name| name.contains("sogou")));
        assert!(!names.iter().any(|name| name.contains("360")));
        assert!(!names.iter().any(|name| name.contains("baidu")));
    }

    #[test]
    fn parses_duckduckgo_lite_results() {
        let html = r#"
            <html><body>
              <a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fpeps.python.org%2Fpep%2D0745%2F&rut=abc">
                PEP 745 - Python 3.14 Release Schedule | peps.python.org
              </a>
              <td class="result-snippet">Python 3.14 release schedule with beta and final dates.</td>
            </body></html>
        "#;
        let results = parse_duckduckgo_lite_results("Python 3.14 release schedule", html, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://peps.python.org/pep-0745/");
        assert!(results[0]["title"]
            .as_str()
            .unwrap_or_default()
            .contains("PEP 745"));
        assert!(results[0]["score"].as_f64().unwrap_or_default() >= 0.16);
    }

    #[test]
    fn parses_brave_results() {
        let html = r#"
            <html><body>
              <div class="snippet" data-type="web">
                <a href="https://peps.python.org/pep-0745/" class="result-header">
                  <div class="title">PEP 745 - Python 3.14 Release Schedule | peps.python.org</div>
                </a>
                <div class="generic-snippet">
                  Python 3.14 release schedule with final release dates and bugfix cadence.
                </div>
              </div>
            </body></html>
        "#;
        let results = parse_brave_results("Python 3.14 release schedule", html, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://peps.python.org/pep-0745/");
        assert!(results[0]["title"]
            .as_str()
            .unwrap_or_default()
            .contains("PEP 745"));
        assert!(results[0]["score"].as_f64().unwrap_or_default() >= 0.16);
    }
}
