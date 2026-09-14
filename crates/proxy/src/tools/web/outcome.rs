//! Orchestration: runs the engines, ranks the candidates, opens evidence pages,
//! and serialises the single result contract.

use codeseex_core::NetworkProxyMode;
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::extract::{select_blocks, BlockKind, ExtractedDocument, SelectedBlock};
use super::fetch::{fetch_page, FetchedPage};
use super::ids::{lookup_from_messages, resolve_open_ids};
use super::net::web_client;
use super::rank::{average_relevance, meaningful_terms, rank, Candidate};
use super::request::{WebMode, WebRequest};
use super::sanitize::sanitize_value;
use super::sources::{
    plan, proxy_cache_key, record_outcome, search_source, source_outcome_detail, RawHit,
    SourceOutcome,
};
use super::{
    AUTO_OPEN_TIMEOUT_SECS, EVIDENCE_BUDGET_CHARS, EVIDENCE_TARGETS, MAX_OPEN_TARGETS, MAX_RESULTS,
};

/// How long a query's ranked candidates stay reusable.
const CACHE_TTL: Duration = Duration::from_secs(600);
const CACHE_MAX_ENTRIES: usize = 128;
/// Link targets kept per evidence page.
const MAX_EVIDENCE_LINKS: usize = 12;

#[derive(Clone)]
struct CacheEntry {
    stored_at: Instant,
    candidates: Vec<Candidate>,
}

static SEARCH_CACHE: OnceLock<Mutex<BTreeMap<String, CacheEntry>>> = OnceLock::new();

fn search_cache() -> &'static Mutex<BTreeMap<String, CacheEntry>> {
    SEARCH_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

async fn cache_get(key: &str) -> Option<Vec<Candidate>> {
    let cache = search_cache().lock().await;
    cache
        .get(key)
        .filter(|entry| entry.stored_at.elapsed() < CACHE_TTL)
        .map(|entry| entry.candidates.clone())
}

async fn cache_put(key: String, candidates: Vec<Candidate>) {
    let mut cache = search_cache().lock().await;
    cache.retain(|_, entry| entry.stored_at.elapsed() < CACHE_TTL);
    if cache.len() >= CACHE_MAX_ENTRIES {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.stored_at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(
        key,
        CacheEntry {
            stored_at: Instant::now(),
            candidates,
        },
    );
}

pub(super) async fn run(
    proxy_mode: NetworkProxyMode,
    request: WebRequest,
    messages: &[Value],
) -> Value {
    let outcome = match request.mode {
        WebMode::Open => run_open(proxy_mode, &request, messages).await,
        WebMode::Search => run_search(proxy_mode, &request).await,
    };
    payload_guard(outcome)
}

/// The last thing that touches a web result before it can reach the model.
///
/// Block text is already sanitized where it is produced; this pass exists so a
/// future producer cannot leak a resource payload unnoticed. `payload_guard`
/// in the result means the primary filter missed something, so the flag is a
/// defect signal worth logging rather than a normal outcome.
fn payload_guard(outcome: Value) -> Value {
    let (mut outcome, changed) = sanitize_value(outcome);
    if changed {
        if let Some(object) = outcome.as_object_mut() {
            object.insert("payload_guard".to_owned(), Value::Bool(true));
        }
    }
    outcome
}

async fn run_open(proxy_mode: NetworkProxyMode, request: &WebRequest, messages: &[Value]) -> Value {
    let mut targets = request.open_urls.clone();
    let lookup = lookup_from_messages(messages);
    let unresolved = resolve_open_ids(&request.open_ids, &lookup, &mut targets);
    if targets.is_empty() {
        return json!({
            "ok": false,
            "stage": "open",
            "mode": "open",
            "error": if unresolved.is_empty() { "missing_url" } else { "unknown_candidate_ids" },
            "message": "web_search mode=open requires url/urls/open_urls or resolvable open_ids.",
            "open_ids": request.open_ids,
            "unresolved_ids": unresolved
        });
    }
    let targets = targets
        .into_iter()
        .take(MAX_OPEN_TARGETS)
        .collect::<Vec<_>>();
    let pages = join_all(targets.iter().map(|url| fetch_page(proxy_mode, url))).await;
    let terms = meaningful_terms(&request.queries.join(" "));

    let mut opened = Vec::new();
    let mut evidence = Vec::new();
    let mut errors = Vec::new();
    for page in &pages {
        if !page.ok() {
            errors.push(page.diagnostic());
            continue;
        }
        opened.push(opened_summary(page));
        if let Some(item) = evidence_item(page, &terms, "open") {
            evidence.push(item);
        }
    }
    let opened_count = opened.len();
    let evidence_count = evidence.len();
    json!({
        "ok": opened_count > 0,
        "stage": "open",
        "mode": "open",
        "open_urls": targets,
        "open_ids": request.open_ids,
        "unresolved_ids": unresolved,
        "opened": opened,
        "opened_count": opened_count,
        "evidence": evidence,
        "evidence_count": evidence_count,
        "errors": errors,
        "next_action": "Answer from evidence when it contains the needed content. Only open another URL if a specific page is still missing.",
        "truncated": targets.len() < request.open_urls.len() + request.open_ids.len()
    })
}

async fn run_search(proxy_mode: NetworkProxyMode, request: &WebRequest) -> Value {
    let queries = request.queries.clone();
    if queries.is_empty() {
        return json!({
            "ok": false,
            "stage": "search",
            "mode": "search",
            "error": "missing_query",
            "message": "web_search requires query/search_query/queries for mode=search."
        });
    }
    let client = web_client(proxy_mode);
    let proxy_key = proxy_cache_key(proxy_mode);
    let search_plan = plan(&client, &proxy_key).await;
    let max_results = request.max_results.min(MAX_RESULTS);

    let mut per_query = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut all_outcomes: Vec<SourceOutcome> = Vec::new();
    let mut low_confidence_fallback = false;
    let mut cache_hits = 0usize;
    for query in &queries {
        let cache_key = format!("{proxy_key}|{max_results}|{}", query.to_lowercase());
        if let Some(cached) = cache_get(&cache_key).await {
            cache_hits += 1;
            for candidate in cached {
                if !candidates
                    .iter()
                    .any(|existing| existing.url == candidate.url)
                {
                    candidates.push(candidate);
                }
            }
            per_query.push(json!({
                "query": query,
                "candidate_count": candidates.len(),
                "cached": true
            }));
            continue;
        }
        let outcomes = join_all(
            search_plan
                .ordered
                .iter()
                .copied()
                .map(|source| search_source(&client, source, query, max_results)),
        )
        .await;
        let mut hits: Vec<RawHit> = Vec::new();
        for outcome in outcomes {
            record_outcome(&proxy_key, &outcome).await;
            hits.extend(outcome.hits.iter().cloned());
            all_outcomes.push(outcome);
        }
        let ranked = rank(query, &hits, max_results);
        low_confidence_fallback |= ranked.low_confidence_fallback;
        if !ranked.candidates.is_empty() {
            cache_put(cache_key, ranked.candidates.clone()).await;
        }
        // Merge queries in order, de-duplicating by URL so the second query can
        // add new candidates but never reshuffle or repeat the first query's.
        for candidate in ranked.candidates {
            if !candidates
                .iter()
                .any(|existing| existing.url == candidate.url)
            {
                candidates.push(candidate);
            }
        }
        per_query.push(json!({
            "query": query,
            "candidate_count": candidates.len(),
            "cached": false
        }));
    }
    candidates.truncate(max_results);
    let quality = average_relevance(&candidates);
    let low_confidence = candidates.is_empty() || quality < 0.24;
    let terms = meaningful_terms(&queries.join(" "));

    let evidence_urls = candidates
        .iter()
        .take(EVIDENCE_TARGETS)
        .map(|candidate| candidate.url.clone())
        .collect::<Vec<_>>();
    let evidence = open_evidence(proxy_mode, &evidence_urls, &candidates, &terms).await;
    let source_diagnostics = all_outcomes
        .iter()
        .map(source_outcome_detail)
        .collect::<Vec<_>>();

    let sources_attempted = all_outcomes
        .iter()
        .filter(|outcome| !outcome.hits.is_empty())
        .map(|outcome| outcome.source.name().to_owned())
        .collect::<Vec<_>>();
    let skipped = search_plan
        .skipped
        .iter()
        .map(|source| source.name())
        .collect::<Vec<_>>();
    let degraded = if cache_hits == queries.len() {
        false
    } else {
        !search_plan.skipped.is_empty()
            || all_outcomes.iter().any(|outcome| outcome.error.is_some())
    };

    json!({
        "ok": !candidates.is_empty(),
        "stage": "search",
        "mode": "search",
        "queries": queries,
        "candidate_count": candidates.len(),
        "candidates": candidates.iter().map(candidate_json).collect::<Vec<_>>(),
        "evidence": evidence,
        "evidence_count": evidence.len(),
        "sources": source_diagnostics,
        "sources_attempted": sources_attempted,
        "sources_skipped": skipped,
        "quality": {
            "score": quality,
            "low_confidence": low_confidence,
            "low_confidence_fallback": low_confidence_fallback,
            "degraded": degraded
        },
        "per_query": per_query,
        "next_action": "Answer from the evidence when it is sufficient. Do not repeat the same search; open a specific candidate only when its full text is needed.",
        "truncated": false
    })
}

async fn open_evidence(
    proxy_mode: NetworkProxyMode,
    urls: &[String],
    candidates: &[Candidate],
    terms: &[String],
) -> Vec<Value> {
    if urls.is_empty() {
        return Vec::new();
    }
    let pages = match tokio::time::timeout(
        std::time::Duration::from_secs(AUTO_OPEN_TIMEOUT_SECS),
        join_all(urls.iter().map(|url| fetch_page(proxy_mode, url))),
    )
    .await
    {
        Ok(pages) => pages,
        Err(_) => return Vec::new(),
    };
    let mut evidence = Vec::new();
    for page in pages {
        if !page.ok() {
            continue;
        }
        // Reuse the ranked candidate's id so `open_ids` keeps working.
        let id = candidates
            .iter()
            .find(|candidate| candidate.url == page.url || candidate.url == page.requested_url)
            .map(|candidate| candidate.id.clone());
        let mut item = match evidence_item(&page, terms, "search") {
            Some(item) => item,
            None => continue,
        };
        if let Some(id) = id {
            item["id"] = Value::String(id);
        }
        evidence.push(item);
    }
    evidence
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

fn opened_summary(page: &FetchedPage) -> Value {
    json!({
        "id": Value::Null,
        "title": page.title(),
        "url": page.url,
        "status": page.status,
        "chars": page
            .document
            .as_ref()
            .map(|document| document.stats.body_chars)
            .unwrap_or(0),
        "truncated": page.truncated
    })
}

fn evidence_item(page: &FetchedPage, terms: &[String], origin: &str) -> Option<Value> {
    let document = page.document.as_ref()?;
    let selection = select_blocks(document, terms, EVIDENCE_BUDGET_CHARS);
    if selection.blocks.is_empty() {
        return None;
    }
    let links = selection
        .blocks
        .iter()
        .flat_map(|block| block.links.iter().cloned())
        .fold(Vec::new(), |mut acc: Vec<String>, link| {
            if acc.len() < MAX_EVIDENCE_LINKS && !acc.contains(&link) {
                acc.push(link);
            }
            acc
        });
    Some(json!({
        "id": Value::Null,
        "title": page.title(),
        "url": page.url,
        "status": page.status,
        "origin": origin,
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
            "blocks_content": document.stats.blocks_content,
            "code_blocks": selection
                .blocks
                .iter()
                .filter(|block| block.kind == BlockKind::Code)
                .count(),
            "table_blocks": selection
                .blocks
                .iter()
                .filter(|block| block.kind == BlockKind::TableRow)
                .count()
        },
        "confidence": confidence(document),
        "truncated": page.truncated
    }))
}

fn confidence(document: &ExtractedDocument) -> &'static str {
    if document.is_low_confidence() {
        "low"
    } else {
        "ok"
    }
}

fn render_blocks(blocks: &[SelectedBlock]) -> String {
    let mut output = String::new();
    let mut last_heading = String::new();
    for block in blocks {
        if block.skipped_blocks > 0 {
            output.push_str(&format!(
                "\n[... omitted {} blocks / {} chars ...]\n",
                block.skipped_blocks, block.skipped_chars
            ));
        }
        if !block.heading_path.is_empty() && block.heading_path != last_heading {
            output.push_str(&format!("\n[{}]\n", block.heading_path));
            last_heading = block.heading_path.clone();
        }
        output.push_str(&block.text);
        output.push('\n');
    }
    output.trim().to_owned()
}

/// The rendered excerpt is the model-facing budget, markers included, so the
/// selection is never partly discarded by a later cap.
fn bound_excerpt(rendered: &str) -> String {
    if crate::tools::web::text_char_count(rendered) <= EVIDENCE_BUDGET_CHARS {
        return rendered.to_owned();
    }
    crate::tools::web::truncate_to_chars(rendered, EVIDENCE_BUDGET_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_guard_catches_a_producer_that_leaked_a_resource() {
        // Stands in for a future producer that forgets to sanitize: the boundary
        // must still strip the payload and raise the defect flag.
        let leaky = json!({
            "ok": true,
            "evidence": [{
                "url": "https://example.com/",
                "excerpt": format!("logo data:image/png;base64,{} tail", "A".repeat(400))
            }]
        });
        let guarded = payload_guard(leaky);

        assert_eq!(guarded["payload_guard"], Value::Bool(true));
        let excerpt = guarded["evidence"][0]["excerpt"].as_str().unwrap();
        assert!(!excerpt.contains("base64,AAAA"));
        assert!(excerpt.contains("tail"));
    }

    #[test]
    fn payload_guard_stays_quiet_for_clean_results() {
        let clean = json!({ "ok": true, "candidates": [{ "url": "https://example.com/" }] });
        let guarded = payload_guard(clean);

        assert!(guarded.get("payload_guard").is_none());
    }

    #[test]
    fn rendered_evidence_marks_omitted_blocks() {
        let blocks = vec![
            SelectedBlock {
                heading_path: "Docs".to_owned(),
                text: "First paragraph.".to_owned(),
                kind: BlockKind::Prose,
                links: Vec::new(),
                skipped_blocks: 0,
                skipped_chars: 0,
            },
            SelectedBlock {
                heading_path: "Docs".to_owned(),
                text: "Late paragraph.".to_owned(),
                kind: BlockKind::Prose,
                links: Vec::new(),
                skipped_blocks: 12,
                skipped_chars: 3_000,
            },
        ];
        let rendered = render_blocks(&blocks);

        assert!(rendered.contains("First paragraph."));
        assert!(rendered.contains("Late paragraph."));
        assert!(rendered.contains("omitted 12 blocks / 3000 chars"));
    }
}
