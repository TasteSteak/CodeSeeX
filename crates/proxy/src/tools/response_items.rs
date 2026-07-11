use codeseex_core::context::redact_inline_data_urls;
use serde_json::{json, Value};
use std::collections::HashSet;
use uuid::Uuid;

use super::ownership::{is_web_search_tool, ChatToolCall};

pub(crate) fn proxy_visible_response_items(tool_calls: &[ChatToolCall]) -> Vec<Value> {
    let mut output = Vec::new();
    let mut proxy_group = Vec::new();
    for call in tool_calls {
        if is_web_search_tool(&call.name) {
            flush_proxy_tool_group(&mut output, &mut proxy_group);
            output.push(web_search_call_response_item_from_chat_call(call));
        } else {
            proxy_group.push(proxy_tool_call_response_item_from_chat_call(call));
        }
    }
    flush_proxy_tool_group(&mut output, &mut proxy_group);
    output
}

pub(crate) fn native_apply_patch_response_item_from_chat_call(call: &ChatToolCall) -> Value {
    native_apply_patch_response_item_from_chat_call_with_id(
        call,
        &format!("ctc_{}", Uuid::new_v4().simple()),
    )
}

pub(crate) fn native_apply_patch_response_item_from_chat_call_with_id(
    call: &ChatToolCall,
    item_id: &str,
) -> Value {
    let normalized = normalize_apply_patch_response_input_with_diagnostic(&call.arguments);
    json!({
        "id": item_id,
        "type": "custom_tool_call",
        "status": "completed",
        "call_id": call.id,
        "name": "apply_patch",
        "input": normalized.input
    })
}

pub(crate) fn web_search_call_output_response_item(call: &ChatToolCall, output: &str) -> Value {
    json!({
        "id": format!("wso_{}", Uuid::new_v4().simple()),
        "type": "web_search_call_output",
        "call_id": call.id,
        "output": output
    })
}

pub(crate) fn normalize_patch_newlines(value: &str) -> String {
    normalize_patch_newlines_with_diagnostic(value).input
}

fn normalize_patch_newlines_with_diagnostic(value: &str) -> ApplyPatchInputNormalization {
    let normalized =
        normalize_unified_hunk_headers(&value.replace("\r\n", "\n").replace('\r', "\n"));
    let (input, blank_context_lines_repaired) =
        repair_update_hunk_blank_context_lines_with_count(&normalized);
    ApplyPatchInputNormalization {
        input_chars: input.chars().count(),
        input,
        blank_context_lines_repaired,
    }
}

fn normalize_unified_hunk_headers(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for (index, line) in value.split('\n').enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&normalize_patch_line(line));
    }
    output
}

pub(crate) fn normalize_patch_line(line: &str) -> String {
    normalize_unified_hunk_header(line).unwrap_or_else(|| line.to_owned())
}

#[derive(Debug, Default)]
struct HunkBlankRepair {
    blank_lines: Vec<usize>,
    has_nonempty_unprefixed_line: bool,
}

fn repair_update_hunk_blank_context_lines_with_count(value: &str) -> (String, usize) {
    let mut lines = value
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<String>>();
    let mut in_update_file = false;
    let mut hunk_repair = None::<HunkBlankRepair>;
    let mut repaired = 0;

    for index in 0..lines.len() {
        let line = &lines[index];
        if line.starts_with("*** Update File: ") {
            repaired += finish_hunk_blank_repair(&mut lines, &mut hunk_repair);
            in_update_file = true;
            continue;
        }
        if line.starts_with("*** Add File: ")
            || line.starts_with("*** Delete File: ")
            || line.starts_with("*** End Patch")
        {
            repaired += finish_hunk_blank_repair(&mut lines, &mut hunk_repair);
            in_update_file = false;
            continue;
        }
        if line.starts_with("*** Move to: ") {
            repaired += finish_hunk_blank_repair(&mut lines, &mut hunk_repair);
            continue;
        }
        if in_update_file && line.starts_with("@@") {
            repaired += finish_hunk_blank_repair(&mut lines, &mut hunk_repair);
            hunk_repair = Some(HunkBlankRepair::default());
            continue;
        }
        if let Some(repair) = hunk_repair.as_mut() {
            if line.is_empty() {
                repair.blank_lines.push(index);
            } else if !line.starts_with([' ', '+', '-']) {
                repair.has_nonempty_unprefixed_line = true;
            }
        }
    }
    repaired += finish_hunk_blank_repair(&mut lines, &mut hunk_repair);
    (lines.join("\n"), repaired)
}

fn finish_hunk_blank_repair(lines: &mut [String], repair: &mut Option<HunkBlankRepair>) -> usize {
    let Some(repair) = repair.take() else {
        return 0;
    };
    if repair.has_nonempty_unprefixed_line {
        return 0;
    }
    let mut repaired = 0;
    for index in repair.blank_lines {
        if lines[index].is_empty() {
            lines[index] = " ".to_owned();
            repaired += 1;
        }
    }
    repaired
}

fn normalize_unified_hunk_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("@@ -")?;
    let (_old_range, rest) = take_unified_range(rest)?;
    let rest = rest.strip_prefix(" +")?;
    let (_new_range, rest) = take_unified_range(rest)?;
    let tail = rest.strip_prefix(" @@")?.trim_start();
    if tail.is_empty() {
        Some("@@".to_owned())
    } else {
        Some(format!("@@ {tail}"))
    }
}

fn take_unified_range(value: &str) -> Option<(&str, &str)> {
    let bytes = value.as_bytes();
    let mut end = take_ascii_digits(bytes, 0)?;
    if bytes.get(end) == Some(&b',') {
        end = take_ascii_digits(bytes, end + 1)?;
    }
    Some((&value[..end], &value[end..]))
}

fn take_ascii_digits(bytes: &[u8], start: usize) -> Option<usize> {
    let mut end = start;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    (end > start).then_some(end)
}

fn flush_proxy_tool_group(output: &mut Vec<Value>, proxy_group: &mut Vec<Value>) {
    if proxy_group.is_empty() {
        return;
    }
    output.append(proxy_group);
}

fn proxy_tool_call_response_item_from_chat_call(call: &ChatToolCall) -> Value {
    json!({
        "id": format!("ptc_{}", Uuid::new_v4().simple()),
        "type": "proxy_tool_call",
        "status": "completed",
        "call_id": call.id,
        "name": call.name,
        "arguments": redact_inline_data_urls(&call.arguments)
    })
}

fn web_search_call_response_item_from_chat_call(call: &ChatToolCall) -> Value {
    json!({
        "id": format!("ws_{}", Uuid::new_v4().simple()),
        "type": "web_search_call",
        "status": "completed",
        "call_id": call.id,
        "action": web_search_action_from_arguments(&call.arguments)
    })
}

fn web_search_action_from_arguments(arguments: &str) -> Value {
    let parsed = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
    let queries = web_search_queries(&parsed);
    let open_urls = web_search_open_targets(&parsed, &["open_urls", "urls", "url"]);
    let open_ids = web_search_open_targets(&parsed, &["open_ids", "ids", "id"]);
    let explicit_mode = parsed
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let direct_urls = if open_urls.is_empty() && open_ids.is_empty() && queries.len() == 1 {
        web_search_direct_url(&queries[0])
            .map(|value| vec![value])
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let should_open = explicit_mode == "open"
        || !open_urls.is_empty()
        || !open_ids.is_empty()
        || !direct_urls.is_empty();
    if should_open {
        let urls = if open_urls.is_empty() {
            direct_urls
        } else {
            open_urls
        };
        let mut action = json!({ "type": "open_page" });
        if let Some(url) = urls.first() {
            action["url"] = Value::String(url.clone());
        }
        if urls.len() > 1 {
            action["urls"] = Value::Array(urls.into_iter().map(Value::String).collect());
        }
        if !open_ids.is_empty() {
            action["ids"] = Value::Array(open_ids.into_iter().map(Value::String).collect());
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

fn web_search_queries(value: &Value) -> Vec<String> {
    if let Some(queries) = value.get("queries").and_then(Value::as_array) {
        return queries
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_owned)
            .collect();
    }
    if let Some(search_query) = value.get("search_query") {
        if let Some(query) = search_query.as_str() {
            let query = query.trim();
            return (!query.is_empty())
                .then(|| query.to_owned())
                .into_iter()
                .collect();
        }
        if let Some(queries) = search_query.as_array() {
            return queries
                .iter()
                .filter_map(|entry| {
                    entry.as_str().or_else(|| {
                        entry
                            .get("q")
                            .or_else(|| entry.get("query"))
                            .and_then(Value::as_str)
                    })
                })
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .map(str::to_owned)
                .collect();
        }
    }
    value
        .get("query")
        .or_else(|| value.get("q"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(|query| vec![query.to_owned()])
        .unwrap_or_default()
}

fn web_search_open_targets(value: &Value, keys: &[&str]) -> Vec<String> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for key in keys {
        let Some(target) = value.get(*key) else {
            continue;
        };
        let values = target
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![target.clone()]);
        for entry in values {
            let Some(text) = entry
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            let dedupe_key = text.to_ascii_lowercase();
            if seen.insert(dedupe_key) {
                output.push(text.to_owned());
            }
        }
    }
    output
}

fn web_search_direct_url(value: &str) -> Option<String> {
    let text = value.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    if text.starts_with("http://") || text.starts_with("https://") {
        return Some(text.trim_end_matches([',', '.', ';', ')']).to_owned());
    }
    let has_dot = text.split('/').next().unwrap_or_default().contains('.');
    if has_dot {
        return Some(format!(
            "https://{}",
            text.trim_end_matches([',', '.', ';', ')'])
        ));
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyPatchInputNormalization {
    pub(crate) input: String,
    pub(crate) input_chars: usize,
    pub(crate) blank_context_lines_repaired: usize,
}

pub(crate) fn normalize_apply_patch_response_input_with_diagnostic(
    arguments: &str,
) -> ApplyPatchInputNormalization {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return normalize_patch_newlines_with_diagnostic(arguments);
    };
    if let Some(patch) = value.get("patch").and_then(Value::as_str) {
        return normalize_patch_newlines_with_diagnostic(patch);
    }
    if let Some(input) = value.get("input").and_then(Value::as_str) {
        return normalize_patch_newlines_with_diagnostic(input);
    }
    normalize_patch_newlines_with_diagnostic(arguments)
}

pub(crate) fn apply_patch_input_normalization_diagnostic(
    arguments: &str,
) -> ApplyPatchInputNormalization {
    normalize_apply_patch_response_input_with_diagnostic(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str, arguments: &str) -> ChatToolCall {
        ChatToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    #[test]
    fn apply_patch_maps_to_native_custom_tool_call() {
        let item = native_apply_patch_response_item_from_chat_call(&call(
            "call_patch",
            "apply_patch",
            r#"{"patch":"*** Begin Patch\r\n*** End Patch"}"#,
        ));

        assert_eq!(item["type"], "custom_tool_call");
        assert_eq!(item["name"], "apply_patch");
        assert_eq!(item["call_id"], "call_patch");
        assert_eq!(item["input"], "*** Begin Patch\n*** End Patch");
    }

    #[test]
    fn apply_patch_normalizes_unified_nm_hunk_headers() {
        let item = native_apply_patch_response_item_from_chat_call(&call(
            "call_patch",
            "apply_patch",
            r#"{"patch":"*** Begin Patch\n*** Update File: src/main.rs\n@@ -10,2 +10,3 @@ fn main\n old\n+new\n*** End Patch"}"#,
        ));

        assert_eq!(
            item["input"],
            "*** Begin Patch\n*** Update File: src/main.rs\n@@ fn main\n old\n+new\n*** End Patch"
        );
    }

    #[test]
    fn apply_patch_normalizes_bare_unified_nm_hunk_headers() {
        let input = normalize_patch_newlines(
            "*** Begin Patch\r\n*** Update File: src/lib.rs\r\n@@ -1 +1,2 @@\r\n old\r\n+new\r\n*** End Patch",
        );

        assert_eq!(
            input,
            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n old\n+new\n*** End Patch"
        );
    }

    #[test]
    fn apply_patch_repairs_blank_context_lines_in_update_hunks() {
        let input = normalize_patch_newlines(
            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n fn before() {}\n\n pub fn after() {}\n*** End Patch",
        );

        assert_eq!(
            input,
            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n fn before() {}\n \n pub fn after() {}\n*** End Patch"
        );
    }

    #[test]
    fn apply_patch_reports_blank_context_line_micro_repair_count() {
        let normalized = normalize_apply_patch_response_input_with_diagnostic(
            r#"{"patch":"*** Begin Patch\n*** Update File: src/lib.rs\n@@\n fn before() {}\n\n pub fn after() {}\n*** End Patch"}"#,
        );

        assert_eq!(normalized.blank_context_lines_repaired, 1);
        assert!(normalized.input.contains("\n \n"));
    }

    #[test]
    fn apply_patch_leaves_valid_update_hunk_unchanged() {
        let input = normalize_patch_newlines(
            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n fn before() {}\n \n pub fn after() {}\n*** End Patch",
        );

        assert_eq!(
            input,
            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n fn before() {}\n \n pub fn after() {}\n*** End Patch"
        );
    }

    #[test]
    fn apply_patch_reports_no_micro_repair_for_valid_blank_context_line() {
        let normalized = normalize_apply_patch_response_input_with_diagnostic(
            r#"{"patch":"*** Begin Patch\n*** Update File: src/lib.rs\n@@\n fn before() {}\n \n pub fn after() {}\n*** End Patch"}"#,
        );

        assert_eq!(normalized.blank_context_lines_repaired, 0);
        assert!(normalized.input.contains("\n \n"));
    }

    #[test]
    fn apply_patch_does_not_repair_ambiguous_nonempty_unprefixed_hunks() {
        let input = normalize_patch_newlines(
            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n fn before() {}\n\npub fn after() {}\n*** End Patch",
        );

        assert_eq!(
            input,
            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n fn before() {}\n\npub fn after() {}\n*** End Patch"
        );
    }

    #[test]
    fn apply_patch_reports_no_micro_repair_for_ambiguous_hunk() {
        let normalized = normalize_apply_patch_response_input_with_diagnostic(
            r#"{"patch":"*** Begin Patch\n*** Update File: src/lib.rs\n@@\n fn before() {}\n\npub fn after() {}\n*** End Patch"}"#,
        );

        assert_eq!(normalized.blank_context_lines_repaired, 0);
        assert!(normalized.input.contains("\n\npub fn after"));
    }

    #[test]
    fn apply_patch_repairs_only_unambiguous_update_hunk_blank_lines_in_a_multifile_patch() {
        let patch = "*** Begin Patch\n*** Add File: notes.md\n+# Heading\n+\n+body\n*** Update File: src/lib.rs\n@@\n fn before() {}\n\n fn after() {}\n*** End Patch";
        let normalized = normalize_apply_patch_response_input_with_diagnostic(
            &json!({ "patch": patch }).to_string(),
        );

        assert_eq!(normalized.blank_context_lines_repaired, 1);
        assert_eq!(
            normalized.input,
            "*** Begin Patch\n*** Add File: notes.md\n+# Heading\n+\n+body\n*** Update File: src/lib.rs\n@@\n fn before() {}\n \n fn after() {}\n*** End Patch"
        );
    }

    #[test]
    fn apply_patch_leaves_a_valid_multifile_native_patch_byte_for_byte_unchanged() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n old\n+new\n*** Update File: tests/lib.rs\n@@\n old test\n+new test\n*** Add File: docs/notes.md\n+# Notes\n+\n*** End Patch";
        let normalized = normalize_apply_patch_response_input_with_diagnostic(
            &json!({ "patch": patch }).to_string(),
        );

        assert_eq!(normalized.blank_context_lines_repaired, 0);
        assert_eq!(normalized.input, patch);
    }

    #[test]
    fn apply_patch_preserves_add_file_blank_and_hash_content_lines() {
        let item = native_apply_patch_response_item_from_chat_call(&call(
            "call_patch",
            "apply_patch",
            r#"{"patch":"*** Begin Patch\n*** Add File: notes.md\n+# Title\n+\n+body\n*** End Patch"}"#,
        ));

        assert_eq!(
            item["input"],
            "*** Begin Patch\n*** Add File: notes.md\n+# Title\n+\n+body\n*** End Patch"
        );
    }

    #[test]
    fn web_search_maps_to_web_search_call_not_proxy_tool() {
        let items = proxy_visible_response_items(&[call(
            "call_web",
            "web_search",
            r#"{"query":"today weather"}"#,
        )]);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["type"], "web_search_call");
        assert_eq!(items[0]["call_id"], "call_web");
        assert_eq!(items[0]["action"]["type"], "search");
    }

    #[test]
    fn web_search_open_url_maps_to_native_open_page_action() {
        let items = proxy_visible_response_items(&[call(
            "call_web",
            "web_search",
            r#"{"mode":"open","url":"https://example.com/page"}"#,
        )]);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["type"], "web_search_call");
        assert_eq!(items[0]["action"]["type"], "open_page");
        assert_eq!(items[0]["action"]["url"], "https://example.com/page");
    }

    #[test]
    fn web_search_output_item_keeps_call_result_replayable() {
        let item = web_search_call_output_response_item(
            &call("call_web", "web_search", r#"{"query":"today weather"}"#),
            r#"{"ok":true}"#,
        );

        assert_eq!(item["type"], "web_search_call_output");
        assert_eq!(item["call_id"], "call_web");
        assert_eq!(item["output"], r#"{"ok":true}"#);
    }

    #[test]
    fn regular_codeseex_tools_use_proxy_item_without_text_message() {
        let items =
            proxy_visible_response_items(&[call("call_ls", "list_directory", r#"{"path":"."}"#)]);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["type"], "proxy_tool_call");
        assert_eq!(items[0]["name"], "list_directory");
    }

    #[test]
    fn proxy_tool_call_arguments_redact_inline_image_data_urls() {
        let items = proxy_visible_response_items(&[call(
            "call_vision",
            "vision_analyze",
            r#"{"image":"data:image/png;base64,AAAASECRETBBBB","prompt":"inspect"}"#,
        )]);

        let arguments = items[0]["arguments"].as_str().unwrap();
        assert!(arguments.contains("inline-data-url omitted"));
        assert!(!arguments.contains("AAAASECRETBBBB"));
    }
}
