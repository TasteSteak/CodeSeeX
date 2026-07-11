use crate::http_utils::now_seconds;
use crate::response_sse::{
    custom_tool_call_sse_added, custom_tool_call_sse_done, function_call_sse_added,
    function_call_sse_done, next_sequence, sse_bytes,
};
use crate::responses::stream_tool_calls::StreamingVisibleToolBridge;
use crate::text::compact_line;
use crate::tool_passthrough::ToolContext;
use crate::tools::ownership::ChatToolCall;
use crate::tools::response_items::{
    apply_patch_input_normalization_diagnostic, native_apply_patch_response_item_from_chat_call,
};
use axum::body::Bytes;
use axum::http::{header, HeaderMap, StatusCode};
use codeseex_core::{AppConfig, UserConfig};
use codeseex_store::Store;
use serde_json::{json, Value};
use uuid::Uuid;

pub(super) fn prepend_response_output_items(response: &mut Value, mut items: Vec<Value>) {
    if items.is_empty() {
        return;
    }
    let Some(output) = response.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };
    items.append(output);
    *output = items;
}

pub(super) fn upstream_error_detail(body_json: Option<&Value>, bytes: &[u8]) -> Value {
    let message = body_json
        .and_then(|body| body.pointer("/error/message").and_then(Value::as_str))
        .or_else(|| body_json.and_then(|body| body.get("message").and_then(Value::as_str)))
        .map(str::to_owned)
        .unwrap_or_else(|| compact_line(&String::from_utf8_lossy(bytes), 2_000));
    json!({
        "message": message,
        "body": {
            "omitted": true,
            "bytes": bytes.len(),
            "note": "Raw upstream error bodies are not logged to avoid leaking secrets or prompt content."
        }
    })
}

pub(super) fn upstream_body_read_error_detail(
    id: &str,
    requested_model: Option<&str>,
    model: Option<&str>,
    status: StatusCode,
    headers: &HeaderMap,
    error: &reqwest::Error,
) -> Value {
    json!({
        "id": id,
        "requested_model": requested_model,
        "model": model,
        "status": status.as_u16(),
        "error": error.to_string(),
        "error_kind": {
            "body": error.is_body(),
            "connect": error.is_connect(),
            "decode": error.is_decode(),
            "request": error.is_request(),
            "status": error.is_status(),
            "timeout": error.is_timeout()
        },
        "upstream_response": upstream_response_header_summary(headers),
        "body": {
            "omitted": true,
            "note": "Raw upstream bodies are not logged to avoid leaking secrets or prompt content."
        }
    })
}

pub(super) fn upstream_json_parse_error_detail(
    id: &str,
    requested_model: Option<&str>,
    model: Option<&str>,
    status: StatusCode,
    headers: &HeaderMap,
    bytes_len: usize,
    error: &serde_json::Error,
) -> Value {
    json!({
        "id": id,
        "requested_model": requested_model,
        "model": model,
        "status": status.as_u16(),
        "error": error.to_string(),
        "error_kind": "json_parse",
        "upstream_response": upstream_response_header_summary(headers),
        "body": {
            "omitted": true,
            "bytes": bytes_len,
            "note": "Raw upstream bodies are not logged to avoid leaking secrets or prompt content."
        }
    })
}

fn upstream_response_header_summary(headers: &HeaderMap) -> Value {
    json!({
        "content_type": header_value(headers, &header::CONTENT_TYPE),
        "content_encoding": header_value(headers, &header::CONTENT_ENCODING),
        "content_length": header_value(headers, &header::CONTENT_LENGTH),
        "transfer_encoding": header_value(headers, &header::TRANSFER_ENCODING)
    })
}

fn header_value(headers: &HeaderMap, name: &'static header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

pub(super) fn request_completed_detail(
    id: &str,
    requested_model: Option<&str>,
    model: Option<&str>,
    lifecycle: Option<&str>,
    response: Option<&Value>,
) -> Value {
    let mut detail = json!({
        "id": id,
        "requested_model": requested_model,
        "model": model,
    });
    if let Some(lifecycle) = lifecycle {
        detail["lifecycle"] = json!(lifecycle);
    }
    if let Some(usage) = response.and_then(response_usage_for_log) {
        let input_tokens = usage_u64(usage, &["input_tokens", "prompt_tokens"]).unwrap_or(0);
        let cached_input_tokens = usage_u64(
            usage,
            &[
                "cached_input_tokens",
                "input_cached_tokens",
                "prompt_cache_hit_tokens",
                "cache_hit_input_tokens",
                "cached_tokens",
            ],
        )
        .or_else(|| {
            usage
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(value_to_u64_for_log)
        })
        .or_else(|| {
            usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(value_to_u64_for_log)
        })
        .unwrap_or(0);
        let cache_miss_input_tokens = usage_u64(
            usage,
            &[
                "cache_miss_input_tokens",
                "input_cache_miss_tokens",
                "prompt_cache_miss_tokens",
                "cache_miss_tokens",
            ],
        )
        .unwrap_or_else(|| input_tokens.saturating_sub(cached_input_tokens));
        let output_tokens = usage_u64(usage, &["output_tokens", "completion_tokens"]).unwrap_or(0);
        let total_tokens = usage_u64(usage, &["total_tokens"]).unwrap_or_else(|| {
            cached_input_tokens
                .saturating_add(cache_miss_input_tokens)
                .saturating_add(output_tokens)
        });
        detail["input_tokens"] = json!(input_tokens);
        detail["cached_input_tokens"] = json!(cached_input_tokens);
        detail["cache_miss_input_tokens"] = json!(cache_miss_input_tokens);
        detail["output_tokens"] = json!(output_tokens);
        detail["total_tokens"] = json!(total_tokens);
    }
    detail
}

fn response_usage_for_log(response: &Value) -> Option<&Value> {
    response
        .get("usage")
        .or_else(|| response.pointer("/response/usage"))
        .or_else(|| response.pointer("/choices/0/usage"))
}

pub(super) fn failed_billable_response(
    id: &str,
    model: &str,
    code: &str,
    message: &str,
    usage: &Value,
) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": now_seconds(),
        "model": model,
        "status": "failed",
        "error": {
            "code": code,
            "message": message
        },
        "incomplete_details": Value::Null,
        "parallel_tool_calls": true,
        "output": [],
        "usage": usage
    })
}

pub(super) fn client_handoff_guard_terminal_response(
    id: &str,
    model: &str,
    code: &str,
    message: &str,
    usage: &Value,
) -> Value {
    failed_billable_response(id, model, code, message, usage)
}

pub(super) fn client_handoff_guard_terminal_diagnostic(
    stop: &codeseex_store::ClientToolHandoffGuardStop,
) -> Value {
    json!({
        "error": stop.message.clone(),
        "codeseex_lifecycle": "failed_billable",
        "client_tool_handoff_guard_stopped": true,
        "client_tool_handoff_guard": stop.diagnostic()
    })
}

pub(super) fn client_handoff_guard_terminal_sse(
    response_id: &str,
    model: &str,
    code: &str,
    message: &str,
    usage: &Value,
    sequence: &mut u64,
) -> Bytes {
    let response = client_handoff_guard_terminal_response(response_id, model, code, message, usage);
    sse_bytes(
        "response.failed",
        json!({
            "type": "response.failed",
            "response": response,
            "sequence_number": next_sequence(sequence)
        }),
    )
}

fn usage_u64(usage: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .filter_map(|key| usage.get(*key))
        .find_map(value_to_u64_for_log)
}

fn value_to_u64_for_log(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
        .or_else(|| {
            value
                .as_f64()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number as u64)
        })
}

pub(super) fn response_id_from_input(input: &Value) -> String {
    input
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("resp_{}", Uuid::new_v4().simple()))
}

pub(super) async fn record_client_tool_handoff_guard_stop(
    store: &Store,
    request_id: &str,
    stop: &codeseex_store::ClientToolHandoffGuardStop,
) {
    let mut detail = stop.diagnostic();
    detail["id"] = json!(request_id);
    let _ = store
        .record_event(
            "warn",
            "client_tool_handoff_guard_diagnostic",
            "CodeSeeX stopped repeated client tool handoffs.",
            Some(&detail),
        )
        .await;
}

pub(super) async fn record_apply_patch_input_micro_repair_diagnostics(
    store: &Store,
    response_id: &str,
    calls: &[ChatToolCall],
) {
    for call in calls {
        record_apply_patch_input_micro_repair_diagnostic(store, response_id, call).await;
    }
}

pub(super) async fn record_apply_patch_input_micro_repair_diagnostic(
    store: &Store,
    response_id: &str,
    call: &ChatToolCall,
) {
    let diagnostic = apply_patch_input_normalization_diagnostic(&call.arguments);
    if diagnostic.blank_context_lines_repaired == 0 {
        return;
    }
    let _ = store
        .record_event(
            "info",
            "apply_patch_input_micro_repair_diagnostic",
            "CodeSeeX repaired blank apply_patch hunk context lines.",
            Some(&json!({
                "id": response_id,
                "call_id": call.id,
                "tool_name": call.name,
                "repair_kind": "blank_update_hunk_context_line",
                "blank_context_lines_repaired": diagnostic.blank_context_lines_repaired,
                "input_chars": diagnostic.input_chars,
            })),
        )
        .await;
}

pub(super) fn show_thinking_enabled(config: &AppConfig) -> bool {
    UserConfig::read_from(&config.config_path())
        .ok()
        .and_then(|user_config| user_config.ui.and_then(|ui| ui.show_thinking))
        .unwrap_or(true)
}

pub(super) fn native_apply_patch_client_tool_sse_events(
    response_id: &str,
    call: &ChatToolCall,
    visible_tool_bridge: &mut StreamingVisibleToolBridge,
    output_index: &mut u64,
    sequence: &mut u64,
) -> (Bytes, Value) {
    if let Some(finished) =
        visible_tool_bridge.finish_native_apply_patch(response_id, call, sequence)
    {
        return (finished.bytes, finished.item);
    }
    let item = native_apply_patch_response_item_from_chat_call(call);
    let call_output_index = *output_index;
    *output_index += 1;
    let mut bytes =
        custom_tool_call_sse_added(response_id, call_output_index, &item, sequence).to_vec();
    if let Some(input) = item
        .get("input")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        bytes.extend_from_slice(&sse_bytes(
            "response.custom_tool_call_input.delta",
            json!({
                "type": "response.custom_tool_call_input.delta",
                "response_id": response_id,
                "item_id": item["id"],
                "output_index": call_output_index,
                "delta": input,
                "sequence_number": next_sequence(sequence)
            }),
        ));
    }
    bytes.extend_from_slice(&custom_tool_call_sse_done(
        response_id,
        call_output_index,
        &item,
        sequence,
    ));
    (Bytes::from(bytes), item)
}

pub(super) fn external_client_tool_sse_events(
    response_id: &str,
    call: &ChatToolCall,
    external_tool_context: &ToolContext,
    visible_tool_bridge: &mut StreamingVisibleToolBridge,
    output_index: &mut u64,
    sequence: &mut u64,
) -> (Bytes, Value) {
    if external_tool_context.is_codex_tool_search_tool(&call.name) {
        if let Some(finished) = visible_tool_bridge.finish_codex_tool_search(
            response_id,
            call,
            external_tool_context,
            sequence,
        ) {
            return (finished.bytes, finished.item);
        }
        let item = external_tool_context.response_item_from_chat_call(call);
        let call_output_index = *output_index;
        *output_index += 1;
        let mut added_item = item.clone();
        added_item["status"] = Value::String("in_progress".to_owned());
        let mut bytes = sse_bytes(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "response_id": response_id,
                "output_index": call_output_index,
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
                "output_index": call_output_index,
                "item": item,
                "sequence_number": next_sequence(sequence)
            }),
        ));
        return (Bytes::from(bytes), item);
    }
    if let Some(finished) = visible_tool_bridge.finish_external_function(
        response_id,
        call,
        external_tool_context,
        sequence,
    ) {
        return (finished.bytes, finished.item);
    }
    let item = external_tool_context.response_item_from_chat_call(call);
    let call_output_index = *output_index;
    *output_index += 1;
    let mut bytes =
        function_call_sse_added(response_id, call_output_index, &item, sequence).to_vec();
    if !call.arguments.is_empty() {
        bytes.extend_from_slice(&sse_bytes(
            "response.function_call_arguments.delta",
            json!({
                "type": "response.function_call_arguments.delta",
                "response_id": response_id,
                "item_id": item["id"],
                "output_index": call_output_index,
                "delta": call.arguments,
                "sequence_number": next_sequence(sequence)
            }),
        ));
    }
    bytes.extend_from_slice(&function_call_sse_done(
        response_id,
        call_output_index,
        &item,
        sequence,
    ));
    (Bytes::from(bytes), item)
}
