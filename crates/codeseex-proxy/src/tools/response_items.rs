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
    json!({
        "id": format!("ctc_{}", Uuid::new_v4().simple()),
        "type": "custom_tool_call",
        "status": "completed",
        "call_id": call.id,
        "name": "apply_patch",
        "input": normalize_apply_patch_response_input(&call.arguments)
    })
}

pub(crate) fn normalize_patch_newlines(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn flush_proxy_tool_group(output: &mut Vec<Value>, proxy_group: &mut Vec<Value>) {
    if proxy_group.is_empty() {
        return;
    }
    output.push(tool_usage_message_item(proxy_group));
    output.append(proxy_group);
}

fn proxy_tool_call_response_item_from_chat_call(call: &ChatToolCall) -> Value {
    json!({
        "id": format!("ptc_{}", Uuid::new_v4().simple()),
        "type": "proxy_tool_call",
        "status": "completed",
        "call_id": call.id,
        "name": call.name,
        "arguments": call.arguments
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
        let mut action = json!({ "type": "open" });
        if !urls.is_empty() {
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

fn tool_usage_message_item(items: &[Value]) -> Value {
    let names = items
        .iter()
        .filter_map(|item| item.get("name").and_then(Value::as_str))
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let text = tool_usage_display_text(&names);
    json!({
        "id": format!("msg_{}", Uuid::new_v4().simple()),
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "phase": "commentary",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        "codeseex_display_only": "tool_usage",
        "metadata": { "codeseex_display_only": true, "kind": "tool_usage", "tools": names }
    })
}

fn tool_usage_display_text(names: &[String]) -> String {
    if names.len() == 1 {
        return format!("\u{5df2}\u{4f7f}\u{7528}\u{5de5}\u{5177} `{}`", names[0]);
    }
    tool_usage_batch_display_text(names)
}

fn tool_usage_batch_display_text(names: &[String]) -> String {
    let mut unique_names = Vec::new();
    for name in names {
        if !unique_names.contains(name) {
            unique_names.push(name.clone());
        }
    }
    let visible_names = unique_names.iter().take(3).cloned().collect::<Vec<_>>();
    let hidden_count = unique_names.len().saturating_sub(visible_names.len());
    let suffix = if hidden_count > 0 {
        format!(" +{hidden_count}")
    } else {
        String::new()
    };
    format!(
        "\u{5df2}\u{4f7f}\u{7528} {} \u{4e2a}\u{5de5}\u{5177}\n{}{}",
        names.len(),
        visible_names
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(" \u{00b7} "),
        suffix
    )
}

fn normalize_apply_patch_response_input(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return normalize_patch_newlines(arguments);
    };
    if let Some(patch) = value.get("patch").and_then(Value::as_str) {
        return normalize_patch_newlines(patch);
    }
    if let Some(input) = value.get("input").and_then(Value::as_str) {
        return normalize_patch_newlines(input);
    }
    if let Some(command) = value.get("command").and_then(Value::as_array) {
        if command.first().and_then(Value::as_str) == Some("apply_patch") {
            if let Some(patch) = command.get(1).and_then(Value::as_str) {
                return normalize_patch_newlines(patch);
            }
        }
    }
    normalize_patch_newlines(arguments)
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
    fn apply_patch_command_arguments_map_to_raw_native_input() {
        let patch = [
            "*** Begin Patch",
            "*** Update File: old.txt",
            "*** Move to: new.txt",
            "*** End Patch",
        ]
        .join("\n");
        let item = native_apply_patch_response_item_from_chat_call(&call(
            "call_patch",
            "apply_patch",
            &json!({ "command": ["apply_patch", patch] }).to_string(),
        ));

        assert_eq!(item["type"], "custom_tool_call");
        assert_eq!(item["input"], patch);
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
    fn regular_codeseex_tools_use_display_message_plus_proxy_item() {
        let items =
            proxy_visible_response_items(&[call("call_ls", "list_directory", r#"{"path":"."}"#)]);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["codeseex_display_only"], "tool_usage");
        assert_eq!(items[1]["type"], "proxy_tool_call");
        assert_eq!(items[1]["name"], "list_directory");
    }
}
