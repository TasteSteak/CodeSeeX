//! Search-result parsing. Every engine is parsed from a real DOM instead of
//! nested regular expressions, so markup changes degrade into missing results
//! rather than into corrupted fields.

use scraper::{Html, Selector};
use serde_json::Value;

use super::safety::normalize_candidate_url;
use super::sources::RawHit;
use super::text::{clean_visible_text, truncate_chars};

pub(super) fn parse_bing_results(html: &str, max_results: usize) -> Vec<RawHit> {
    let document = Html::parse_document(html);
    let mut hits: Vec<RawHit> = Vec::new();
    for selector in ["li.b_algo", "#b_results > li", ".b_algo"] {
        collect_from_selector(
            &document,
            selector,
            "bing_html",
            max_results,
            &mut hits,
            |element| {
                let link = element
                    .select(&selector_or("h2 a[href]"))
                    .find_map(|anchor| anchor.value().attr("href"))
                    .or_else(|| {
                        element
                            .select(&selector_or("a[href]"))
                            .find_map(|anchor| anchor.value().attr("href"))
                    })?;
                let title = element
                    .select(&selector_or("h2"))
                    .next()
                    .map(|heading| heading.text().collect::<String>())
                    .unwrap_or_default();
                let snippet = [
                    "p",
                    ".b_caption",
                    ".b_snippet",
                    ".b_lineclamp2",
                    ".b_lineclamp3",
                ]
                .iter()
                .find_map(|selector| {
                    element
                        .select(&selector_or(selector))
                        .map(|node| clean_visible_text(&node.text().collect::<String>()))
                        .find(|text| !text.is_empty())
                })
                .unwrap_or_default();
                Some((title, link.to_owned(), snippet))
            },
        );
        if !hits.is_empty() {
            break;
        }
    }
    hits.truncate(max_results);
    hits
}

pub(super) fn parse_brave_results(html: &str, max_results: usize) -> Vec<RawHit> {
    let document = Html::parse_document(html);
    let mut hits = Vec::new();
    for selector in ["div.snippet", "#results > div", "div.result"] {
        collect_from_selector(
            &document,
            selector,
            "brave_html",
            max_results,
            &mut hits,
            |element| {
                let link = element
                    .select(&selector_or("a[href]"))
                    .find_map(|anchor| anchor.value().attr("href"))?;
                let title = element
                    .select(&selector_or(".title"))
                    .next()
                    .or_else(|| element.select(&selector_or("a[href]")).next())
                    .map(|node| clean_visible_text(&node.text().collect::<String>()))
                    .unwrap_or_default();
                let snippet = element
                    .select(&selector_or(
                        ".generic-snippet, .snippet-description, .snippet-content, p",
                    ))
                    .map(|node| clean_visible_text(&node.text().collect::<String>()))
                    .find(|text| !text.is_empty())
                    .unwrap_or_default();
                Some((title, link.to_owned(), snippet))
            },
        );
        if !hits.is_empty() {
            break;
        }
    }
    hits.truncate(max_results);
    hits
}

pub(super) fn parse_duckduckgo_lite_results(html: &str, max_results: usize) -> Vec<RawHit> {
    let document = Html::parse_document(html);
    let Ok(link_selector) = Selector::parse("a.result-link") else {
        return Vec::new();
    };
    let Ok(snippet_selector) = Selector::parse("td.result-snippet") else {
        return Vec::new();
    };
    let mut hits: Vec<RawHit> = Vec::new();
    for element in document.select(&link_selector) {
        if hits.len() >= max_results {
            break;
        }
        let Some(href) = element.value().attr("href") else {
            continue;
        };
        let Some(url) = normalize_candidate_url(href) else {
            continue;
        };
        if is_engine_internal_url(&url) || hits.iter().any(|hit| hit.url == url) {
            continue;
        }
        let title = clean_visible_text(&element.text().collect::<String>());
        if title.chars().count() < 3 {
            continue;
        }
        let snippet = duckduckgo_row_snippet(element, &link_selector, &snippet_selector);
        hits.push(RawHit {
            title,
            url,
            snippet: truncate_chars(&snippet, 600),
            source: "duckduckgo_lite",
            rank: hits.len(),
        });
    }
    hits
}

/// DuckDuckGo publishes the snippet in the row *after* the one holding the link.
fn duckduckgo_row_snippet(
    link: scraper::ElementRef<'_>,
    link_selector: &Selector,
    snippet_selector: &Selector,
) -> String {
    let mut row = link;
    for _ in 0..4 {
        let Some(parent) = row.parent().and_then(scraper::ElementRef::wrap) else {
            return String::new();
        };
        row = parent;
        if row.value().name() == "tr" {
            break;
        }
    }
    if row.value().name() != "tr" {
        return String::new();
    }
    for sibling in row.next_siblings() {
        let Some(element) = scraper::ElementRef::wrap(sibling) else {
            continue;
        };
        if element.value().name() != "tr" {
            continue;
        }
        if let Some(snippet) = element.select(snippet_selector).next() {
            return clean_visible_text(&snippet.text().collect::<String>());
        }
        if element.select(link_selector).next().is_some() {
            break;
        }
    }
    String::new()
}

pub(super) fn parse_duckduckgo_instant_answer(json: &str, max_results: usize) -> Vec<RawHit> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let mut hits: Vec<(String, String, String)> = Vec::new();
    if let (Some(text), Some(url)) = (
        value.get("AbstractText").and_then(Value::as_str),
        value.get("AbstractURL").and_then(Value::as_str),
    ) {
        let heading = value
            .get("Heading")
            .and_then(Value::as_str)
            .unwrap_or("Instant answer");
        push_instant(&mut hits, heading, url, text, max_results);
    }
    if let Some(definition) = value.get("Definition").and_then(Value::as_str) {
        if let Some(url) = value.get("DefinitionURL").and_then(Value::as_str) {
            push_instant(&mut hits, "Definition", url, definition, max_results);
        }
    }
    if let Some(topics) = value.get("RelatedTopics").and_then(Value::as_array) {
        collect_related_topics(topics, &mut hits, max_results);
    }
    finalize_instant(hits)
}

fn collect_related_topics(
    topics: &[Value],
    hits: &mut Vec<(String, String, String)>,
    max_results: usize,
) {
    for topic in topics {
        if hits.len() >= max_results {
            return;
        }
        if let Some(children) = topic.get("Topics").and_then(Value::as_array) {
            collect_related_topics(children, hits, max_results);
            continue;
        }
        let Some(text) = topic.get("Text").and_then(Value::as_str) else {
            continue;
        };
        let Some(url) = topic.get("FirstURL").and_then(Value::as_str) else {
            continue;
        };
        let heading = text.split(" - ").next().unwrap_or(text);
        push_instant(hits, heading, url, text, max_results);
    }
}

fn push_instant(
    hits: &mut Vec<(String, String, String)>,
    title: &str,
    url: &str,
    snippet: &str,
    max_results: usize,
) {
    if hits.len() >= max_results {
        return;
    }
    let title = clean_visible_text(title);
    let snippet = clean_visible_text(snippet);
    if title.is_empty() || url.is_empty() {
        return;
    }
    hits.push((title, url.to_owned(), snippet));
}

fn finalize_instant(hits: Vec<(String, String, String)>) -> Vec<RawHit> {
    let mut ranked = Vec::new();
    for (rank, (title, url, snippet)) in hits.into_iter().enumerate() {
        if let Some(url) = normalize_candidate_url(&url) {
            ranked.push(RawHit {
                title,
                url,
                snippet: truncate_chars(&snippet, 600),
                source: "duckduckgo_instant_answer",
                rank,
            });
        }
    }
    ranked
}

fn collect_from_selector<'a>(
    document: &'a Html,
    selector: &str,
    source: &'static str,
    max_results: usize,
    hits: &mut Vec<RawHit>,
    extract: impl Fn(scraper::ElementRef<'a>) -> Option<(String, String, String)>,
) {
    let Ok(parsed) = Selector::parse(selector) else {
        return;
    };
    for element in document.select(&parsed) {
        if hits.len() >= max_results {
            return;
        }
        let Some((title, url, snippet)) = extract(element) else {
            continue;
        };
        // Resolve engine redirect wrappers first: a DuckDuckGo result link is
        // published as a duckduckgo.com redirect, so the target host is only
        // visible after normalisation.
        let Some(url) = normalize_candidate_url(&url) else {
            continue;
        };
        if is_engine_internal_url(&url) {
            continue;
        }
        if hits.iter().any(|hit| hit.url == url) {
            continue;
        }
        let title = clean_visible_text(&title);
        if title.chars().count() < 3 {
            continue;
        }
        hits.push(RawHit {
            title,
            url,
            snippet: truncate_chars(&snippet, 600),
            source,
            rank: hits.len(),
        });
    }
}

fn selector_or(selector: &str) -> Selector {
    Selector::parse(selector).expect("static selector")
}

fn is_engine_internal_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("javascript:") || lower.starts_with('#') {
        return true;
    }
    let Ok(parsed) = reqwest::Url::parse(&lower) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or_default();
    let path = parsed.path();
    matches!(
        host,
        "www.bing.com"
            | "bing.com"
            | "go.microsoft.com"
            | "search.brave.com"
            | "duckduckgo.com"
            | "lite.duckduckgo.com"
            | "html.duckduckgo.com"
    ) || path.contains("/privacy")
        || path.contains("/terms")
        || path.contains("/account")
        || path.contains("/settings")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bing_result_blocks() {
        let html = r#"
            <ol id="b_results">
              <li class="b_algo">
                <h2><a href="https://example.com/a">Example A</a></h2>
                <div class="b_caption"><p>First snippet text.</p></div>
              </li>
              <li class="b_algo">
                <h2><a href="https://example.com/b">Example B</a></h2>
                <div class="b_caption"><p>Second snippet text.</p></div>
              </li>
              <li><a href="https://www.bing.com/search?q=more">More results</a></li>
            </ol>"#;
        let hits = parse_bing_results(html, 5);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/a");
        assert!(hits[0].snippet.contains("First snippet"));
        assert!(hits[0].title.contains("Example A"));
    }

    #[test]
    fn parses_duckduckgo_lite_uddg_links() {
        let html = r#"
            <table>
              <tr><td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fx" class='result-link'>Example X</a></td></tr>
            </table>"#;
        let hits = parse_duckduckgo_lite_results(html, 5);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/x");
    }

    #[test]
    fn instant_answer_reads_abstract_topics() {
        let json = r#"{
            "Heading": "Rust",
            "AbstractText": "Rust is a programming language.",
            "AbstractURL": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "RelatedTopics": [
                {"Text": "Rust reference - docs", "FirstURL": "https://doc.rust-lang.org/"}
            ]
        }"#;
        let hits = parse_duckduckgo_instant_answer(json, 5);

        assert!(hits.len() >= 1);
        assert!(hits[0].title.contains("Rust"));
    }
}
