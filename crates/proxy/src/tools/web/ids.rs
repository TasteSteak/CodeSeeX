//! Candidate identity and `open_ids` resolution.
//!
//! Every published candidate carries a stable id derived from its URL, title,
//! query, and source. Later turns resolve an id back to a URL by reading the
//! structured tool results already present in the conversation, never by
//! guessing at free text.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

use super::safety::normalize_candidate_url;

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

/// Collects every `id -> url` pair a previous tool result published.
pub(super) fn lookup_from_messages(messages: &[Value]) -> BTreeMap<String, String> {
    let mut lookup = BTreeMap::new();
    for message in messages {
        collect_from_value(message, &mut lookup);
        if let Some(content) = message.get("content") {
            collect_from_content(content, &mut lookup);
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

fn collect_from_content(content: &Value, lookup: &mut BTreeMap<String, String>) {
    match content {
        Value::String(text) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                collect_from_value(&parsed, lookup);
                return;
            }
            if let Some(json) = embedded_json_after(text, "output=") {
                collect_from_value(&json, lookup);
            }
        }
        other => collect_from_value(other, lookup),
    }
}

fn collect_from_value(value: &Value, lookup: &mut BTreeMap<String, String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_from_value(item, lookup);
            }
        }
        Value::Object(object) => {
            if let (Some(id), Some(url)) = (
                object.get("id").and_then(Value::as_str),
                object
                    .get("url")
                    .or_else(|| object.get("link"))
                    .and_then(Value::as_str),
            ) {
                let id = id.trim();
                if id.starts_with("cand_") {
                    if let Some(url) = normalize_candidate_url(url) {
                        lookup.entry(id.to_ascii_lowercase()).or_insert(url);
                    }
                }
            }
            for (key, child) in object {
                match key.as_str() {
                    "id" | "url" | "link" | "type" | "score" | "rank" => continue,
                    _ => collect_from_value(child, lookup),
                }
            }
        }
        _ => {}
    }
}

/// Reads the first complete JSON object that follows `marker` in `text`.
fn embedded_json_after(text: &str, marker: &str) -> Option<Value> {
    let start = text.find(marker)? + marker.len();
    let open = text[start..].find('{')? + start;
    let object = balanced_object(&text[open..])?;
    serde_json::from_str::<Value>(object).ok()
}

fn balanced_object(text: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&text[..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn resolves_open_ids_from_structured_tool_results() {
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
    fn resolves_open_ids_from_a_flattened_fact_line() {
        let prior = json!({
            "role": "user",
            "content": "Verified facts:\n- type=web_search_call_output output={\"candidates\":[{\"id\":\"cand_fact\",\"title\":\"Docs\",\"url\":\"https://example.com/docs\",\"snippet\":\"ok\"}]}"
        });
        let lookup = lookup_from_messages(&[prior]);
        let mut targets = Vec::new();
        let unresolved = resolve_open_ids(&["cand_fact".to_owned()], &lookup, &mut targets);

        assert!(unresolved.is_empty());
        assert_eq!(targets, vec!["https://example.com/docs"]);
    }

    #[test]
    fn unknown_ids_stay_unresolved() {
        let mut targets = Vec::new();
        let lookup = BTreeMap::new();
        let unresolved = resolve_open_ids(&["cand_missing".to_owned()], &lookup, &mut targets);

        assert_eq!(unresolved, vec!["cand_missing"]);
        assert!(targets.is_empty());
    }
}
