//! Pure ranking: site filters, cross-source fusion, the relevance gate, and the
//! final ordering. Nothing here performs IO, so the behaviour is fully testable.

use regex::Regex;
use std::collections::BTreeMap;

use super::ids::candidate_id_for;
use super::safety::normalize_candidate_url;
use super::sources::RawHit;
use super::text::{clean_visible_text, truncate_chars};

/// Reciprocal-rank-fusion constant. Standard value; large enough that the top
/// few results of each source dominate but deep results still contribute.
const RRF_K: f64 = 60.0;
/// A candidate must reach this lexical relevance to be considered usable.
pub(super) const MIN_RELEVANCE: f64 = 0.18;
/// When nothing clears `MIN_RELEVANCE`, keep the best weak candidates above this
/// floor so the model sees something instead of an empty result.
const LOW_CONFIDENCE_FLOOR: f64 = 0.08;
const LOW_CONFIDENCE_MAX_RESULTS: usize = 3;

#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) url: String,
    pub(super) snippet: String,
    pub(super) source: String,
    pub(super) rank: usize,
    pub(super) score: f64,
    pub(super) relevance: f64,
    pub(super) matched_sources: usize,
}

#[derive(Clone, Debug)]
pub(super) struct RankedSet {
    pub(super) candidates: Vec<Candidate>,
    /// True when the result list only exists because weak candidates were kept.
    pub(super) low_confidence_fallback: bool,
}

struct Aggregated {
    url: String,
    title: String,
    snippet: String,
    best_source: String,
    best_rank: usize,
    best_source_rank: usize,
    sources: Vec<(String, usize)>,
}

/// Fuses the raw hits of every source into one ordered candidate list.
///
/// The ordering is reciprocal-rank fusion, so it depends only on each source's
/// own ranking and never on the order responses happened to arrive in.
pub(super) fn rank(query: &str, hits: &[RawHit], max_results: usize) -> RankedSet {
    let domains = site_filter_domains(query);
    let mut aggregated: BTreeMap<String, Aggregated> = BTreeMap::new();
    for hit in hits {
        let Some(url) = normalize_candidate_url(&hit.url) else {
            continue;
        };
        if !domains.is_empty() && !url_matches_domains(&url, &domains) {
            continue;
        }
        let title = clean_visible_text(&hit.title);
        let snippet = truncate_chars(&clean_visible_text(&hit.snippet), 600);
        if title.is_empty() && snippet.is_empty() {
            continue;
        }
        let key = url.to_ascii_lowercase();
        let entry = aggregated.entry(key).or_insert_with(|| Aggregated {
            url: url.clone(),
            title: title.clone(),
            snippet: snippet.clone(),
            best_source: hit.source.to_owned(),
            best_rank: hit.rank,
            best_source_rank: hit.rank,
            sources: Vec::new(),
        });
        // Keep the richest title/snippet we saw for this URL.
        if title.chars().count() > entry.title.chars().count() {
            entry.title = title;
        }
        if snippet.chars().count() > entry.snippet.chars().count() {
            entry.snippet = snippet;
        }
        if hit.rank < entry.best_source_rank {
            entry.best_source_rank = hit.rank;
            entry.best_rank = hit.rank;
            entry.best_source = hit.source.to_owned();
        }
        entry.sources.push((hit.source.to_owned(), hit.rank));
    }

    let mut candidates = Vec::with_capacity(aggregated.len());
    for entry in aggregated.into_values() {
        let display_url = entry.url.as_str();
        let title = if entry.title.is_empty() {
            display_url.to_owned()
        } else {
            entry.title.clone()
        };
        let relevance = relevance_score(query, &title, &entry.url, &entry.snippet);
        // Fusion sums each source's contribution; a URL found by three sources
        // outranks a URL found by one, which is the whole point of fusing.
        let fused = entry
            .sources
            .iter()
            .map(|(_, rank)| 1.0 / (RRF_K + *rank as f64))
            .sum::<f64>();
        candidates.push(Candidate {
            id: candidate_id_for(&entry.url, &title, query, &entry.best_source),
            title,
            url: entry.url,
            snippet: entry.snippet,
            source: entry.best_source,
            rank: entry.best_rank,
            score: fused,
            relevance,
            matched_sources: entry.sources.len(),
        });
    }

    candidates.sort_by(|left, right| {
        right
            .matched_sources
            .cmp(&left.matched_sources)
            .then_with(|| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                right
                    .relevance
                    .partial_cmp(&left.relevance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut usable = candidates
        .iter()
        .filter(|candidate| candidate.relevance >= MIN_RELEVANCE)
        .cloned()
        .collect::<Vec<_>>();
    if usable.is_empty() {
        usable = candidates
            .into_iter()
            .filter(|candidate| candidate.relevance >= LOW_CONFIDENCE_FLOOR)
            .take(LOW_CONFIDENCE_MAX_RESULTS)
            .collect();
        usable.truncate(max_results);
        let used_fallback = !usable.is_empty();
        return RankedSet {
            candidates: usable,
            low_confidence_fallback: used_fallback,
        };
    }
    usable.truncate(max_results);
    RankedSet {
        candidates: usable,
        low_confidence_fallback: false,
    }
}

/// Mean lexical relevance of a candidate list, in `[0, 1]`.
pub(super) fn average_relevance(candidates: &[Candidate]) -> f64 {
    if candidates.is_empty() {
        return 0.0;
    }
    let sum = candidates.iter().map(|item| item.relevance).sum::<f64>();
    round_score(sum / candidates.len() as f64)
}

fn relevance_score(query: &str, title: &str, url: &str, snippet: &str) -> f64 {
    let terms = meaningful_terms(query);
    if terms.is_empty() {
        return 0.3;
    }
    let title_l = title.to_lowercase();
    let url_l = url.to_lowercase();
    let snippet_l = snippet.to_lowercase();
    let haystack = format!("{title_l} {url_l} {snippet_l}");
    let matched = terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    let coverage = matched as f64 / terms.len() as f64;
    if coverage <= f64::EPSILON {
        return 0.0;
    }
    let title_hit = terms
        .iter()
        .filter(|term| title_l.contains(term.as_str()))
        .count() as f64
        / terms.len() as f64;
    let phrase_bonus = phrase_match_bonus(query, &title_l, &url_l, &snippet_l);
    round_score((coverage * 0.72 + title_hit * 0.20 + phrase_bonus).clamp(0.0, 1.0))
}

fn phrase_match_bonus(query: &str, title: &str, url: &str, snippet: &str) -> f64 {
    let haystack = format!("{title} {url} {snippet}");
    let phrases = query
        .split(['"', '\'', ':', ',', ';', '|'])
        .map(str::trim)
        .filter(|value| value.chars().count() >= 4)
        .take(6);
    let mut bonus = 0.0_f64;
    for phrase in phrases {
        if haystack.contains(&phrase.to_lowercase()) {
            bonus += 0.08;
        }
    }
    bonus.min(0.16)
}

/// Splits a query into matching terms without favouring any language or region.
///
/// Latin words are kept whole; CJK runs are expanded into bigrams so a query
/// like `今日北京天气` matches documents that contain `北京` and `天气`
/// separately instead of only when the whole run appears verbatim.
pub(super) fn meaningful_terms(query: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "from", "latest", "today", "current", "search", "query",
        "best", "top", "how", "what", "when", "where", "who", "why", "this", "that",
    ];
    const CJK_STOPWORDS: &[char] = &['的', '了', '是', '在', '和', '与', '我', '你', '他', '它'];

    let mut terms = Vec::new();
    for token in query.split_whitespace() {
        let mut ascii = String::new();
        let mut cjk = Vec::new();
        for ch in token.chars() {
            if is_cjk(ch) {
                flush_ascii(&mut ascii, &mut terms, STOPWORDS);
                cjk.push(ch);
            } else {
                flush_cjk(&mut cjk, &mut terms, CJK_STOPWORDS);
                if ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-' | '+' | '#') {
                    ascii.push(ch);
                } else {
                    flush_ascii(&mut ascii, &mut terms, STOPWORDS);
                }
            }
        }
        flush_ascii(&mut ascii, &mut terms, STOPWORDS);
        flush_cjk(&mut cjk, &mut terms, CJK_STOPWORDS);
    }
    terms.dedup();
    terms.truncate(24);
    terms
}

fn flush_ascii(pending: &mut String, terms: &mut Vec<String>, stopwords: &[&str]) {
    if pending.chars().count() < 2 {
        pending.clear();
        return;
    }
    let token = pending.to_lowercase();
    pending.clear();
    if stopwords.contains(&token.as_str()) {
        return;
    }
    terms.push(token);
}

fn flush_cjk(pending: &mut Vec<char>, terms: &mut Vec<String>, stopwords: &[char]) {
    if pending.is_empty() {
        return;
    }
    let run = std::mem::take(pending);
    if run.len() == 1 {
        if !stopwords.contains(&run[0]) {
            terms.push(run[0].to_string());
        }
        return;
    }
    if run.len() == 2 {
        terms.push(run.iter().collect());
        return;
    }
    for window in run.windows(2) {
        terms.push(window.iter().collect());
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3040..=0x30ff | 0x3400..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff)
}

fn site_filter_domains(query: &str) -> Vec<String> {
    let Ok(re) = Regex::new(r#"(?i)\bsite:([a-z0-9.-]+\.[a-z]{2,})"#) else {
        return Vec::new();
    };
    re.captures_iter(query)
        .filter_map(|caps| caps.get(1).map(|value| value.as_str().to_ascii_lowercase()))
        .collect()
}

fn url_matches_domains(url: &str, domains: &[String]) -> bool {
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

fn round_score(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(source: &'static str, rank: usize, title: &str, url: &str, snippet: &str) -> RawHit {
        RawHit {
            title: title.to_owned(),
            url: url.to_owned(),
            snippet: snippet.to_owned(),
            source,
            rank,
        }
    }

    #[test]
    fn cjk_runs_expand_to_bigrams() {
        let terms = meaningful_terms("今日北京天气");

        assert!(terms.contains(&"北京".to_owned()), "{terms:?}");
        assert!(terms.contains(&"天气".to_owned()), "{terms:?}");
    }

    #[test]
    fn cjk_query_matches_partial_document_terms() {
        let score = relevance_score(
            "今日北京天气",
            "北京天气预报",
            "https://weather.example.com/beijing",
            "北京今天白天晴，最高气温 30 度，适合出行。",
        );

        assert!(score >= MIN_RELEVANCE, "unexpected score: {score}");
    }

    #[test]
    fn unrelated_latin_result_is_gated_out() {
        let set = rank(
            "Python 3.14 release date status 2025",
            &[hit(
                "bing_html",
                0,
                "\u{54d4}\u{54a9}\u{54d4}\u{54a9}",
                "https://www.bilibili.com/",
                "anime and creative video",
            )],
            5,
        );

        assert!(set.candidates.is_empty());
        assert!(!set.low_confidence_fallback);
    }

    #[test]
    fn fusion_rewards_urls_found_by_more_sources() {
        let shared = "https://example.com/shared";
        let hits = vec![
            hit(
                "bing_html",
                0,
                "Alpha release notes",
                shared,
                "alpha release notes",
            ),
            hit(
                "brave_html",
                1,
                "Alpha release notes",
                shared,
                "alpha release notes",
            ),
            hit(
                "duckduckgo_lite",
                0,
                "Alpha lone",
                "https://lone.example.com/",
                "alpha release notes",
            ),
        ];
        let set = rank("alpha release notes", &hits, 5);

        assert_eq!(set.candidates.len(), 2);
        assert_eq!(set.candidates[0].url, shared);
        assert_eq!(set.candidates[0].matched_sources, 2);
    }

    #[test]
    fn site_filter_keeps_only_the_requested_domain() {
        let set = rank(
            "site:python.org 3.14 release schedule",
            &[
                hit(
                    "bing_html",
                    0,
                    "Baidu",
                    "https://baidu.com/item/3",
                    "unrelated",
                ),
                hit(
                    "bing_html",
                    1,
                    "Python release schedule",
                    "https://peps.python.org/pep-0745/",
                    "Python 3.14 release schedule",
                ),
            ],
            5,
        );

        assert_eq!(set.candidates.len(), 1);
        assert!(set.candidates[0].url.contains("peps.python.org"));
    }

    #[test]
    fn weak_but_nonzero_results_fall_back_instead_of_vanishing() {
        let set = rank(
            "alpha beta gamma delta epsilon zeta",
            &[hit(
                "bing_html",
                3,
                "alpha",
                "https://weak.example.com/noise",
                "",
            )],
            5,
        );

        assert!(set.low_confidence_fallback);
        assert_eq!(set.candidates.len(), 1);
    }
}
