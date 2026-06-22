use crate::tools::ownership::{ChatToolCall, ToolCallPartition};
use serde_json::{json, Value};
use std::hash::{Hash, Hasher};

pub(crate) fn stable_payload_hash(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(crate) fn safe_request_shape(input: &Value) -> Value {
    json!({
        "input_items": input.get("input").and_then(Value::as_array).map(Vec::len),
        "has_prompt_cache_key": input.get("prompt_cache_key").is_some(),
        "has_client_metadata": input.get("client_metadata").is_some(),
        "has_previous_response_id": input
            .get("previous_response_id")
            .and_then(Value::as_str)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false),
        "request_hash": stable_payload_hash(input),
        "input_hash": stable_payload_hash(input.get("input").unwrap_or(&Value::Null))
    })
}

pub(crate) fn safe_usage(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let input = usage_u64(usage, &["input_tokens", "prompt_tokens"]).unwrap_or(0);
    let cached = usage_u64(
        usage,
        &[
            "cached_input_tokens",
            "cache_hit_input_tokens",
            "prompt_cache_hit_tokens",
            "cache_hit_tokens",
        ],
    )
    .or_else(|| {
        usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(value_to_u64)
    })
    .or_else(|| {
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(value_to_u64)
    })
    .unwrap_or(0);
    let miss = usage_u64(
        usage,
        &[
            "cache_miss_input_tokens",
            "input_cache_miss_tokens",
            "prompt_cache_miss_tokens",
            "cache_miss_tokens",
        ],
    )
    .unwrap_or_else(|| input.saturating_sub(cached));
    let output = usage_u64(usage, &["output_tokens", "completion_tokens"]).unwrap_or(0);
    let total = usage_u64(usage, &["total_tokens"]).unwrap_or_else(|| input.saturating_add(output));
    json!({
        "input_tokens": input,
        "cached_input_tokens": cached,
        "cache_miss_input_tokens": miss,
        "output_tokens": output,
        "total_tokens": total
    })
}

pub(crate) fn context_compile_diagnostic_event(
    id: &str,
    context: &Value,
    request: &Value,
    runtime_context_storage: &Value,
) -> Value {
    json!({
        "id": id,
        "request": safe_request_shape(request),
        "runtime_context_storage": runtime_context_storage,
        "context": context
    })
}

pub(crate) fn upstream_call_usage_breakdown_event(
    id: &str,
    phase: &str,
    iteration: u32,
    request: &Value,
    payload: &Value,
    usage: Option<&Value>,
    duration_ms: Option<u64>,
    final_handoff: bool,
) -> Value {
    json!({
        "id": id,
        "phase": phase,
        "iteration": iteration,
        "duration_ms": duration_ms,
        "final_handoff": final_handoff,
        "request": safe_request_shape(request),
        "payload": {
            "message_count": payload.get("messages").and_then(Value::as_array).map(Vec::len),
            "tools_count": payload.get("tools").and_then(Value::as_array).map(Vec::len),
            "payload_hash": stable_payload_hash(payload)
        },
        "usage": safe_usage(usage)
    })
}

pub(crate) fn client_tool_handoff_diagnostic_event(
    id: &str,
    phase: &str,
    iteration: u32,
    request: &Value,
    context: &Value,
    runtime_context_storage: &Value,
    partition: Option<&ToolCallPartition>,
    usage: Option<&Value>,
) -> Value {
    json!({
        "id": id,
        "phase": phase,
        "iteration": iteration,
        "lifecycle": "client_tool_handoff",
        "request": safe_request_shape(request),
        "runtime_context_storage": runtime_context_storage,
        "context": context,
        "tools": partition.map(partition_summary).unwrap_or_else(|| json!({})),
        "usage": safe_usage(usage)
    })
}

pub(crate) fn retry_cache_diagnostic_event(
    id: &str,
    requested_model: Option<&str>,
    model: Option<&str>,
    request: &Value,
    payload: Option<&Value>,
    error_kind: &str,
) -> Value {
    json!({
        "id": id,
        "requested_model": requested_model,
        "model": model,
        "error_kind": error_kind,
        "request": safe_request_shape(request),
        "payload": payload.map(|payload| json!({
            "message_count": payload.get("messages").and_then(Value::as_array).map(Vec::len),
            "tools_count": payload.get("tools").and_then(Value::as_array).map(Vec::len),
            "payload_hash": stable_payload_hash(payload)
        }))
    })
}

pub(crate) fn tool_result_size_summary(result: &Value) -> Value {
    let serialized = serde_json::to_string(result).unwrap_or_default();
    json!({
        "result_json_chars": serialized.chars().count(),
        "result_hash": stable_payload_hash(result),
        "ok": result.get("ok").and_then(Value::as_bool),
        "diagnostic_bytes": result.pointer("/_diagnostics/bytes").and_then(value_to_u64),
        "diagnostic_opened_count": result.pointer("/_diagnostics/opened_count").and_then(value_to_u64),
        "diagnostic_failure_count": result.pointer("/_diagnostics/failure_count").and_then(value_to_u64)
    })
}

fn partition_summary(partition: &ToolCallPartition) -> Value {
    json!({
        "code": tool_names(&partition.code),
        "hosted": tool_names(&partition.hosted),
        "native": tool_names(&partition.native),
        "external": tool_names(&partition.external),
        "unknown": tool_names(&partition.unknown),
        "counts": {
            "code": partition.code.len(),
            "hosted": partition.hosted.len(),
            "native": partition.native.len(),
            "external": partition.external.len(),
            "unknown": partition.unknown.len()
        }
    })
}

fn tool_names(calls: &[ChatToolCall]) -> Vec<&str> {
    calls.iter().map(|call| call.name.as_str()).collect()
}

fn usage_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(value_to_u64)
}

fn value_to_u64(value: &Value) -> Option<u64> {
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
