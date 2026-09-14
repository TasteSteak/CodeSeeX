//! The single argument-normalisation entry point for the web search tool.
//!
//! Both the executor and the client-facing `web_search_call` action item read
//! the model's arguments through this module, so there is exactly one parser
//! for every argument shape the tool accepts.

use serde_json::{json, Value};
use std::collections::HashSet;

use super::safety::normalize_candidate_url;
use super::text::compact_whitespace;
use super::{MAX_OPEN_TARGETS, MAX_QUERIES, MAX_RESULTS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum WebMode {
    Search,
    Open,
}

impl WebMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            WebMode::Search => "search",
            WebMode::Open => "open",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct WebRequest {
    pub(super) mode: WebMode,
    pub(super) queries: Vec<String>,
    pub(super) open_urls: Vec<String>,
    pub(super) open_ids: Vec<String>,
    pub(super) max_results: usize,
}

impl WebRequest {
    pub(super) fn parse(args: &Value) -> Self {
        let declared = args
            .get("mode")
            .or_else(|| args.get("type"))
            .and_then(Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase());
        let queries = collect_queries(args);
        let mut open_urls = collect_targets(args, &["open_urls", "urls", "url"]);
        let open_ids = collect_targets(args, &["open_ids", "ids", "id"]);
        if open_urls.is_empty() && open_ids.is_empty() {
            for query in &queries {
                if let Some(url) = normalize_candidate_url(query) {
                    open_urls.push(url);
                }
            }
        }
        let open_urls = dedupe(open_urls, MAX_OPEN_TARGETS, 2048)
            .into_iter()
            .filter_map(|value| normalize_candidate_url(&value))
            .collect::<Vec<_>>();
        let open_ids = dedupe(open_ids, MAX_OPEN_TARGETS, 128);
        let mode =
            if declared.as_deref() == Some("open") || !open_urls.is_empty() || !open_ids.is_empty()
            {
                WebMode::Open
            } else {
                WebMode::Search
            };
        WebRequest {
            mode,
            queries,
            open_urls,
            open_ids,
            max_results: desired_max_results(args),
        }
    }

    /// Parses the raw argument string the provider emitted for a hosted call.
    pub(super) fn parse_arguments(arguments: &str) -> Self {
        let parsed = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
        Self::parse(&parsed)
    }

    /// The `action` payload Codex renders inside a `web_search_call` item.
    pub(super) fn client_action(&self) -> Value {
        let queries = self.queries.clone();
        if self.mode == WebMode::Open {
            let mut action = json!({ "type": "open_page" });
            if let Some(url) = self.open_urls.first() {
                action["url"] = Value::String(url.clone());
            }
            if self.open_urls.len() > 1 {
                action["urls"] =
                    Value::Array(self.open_urls.iter().cloned().map(Value::String).collect());
            }
            if !self.open_ids.is_empty() {
                action["ids"] =
                    Value::Array(self.open_ids.iter().cloned().map(Value::String).collect());
            }
            if !queries.is_empty() {
                action["query"] = Value::String(queries.join("\n"));
            }
            return action;
        }
        let mut action = json!({ "type": "search", "query": queries.join("\n") });
        if queries.len() > 1 {
            action["queries"] = Value::Array(queries.into_iter().map(Value::String).collect());
        }
        action
    }
}

pub(crate) fn client_action_from_arguments(arguments: &str) -> Value {
    WebRequest::parse_arguments(arguments).client_action()
}

fn desired_max_results(args: &Value) -> usize {
    args.get("max_results")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(5)
        .clamp(1, MAX_RESULTS)
}

fn collect_queries(args: &Value) -> Vec<String> {
    let mut values = Vec::new();
    push_query_values(args.get("queries"), &mut values);
    push_string_or_array(args.get("search_query"), &mut values);
    push_string_or_array(args.get("query"), &mut values);
    push_string_or_array(args.get("q"), &mut values);
    dedupe(values, MAX_QUERIES, 300)
}

fn push_query_values(value: Option<&Value>, output: &mut Vec<String>) {
    match value {
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item
                    .as_str()
                    .or_else(|| item.get("q").and_then(Value::as_str))
                    .or_else(|| item.get("query").and_then(Value::as_str))
                {
                    output.push(text.to_owned());
                }
            }
        }
        other => push_string_or_array(other, output),
    }
}

fn push_string_or_array(value: Option<&Value>, output: &mut Vec<String>) {
    match value {
        Some(Value::String(text)) => {
            for line in text.split(['\n', '\r']) {
                output.push(line.to_owned());
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    output.push(text.to_owned());
                }
            }
        }
        _ => {}
    }
}

fn collect_targets(args: &Value, keys: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    for key in keys {
        push_string_or_array(args.get(*key), &mut values);
    }
    values
}

fn dedupe(values: Vec<String>, max_items: usize, max_chars: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for value in values {
        let cleaned = compact_whitespace(&value)
            .chars()
            .take(max_chars)
            .collect::<String>();
        if cleaned.is_empty() {
            continue;
        }
        let key = cleaned.to_ascii_lowercase();
        if seen.insert(key) {
            output.push(cleaned);
            if output.len() >= max_items {
                break;
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_legacy_argument_shapes() {
        let request = WebRequest::parse(&json!({
            "type": "search",
            "queries": ["CodeSeeX GitHub", { "q": "DeepSeek V4 pricing" }],
            "q": "ignored after max"
        }));

        assert_eq!(request.mode, WebMode::Search);
        assert_eq!(
            request.queries,
            vec![
                "CodeSeeX GitHub",
                "DeepSeek V4 pricing",
                "ignored after max"
            ]
        );
    }

    #[test]
    fn bare_domain_query_becomes_an_open_request() {
        let request = WebRequest::parse(&json!({ "query": "example.com/a" }));

        assert_eq!(request.mode, WebMode::Open);
        assert_eq!(request.open_urls, vec!["https://example.com/a"]);
        assert!(request.queries.is_empty() || request.queries == vec!["example.com/a"]);
    }

    #[test]
    fn open_ids_force_open_mode() {
        let request = WebRequest::parse(&json!({ "open_ids": ["cand_abc"], "id": "cand_def" }));

        assert_eq!(request.mode, WebMode::Open);
        assert_eq!(request.open_ids, vec!["cand_abc", "cand_def"]);
    }

    #[test]
    fn client_action_matches_the_declared_mode() {
        let search = WebRequest::parse(&json!({ "query": "rust 1.0 release" }));
        assert_eq!(
            search.client_action()["type"],
            Value::String("search".to_owned())
        );

        let open = WebRequest::parse(&json!({ "open_urls": ["https://example.com/a"] }));
        assert_eq!(
            open.client_action()["type"],
            Value::String("open_page".to_owned())
        );
    }
}
