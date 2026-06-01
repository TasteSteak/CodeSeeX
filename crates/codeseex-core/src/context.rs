use crate::protocol::ChatMessage;
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

const MAX_FACT_TEXT_CHARS: usize = 4096;
const MAX_FACTS: usize = 128;
const MAX_TOOL_OUTPUT_CHARS: usize = 12_000;
const MAX_MESSAGE_CONTENT_CHARS: usize = 24_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextDiagnostic {
    pub input_items: u64,
    pub message_items: u64,
    pub verified_fact_items: u64,
    pub unsupported_items: u64,
    pub truncated_items: u64,
    pub estimated_chars: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompiledResponsesInput {
    pub messages: Vec<ChatMessage>,
    pub diagnostic: ContextDiagnostic,
}

pub fn responses_input_to_messages(input: &Value) -> Vec<ChatMessage> {
    compile_responses_input(input).messages
}

pub fn compile_responses_input(input: &Value) -> CompiledResponsesInput {
    compile_responses_input_with_tool_outputs(input, &HashSet::new())
}

pub fn compile_responses_input_with_tool_outputs(
    input: &Value,
    valid_tool_call_ids: &HashSet<String>,
) -> CompiledResponsesInput {
    match input {
        Value::String(text) => {
            let message = user_message(text);
            compiled(
                vec![message],
                ContextDiagnostic {
                    input_items: 1,
                    message_items: 1,
                    estimated_chars: text.chars().count() as u64,
                    ..ContextDiagnostic::default()
                },
            )
        }
        Value::Array(items) => {
            let mut messages = Vec::new();
            let mut facts = Vec::new();
            let mut diagnostic = ContextDiagnostic {
                input_items: items.len() as u64,
                ..ContextDiagnostic::default()
            };
            let resolved_tool_call_ids = collect_resolved_tool_call_ids(items);
            let mut seen_tool_call_ids = HashSet::new();
            let mut pending_assistant = PendingAssistant::default();
            let mut pending_reasoning = String::new();

            for item in items {
                if response_item_is_display_only(item) {
                    continue;
                }

                if let Some(reasoning) = response_item_to_reasoning_text(item) {
                    diagnostic.estimated_chars += reasoning.chars().count() as u64;
                    pending_reasoning = join_nonempty(&pending_reasoning, &reasoning);
                } else if let Some(tool_call) =
                    response_item_to_chat_tool_call(item, &resolved_tool_call_ids)
                {
                    if let Some(call_id) = tool_call.get("id").and_then(Value::as_str) {
                        seen_tool_call_ids.insert(call_id.to_owned());
                    }
                    pending_assistant.reasoning =
                        join_nonempty(&pending_assistant.reasoning, &pending_reasoning);
                    pending_reasoning.clear();
                    pending_assistant.tool_calls.push(tool_call);
                    diagnostic.message_items += 1;
                } else if let Some(message) = response_item_to_tool_result_message(
                    item,
                    &resolved_tool_call_ids,
                    &seen_tool_call_ids,
                    valid_tool_call_ids,
                ) {
                    flush_pending_assistant(&mut messages, &mut pending_assistant);
                    diagnostic.message_items += 1;
                    diagnostic.estimated_chars += message.content.chars().count() as u64;
                    messages.push(message);
                } else if let Some(message) = response_item_to_compaction_message(item) {
                    flush_pending_assistant(&mut messages, &mut pending_assistant);
                    pending_reasoning.clear();
                    diagnostic.message_items += 1;
                    diagnostic.estimated_chars += message.content.chars().count() as u64;
                    messages.push(message);
                } else if let Some(fact) = response_item_to_verified_fact(item) {
                    flush_pending_assistant(&mut messages, &mut pending_assistant);
                    pending_reasoning.clear();
                    diagnostic.verified_fact_items += 1;
                    diagnostic.estimated_chars += fact.chars().count() as u64;
                    if facts.len() < MAX_FACTS {
                        facts.push(fact);
                    } else {
                        diagnostic.truncated_items += 1;
                    }
                } else if let Some(message) = response_item_to_message(item) {
                    flush_pending_assistant(&mut messages, &mut pending_assistant);
                    if message.role == "assistant" {
                        pending_assistant.content = message.content;
                        pending_assistant.reasoning =
                            join_nonempty(&pending_assistant.reasoning, &pending_reasoning);
                        pending_reasoning.clear();
                    } else {
                        pending_reasoning.clear();
                        diagnostic.message_items += 1;
                        diagnostic.estimated_chars += message.content.chars().count() as u64;
                        messages.push(message);
                    }
                } else {
                    flush_pending_assistant(&mut messages, &mut pending_assistant);
                    pending_reasoning.clear();
                    diagnostic.unsupported_items += 1;
                }
            }
            flush_pending_assistant(&mut messages, &mut pending_assistant);

            if !facts.is_empty() {
                messages.insert(
                    0,
                    verified_facts_message(&facts, diagnostic.truncated_items),
                );
            }

            if messages.is_empty() {
                let fallback = input.to_string();
                diagnostic.message_items += 1;
                diagnostic.estimated_chars += fallback.chars().count() as u64;
                messages.push(user_message(&fallback));
            }
            compiled(messages, diagnostic)
        }
        Value::Null => CompiledResponsesInput::default(),
        other => {
            let text = other.to_string();
            compiled(
                vec![user_message(&text)],
                ContextDiagnostic {
                    input_items: 1,
                    message_items: 1,
                    estimated_chars: text.chars().count() as u64,
                    ..ContextDiagnostic::default()
                },
            )
        }
    }
}

#[derive(Debug, Default)]
struct PendingAssistant {
    content: String,
    reasoning: String,
    tool_calls: Vec<Value>,
}

fn flush_pending_assistant(messages: &mut Vec<ChatMessage>, pending: &mut PendingAssistant) {
    if pending.content.trim().is_empty()
        && pending.reasoning.trim().is_empty()
        && pending.tool_calls.is_empty()
    {
        return;
    }

    if pending.tool_calls.is_empty() {
        if !pending.content.trim().is_empty() {
            messages.push(ChatMessage::text("assistant", pending.content.clone()));
        }
    } else {
        let tool_calls = pending.tool_calls.clone();
        if pending.reasoning.trim().is_empty() {
            messages.push(ChatMessage::assistant_tool_calls(
                tool_calls,
                pending.content.clone(),
            ));
        } else {
            messages.push(ChatMessage::assistant_tool_calls_with_reasoning(
                tool_calls,
                pending.content.clone(),
                pending.reasoning.clone(),
            ));
        }
    }

    *pending = PendingAssistant::default();
}

fn join_nonempty(left: &str, right: &str) -> String {
    let left = left.trim();
    let right = right.trim();
    if left.is_empty() {
        right.to_owned()
    } else if right.is_empty() {
        left.to_owned()
    } else {
        format!("{left}\n{right}")
    }
}

fn collect_resolved_tool_call_ids(items: &[Value]) -> HashSet<String> {
    items
        .iter()
        .filter(|item| response_item_is_tool_output(item))
        .filter_map(response_item_call_id)
        .map(str::to_owned)
        .collect()
}

fn response_item_to_chat_tool_call(
    item: &Value,
    resolved_tool_call_ids: &HashSet<String>,
) -> Option<Value> {
    let item_type = item.get("type").and_then(Value::as_str);
    if !matches!(
        item_type,
        Some("function_call") | Some("custom_tool_call") | Some("web_search_call")
    ) {
        return None;
    }
    let call_id = response_item_call_id(item)?;
    if !resolved_tool_call_ids.contains(call_id) {
        return None;
    }
    let name = response_item_tool_name(item)?;
    Some(serde_json::json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": response_item_tool_arguments(item)
        }
    }))
}

fn response_item_tool_name(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) == Some("web_search_call") {
        return Some("web_search".to_owned());
    }
    item.get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|name| !name.trim().is_empty())
}

fn response_item_tool_arguments(item: &Value) -> String {
    if item.get("type").and_then(Value::as_str) == Some("custom_tool_call")
        && item.get("name").and_then(Value::as_str) == Some("apply_patch")
    {
        let patch = item
            .get("input")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        return serde_json::to_string(&serde_json::json!({ "patch": patch }))
            .unwrap_or_else(|_| "{}".to_owned());
    }
    if item.get("type").and_then(Value::as_str) == Some("web_search_call") {
        return item
            .get("action")
            .map(value_to_arguments)
            .unwrap_or_else(|| "{}".to_owned());
    }
    item.get("arguments")
        .map(value_to_arguments)
        .unwrap_or_else(|| "{}".to_owned())
}

fn value_to_arguments(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        text.to_owned()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned())
    }
}

fn response_item_call_id(item: &Value) -> Option<&str> {
    item.get("call_id")
        .or_else(|| item.get("tool_call_id"))
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn response_item_is_tool_output(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("function_call_output")
            | Some("custom_tool_call_output")
            | Some("web_search_call_output")
    )
}

fn response_item_to_reasoning_text(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }
    let text = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .and_then(decode_reasoning_content)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| {
            item.get("summary")
                .map(content_to_text)
                .filter(|text| !text.trim().is_empty())
        })
        .or_else(|| {
            item.get("content")
                .map(content_to_text)
                .filter(|text| !text.trim().is_empty())
        })?;
    Some(truncate_text(&text, MAX_MESSAGE_CONTENT_CHARS))
}

fn decode_reasoning_content(value: &str) -> Option<String> {
    general_purpose::STANDARD
        .decode(value.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

fn response_item_is_display_only(item: &Value) -> bool {
    item.get("codeseex_display_only").is_some()
        || item
            .pointer("/metadata/codeseex_display_only")
            .and_then(Value::as_bool)
            == Some(true)
        || item
            .get("content")
            .map(content_to_text)
            .map(|text| response_text_is_display_only(&text))
            .unwrap_or(false)
}

fn response_text_is_display_only(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("---\ncodeseex_display_only:")
        || text.starts_with("---\n**DeepSeek Thinking**")
        || text.starts_with("已使用工具 `")
        || (text.starts_with("已使用 ") && text.contains(" 个工具\n`"))
}

fn response_item_to_tool_result_message(
    item: &Value,
    resolved_tool_call_ids: &HashSet<String>,
    seen_tool_call_ids: &HashSet<String>,
    valid_tool_call_ids: &HashSet<String>,
) -> Option<ChatMessage> {
    if !response_item_is_tool_output(item) {
        return None;
    }
    let call_id = response_item_call_id(item)?;
    let paired_in_current_input =
        resolved_tool_call_ids.contains(call_id) && seen_tool_call_ids.contains(call_id);
    if !paired_in_current_input && !valid_tool_call_ids.contains(call_id) {
        return None;
    }
    let content = item
        .get("output")
        .or_else(|| item.get("content"))
        .or_else(|| item.get("result"))
        .map(content_to_text)
        .unwrap_or_default();
    Some(ChatMessage::tool_result(
        call_id,
        truncate_text(&redact_inline_data_urls(&content), MAX_TOOL_OUTPUT_CHARS),
    ))
}

pub fn response_item_to_message(item: &Value) -> Option<ChatMessage> {
    let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
    let content = item
        .get("content")
        .map(content_to_text)
        .map(|text| truncate_text(&text, MAX_MESSAGE_CONTENT_CHARS))
        .unwrap_or_default();
    if content.trim().is_empty() {
        return None;
    }
    Some(ChatMessage::text(normalize_role(role), content))
}

pub fn content_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => redact_inline_data_urls(text),
        Value::Array(parts) => parts
            .iter()
            .filter_map(content_part_to_text)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => content_part_to_text(value).unwrap_or_else(|| value.to_string()),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn response_item_to_compaction_message(item: &Value) -> Option<ChatMessage> {
    if item.get("type").and_then(Value::as_str) != Some("compaction") {
        return None;
    }
    let text = item
        .get("summary")
        .map(content_to_text)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| {
            item.get("content")
                .map(content_to_text)
                .filter(|text| !text.trim().is_empty())
        })?;
    Some(ChatMessage::text(
        "system",
        format!(
            "Recovered CodeSeeX compaction summary. Treat as historical context:\n{}",
            truncate_text(&text, MAX_FACT_TEXT_CHARS * 2)
        ),
    ))
}

fn content_part_to_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Some(redact_inline_data_urls(text));
    }
    if let Some(text) = value.get("input_text").and_then(Value::as_str) {
        return Some(redact_inline_data_urls(text));
    }
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        return Some(redact_inline_data_urls(text));
    }
    None
}

fn response_item_to_verified_fact(item: &Value) -> Option<String> {
    if !response_item_is_fact_like(item) {
        return None;
    }

    let mut fields = Vec::new();
    if let Some(item_type) = item.get("type").and_then(Value::as_str) {
        fields.push(format!("type={item_type}"));
    }
    if let Some(role) = item.get("role").and_then(Value::as_str) {
        fields.push(format!("role={role}"));
    }
    for key in [
        "call_id",
        "id",
        "name",
        "server",
        "tool",
        "status",
        "arguments",
        "content",
        "output",
        "result",
        "error",
    ] {
        if let Some(value) = item.get(key) {
            fields.push(format!("{key}={}", summarize_value(value)));
        }
    }
    if fields.len() == 1 {
        fields.push(format!("item={}", summarize_value(item)));
    }
    Some(fields.join(" "))
}

fn response_item_is_fact_like(item: &Value) -> bool {
    if item.get("role").and_then(Value::as_str) == Some("tool") {
        return true;
    }

    let Some(item_type) = item.get("type").and_then(Value::as_str) else {
        return false;
    };

    item_type.contains("tool")
        || item_type.contains("function_call")
        || item_type.contains("mcp")
        || item_type.ends_with("_call")
        || item_type.ends_with("_result")
        || item_type.ends_with("_output")
}

fn verified_facts_message(facts: &[String], truncated_items: u64) -> ChatMessage {
    let mut content = String::from(
        "Verified prior tool/request facts from the client context. Treat these as higher confidence than assistant self-descriptions:\n",
    );
    for fact in facts {
        content.push_str("- ");
        content.push_str(fact);
        content.push('\n');
    }
    if truncated_items > 0 {
        content.push_str(&format!("- {truncated_items} additional fact item(s) were omitted by the deterministic context budget.\n"));
    }
    ChatMessage::text("system", content)
}

fn summarize_value(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.to_owned(),
        Value::Array(_) | Value::Object(_) => content_to_text(value)
            .trim()
            .to_owned()
            .if_empty_then(|| value.to_string()),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    };
    truncate_text(&redact_inline_data_urls(&text), MAX_FACT_TEXT_CHARS)
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_owned();
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    format!(
        "{prefix}...[truncated chars={} bytes={}]",
        char_count,
        text.len()
    )
}

fn user_message(text: &str) -> ChatMessage {
    ChatMessage::text("user", redact_inline_data_urls(text))
}

fn compiled(messages: Vec<ChatMessage>, diagnostic: ContextDiagnostic) -> CompiledResponsesInput {
    CompiledResponsesInput {
        messages,
        diagnostic,
    }
}

fn normalize_role(role: &str) -> &str {
    match role {
        "system" | "developer" => "system",
        "assistant" => "assistant",
        "tool" => "tool",
        _ => "user",
    }
}

trait EmptyStringExt {
    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String;
}

impl EmptyStringExt for String {
    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() {
            fallback()
        } else {
            self
        }
    }
}

pub fn redact_inline_data_urls(text: &str) -> String {
    let mut cursor = 0;
    let mut redacted = String::new();
    let mut changed = false;

    while let Some(relative_start) = text[cursor..].find("data:") {
        let start = cursor + relative_start;
        redacted.push_str(&text[cursor..start]);

        let mut end = text.len();
        for (offset, ch) in text[start..].char_indices().skip(1) {
            if is_data_url_terminator(ch) {
                end = start + offset;
                break;
            }
        }

        let segment = &text[start..end];
        redacted.push_str(&format!(
            "[inline-data-url omitted chars={} bytes={} hash={}]",
            segment.chars().count(),
            segment.len(),
            stable_hash_hex(segment.as_bytes())
        ));
        cursor = end;
        changed = true;
    }

    if !changed {
        return text.to_owned();
    }

    redacted.push_str(&text[cursor..]);
    redacted
}

fn is_data_url_terminator(ch: char) -> bool {
    matches!(
        ch,
        '"' | '\'' | ')' | ']' | '}' | '<' | '>' | '\n' | '\r' | '\t' | ' '
    )
}

fn stable_hash_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_responses_text_parts() {
        let input = json!([
            {"role":"user","content":[{"type":"input_text","text":"hello"}]},
            {"role":"assistant","content":[{"type":"output_text","text":"world"}]}
        ]);
        let messages = responses_input_to_messages(&input);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn reconstructs_current_input_tool_protocol_pairs() {
        let input = json!([
            {"type":"function_call","call_id":"call_1","name":"list_files","arguments":"{\"path\":\".\"}"},
            {"type":"function_call_output","call_id":"call_1","output":"Cargo.toml\nREADME.md"},
            {"role":"user","content":[{"type":"input_text","text":"what happened?"}]}
        ]);
        let compiled = compile_responses_input(&input);
        assert_eq!(compiled.diagnostic.verified_fact_items, 0);
        assert_eq!(compiled.messages[0].role, "assistant");
        assert_eq!(
            compiled.messages[0].tool_calls.as_ref().unwrap()[0]["function"]["name"],
            "list_files"
        );
        assert_eq!(compiled.messages[1].role, "tool");
        assert_eq!(compiled.messages[1].tool_call_id.as_deref(), Some("call_1"));
        assert!(compiled.messages[1].content.contains("Cargo.toml"));
        assert_eq!(compiled.messages[2].content, "what happened?");
    }

    #[test]
    fn preserves_reasoning_content_for_current_input_tool_pairs() {
        let input = json!([
            {
                "type": "reasoning",
                "summary": [
                    { "type": "summary_text", "text": "need to inspect files first" }
                ]
            },
            {"type":"function_call","call_id":"call_1","name":"read_file_range","arguments":"{\"path\":\"Cargo.toml\"}"},
            {"type":"function_call_output","call_id":"call_1","output":"[package]"},
            {"role":"user","content":[{"type":"input_text","text":"continue"}]}
        ]);
        let compiled = compile_responses_input(&input);

        assert_eq!(compiled.messages[0].role, "assistant");
        assert_eq!(
            compiled.messages[0].reasoning_content.as_deref(),
            Some("need to inspect files first")
        );
        assert_eq!(compiled.messages[1].role, "tool");
    }

    #[test]
    fn decodes_encrypted_reasoning_content_for_tool_replay() {
        let input = json!([
            {
                "type": "reasoning",
                "encrypted_content": "bmVlZCB0aGUgdG9vbA=="
            },
            {"type":"function_call","call_id":"call_1","name":"read_file_range","arguments":"{\"path\":\"Cargo.toml\"}"},
            {"type":"function_call_output","call_id":"call_1","output":"[package]"}
        ]);
        let compiled = compile_responses_input(&input);

        assert_eq!(
            compiled.messages[0].reasoning_content.as_deref(),
            Some("need the tool")
        );
    }

    #[test]
    fn preserves_custom_tool_call_output_as_tool_result() {
        let call_id = "call_patch".to_owned();
        let compiled = compile_responses_input_with_tool_outputs(
            &json!([
                {
                    "type": "custom_tool_call_output",
                    "call_id": call_id,
                    "output": "{\"output\":\"Success. Updated files.\"}"
                }
            ]),
            &HashSet::from([call_id]),
        );

        assert_eq!(compiled.messages.len(), 1);
        assert_eq!(compiled.messages[0].role, "tool");
        assert_eq!(
            compiled.messages[0].tool_call_id.as_deref(),
            Some("call_patch")
        );
        assert!(compiled.messages[0]
            .content
            .contains("Success. Updated files."));
    }

    #[test]
    fn reconstructs_custom_apply_patch_as_patch_argument() {
        let input = json!([
            {
                "type": "custom_tool_call",
                "call_id": "call_patch",
                "name": "apply_patch",
                "input": "*** Begin Patch\r\n*** Add File: hi.txt\r\n+hi\r\n*** End Patch"
            },
            {
                "type": "custom_tool_call_output",
                "call_id": "call_patch",
                "output": "Success. Updated the following files:\nA hi.txt"
            }
        ]);
        let compiled = compile_responses_input(&input);
        let arguments = compiled.messages[0].tool_calls.as_ref().unwrap()[0]["function"]
            ["arguments"]
            .as_str()
            .unwrap();
        let parsed: Value = serde_json::from_str(arguments).unwrap();

        assert_eq!(compiled.messages[0].role, "assistant");
        assert_eq!(
            compiled.messages[0].tool_calls.as_ref().unwrap()[0]["function"]["name"],
            "apply_patch"
        );
        assert!(parsed["patch"]
            .as_str()
            .unwrap()
            .contains("*** Begin Patch\n"));
        assert_eq!(compiled.messages[1].role, "tool");
    }

    #[test]
    fn truncates_large_tool_outputs_deterministically() {
        let call_id = "call_big";
        let input = json!([{
            "type":"function_call",
            "call_id": call_id,
            "name": "read_file_range",
            "arguments": "{\"path\":\"big.txt\"}"
        }, {
            "type":"function_call_output",
            "call_id": call_id,
            "output":"x".repeat(MAX_TOOL_OUTPUT_CHARS + 32)
        }]);
        let compiled = compile_responses_input(&input);
        assert!(compiled.messages[1].content.contains("[truncated"));
        assert!(compiled.messages[1].content.len() < MAX_TOOL_OUTPUT_CHARS + 512);
    }

    #[test]
    fn ignores_image_parts_in_regular_messages() {
        let input = json!([{
            "role":"user",
            "content":[
                {"type":"input_text","text":"please inspect this"},
                {"type":"input_image","image_url":"data:image/jpeg;base64,AAAAAAAAAA"}
            ]
        }]);
        let compiled = compile_responses_input(&input);
        assert_eq!(compiled.messages.len(), 1);
        assert_eq!(compiled.messages[0].content, "please inspect this");
    }

    #[test]
    fn skips_display_only_text_even_if_metadata_is_missing() {
        let input = json!([
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "---\n**DeepSeek Thinking**\n> hidden\n---" }]
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "已使用工具 `list_directory`" }]
            },
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "continue" }]
            }
        ]);
        let compiled = compile_responses_input(&input);

        assert_eq!(compiled.messages.len(), 1);
        assert_eq!(compiled.messages[0].role, "user");
        assert_eq!(compiled.messages[0].content, "continue");
    }

    #[test]
    fn redacts_inline_data_urls_in_tool_facts() {
        let input = json!([{
            "type":"function_call_output",
            "call_id":"call_image",
            "output":"screenshot=data:image/png;base64,AAAAAAAAAABBBBBBBBBB"
        }]);
        let compiled = compile_responses_input(&input);
        assert!(compiled.messages[0]
            .content
            .contains("[inline-data-url omitted"));
        assert!(compiled.messages[0].content.contains("hash="));
        assert!(!compiled.messages[0]
            .content
            .contains("AAAAAAAAAABBBBBBBBBB"));
    }

    #[test]
    fn treats_tool_role_messages_as_verified_facts() {
        let input = json!([{
            "role":"tool",
            "content":[{"type":"output_text","text":"tool result text"}]
        }]);
        let compiled = compile_responses_input(&input);
        assert_eq!(compiled.diagnostic.verified_fact_items, 1);
        assert_eq!(compiled.messages.len(), 1);
        assert_eq!(compiled.messages[0].role, "system");
        assert!(compiled.messages[0].content.contains("role=tool"));
        assert!(compiled.messages[0].content.contains("tool result text"));
    }

    #[test]
    fn preserves_compaction_items_as_historical_context() {
        let input = json!([{
            "type":"compaction",
            "status":"completed",
            "summary":[{"type":"summary_text","text":"Earlier facts: Cargo.toml existed."}]
        }]);
        let compiled = compile_responses_input(&input);
        assert_eq!(compiled.messages.len(), 1);
        assert_eq!(compiled.messages[0].role, "system");
        assert!(compiled.messages[0]
            .content
            .contains("Recovered CodeSeeX compaction"));
        assert!(compiled.messages[0].content.contains("Cargo.toml existed"));
    }
}
