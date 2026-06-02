use axum::body::Bytes;
use base64::{engine::general_purpose, Engine as _};
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) fn reasoning_response_item(reasoning: &str, visible_summary: bool) -> Value {
    reasoning_response_item_with_id(
        &format!("rs_{}", Uuid::new_v4().simple()),
        reasoning,
        visible_summary,
    )
}

pub(crate) fn reasoning_response_item_with_id(
    id: &str,
    reasoning: &str,
    visible_summary: bool,
) -> Value {
    json!({
        "id": id,
        "type": "reasoning",
        "status": "completed",
        "summary": if visible_summary {
            vec![json!({
                "type": "summary_text",
                "text": reasoning,
                "title": "DeepSeek Thinking"
            })]
        } else {
            Vec::<Value>::new()
        },
        "encrypted_content": encode_reasoning_content(reasoning),
        "content": Value::Null
    })
}

pub(crate) fn function_call_sse_added(
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
                "type": "function_call",
                "status": "in_progress",
                "call_id": item["call_id"],
                "name": item["name"],
                "arguments": ""
            },
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

pub(crate) fn reasoning_done_sse_events(
    response_id: &str,
    output_index: u64,
    item_id: &str,
    reasoning: &str,
    sequence: &mut u64,
) -> (Bytes, Value) {
    let item = reasoning_response_item_with_id(item_id, reasoning, true);
    let mut bytes = sse_bytes(
        "response.reasoning_summary_text.done",
        json!({
            "type": "response.reasoning_summary_text.done",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "summary_index": 0,
            "text": reasoning,
            "sequence_number": next_sequence(sequence)
        }),
    )
    .to_vec();
    bytes.extend_from_slice(&sse_bytes(
        "response.reasoning_summary_part.done",
        json!({
            "type": "response.reasoning_summary_part.done",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "summary_index": 0,
            "part": { "type": "summary_text", "text": reasoning },
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

pub(crate) fn thinking_display_added_sse_events(
    response_id: &str,
    output_index: u64,
    item_id: &str,
    prefix: &str,
    sequence: &mut u64,
) -> Bytes {
    let item = thinking_display_stream_item(item_id, "");
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
            "item_id": item_id,
            "output_index": output_index,
            "content_index": 0,
            "part": { "type": "output_text", "text": "", "annotations": [] },
            "sequence_number": next_sequence(sequence)
        }),
    ));
    if !prefix.is_empty() {
        bytes.extend_from_slice(&sse_bytes(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "response_id": response_id,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "delta": prefix,
                "sequence_number": next_sequence(sequence)
            }),
        ));
    }
    Bytes::from(bytes)
}

pub(crate) fn thinking_display_prefix() -> &'static str {
    "**DeepSeek Thinking**\n"
}

pub(crate) fn thinking_display_delta_sse_event(
    response_id: &str,
    output_index: u64,
    item_id: &str,
    delta: &str,
    sequence: &mut u64,
) -> Bytes {
    sse_bytes(
        "response.output_text.delta",
        json!({
            "type": "response.output_text.delta",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "content_index": 0,
            "delta": delta,
            "sequence_number": next_sequence(sequence)
        }),
    )
}

pub(crate) fn thinking_display_done_sse_events(
    response_id: &str,
    output_index: u64,
    item_id: &str,
    text: &str,
    sequence: &mut u64,
) -> (Bytes, Value) {
    let item = thinking_display_stream_item(item_id, text);
    let part = item
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .cloned()
        .unwrap_or_else(|| json!({ "type": "output_text", "text": text, "annotations": [] }));
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
    (Bytes::from(bytes), item)
}

pub(crate) fn thinking_display_stream_item(item_id: &str, text: &str) -> Value {
    json!({
        "id": item_id,
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "phase": "commentary",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        "codeseex_display_only": "thinking_markdown",
        "metadata": { "codeseex_display_only": true, "kind": "thinking_markdown" }
    })
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

pub(crate) fn quote_thinking_delta(delta: &str, at_line_start: &mut bool) -> String {
    let source = delta.replace("\r\n", "\n").replace('\r', "\n");
    let mut output = String::new();
    for ch in source.chars() {
        if *at_line_start {
            output.push_str("> ");
            *at_line_start = false;
        }
        output.push(ch);
        if ch == '\n' {
            *at_line_start = true;
        }
    }
    output
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
    let event_name = if item.pointer("/action/type").and_then(Value::as_str) == Some("open") {
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
    let mut bytes = sse_bytes(
        "response.function_call_arguments.done",
        json!({
            "type": "response.function_call_arguments.done",
            "response_id": response_id,
            "item_id": item["id"],
            "output_index": output_index,
            "name": item["name"],
            "arguments": item["arguments"],
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

fn encode_reasoning_content(text: &str) -> String {
    general_purpose::STANDARD.encode(text.as_bytes())
}

pub(crate) fn decode_reasoning_content(value: &str) -> Option<String> {
    general_purpose::STANDARD
        .decode(value.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
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
