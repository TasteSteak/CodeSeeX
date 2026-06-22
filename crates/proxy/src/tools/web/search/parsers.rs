use regex::Regex;
use serde_json::{json, Value};

use super::super::candidates::make_search_result;
use super::super::extract::{
    clean_visible_text, decode_basic_html_entities, strip_html_tags, truncate_chars,
};
pub(super) fn collect_duckduckgo_related(
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

pub(super) fn parse_bing_results(
    query: &str,
    html: &str,
    max_results: usize,
    source: &'static str,
) -> Vec<Value> {
    let Ok(link_re) = Regex::new(r#"(?is)<a\b[^>]+href\s*=\s*"([^"]+)"[^>]*>(.*?)</a>"#) else {
        return Vec::new();
    };
    let Ok(block_re) = Regex::new(
        r#"(?is)<(?:li|div)\b[^>]*\bclass\s*=\s*(?:"[^"]*\b(?:b_algo|b_ans|b_entityTP|b_top|b_card|b_rrsr)\b[^"]*"|'[^']*\b(?:b_algo|b_ans|b_entityTP|b_top|b_card|b_rrsr)\b[^']*'|[^\s>]*\b(?:b_algo|b_ans|b_entityTP|b_top|b_card|b_rrsr)\b[^\s>]*)[^>]*>(.*?)</(?:li|div)>"#,
    ) else {
        return Vec::new();
    };
    let Ok(snippet_re) = Regex::new(
        r#"(?is)<(?:p|div|span)\b[^>]*\bclass\s*=\s*(?:"[^"]*\b(?:b_caption|b_snippet|b_lineclamp\d*|b_factrow|b_focusTextLarge|b_secondaryText|news_dt|wr_fav|tab-content)\b[^"]*"|'[^']*\b(?:b_caption|b_snippet|b_lineclamp\d*|b_factrow|b_focusTextLarge|b_secondaryText|news_dt|wr_fav|tab-content)\b[^']*'|[^\s>]*\b(?:b_caption|b_snippet|b_lineclamp\d*|b_factrow|b_focusTextLarge|b_secondaryText|news_dt|wr_fav|tab-content)\b[^\s>]*)[^>]*>(.*?)</(?:p|div|span)>"#,
    ) else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for block in block_re.captures_iter(html) {
        if results.len() >= max_results {
            break;
        }
        let block = block.get(1).map(|value| value.as_str()).unwrap_or_default();
        collect_bing_block_result(
            query,
            block,
            &link_re,
            &snippet_re,
            source,
            max_results,
            &mut results,
        );
    }
    if results.len() < max_results {
        collect_bing_link_fallbacks(query, html, &link_re, source, max_results, &mut results);
    }
    results
}

fn collect_bing_block_result(
    query: &str,
    block: &str,
    link_re: &Regex,
    snippet_re: &Regex,
    source: &'static str,
    max_results: usize,
    results: &mut Vec<Value>,
) {
    let Some(link) = link_re.captures(block) else {
        return;
    };
    let url = decode_basic_html_entities(link.get(1).map(|value| value.as_str()).unwrap_or(""));
    if bing_internal_url(&url) {
        return;
    }
    let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
    let snippet = snippet_re
        .captures_iter(block)
        .filter_map(|caps| caps.get(1).map(|value| strip_html_tags(value.as_str())))
        .filter(|text| !text.trim().is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join(" ");
    let snippet = if snippet.is_empty() {
        block_text_near_link(block, link.get(0).map(|value| value.end()).unwrap_or(0))
    } else {
        snippet
    };
    if let Some(item) = make_search_result(query, &title, &url, &snippet, source, results.len()) {
        results.push(item);
        results.truncate(max_results);
    }
}

fn collect_bing_link_fallbacks(
    query: &str,
    html: &str,
    link_re: &Regex,
    source: &'static str,
    max_results: usize,
    results: &mut Vec<Value>,
) {
    for link in link_re.captures_iter(html) {
        if results.len() >= max_results {
            break;
        }
        let url = decode_basic_html_entities(link.get(1).map(|value| value.as_str()).unwrap_or(""));
        if bing_internal_url(&url)
            || search_chrome_url(&url)
            || results
                .iter()
                .any(|item| item.get("url").and_then(Value::as_str) == Some(url.as_str()))
        {
            continue;
        }
        let title = strip_html_tags(link.get(2).map(|value| value.as_str()).unwrap_or(""));
        if title.chars().count() < 3 {
            continue;
        }
        let snippet = block_text_near_link(html, link.get(0).map(|value| value.end()).unwrap_or(0));
        if let Some(item) = make_search_result(query, &title, &url, &snippet, source, results.len())
        {
            results.push(item);
        }
    }
}

fn block_text_near_link(html: &str, link_end: usize) -> String {
    let end = html.len().min(link_end.saturating_add(1_400));
    html.get(link_end..end)
        .map(strip_html_tags)
        .unwrap_or_default()
}

fn bing_internal_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("bing.com/search")
        || lower.contains("bing.com/images")
        || lower.contains("bing.com/videos")
        || lower.contains("go.microsoft.com")
        || lower.starts_with("javascript:")
        || lower.starts_with('#')
}

fn search_chrome_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let path = parsed.path().to_ascii_lowercase();
    matches!(
        host.as_str(),
        "privacy.microsoft.com"
            | "account.microsoft.com"
            | "support.microsoft.com"
            | "help.bing.microsoft.com"
            | "www.microsoft.com"
    ) || path.contains("/privacy")
        || path.contains("/terms")
        || path.contains("/account")
        || path.contains("/settings")
}

pub(super) fn parse_duckduckgo_results(query: &str, html: &str, max_results: usize) -> Vec<Value> {
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

pub(super) fn parse_brave_results(query: &str, html: &str, max_results: usize) -> Vec<Value> {
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

pub(super) fn parse_duckduckgo_lite_results(
    query: &str,
    html: &str,
    max_results: usize,
) -> Vec<Value> {
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

pub(super) struct WebLocale {
    pub(super) bing_market: &'static str,
    pub(super) bing_setlang: &'static str,
    pub(super) bing_country: &'static str,
    pub(super) bing_english_search: &'static str,
    pub(super) duckduckgo_kl: &'static str,
}

pub(super) fn web_locale(query: &str) -> WebLocale {
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
