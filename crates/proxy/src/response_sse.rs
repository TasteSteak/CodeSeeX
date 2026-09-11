use axum::body::Bytes;
use base64::{engine::general_purpose, Engine as _};
use codeseex_core::AppConfig;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::responses::compaction::{decode_opaque_text, encode_opaque_text};

const REASONING_PREFIX: &str = "codeseex-reasoning-v1:";

/// A hidden reasoning carrier.
///
/// The provider's reasoning text is kept only in `encrypted_content` so it can
/// be replayed to DeepSeek on the next turn. CodeSeeX does not present it to
/// Codex and never makes it visible: what the client shows is Codex's decision.
pub(crate) fn reasoning_response_item(
    config: &AppConfig,
    reasoning: &str,
) -> Value {
    reasoning_response_item_with_id(
        config,
        &format!("rs_{}", Uuid::new_v4().simple()),
        reasoning,
    )
}

pub(crate) fn reasoning_response_item_with_id(
    config: &AppConfig,
    id: &str,
    reasoning: &str,
) -> Value {
    let mut item = json!({
        "id": id,
        "type": "reasoning",
        "status": "completed",
        "summary": Vec::<Value>::new(),
        "content": Value::Null
    });
    if let Ok(encrypted_content) = encode_reasoning_content(config, reasoning) {
        item["encrypted_content"] = Value::String(encrypted_content);
    }
    item
}

pub(crate) fn function_call_sse_added(
    response_id: &str,
    output_index: u64,
    item: &Value,
    sequence: &mut u64,
) -> Bytes {
    let mut added_item = json!({
        "id": item["id"],
        "type": "function_call",
        "status": "in_progress",
        "call_id": item["call_id"],
        "name": item["name"],
        "arguments": ""
    });
    if let Some(namespace) = item.get("namespace") {
        added_item["namespace"] = namespace.clone();
    }
    sse_bytes(
        "response.output_item.added",
        json!({
            "type": "response.output_item.added",
            "response_id": response_id,
            "output_index": output_index,
            "item": added_item,
            "sequence_number": next_sequence(sequence)
        }),
    )
}

pub(crate) fn custom_tool_call_sse_added(
    response_id: &str,
    output_index: u64,
    item: &Value,
    sequence: &mut u64,
) -> Bytes {
    sse_bytes(
        "response.output_item.added",
        json!({
            "type": "response.output_item.added",
            "response_id": response_id,
            "output_index": output_index,
            "item": {
                "id": item["id"],
                "type": "custom_tool_call",
                "status": "in_progress",
                "call_id": item["call_id"],
                "name": item["name"],
                "input": ""
            },
            "sequence_number": next_sequence(sequence)
        }),
    )
}

pub(crate) fn custom_tool_call_sse_done(
    response_id: &str,
    output_index: u64,
    item: &Value,
    sequence: &mut u64,
) -> Bytes {
    let mut bytes = sse_bytes(
        "response.custom_tool_call_input.done",
        json!({
            "type": "response.custom_tool_call_input.done",
            "response_id": response_id,
            "item_id": item["id"],
            "output_index": output_index,
            "input": item["input"],
            "sequence_number": next_sequence(sequence)
        }),
    )
    .to_vec();
    let done = sse_bytes(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    );
    bytes.extend_from_slice(&done);
    Bytes::from(bytes)
}

pub(crate) fn message_item_sse_events(
    response_id: &str,
    output_index: u64,
    item: &Value,
    sequence: &mut u64,
) -> Bytes {
    let item_id = item
        .get("id")
        .cloned()
        .unwrap_or_else(|| json!(format!("msg_{}", Uuid::new_v4().simple())));
    let part = item
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .cloned()
        .unwrap_or_else(|| json!({ "type": "output_text", "text": "", "annotations": [] }));
    let text = part
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut added_item = item.clone();
    added_item["status"] = Value::String("in_progress".to_owned());
    added_item["content"] = Value::Array(Vec::new());
    let mut bytes = sse_bytes(
        "response.output_item.added",
        json!({
            "type": "response.output_item.added",
            "response_id": response_id,
            "output_index": output_index,
            "item": added_item,
            "sequence_number": next_sequence(sequence)
        }),
    )
    .to_vec();
    bytes.extend_from_slice(&sse_bytes(
        "response.content_part.added",
        json!({
            "type": "response.content_part.added",
            "response_id": response_id,
            "item_id": item_id.clone(),
            "output_index": output_index,
            "content_index": 0,
            "part": { "type": "output_text", "text": "", "annotations": [] },
            "sequence_number": next_sequence(sequence)
        }),
    ));
    if !text.is_empty() {
        bytes.extend_from_slice(&sse_bytes(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "response_id": response_id,
                "item_id": item_id.clone(),
                "output_index": output_index,
                "content_index": 0,
                "delta": text.clone(),
                "sequence_number": next_sequence(sequence)
            }),
        ));
    }
    bytes.extend_from_slice(&sse_bytes(
        "response.output_text.done",
        json!({
            "type": "response.output_text.done",
            "response_id": response_id,
            "item_id": item_id.clone(),
            "output_index": output_index,
            "content_index": 0,
            "text": text.clone(),
            "sequence_number": next_sequence(sequence)
        }),
    ));
    bytes.extend_from_slice(&sse_bytes(
        "response.content_part.done",
        json!({
            "type": "response.content_part.done",
            "response_id": response_id,
            "item_id": item_id.clone(),
            "output_index": output_index,
            "content_index": 0,
            "part": part,
            "sequence_number": next_sequence(sequence)
        }),
    ));
    bytes.extend_from_slice(&sse_bytes(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    ));
    Bytes::from(bytes)
}

pub(crate) fn hidden_reasoning_item_sse_events(
    response_id: &str,
    output_index: u64,
    item: &Value,
    sequence: &mut u64,
) -> Bytes {
    let mut bytes = sse_bytes(
        "response.output_item.added",
        json!({
            "type": "response.output_item.added",
            "response_id": response_id,
            "output_index": output_index,
            "item": {
                "id": item["id"],
                "type": "reasoning",
                "status": "in_progress",
                "summary": []
            },
            "sequence_number": next_sequence(sequence)
        }),
    )
    .to_vec();
    bytes.extend_from_slice(&sse_bytes(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    ));
    Bytes::from(bytes)
}

pub(crate) fn streaming_message_done_sse_events(
    response_id: &str,
    output_index: u64,
    item_id: &str,
    text: &str,
    phase: &str,
    sequence: &mut u64,
) -> (Bytes, Value) {
    let item = json!({
        "id": item_id,
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "phase": phase,
        "content": [{ "type": "output_text", "text": text, "annotations": [] }]
    });
    let mut bytes = sse_bytes(
        "response.output_text.done",
        json!({
            "type": "response.output_text.done",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "content_index": 0,
            "text": text,
            "sequence_number": next_sequence(sequence)
        }),
    )
    .to_vec();
    bytes.extend_from_slice(&sse_bytes(
        "response.content_part.done",
        json!({
            "type": "response.content_part.done",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "content_index": 0,
            "part": item["content"][0],
            "sequence_number": next_sequence(sequence)
        }),
    ));
    bytes.extend_from_slice(&sse_bytes(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    ));
    (Bytes::from(bytes), item)
}

pub(crate) fn generic_output_item_sse_events(
    response_id: &str,
    output_index: u64,
    item: &Value,
    sequence: &mut u64,
) -> Bytes {
    let mut bytes = sse_bytes(
        "response.output_item.added",
        json!({
            "type": "response.output_item.added",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    )
    .to_vec();
    bytes.extend_from_slice(&sse_bytes(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    ));
    Bytes::from(bytes)
}

pub(crate) fn proxy_tool_call_sse_events(
    response_id: &str,
    output_index: u64,
    item: &Value,
    sequence: &mut u64,
) -> Bytes {
    let mut added_item = item.clone();
    added_item["status"] = Value::String("in_progress".to_owned());
    let mut bytes = sse_bytes(
        "response.output_item.added",
        json!({
            "type": "response.output_item.added",
            "response_id": response_id,
            "output_index": output_index,
            "item": added_item,
            "sequence_number": next_sequence(sequence)
        }),
    )
    .to_vec();
    bytes.extend_from_slice(&sse_bytes(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    ));
    Bytes::from(bytes)
}

pub(crate) fn web_search_call_sse_events(
    response_id: &str,
    output_index: u64,
    item: &Value,
    sequence: &mut u64,
) -> Bytes {
    let mut bytes = sse_bytes(
        "response.output_item.added",
        json!({
            "type": "response.output_item.added",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    )
    .to_vec();
    bytes.extend_from_slice(&sse_bytes(
        "response.web_search_call.in_progress",
        json!({
            "type": "response.web_search_call.in_progress",
            "response_id": response_id,
            "output_index": output_index,
            "item_id": item["id"],
            "sequence_number": next_sequence(sequence)
        }),
    ));
    let action_type = item.pointer("/action/type").and_then(Value::as_str);
    let event_name = if matches!(action_type, Some("open") | Some("open_page")) {
        "response.web_search_call.opening"
    } else {
        "response.web_search_call.searching"
    };
    bytes.extend_from_slice(&sse_bytes(
        event_name,
        json!({
            "type": event_name,
            "response_id": response_id,
            "output_index": output_index,
            "item_id": item["id"],
            "action": item.get("action").cloned().unwrap_or(Value::Null),
            "sequence_number": next_sequence(sequence)
        }),
    ));
    bytes.extend_from_slice(&sse_bytes(
        "response.web_search_call.completed",
        json!({
            "type": "response.web_search_call.completed",
            "response_id": response_id,
            "output_index": output_index,
            "item_id": item["id"],
            "sequence_number": next_sequence(sequence)
        }),
    ));
    bytes.extend_from_slice(&sse_bytes(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    ));
    Bytes::from(bytes)
}

pub(crate) fn function_call_sse_done(
    response_id: &str,
    output_index: u64,
    item: &Value,
    sequence: &mut u64,
) -> Bytes {
    let mut arguments_done = json!({
        "type": "response.function_call_arguments.done",
        "response_id": response_id,
        "item_id": item["id"],
        "output_index": output_index,
        "name": item["name"],
        "arguments": item["arguments"],
        "sequence_number": next_sequence(sequence)
    });
    if let Some(namespace) = item.get("namespace") {
        arguments_done["namespace"] = namespace.clone();
    }
    let mut bytes = sse_bytes("response.function_call_arguments.done", arguments_done).to_vec();
    let done = sse_bytes(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
            "sequence_number": next_sequence(sequence)
        }),
    );
    bytes.extend_from_slice(&done);
    Bytes::from(bytes)
}

pub(crate) fn stream_failed_event(
    response_id: &str,
    model: &str,
    created_at: u64,
    sequence: &mut u64,
    code: &str,
    message: &str,
) -> Bytes {
    sse_bytes(
        "response.failed",
        json!({
            "type": "response.failed",
            "response": {
                "id": response_id,
                "object": "response",
                "created_at": created_at,
                "model": model,
                "status": "failed",
                "error": {
                    "code": code,
                    "message": message
                }
            },
            "sequence_number": next_sequence(sequence)
        }),
    )
}

pub(crate) fn next_sequence(sequence: &mut u64) -> u64 {
    *sequence += 1;
    *sequence
}

pub(crate) fn sse_bytes(event: &str, payload: Value) -> Bytes {
    let data = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_owned());
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

fn encode_reasoning_content(config: &AppConfig, text: &str) -> anyhow::Result<String> {
    encode_opaque_text(config, REASONING_PREFIX, text)
}

pub(crate) fn decode_reasoning_content(config: &AppConfig, value: &str) -> Option<String> {
    decode_opaque_text(config, REASONING_PREFIX, value)
        .ok()
        .or_else(|| {
            general_purpose::STANDARD
                .decode(value.trim())
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        })
}

pub(crate) fn take_sse_frame(buffer: &mut String) -> Option<String> {
    let lf = buffer.find("\n\n").map(|index| (index, 2_usize));
    let crlf = buffer.find("\r\n\r\n").map(|index| (index, 4_usize));
    let (index, delimiter_len) = match (lf, crlf) {
        (Some(left), Some(right)) => {
            if left.0 <= right.0 {
                left
            } else {
                right
            }
        }
        (Some(value), None) | (None, Some(value)) => value,
        (None, None) => return None,
    };
    let frame = buffer[..index].to_owned();
    buffer.replace_range(..index + delimiter_len, "");
    Some(frame)
}

pub(crate) fn sse_data(frame: &str) -> Option<String> {
    let parts: Vec<_> = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}
