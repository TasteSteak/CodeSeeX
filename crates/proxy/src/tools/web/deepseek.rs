//! DeepSeek Search and Fetch API backend.
//!
//! The provider endpoints return their own ranked results. CodeSeeX keeps that
//! ordering, normalizes each result into the shared web-search contract, and
//! never falls back silently to the scraped local backend.

use codeseex_core::config::AppConfig;
use codeseex_core::urls::{tools_fetch_url, tools_search_url};
use codeseex_core::NetworkProxyMode;
use futures_util::future::join_all;
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::tools::WebAuthContext;

use super::extract::{markdown_to_document, select_blocks, ExtractedDocument};
use super::ids::{candidate_id_for, lookup_from_messages, resolve_open_ids};
use super::net::{web_client, WEB_REQUEST_TIMEOUT_SECS};
use super::outcome::{bound_excerpt, render_blocks};
use super::rank::{average_relevance, meaningful_terms, Candidate};
use super::request::{WebMode, WebRequest};
use super::safety::normalize_candidate_url;
use super::sanitize::sanitize_prose;
use super::text::{clean_visible_text, truncate_chars};
use super::{EVIDENCE_BUDGET_CHARS, EVIDENCE_TARGETS, MAX_OPEN_TARGETS, MAX_RESULTS};

const MAX_API_BODY_BYTES: usize = 2 * 1024 * 1024;
const QUERY_CHAR_LIMIT: usize = 128;
const ORIGINAL_QUESTION_CHAR_LIMIT: usize = 128;
const RETRY_DELAY: Duration = Duration::from_millis(300);
const OFFICIAL_SOURCE: &str = "deepseek_search";

#[derive(Clone, Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchResult>,
}

#[derive(Clone, Debug, Deserialize)]
struct SearchResult {
    #[serde(default)]
    rerank_score: Option<f64>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    snippet: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    site_name: Option<String>,
    #[serde(default)]
    site_icon: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    authority: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
struct FetchResponse {
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
}

#[derive(Clone, Debug)]
struct SearchQueryOutcome {
    query: String,
    status: Option<u16>,
    latency_ms: u64,
    body_bytes: usize,
    results: Vec<SearchResult>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct RankedOfficialResult {
    candidate: Candidate,
    summary: String,
    site_name: Option<String>,
    site_icon: Option<String>,
    date: Option<String>,
    authority: Option<i64>,
}

#[derive(Debug)]
struct ApiFailure {
    status: Option<u16>,
    message: String,
}

pub(super) async fn execute(
    proxy_mode: NetworkProxyMode,
    config: &AppConfig,
    auth: WebAuthContext<'_>,
    arguments: &Value,
    messages: &[Value],
) -> Value {
    let request = WebRequest::parse(arguments);
    let Some(authorization) = authorization_header(config, auth) else {
        return json!({
            "ok": false,
            "stage": request.mode.as_str(),
            "mode": request.mode.as_str(),
            "backend": "official",
            "error": "official_web_search_credential_missing",
            "message": "The official web-search backend needs a DeepSeek API key. Configure the upstream key in CodeSeeX or select the local backend."
        });
    };
    match request.mode {
        WebMode::Search => run_search(proxy_mode, config, &authorization, &request, messages).await,
        WebMode::Open => run_open(proxy_mode, config, &authorization, &request, messages).await,
    }
}

fn authorization_header(config: &AppConfig, auth: WebAuthContext<'_>) -> Option<String> {
    let managed_key = crate::secrets::upstream_api_key(config);
    crate::upstream::resolve_upstream_authorization(
        &config.upstream,
        crate::upstream::UpstreamAuthRequest {
            inbound: auth.inbound,
            local_access_token: auth.local_access_token,
            managed_key: managed_key.as_deref(),
            passthrough: crate::upstream::UpstreamPassthrough::default(),
        },
        &json!({ "client_metadata": { "name": "web_search" } }),
        &|| {
            codeseex_core::codex_auth::read_deepseek_api_key()
                .or_else(|| codeseex_core::codex_auth::read_codex_auth_api_key(false))
        },
    )
    .header
}

async fn run_search(
    proxy_mode: NetworkProxyMode,
    config: &AppConfig,
    authorization: &str,
    request: &WebRequest,
    messages: &[Value],
) -> Value {
    if request.queries.is_empty() {
        return json!({
            "ok": false,
            "stage": "search",
            "mode": "search",
            "backend": "official",
            "error": "missing_query",
            "message": "web_search requires query/search_query/queries for mode=search."
        });
    }

    let client = web_client(proxy_mode);
    let max_results = request.max_results.clamp(1, MAX_RESULTS);
    let original_question = original_question(messages);
    let outcomes = join_all(request.queries.iter().map(|query| {
        search_query(
            &client,
            config,
            authorization,
            query,
            max_results,
            original_question.as_deref(),
        )
    }))
    .await;

    let (ranked, per_query) = merge_results(&outcomes, max_results);
    let candidates = ranked
        .iter()
        .map(|item| item.candidate.clone())
        .collect::<Vec<_>>();
    let evidence = search_evidence(&ranked);
    let quality = average_relevance(&candidates);
    let any_error = outcomes.iter().any(|outcome| outcome.error.is_some());
    let sources = outcomes
        .iter()
        .map(|outcome| {
            json!({
                "source": OFFICIAL_SOURCE,
                "ok": outcome.error.is_none() && !outcome.results.is_empty(),
                "status": outcome.status,
                "error": outcome.error,
                "result_count": outcome.results.len(),
                "latency_ms": outcome.latency_ms,
                "body_bytes": outcome.body_bytes
            })
        })
        .collect::<Vec<_>>();
    let degraded = any_error && !candidates.is_empty();

    json!({
        "ok": !candidates.is_empty(),
        "stage": "search",
        "mode": "search",
        "backend": "official",
        "queries": request.queries,
        "candidate_count": candidates.len(),
        "candidates": candidates.iter().map(candidate_json).collect::<Vec<_>>(),
        "evidence": evidence,
        "evidence_count": evidence.len(),
        "sources": sources,
        "sources_attempted": if candidates.is_empty() { Vec::<String>::new() } else { vec![OFFICIAL_SOURCE.to_owned()] },
        "sources_skipped": Vec::<String>::new(),
        "quality": {
            "score": quality,
            "low_confidence": candidates.is_empty(),
            "low_confidence_fallback": false,
            "degraded": degraded
        },
        "per_query": per_query,
        "next_action": "Answer from the evidence when it is sufficient. Do not repeat the same search; open a specific candidate only when its full text is needed.",
        "truncated": false
    })
}

async fn search_query(
    client: &reqwest::Client,
    config: &AppConfig,
    authorization: &str,
    query: &str,
    count: usize,
    original_question: Option<&str>,
) -> SearchQueryOutcome {
    let started = Instant::now();
    let query = truncate_chars(query, QUERY_CHAR_LIMIT);
    let mut payload = json!({
        "query": query,
        "count": count.clamp(1, 50),
        "need_content": false
    });
    if let Some(original_question) = original_question {
        payload["ori_question"] = Value::String(original_question.to_owned());
    }
    let url = match tools_search_url(&config.upstream.base_url) {
        Ok(url) => url,
        Err(error) => {
            return SearchQueryOutcome {
                query: query.to_owned(),
                status: None,
                latency_ms: started.elapsed().as_millis() as u64,
                body_bytes: 0,
                results: Vec::new(),
                error: Some(format!("invalid_search_url: {error}")),
            }
        }
    };
    match post_json(client, &url, authorization, &payload).await {
        Ok((status, body_bytes, value)) => match serde_json::from_value::<SearchResponse>(value) {
            Ok(response) => SearchQueryOutcome {
                query: query.to_owned(),
                status: Some(status),
                latency_ms: started.elapsed().as_millis() as u64,
                body_bytes,
                results: response.results,
                error: None,
            },
            Err(error) => SearchQueryOutcome {
                query: query.to_owned(),
                status: Some(status),
                latency_ms: started.elapsed().as_millis() as u64,
                body_bytes,
                results: Vec::new(),
                error: Some(format!("official_search_decode: {error}")),
            },
        },
        Err(failure) => SearchQueryOutcome {
            query: query.to_owned(),
            status: failure.status,
            latency_ms: started.elapsed().as_millis() as u64,
            body_bytes: 0,
            results: Vec::new(),
            error: Some(failure.message),
        },
    }
}

fn merge_results(
    outcomes: &[SearchQueryOutcome],
    max_results: usize,
) -> (Vec<RankedOfficialResult>, Vec<Value>) {
    let mut ranked = Vec::<RankedOfficialResult>::new();
    let mut by_url = BTreeMap::<String, usize>::new();
    let mut per_query = Vec::with_capacity(outcomes.len());

    for outcome in outcomes {
        let mut accepted = 0_usize;
        for (index, result) in outcome.results.iter().enumerate() {
            let Some(url) = normalize_candidate_url(&result.url) else {
                continue;
            };
            let title = clean_visible_text(&result.title);
            let snippet =
                truncate_chars(&sanitize_prose(&clean_visible_text(&result.snippet)), 600);
            let summary_source = if result.summary.trim().is_empty() {
                result.content.as_str()
            } else {
                result.summary.as_str()
            };
            let summary = truncate_chars(
                &sanitize_prose(&clean_visible_text(summary_source)),
                EVIDENCE_BUDGET_CHARS,
            );
            if title.is_empty() && snippet.is_empty() && summary.is_empty() {
                continue;
            }
            let id = candidate_id_for(&url, &title, &outcome.query, OFFICIAL_SOURCE);
            let score = result
                .rerank_score
                .unwrap_or_else(|| 1.0 / (index.saturating_add(1) as f64));
            let relevance = (1.0 / (1.0 + index as f64 * 0.12)).max(0.25);
            let candidate = Candidate {
                id,
                title: if title.is_empty() { url.clone() } else { title },
                url: url.clone(),
                snippet,
                source: OFFICIAL_SOURCE.to_owned(),
                rank: index,
                score,
                relevance,
                matched_sources: 1,
            };
            if let Some(existing) = by_url.get(&url.to_ascii_lowercase()).copied() {
                ranked[existing].candidate.matched_sources =
                    ranked[existing].candidate.matched_sources.saturating_add(1);
                if candidate.score > ranked[existing].candidate.score {
                    ranked[existing].candidate.score = candidate.score;
                }
                if ranked[existing].summary.is_empty() {
                    ranked[existing].summary = summary;
                }
            } else {
                by_url.insert(url.to_ascii_lowercase(), ranked.len());
                ranked.push(RankedOfficialResult {
                    candidate,
                    summary,
                    site_name: result.site_name.clone(),
                    site_icon: result.site_icon.clone(),
                    date: result.date.clone(),
                    authority: result.authority,
                });
            }
            accepted += 1;
        }
        per_query.push(json!({
            "query": outcome.query,
            "candidate_count": accepted,
            "cached": false,
            "error": outcome.error
        }));
    }

    ranked.sort_by(|left, right| {
        right
            .candidate
            .matched_sources
            .cmp(&left.candidate.matched_sources)
            .then_with(|| {
                right
                    .candidate
                    .score
                    .partial_cmp(&left.candidate.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.candidate.rank.cmp(&right.candidate.rank))
    });
    ranked.truncate(max_results);
    (ranked, per_query)
}

fn search_evidence(ranked: &[RankedOfficialResult]) -> Vec<Value> {
    ranked
        .iter()
        .take(EVIDENCE_TARGETS)
        .filter_map(|item| {
            let excerpt = if !item.summary.is_empty() {
                item.summary.clone()
            } else if !item.candidate.snippet.is_empty() {
                item.candidate.snippet.clone()
            } else {
                return None;
            };
            Some(json!({
                "id": item.candidate.id,
                "title": item.candidate.title,
                "url": item.candidate.url,
                "status": 200,
                "origin": "search",
                "excerpt": bound_excerpt(&excerpt),
                "links": [],
                "chars": item.summary.chars().count(),
                "omitted_chars": 0,
                "omitted_blocks": 0,
                "confidence": "ok",
                "truncated": false,
                "site_name": item.site_name,
                "site_icon": item.site_icon,
                "date": item.date,
                "authority": item.authority
            }))
        })
        .collect()
}

async fn run_open(
    proxy_mode: NetworkProxyMode,
    config: &AppConfig,
    authorization: &str,
    request: &WebRequest,
    messages: &[Value],
) -> Value {
    let mut targets = request.open_urls.clone();
    let lookup = lookup_from_messages(messages);
    let unresolved = resolve_open_ids(&request.open_ids, &lookup, &mut targets);
    if targets.is_empty() {
        return json!({
            "ok": false,
            "stage": "open",
            "mode": "open",
            "backend": "official",
            "error": if unresolved.is_empty() { "missing_url" } else { "unknown_candidate_ids" },
            "message": "web_search mode=open requires url/urls/open_urls or resolvable open_ids.",
            "open_ids": request.open_ids,
            "unresolved_ids": unresolved
        });
    }

    let client = web_client(proxy_mode);
    let targets = targets
        .into_iter()
        .take(MAX_OPEN_TARGETS)
        .collect::<Vec<_>>();
    let terms = meaningful_terms(&request.queries.join(" "));
    let pages = join_all(
        targets
            .iter()
            .map(|url| fetch_page(&client, config, authorization, url, &lookup, &terms)),
    )
    .await;

    let mut opened = Vec::new();
    let mut evidence = Vec::new();
    let mut errors = Vec::new();
    for page in pages {
        if let Some(error) = page.error {
            errors.push(error);
            continue;
        }
        opened.push(page.opened);
        if let Some(item) = page.evidence {
            evidence.push(item);
        }
    }
    let opened_count = opened.len();
    let evidence_count = evidence.len();
    json!({
        "ok": opened_count > 0,
        "stage": "open",
        "mode": "open",
        "backend": "official",
        "open_urls": targets,
        "open_ids": request.open_ids,
        "unresolved_ids": unresolved,
        "opened": opened,
        "opened_count": opened_count,
        "evidence": evidence,
        "evidence_count": evidence_count,
        "errors": errors,
        "next_action": "Answer from evidence when it contains the needed content. Only open another URL if a specific page is still missing.",
        "truncated": false
    })
}

struct FetchedOfficialPage {
    opened: Value,
    evidence: Option<Value>,
    error: Option<Value>,
}

async fn fetch_page(
    client: &reqwest::Client,
    config: &AppConfig,
    authorization: &str,
    url: &str,
    lookup: &BTreeMap<String, String>,
    terms: &[String],
) -> FetchedOfficialPage {
    let started = Instant::now();
    let endpoint = match tools_fetch_url(&config.upstream.base_url) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            return FetchedOfficialPage {
                opened: Value::Null,
                evidence: None,
                error: Some(json!({
                    "stage": "open",
                    "code": "invalid_fetch_url",
                    "url": url,
                    "message": error.to_string()
                })),
            }
        }
    };
    let payload = json!({ "url": url });
    match post_json(client, &endpoint, authorization, &payload).await {
        Ok((status, body_bytes, value)) => {
            let response = match serde_json::from_value::<FetchResponse>(value) {
                Ok(response) => response,
                Err(error) => {
                    return FetchedOfficialPage {
                        opened: Value::Null,
                        evidence: None,
                        error: Some(json!({
                            "stage": "open",
                            "code": "official_fetch_decode",
                            "url": url,
                            "status": status,
                            "message": error.to_string()
                        })),
                    }
                }
            };
            let title = if response.title.trim().is_empty() {
                url.to_owned()
            } else {
                clean_visible_text(&response.title)
            };
            let document = markdown_to_document(&response.content, Some(url));
            let id = lookup
                .iter()
                .find(|(_, target)| target.as_str() == url)
                .map(|(id, _)| id.clone());
            let opened = json!({
                "id": id,
                "title": title,
                "url": url,
                "status": status,
                "chars": document.stats.body_chars,
                "truncated": false,
                "latency_ms": started.elapsed().as_millis() as u64,
                "body_bytes": body_bytes
            });
            let evidence = official_evidence(&document, &title, url, terms, id);
            FetchedOfficialPage {
                opened,
                evidence,
                error: None,
            }
        }
        Err(failure) => FetchedOfficialPage {
            opened: Value::Null,
            evidence: None,
            error: Some(json!({
                "stage": "open",
                "code": "official_fetch_failed",
                "url": url,
                "status": failure.status,
                "message": failure.message
            })),
        },
    }
}

fn official_evidence(
    document: &ExtractedDocument,
    title: &str,
    url: &str,
    terms: &[String],
    id: Option<String>,
) -> Option<Value> {
    let selection = select_blocks(document, terms, EVIDENCE_BUDGET_CHARS);
    if selection.blocks.is_empty() {
        return None;
    }
    let links = selection
        .blocks
        .iter()
        .flat_map(|block| block.links.iter().cloned())
        .fold(Vec::new(), |mut acc: Vec<String>, link| {
            if acc.len() < 12 && !acc.contains(&link) {
                acc.push(link);
            }
            acc
        });
    Some(json!({
        "id": id,
        "title": title,
        "url": url,
        "status": 200,
        "origin": "open",
        "excerpt": bound_excerpt(&render_blocks(&selection.blocks)),
        "links": links,
        "chars": selection.kept_chars,
        "omitted_chars": selection.omitted_chars,
        "omitted_blocks": selection.omitted_blocks,
        "page": {
            "body_chars": document.stats.body_chars,
            "content_chars": document.stats.content_chars,
            "content_ratio": (document.stats.content_ratio * 100.0).round() / 100.0,
            "link_ratio": (document.stats.link_ratio * 100.0).round() / 100.0,
            "blocks_total": document.stats.blocks_total,
            "blocks_content": document.stats.blocks_content
        },
        "confidence": if document.is_low_confidence() { "low" } else { "ok" },
        "truncated": false
    }))
}

fn candidate_json(candidate: &Candidate) -> Value {
    json!({
        "id": candidate.id,
        "title": candidate.title,
        "url": candidate.url,
        "snippet": candidate.snippet,
        "source": candidate.source,
        "rank": candidate.rank,
        "score": candidate.score,
        "relevance": candidate.relevance,
        "matched_sources": candidate.matched_sources
    })
}

fn original_question(messages: &[Value]) -> Option<String> {
    for message in messages.iter().rev() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let text = message.get("content").and_then(Value::as_str).or_else(|| {
            message
                .get("content")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find_map(|item| item.get("text").and_then(Value::as_str))
                })
        })?;
        let text = clean_visible_text(text);
        if !text.is_empty() {
            return Some(truncate_chars(&text, ORIGINAL_QUESTION_CHAR_LIMIT));
        }
    }
    None
}

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    authorization: &str,
    payload: &Value,
) -> Result<(u16, usize, Value), ApiFailure> {
    let mut last_failure = None;
    for attempt in 0..2_u8 {
        let response = tokio::time::timeout(
            Duration::from_secs(WEB_REQUEST_TIMEOUT_SECS),
            client
                .post(url)
                .header(AUTHORIZATION, authorization)
                .json(payload)
                .send(),
        )
        .await;
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                last_failure = Some(ApiFailure {
                    status: None,
                    message: format!("official_tools_request_failed: {error}"),
                });
                if attempt == 0 {
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
                return Err(last_failure.expect("failure recorded"));
            }
            Err(_) => {
                last_failure = Some(ApiFailure {
                    status: None,
                    message: "official_tools_request_timeout".to_owned(),
                });
                if attempt == 0 {
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
                return Err(last_failure.expect("failure recorded"));
            }
        };
        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                last_failure = Some(ApiFailure {
                    status: Some(status.as_u16()),
                    message: format!("official_tools_read_failed: {error}"),
                });
                if attempt == 0 && status.is_server_error() {
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
                return Err(last_failure.expect("failure recorded"));
            }
        };
        if bytes.len() > MAX_API_BODY_BYTES {
            return Err(ApiFailure {
                status: Some(status.as_u16()),
                message: format!("official_tools_response_too_large: {} bytes", bytes.len()),
            });
        }
        if status.is_success() {
            return serde_json::from_slice::<Value>(&bytes)
                .map(|value| (status.as_u16(), bytes.len(), value))
                .map_err(|error| ApiFailure {
                    status: Some(status.as_u16()),
                    message: format!("official_tools_decode: {error}"),
                });
        }
        let message = api_error_message(&bytes, status.as_u16());
        let retryable =
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
        last_failure = Some(ApiFailure {
            status: Some(status.as_u16()),
            message,
        });
        if retryable && attempt == 0 {
            tokio::time::sleep(RETRY_DELAY).await;
            continue;
        }
        return Err(last_failure.expect("failure recorded"));
    }
    Err(last_failure.unwrap_or(ApiFailure {
        status: None,
        message: "official_tools_request_failed".to_owned(),
    }))
}

fn api_error_message(bytes: &[u8], status: u16) -> String {
    let message = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).trim().to_owned());
    let message = truncate_chars(&message, 600);
    format!("official_tools_http_{status}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Capture {
        requests: Arc<Mutex<Vec<Value>>>,
        auth: Arc<Mutex<Vec<Option<String>>>>,
    }

    async fn search_handler(
        axum::extract::State(capture): axum::extract::State<Capture>,
        headers: axum::http::HeaderMap,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        capture.requests.lock().expect("requests").push(payload);
        capture.auth.lock().expect("auth").push(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        );
        Json(json!({
            "results": [
                {
                    "rerank_score": 2.4,
                    "title": "DeepSeek Search",
                    "url": "https://example.com/search",
                    "snippet": "Search result snippet.",
                    "summary": "A detailed summary suitable for an agent.",
                    "content": "",
                    "site_name": "Example",
                    "date": "2026-10-21T00:00:00+00:00",
                    "authority": 3
                },
                {
                    "rerank_score": 1.2,
                    "title": "Second result",
                    "url": "https://example.com/second",
                    "snippet": "Second snippet.",
                    "summary": "Second summary.",
                    "content": ""
                }
            ]
        }))
    }

    async fn fetch_handler(Json(payload): Json<Value>) -> Json<Value> {
        Json(json!({
            "title": "Fetched page",
            "content": format!("# Overview\n\nFetched content for {}.\n", payload["url"].as_str().unwrap_or(""))
        }))
    }

    async fn test_server() -> (String, Capture) {
        let capture = Capture::default();
        let app = Router::new()
            .route("/v1/tools/search", post(search_handler))
            .route("/v1/tools/fetch", post(fetch_handler))
            .with_state(capture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });
        (format!("http://{address}"), capture)
    }

    async fn retry_search_handler(
        axum::extract::State(attempts): axum::extract::State<Arc<AtomicUsize>>,
        Json(_payload): Json<Value>,
    ) -> axum::response::Response {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            return (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                Json(json!({
                    "error": {
                        "message": "rate limited",
                        "type": "rate_limit_error",
                        "param": null,
                        "code": "rate_limit_error"
                    }
                })),
            )
                .into_response();
        }
        Json(json!({
            "results": [{
                "title": "Retry succeeded",
                "url": "https://example.com/retry",
                "snippet": "retry evidence",
                "summary": "The retry produced a result."
            }]
        }))
        .into_response()
    }

    fn config(base_url: &str) -> AppConfig {
        let mut config = AppConfig::default();
        config.upstream.base_url = base_url.to_owned();
        config.upstream.credential = codeseex_core::config::UpstreamCredentialSource::Env;
        config.upstream.api_key = Some("test-key".to_owned());
        config
    }

    #[tokio::test]
    async fn search_uses_search_api_and_preserves_provider_order() {
        let (base_url, capture) = test_server().await;
        let result = execute(
            NetworkProxyMode::None,
            &config(&base_url),
            WebAuthContext::default(),
            &json!({ "query": "DeepSeek search API", "max_results": 2 }),
            &[json!({ "role": "user", "content": "Find the DeepSeek docs." })],
        )
        .await;

        assert_eq!(result["ok"], true);
        assert_eq!(result["backend"], "official");
        assert_eq!(result["candidate_count"], 2);
        assert_eq!(result["candidates"][0]["url"], "https://example.com/search");
        assert_eq!(result["evidence"][0]["origin"], "search");
        let requests = capture.requests.lock().expect("requests");
        assert_eq!(requests[0]["query"], "DeepSeek search API");
        assert_eq!(requests[0]["count"], 2);
        assert_eq!(requests[0]["need_content"], false);
        assert_eq!(requests[0]["ori_question"], "Find the DeepSeek docs.");
        assert_eq!(
            capture.auth.lock().expect("auth")[0].as_deref(),
            Some("Bearer test-key")
        );
    }

    #[tokio::test]
    async fn open_uses_fetch_api_and_builds_markdown_evidence() {
        let (base_url, _capture) = test_server().await;
        let result = execute(
            NetworkProxyMode::None,
            &config(&base_url),
            WebAuthContext::default(),
            &json!({ "open_urls": ["https://example.com/article"] }),
            &[],
        )
        .await;

        assert_eq!(result["ok"], true);
        assert_eq!(result["opened_count"], 1);
        assert_eq!(result["evidence_count"], 1);
        assert!(result["evidence"][0]["excerpt"]
            .as_str()
            .unwrap_or_default()
            .contains("Fetched content"));
    }

    #[tokio::test]
    async fn missing_credential_fails_without_local_fallback() {
        let mut config = config("https://api.deepseek.com");
        config.upstream.api_key = None;
        config.upstream.credential = codeseex_core::config::UpstreamCredentialSource::Env;
        let result = execute(
            NetworkProxyMode::None,
            &config,
            WebAuthContext::default(),
            &json!({ "query": "no credential" }),
            &[],
        )
        .await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error"], "official_web_search_credential_missing");
    }

    #[tokio::test]
    async fn search_retries_a_rate_limit_once() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/v1/tools/search", post(retry_search_handler))
            .with_state(attempts.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });

        let result = execute(
            NetworkProxyMode::None,
            &config(&format!("http://{address}")),
            WebAuthContext::default(),
            &json!({ "query": "retry test" }),
            &[],
        )
        .await;

        assert_eq!(result["ok"], true);
        assert_eq!(result["candidate_count"], 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
