use regex::Regex;
use serde_json::{json, Value};

use super::candidates::{average_score, dedupe_results, make_search_result, retain_usable_results};
use super::extract::{
    clean_visible_text, decode_basic_html_entities, strip_html_tags, truncate_chars,
};
use super::net::{fetch_text, read_limited_response_bytes, request_error_message, user_agent};

pub(super) async fn query(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    let mut fallback_errors = Vec::new();
    let mut collected = Vec::new();

    let results_by_source = tokio::join!(
        bing_html(client, query, max_results),
        brave_html(client, query, max_results),
        duckduckgo_lite(client, query, max_results),
        duckduckgo_html(client, query, max_results),
        duckduckgo_instant_answer(client, query, max_results)
    );
    for result in [
        results_by_source.0,
        results_by_source.1,
        results_by_source.2,
        results_by_source.3,
        results_by_source.4,
    ] {
        if let Some(results) = result.get("results").and_then(Value::as_array) {
            collected.extend(results.iter().cloned());
        }
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            fallback_errors.push(json!({
                "source": result.get("source").and_then(Value::as_str).unwrap_or("unknown"),
                "error": result.get("error").and_then(Value::as_str).unwrap_or("empty_results"),
                "status": result.get("status").and_then(Value::as_u64),
                "message": result.get("message").and_then(Value::as_str).unwrap_or_default()
            }));
        }
    }

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
    let quality = average_score(&results);

    json!({
        "ok": !results.is_empty(),
        "stage": "search",
        "mode": "search",
        "query": query,
        "source": if results.is_empty() { "none" } else { "multi_source_html" },
        "sources_attempted": ["bing_html", "brave_html", "duckduckgo_lite", "duckduckgo_html", "duckduckgo_instant_answer"],
        "results": results.clone(),
        "candidates": results.clone(),
        "candidate_count": results.len(),
        "quality": quality,
        "low_confidence": results.is_empty() || quality < 0.24,
        "fallback_errors": fallback_errors
    })
}

async fn bing_html(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    let locale = web_locale(query);
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://www.bing.com/search",
        &[
            ("q", query),
            ("setlang", locale.bing_setlang),
            ("mkt", locale.bing_market),
            ("cc", locale.bing_country),
            ("ensearch", locale.bing_english_search),
        ],
    ) else {
        return json!({ "ok": false, "query": query, "source": "bing_html", "error": "invalid_search_url" });
    };
    let fetched = fetch_text(client, url, "text/html,application/xhtml+xml").await;
    if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
        return merge_search_error("bing_html", query, fetched);
    }
    let html = fetched
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let results = parse_bing_results(query, html, max_results);
    json!({
        "ok": !results.is_empty(),
        "query": query,
        "source": "bing_html",
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

fn collect_duckduckgo_related(
    query: &str,
    value: Option<&Value>,
    output: &mut Vec<Value>,
    max_results: usize,
) {
    if output.len() >= max_results {
        return;
    }
    let Some(items) = value.and_then(Value::as_array) else {
        return;
    };
    for item in items {
        if output.len() >= max_results {
            return;
        }
        if let Some(topics) = item.get("Topics") {
            collect_duckduckgo_related(query, Some(topics), output, max_results);
            continue;
        }
        let text = item
            .get("Text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let Some(text) = text else {
            continue;
        };
        let text = clean_visible_text(text);
        output.push(json!({
            "title": truncate_chars(&text, 120),
            "url": item.get("FirstURL").and_then(Value::as_str).unwrap_or(""),
            "snippet": truncate_chars(&text, 1200),
            "query": query,
            "source": "related_topic"
        }));
    }
}

fn parse_bing_results(query: &str, html: &str, max_results: usize) -> Vec<Value> {
    let Ok(block_re) = Regex::new(r#"(?is)<li[^>]+class="[^"]*b_algo[^"]*"[^>]*>(.*?)</li>"#)
    else {
        return Vec::new();
    };
    let Ok(link_re) =
        Regex::new(r#"(?is)<h2[^>]*>.*?<a[^>]+href="([^"]+)"[^>]*>(.*?)</a>.*?</h2>"#)
    else {
        return Vec::new();
    };
    let Ok(snippet_re) = Regex::new(
        r#"(?is)<(?:p|div)[^>]+class="[^"]*(?:b_caption|b_snippet|b_lineclamp)[^"]*"[^>]*>(.*?)</(?:p|div)>"#,
    ) else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for block in block_re.captures_iter(html) {
        if results.len() >= max_results {
            break;
        }
        let block = block.get(1).map(|value| value.as_str()).unwrap_or_default();
        let Some(link) = link_re.captures(block) else {
            continue;
        };
        let url = decode_basic_html_entities(link.get(1).map(|value| value.as_str()).unwrap_or(""));
        let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
        let snippet = snippet_re
            .captures(block)
            .and_then(|caps| caps.get(1).map(|value| strip_html_tags(value.as_str())))
            .unwrap_or_default();
        if let Some(item) =
            make_search_result(query, &title, &url, &snippet, "bing_html", results.len())
        {
            results.push(item);
        }
    }
    results
}

fn parse_duckduckgo_results(query: &str, html: &str, max_results: usize) -> Vec<Value> {
    let Ok(link_re) =
        Regex::new(r#"(?is)<a[^>]+class="[^"]*result__a[^"]*"[^>]+href="([^"]+)"[^>]*>(.*?)</a>"#)
    else {
        return Vec::new();
    };
    let Ok(snippet_re) = Regex::new(
        r#"(?is)<a[^>]+class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</a>|<div[^>]+class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</div>"#,
    ) else {
        return Vec::new();
    };
    let snippets = snippet_re
        .captures_iter(html)
        .map(|caps| {
            caps.get(1)
                .or_else(|| caps.get(2))
                .map(|value| strip_html_tags(value.as_str()))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let mut results = Vec::new();
    for (index, link) in link_re.captures_iter(html).enumerate() {
        if results.len() >= max_results {
            break;
        }
        let raw_url = link.get(1).map(|value| value.as_str()).unwrap_or_default();
        let url = normalize_duckduckgo_result_url(raw_url);
        let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
        let snippet = snippets.get(index).cloned().unwrap_or_default();
        if let Some(item) = make_search_result(
            query,
            &title,
            &url,
            &snippet,
            "duckduckgo_html",
            results.len(),
        ) {
            results.push(item);
        }
    }
    results
}

fn parse_brave_results(query: &str, html: &str, max_results: usize) -> Vec<Value> {
    let Ok(link_re) = Regex::new(r#"(?is)<a[^>]+href="(https?://[^"]+)"[^>]*>(.*?)</a>"#) else {
        return Vec::new();
    };
    let matches = link_re.captures_iter(html).collect::<Vec<_>>();
    let mut results = Vec::new();
    for (index, link) in matches.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }
        let url = decode_basic_html_entities(link.get(1).map(|value| value.as_str()).unwrap_or(""));
        if url.contains("search.brave.com") || url.contains("imgs.search.brave.com") {
            continue;
        }
        let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
        let title = title.trim();
        if title.is_empty() {
            continue;
        }
        let snippet_start = link.get(0).map(|value| value.end()).unwrap_or_default();
        let snippet_end = matches
            .get(index + 1)
            .and_then(|next| next.get(0).map(|value| value.start()))
            .unwrap_or(html.len())
            .min(snippet_start.saturating_add(1800));
        let snippet = html
            .get(snippet_start..snippet_end)
            .map(strip_html_tags)
            .unwrap_or_default();
        if let Some(item) =
            make_search_result(query, title, &url, &snippet, "brave_html", results.len())
        {
            results.push(item);
        }
    }
    results
}

fn normalize_duckduckgo_result_url(value: &str) -> String {
    let raw = decode_basic_html_entities(value);
    let raw = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw
    };
    let Ok(url) = reqwest::Url::parse(&raw)
        .or_else(|_| reqwest::Url::parse(&format!("https://duckduckgo.com{raw}")))
    else {
        return raw;
    };
    url.query_pairs()
        .find(|(key, _)| key == "uddg")
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| url.to_string())
}

fn parse_duckduckgo_lite_results(query: &str, html: &str, max_results: usize) -> Vec<Value> {
    let Ok(link_re) = Regex::new(r#"(?is)<a[^>]+href="([^"]+)"[^>]*>(.*?)</a>"#) else {
        return Vec::new();
    };
    let matches = link_re.captures_iter(html).collect::<Vec<_>>();
    let mut results = Vec::new();
    for (index, link) in matches.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }
        let raw_url = link.get(1).map(|value| value.as_str()).unwrap_or_default();
        if !raw_url.contains("uddg=") {
            continue;
        }
        let url = normalize_duckduckgo_result_url(raw_url);
        let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
        let title = title.trim();
        if title.is_empty() {
            continue;
        }
        let snippet_start = link.get(0).map(|value| value.end()).unwrap_or_default();
        let snippet_end = matches
            .get(index + 1)
            .and_then(|next| next.get(0).map(|value| value.start()))
            .unwrap_or(html.len())
            .min(snippet_start.saturating_add(1600));
        let snippet = html
            .get(snippet_start..snippet_end)
            .map(strip_html_tags)
            .unwrap_or_default();
        if let Some(item) = make_search_result(
            query,
            title,
            &url,
            &snippet,
            "duckduckgo_lite",
            results.len(),
        ) {
            results.push(item);
        }
    }
    results
}

struct WebLocale {
    bing_market: &'static str,
    bing_setlang: &'static str,
    bing_country: &'static str,
    bing_english_search: &'static str,
    duckduckgo_kl: &'static str,
}

fn web_locale(query: &str) -> WebLocale {
    if query
        .chars()
        .any(|ch| ('\u{3400}'..='\u{9fff}').contains(&ch))
    {
        return WebLocale {
            bing_market: "zh-CN",
            bing_setlang: "zh-CN",
            bing_country: "CN",
            bing_english_search: "0",
            duckduckgo_kl: "cn-zh",
        };
    }
    WebLocale {
        bing_market: "en-US",
        bing_setlang: "en-US",
        bing_country: "US",
        bing_english_search: "1",
        duckduckgo_kl: "us-en",
    }
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
        let results = parse_bing_results("CodeSeeX", html, 5);

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
        assert!(results[0]["score"].as_f64().unwrap_or_default() >= 0.24);
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
        assert!(results[0]["score"].as_f64().unwrap_or_default() >= 0.24);
    }
}
