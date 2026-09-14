use codeseex_core::context::redact_inline_data_urls;
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

use super::extract::{clean_visible_text, truncate_chars};
use super::safety::normalize_candidate_url;

pub(super) const MIN_USABLE_SEARCH_SCORE: f64 = 0.18;

pub(super) fn lookup_from_messages(messages: &[Value]) -> BTreeMap<String, String> {
    let mut lookup = BTreeMap::new();
    for message in messages {
        collect_candidates_from_value(message, &mut lookup);
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            collect_candidates_from_text(content, &mut lookup);
        }
        if let Some(parts) = message.get("content").and_then(Value::as_array) {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    collect_candidates_from_text(text, &mut lookup);
                }
            }
        }
    }
    lookup
}

pub(super) fn resolve_open_ids(
    ids: &[String],
    lookup: &BTreeMap<String, String>,
    targets: &mut Vec<String>,
) -> Vec<String> {
    let mut unresolved = Vec::new();
    let mut seen = targets
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    for id in ids {
        let key = id.trim().to_ascii_lowercase();
        let Some(url) = lookup.get(&key) else {
            unresolved.push(id.clone());
            continue;
        };
        if seen.insert(url.to_ascii_lowercase()) {
            targets.push(url.clone());
        }
    }
    unresolved
}

pub(super) fn open_result_item(value: &Value) -> Value {
    json!({
        "id": value.get("id").cloned().unwrap_or(Value::Null),
        "title": value.get("title").cloned().unwrap_or(Value::Null),
        "url": value.get("url").cloned().unwrap_or(Value::Null),
        "snippet": value.get("snippet").cloned().unwrap_or(Value::Null),
        "content": value.get("content").cloned().or_else(|| value.get("text").cloned()).unwrap_or(Value::Null),
        "source": "open",
        "opened": true
    })
}

pub(super) fn open_summary_item(value: &Value) -> Value {
    json!({
        "id": value.get("id").cloned().unwrap_or(Value::Null),
        "title": value.get("title").cloned().unwrap_or(Value::Null),
        "url": value.get("url").cloned().unwrap_or(Value::Null),
        "snippet": value.get("snippet").cloned().unwrap_or(Value::Null),
        "source": "open",
        "opened": true,
        "status": value.get("status").cloned().unwrap_or(Value::Null),
        "content_chars": text_value_chars(value)
    })
}

/// The evidence entry for one explicitly opened page.
///
/// The search path opens its top candidates for evidence; open mode has to
/// publish the same shape, otherwise the page text it just fetched is dropped by
/// the model-facing compaction and the model answers from a 500-character
/// snippet alone.
pub(super) fn open_evidence_item(value: &Value) -> Value {
    let content = value
        .get("content")
        .or_else(|| value.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    json!({
        "id": value.get("id").cloned().unwrap_or(Value::Null),
        "title": value.get("title").cloned().unwrap_or(Value::Null),
        "url": value.get("url").cloned().unwrap_or(Value::Null),
        "status": value.get("status").cloned().unwrap_or(Value::Null),
        "snippet": value.get("snippet").cloned().unwrap_or(Value::Null),
        "content_excerpt": super::extract::truncate_chars(content, super::MAX_EVIDENCE_EXCERPT_CHARS),
        "content_chars": content.chars().count(),
        "source": "opened",
        "opened": true,
        "truncated": value.get("truncated").cloned().unwrap_or(Value::Null)
    })
}

pub(super) fn open_diagnostic_item(value: &Value) -> Value {
    let mut item = value.clone();
    let content_chars = text_value_chars(value);
    if let Some(object) = item.as_object_mut() {
        object.remove("content");
        object.remove("text");
        object.insert("content_chars".to_owned(), json!(content_chars));
    }
    item
}

fn text_value_chars(value: &Value) -> usize {
    value
        .get("content")
        .or_else(|| value.get("text"))
        .and_then(Value::as_str)
        .map(|text| text.chars().count())
        .unwrap_or(0)
}

pub(super) fn candidate_id_for(url: &str, title: &str, query: &str, source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.trim().to_ascii_lowercase().as_bytes());
    hasher.update([0]);
    hasher.update(title.trim().to_ascii_lowercase().as_bytes());
    hasher.update([0]);
    hasher.update(query.trim().to_ascii_lowercase().as_bytes());
    hasher.update([0]);
    hasher.update(source.trim().to_ascii_lowercase().as_bytes());
    let digest = hasher.finalize();
    let mut suffix = String::new();
    for byte in digest.iter().take(6) {
        suffix.push_str(&format!("{byte:02x}"));
    }
    format!("cand_{suffix}")
}

pub(super) fn make_search_result(
    query: &str,
    title: &str,
    url: &str,
    snippet: &str,
    source: &str,
    rank: usize,
) -> Option<Value> {
    let url = normalize_candidate_url(url)?;
    if !matches_site_filters(query, &url) {
        return None;
    }
    let title = clean_visible_text(title);
    let snippet = truncate_chars(&redact_inline_data_urls(&clean_visible_text(snippet)), 600);
    if title.is_empty() && snippet.is_empty() {
        return None;
    }
    let score = score_search_result(query, &title, &url, &snippet, rank);
    Some(json!({
        "id": candidate_id_for(&url, &title, query, source),
        "title": if title.is_empty() { url.as_str() } else { title.as_str() },
        "url": url,
        "snippet": snippet,
        "query": query,
        "source": source,
        "rank": rank + 1,
        "score": score,
        "candidate": true
    }))
}

pub(super) fn retain_usable_results(results: &mut Vec<Value>) {
    results.retain(|item| {
        item.get("score")
            .and_then(Value::as_f64)
            .is_some_and(|score| score >= MIN_USABLE_SEARCH_SCORE)
    });
}

pub(super) fn dedupe_results(results: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for item in results {
        let url = item
            .get("url")
            .and_then(Value::as_str)
            .and_then(normalize_candidate_url);
        let Some(url) = url else {
            continue;
        };
        let key = url.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(url.as_str());
        let snippet = item.get("snippet").and_then(Value::as_str).unwrap_or("");
        let query = item.get("query").and_then(Value::as_str).unwrap_or("");
        let source = item
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("search");
        let rank = item
            .get("rank")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value.saturating_sub(1)).ok())
            .unwrap_or(output.len());
        if let Some(normalized) = make_search_result(query, title, &url, snippet, source, rank) {
            output.push(normalized);
        }
    }
    output
}

pub(super) fn average_score(results: &[Value]) -> f64 {
    if results.is_empty() {
        return 0.0;
    }
    let sum = results
        .iter()
        .map(|item| item.get("score").and_then(Value::as_f64).unwrap_or(0.0))
        .sum::<f64>();
    round_score(sum / results.len() as f64)
}

fn collect_candidates_from_value(value: &Value, lookup: &mut BTreeMap<String, String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_candidates_from_value(item, lookup);
            }
        }
        Value::Object(object) => {
            collect_candidate_entry(value, lookup);
            for key in ["content", "output", "result", "text"] {
                if let Some(text) = object.get(key).and_then(Value::as_str) {
                    collect_candidates_from_text(text, lookup);
                }
            }
            for key in [
                "candidates",
                "results",
                "opened_results",
                "grouped_results",
                "per_query",
                "opened",
            ] {
                if let Some(child) = object.get(key) {
                    collect_candidates_from_value(child, lookup);
                }
            }
        }
        _ => {}
    }
}

fn collect_candidate_entry(value: &Value, lookup: &mut BTreeMap<String, String>) {
    let Some(id) = value
        .get("id")
        .or_else(|| value.get("candidate_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let Some(url) = value
        .get("url")
        .or_else(|| value.get("link"))
        .and_then(Value::as_str)
        .and_then(normalize_candidate_url)
    else {
        return;
    };
    lookup.entry(id.to_ascii_lowercase()).or_insert(url);
}

fn collect_candidates_from_text(text: &str, lookup: &mut BTreeMap<String, String>) {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        collect_candidates_from_value(&value, lookup);
    }
    collect_candidate_pairs_from_loose_text(text, lookup);
}

fn collect_candidate_pairs_from_loose_text(text: &str, lookup: &mut BTreeMap<String, String>) {
    let Ok(id_re) = Regex::new(r#""(?:id|candidate_id)"\s*:\s*"(cand_[^"]+)""#) else {
        return;
    };
    let Ok(url_re) = Regex::new(r#""(?:url|link)"\s*:\s*"([^"]+)""#) else {
        return;
    };
    let urls = url_re
        .captures_iter(text)
        .filter_map(|caps| {
            let position = caps.get(0)?.start();
            let url = caps.get(1)?.as_str();
            normalize_candidate_url(url).map(|url| (position, url))
        })
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return;
    }
    for caps in id_re.captures_iter(text) {
        let Some(id_match) = caps.get(1) else {
            continue;
        };
        let id_position = id_match.start();
        let Some((_, url)) = urls
            .iter()
            .find(|(position, _)| *position >= id_position && *position - id_position <= 2400)
            .or_else(|| {
                urls.iter()
                    .rev()
                    .find(|(position, _)| id_position.saturating_sub(*position) <= 2400)
            })
        else {
            continue;
        };
        lookup
            .entry(id_match.as_str().to_ascii_lowercase())
            .or_insert_with(|| url.clone());
    }
}

fn score_search_result(query: &str, title: &str, url: &str, snippet: &str, rank: usize) -> f64 {
    let terms = meaningful_search_terms(query);
    let haystack = format!("{title} {url} {snippet}").to_ascii_lowercase();
    let semantic_terms = terms
        .iter()
        .filter(|term| term.chars().any(|ch| ch.is_alphabetic() || is_cjk(ch)))
        .collect::<Vec<_>>();
    let matched_semantic_terms = semantic_terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    let matched_terms = terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    let coverage = if terms.is_empty() {
        0.3
    } else {
        matched_terms as f64 / terms.len() as f64
    };
    if coverage <= f64::EPSILON {
        return round_score((0.08_f64 - rank as f64 * 0.01).max(0.0));
    }
    if !semantic_terms.is_empty() && matched_semantic_terms == 0 {
        return round_score((0.10_f64 - rank as f64 * 0.01).max(0.0));
    }
    if semantic_terms.len() >= 3 && matched_semantic_terms < 2 {
        return round_score((0.12_f64 - rank as f64 * 0.01).max(0.0));
    }
    if terms.len() >= 4 && coverage < 0.5 {
        return round_score((0.14_f64 - rank as f64 * 0.01).max(0.0));
    }
    let exact_phrase_bonus =
        phrase_match_bonus(&query.to_ascii_lowercase(), title, url, snippet) * coverage;
    let title_bonus = (!title.trim().is_empty()) as u8 as f64 * 0.10 * coverage;
    let snippet_bonus = (snippet.chars().count() >= 40) as u8 as f64 * 0.10 * coverage;
    let url_bonus =
        (url.starts_with("http://") || url.starts_with("https://")) as u8 as f64 * 0.08 * coverage;
    let rank_bonus = (0.14_f64 - rank as f64 * 0.012).max(0.0) * coverage;
    let low_value_penalty = low_value_search_penalty(url, title, snippet);
    round_score(
        (coverage * 0.58
            + exact_phrase_bonus
            + title_bonus
            + snippet_bonus
            + url_bonus
            + rank_bonus
            - low_value_penalty)
            .clamp(0.0, 1.0),
    )
}

fn phrase_match_bonus(query: &str, title: &str, url: &str, snippet: &str) -> f64 {
    let haystack = format!("{title} {url} {snippet}").to_ascii_lowercase();
    let phrases = query
        .split(['"', '\'', ':', ',', ';', '|'])
        .map(str::trim)
        .filter(|value| value.chars().count() >= 4)
        .take(6);
    let mut bonus = 0.0_f64;
    for phrase in phrases {
        if haystack.contains(phrase) {
            bonus += 0.08;
        }
    }
    bonus.min(0.16)
}

fn is_cjk(ch: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&ch)
}

fn matches_site_filters(query: &str, url: &str) -> bool {
    let domains = site_filter_domains(query);
    if domains.is_empty() {
        return true;
    }
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str().map(|value| value.to_ascii_lowercase()) else {
        return false;
    };
    domains
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn site_filter_domains(query: &str) -> Vec<String> {
    let Ok(re) = Regex::new(r#"(?i)\bsite:([a-z0-9.-]+\.[a-z]{2,})"#) else {
        return Vec::new();
    };
    re.captures_iter(query)
        .filter_map(|caps| caps.get(1).map(|value| value.as_str().to_ascii_lowercase()))
        .collect()
}

fn meaningful_search_terms(query: &str) -> Vec<String> {
    let Ok(re) = Regex::new(r#"[\p{Han}]{2,}|[a-zA-Z0-9][a-zA-Z0-9._-]{1,}"#) else {
        return Vec::new();
    };
    let stop = [
        "the", "and", "for", "with", "from", "latest", "today", "current", "search", "query",
        "best", "top",
    ];
    re.find_iter(&query.to_ascii_lowercase())
        .map(|value| value.as_str().to_owned())
        .filter(|term| !stop.contains(&term.as_str()))
        .take(12)
        .collect()
}

fn low_value_search_penalty(url: &str, title: &str, snippet: &str) -> f64 {
    let text = format!("{url} {title} {snippet}").to_ascii_lowercase();
    let mut penalty = 0.0;
    if text.contains("/search?") || text.contains("duckduckgo.com/html") {
        penalty += 0.25;
    }
    if text.contains("dictionary")
        || text.contains("thesaurus")
        || text.contains("definition")
        || text.contains("advertisement")
    {
        penalty += 0.25;
    }
    penalty
}

fn round_score(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn resolves_open_ids_from_prior_tool_messages() {
        let prior = json!({
            "role": "tool",
            "tool_call_id": "call_web",
            "content": serde_json::to_string(&json!({
                "candidates": [{
                    "id": "cand_test",
                    "title": "Example",
                    "url": "https://example.com/page",
                    "snippet": "hello"
                }]
            })).unwrap()
        });
        let lookup = lookup_from_messages(&[prior]);
        let mut targets = Vec::new();
        let unresolved = resolve_open_ids(&["cand_test".to_owned()], &lookup, &mut targets);

        assert!(unresolved.is_empty());
        assert_eq!(targets, vec!["https://example.com/page"]);
    }

    #[test]
    fn resolves_open_ids_from_verified_fact_text() {
        let prior = json!({
            "role": "user",
            "content": format!(
                "Verified facts:\n- type=web_search_call_output output={{\"results\":[{{\"id\":\"cand_fact\",\"title\":\"Docs\",\"url\":\"https://example.com/docs\",\"snippet\":\"ok\"}}]}}"
            )
        });
        let lookup = lookup_from_messages(&[prior]);
        let mut targets = Vec::new();
        let unresolved = resolve_open_ids(&["cand_fact".to_owned()], &lookup, &mut targets);

        assert!(unresolved.is_empty());
        assert_eq!(targets, vec!["https://example.com/docs"]);
    }

    #[test]
    fn unrelated_results_do_not_score_as_confident() {
        let score = score_search_result(
            "Python 3.14 release date status 2025",
            "哔哩哔哩",
            "https://www.bilibili.com/",
            "动漫新番、ACG内容和创意视频。",
            0,
        );

        assert!(score < 0.1, "unexpected score: {score}");
    }

    #[test]
    fn numeric_only_match_does_not_beat_missing_semantic_terms() {
        let score = score_search_result(
            "Python 3.14 release date",
            "3.14 square meter apartment",
            "https://example.com/real-estate",
            "A listing for parcel 3.14 with no programming language context.",
            0,
        );

        assert!(score < MIN_USABLE_SEARCH_SCORE, "unexpected score: {score}");
    }

    #[test]
    fn generic_single_term_match_does_not_satisfy_specific_query() {
        let score = score_search_result(
            "Python 3.14 release schedule",
            "Welcome to Python.org",
            "https://www.python.org/",
            "Python allows mandatory and optional arguments.",
            0,
        );

        assert!(score < MIN_USABLE_SEARCH_SCORE, "unexpected score: {score}");
    }

    #[test]
    fn relevant_results_score_above_low_confidence_threshold() {
        let score = score_search_result(
            "Python 3.14 release date status 2025",
            "PEP 745 – Python 3.14 Release Schedule",
            "https://peps.python.org/pep-0745/",
            "Python 3.14 release schedule, final release date, beta, release candidate, and status.",
            0,
        );

        assert!(score >= 0.24, "unexpected score: {score}");
    }

    #[test]
    fn exact_phrase_result_gets_enough_confidence_for_evidence_opening() {
        let score = score_search_result(
            "Zhongshan weather today",
            "Zhongshan weather today",
            "https://example.com/weather/zhongshan",
            "Zhongshan weather today, heavy rain, temperature forecast and current conditions.",
            0,
        );

        assert!(score >= 0.24, "unexpected score: {score}");
    }

    #[test]
    fn site_filter_rejects_other_domains() {
        assert!(make_search_result(
            "site:python.org 3.14 release schedule",
            "Baidu",
            "https://baidu.com/item/3",
            "unrelated",
            "bing_html",
            0,
        )
        .is_none());

        assert!(make_search_result(
            "site:python.org 3.14 release schedule",
            "Python release schedule",
            "https://peps.python.org/pep-0745/",
            "Python 3.14 release schedule",
            "bing_html",
            0,
        )
        .is_some());
    }

    #[test]
    fn retain_usable_results_drops_low_score_noise() {
        let mut results = vec![json!({"score": 0.08}), json!({"score": 0.24})];
        retain_usable_results(&mut results);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["score"], 0.24);
    }

    #[test]
    fn open_diagnostic_item_removes_duplicate_full_text() {
        let item = json!({
            "id": "cand_open",
            "url": "https://example.com/readme.md",
            "title": "Readme",
            "snippet": "short",
            "content": "large page body",
            "text": "large page body",
            "status": 200
        });

        let summary = open_summary_item(&item);
        let diagnostic = open_diagnostic_item(&item);

        assert_eq!(summary["content_chars"], 15);
        assert!(summary.get("content").is_none());
        assert!(diagnostic.get("content").is_none());
        assert!(diagnostic.get("text").is_none());
        assert_eq!(diagnostic["content_chars"], 15);
    }

    #[test]
    fn open_evidence_item_publishes_bounded_page_text() {
        let limit = super::super::MAX_EVIDENCE_EXCERPT_CHARS;
        let text = "x".repeat(limit + 500);
        let item = json!({
            "id": "cand_open",
            "title": "Page",
            "url": "https://example.com/",
            "status": 200,
            "snippet": "snippet",
            "content": text,
            "truncated": false
        });

        let evidence = open_evidence_item(&item);
        let excerpt = evidence["content_excerpt"].as_str().expect("excerpt");

        assert!(
            excerpt.starts_with(&"x".repeat(limit)),
            "the opened page text must reach the model-facing evidence"
        );
        assert!(excerpt.contains("truncated chars="));
        assert_eq!(evidence["content_chars"], (limit + 500) as u64);
        assert_eq!(evidence["source"], "opened");
        assert_eq!(evidence["opened"], true);
        assert_eq!(evidence["status"], 200);
    }
}
