use crate::http_utils::now_seconds;
use crate::response_sse::reasoning_response_item;
use crate::responses::usage::response_usage_from_chat_usage;
use crate::tools::chat_protocol::chat_tool_calls;
use crate::tools::ownership::{partition_tool_calls, proxy_executed_calls_in_order};
use crate::tools::response_items::{
    native_apply_patch_response_item_from_chat_call, proxy_visible_response_items,
};
use codeseex_core::AppConfig;
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) fn chat_completion_to_response(
    config: &AppConfig,
    id: &str,
    model: &str,
    chat: Value,
    visible_thinking_enabled: bool,
) -> Value {
    let message = chat
        .pointer("/choices/0/message")
        .cloned()
        .unwrap_or_else(|| json!({ "role": "assistant", "content": "" }));
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reasoning = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut output = Vec::new();
    if !reasoning.trim().is_empty() {
        output.push(reasoning_response_item(
            config,
            reasoning,
            visible_thinking_enabled,
        ));
    }
    output.push(json!({
        "id": format!("msg_{}", Uuid::new_v4().simple()),
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{ "type": "output_text", "text": text }]
    }));
    json!({
        "id": id,
        "object": "response",
        "created_at": now_seconds(),
        "model": model,
        "status": "completed",
        "error": Value::Null,
        "incomplete_details": Value::Null,
        "parallel_tool_calls": true,
        "output": output,
        "usage": response_usage_from_chat_usage(chat.get("usage"))
    })
}

pub(crate) fn chat_completion_tool_calls_to_response(
    config: &AppConfig,
    id: &str,
    model: &str,
    chat: Value,
    community_tools: &crate::community_tools::CommunityToolSet,
    tool_context: &crate::tool_passthrough::ToolContext,
    visible_thinking_enabled: bool,
) -> Value {
    let calls = chat_tool_calls(&chat);
    let mut output = Vec::new();
    if let Some(reasoning) = chat
        .pointer("/choices/0/message/reasoning_content")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        output.push(reasoning_response_item(
            config,
            reasoning,
            visible_thinking_enabled,
        ));
    }
    if let Some(text) = chat
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        output.push(json!({
            "id": format!("msg_{}", Uuid::new_v4().simple()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "phase": "commentary",
            "content": [{ "type": "output_text", "text": text }]
        }));
    }
    let partition = partition_tool_calls(calls.clone(), community_tools, tool_context);
    let proxy_executed_calls = proxy_executed_calls_in_order(&calls, &partition);
    output.extend(proxy_visible_response_items(&proxy_executed_calls));
    for call in partition.native {
        output.push(native_apply_patch_response_item_from_chat_call(&call));
    }
    for call in partition.external {
        output.push(tool_context.response_item_from_chat_call(&call));
    }
    json!({
        "id": id,
        "object": "response",
        "created_at": now_seconds(),
        "model": model,
        "status": "completed",
        "error": Value::Null,
        "incomplete_details": Value::Null,
        "parallel_tool_calls": true,
        "output": output,
        "usage": response_usage_from_chat_usage(chat.get("usage"))
    })
}

pub(crate) fn final_chat_turn_message(chat: &Value) -> Option<Value> {
    let text = chat
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reasoning_content = chat
        .pointer("/choices/0/message/reasoning_content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if (text.trim().is_empty() && reasoning_content.trim().is_empty())
        || text_is_thinking_display_markdown(text)
    {
        return None;
    }
    let mut message = json!({
        "role": "assistant",
        "content": text
    });
    if !reasoning_content.trim().is_empty() {
        message["reasoning_content"] = Value::String(reasoning_content.to_owned());
    }
    Some(message)
}

pub(crate) fn text_is_thinking_display_markdown(text: &str) -> bool {
    text.trim_start().starts_with("**DeepSeek Thinking**")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_turn_message_skips_thinking_display_markdown() {
        let chat = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "**DeepSeek Thinking**\n> hidden reasoning"
                }
            }]
        });

        assert!(final_chat_turn_message(&chat).is_none());
    }

    #[test]
    fn final_turn_message_preserves_reasoning_content_for_next_chat_turn() {
        let chat = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The answer.",
                    "reasoning_content": "The prior reasoning must be replayed."
                }
            }]
        });

        let message = final_chat_turn_message(&chat).expect("final assistant message");
        assert_eq!(message["content"], "The answer.");
        assert_eq!(
            message["reasoning_content"],
            "The prior reasoning must be replayed."
        );
    }
}
