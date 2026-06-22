use super::{
    latest_user_summary_from_input, merge_request_diagnostic, runtime_latest_user_summary,
    stable_hash_hex, ClientHandoffGuardState, ClientHandoffPendingKey, ClientHandoffTurnState,
    ClientToolHandoffGuardStop, StoreInner, MAX_CLIENT_HANDOFF_GUARD_TURNS,
    MAX_CLIENT_HANDOFF_PENDING_CALLS, MAX_USAGE_SEGMENT_SUMMARY_CHARS,
};
use chrono::Utc;
use codeseex_core::context::content_to_text;
use serde_json::{json, Value};
#[derive(Debug)]
pub(super) struct ClientHandoffOutputItem {
    pub(super) call_id: String,
    pub(super) failed: bool,
    pub(super) failure_summary: Option<String>,
}

pub(super) fn ensure_client_handoff_turn(guard: &mut ClientHandoffGuardState, turn_key: &str) {
    if !guard.turns.contains_key(turn_key) {
        guard.turn_order.push_back(turn_key.to_owned());
        guard
            .turns
            .insert(turn_key.to_owned(), ClientHandoffTurnState::default());
    }
    prune_client_handoff_guard(guard);
}

pub(super) fn prune_client_handoff_guard(guard: &mut ClientHandoffGuardState) {
    while guard.turn_order.len() > MAX_CLIENT_HANDOFF_GUARD_TURNS {
        let Some(oldest) = guard.turn_order.pop_front() else {
            break;
        };
        guard.turns.remove(&oldest);
        guard
            .pending_by_key
            .retain(|key, pending| key.turn_key != oldest && pending.turn_key != oldest);
    }
    if guard.pending_by_key.len() > MAX_CLIENT_HANDOFF_PENDING_CALLS {
        let excess = guard
            .pending_by_key
            .len()
            .saturating_sub(MAX_CLIENT_HANDOFF_PENDING_CALLS);
        let keys = guard
            .pending_by_key
            .keys()
            .take(excess)
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            guard.pending_by_key.remove(&key);
        }
    }
}

pub(super) fn client_handoff_pending_request_keys(request_id: &str, input: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(previous) = input
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        keys.push(previous.to_owned());
    }
    if !request_id.trim().is_empty() && !keys.iter().any(|key| key == request_id) {
        keys.push(request_id.to_owned());
    }
    keys
}

pub(super) fn unique_client_handoff_pending_key_for_turn(
    guard: &ClientHandoffGuardState,
    turn_key: &str,
    call_id: &str,
) -> Option<ClientHandoffPendingKey> {
    let mut matches = guard
        .pending_by_key
        .keys()
        .filter(|key| key.turn_key == turn_key && key.call_id == call_id);
    let first = matches.next()?.clone();
    matches.next().is_none().then_some(first)
}

pub(super) fn mark_request_guard_stopped(
    inner: &mut StoreInner,
    request_id: &str,
    stop: &ClientToolHandoffGuardStop,
) {
    if let Some(request) = inner.requests.get_mut(request_id) {
        request.diagnostic = Some(merge_request_diagnostic(
            request.diagnostic.as_ref(),
            &json!({
                "codeseex_lifecycle": "failed_billable",
                "client_tool_handoff_guard_stopped": true,
                "client_tool_handoff_guard": stop.diagnostic()
            }),
        ));
        request.updated_at = Utc::now();
    }
}

pub(super) fn client_handoff_guard_stop(
    reason: &str,
    turn_key: &str,
    tool_name: Option<&str>,
    arguments_hash: Option<&str>,
    failure_summary: Option<&str>,
    state: &ClientHandoffTurnState,
    repeated_signature_count: u32,
    consecutive_failure_count: u32,
) -> ClientToolHandoffGuardStop {
    let failure_summary = failure_summary.and_then(compact_client_handoff_failure_summary);
    let failure_summary_hash = failure_summary
        .as_ref()
        .map(|value| stable_hash_hex(value.as_bytes()));
    let message = match reason {
        "consecutive_failures" => format!(
            "CodeSeeX stopped repeated client tool handoffs after {consecutive_failure_count} consecutive failure(s) for tool '{}'.",
            tool_name.unwrap_or("unknown")
        ),
        "repeated_signature" => format!(
            "CodeSeeX stopped repeated client tool handoffs after the same tool call signature repeated {repeated_signature_count} time(s)."
        ),
        _ => "CodeSeeX stopped repeated client tool handoffs.".to_owned(),
    };
    ClientToolHandoffGuardStop {
        code: "client_tool_handoff_guard_stopped".to_owned(),
        message,
        reason: reason.to_owned(),
        turn_key: turn_key.to_owned(),
        tool_name: tool_name.map(str::to_owned),
        arguments_hash: arguments_hash.map(str::to_owned),
        failure_summary,
        failure_summary_hash,
        handoff_requests: state.handoff_requests,
        tool_calls: state.tool_calls,
        repeated_signature_count,
        consecutive_failure_count,
        cumulative_input_tokens: state.cumulative_input_tokens,
        cumulative_total_tokens: state.cumulative_total_tokens,
    }
}

pub(super) fn client_handoff_turn_key(input: &Value) -> String {
    runtime_latest_user_summary(input)
        .map(|summary| format!("summary:{}", stable_hash_hex(summary.as_bytes())))
        .or_else(|| {
            input
                .pointer("/_codeseex_runtime/original_input_hash")
                .and_then(Value::as_str)
                .map(|hash| format!("full_context:{hash}"))
        })
        .or_else(|| {
            input
                .get("prompt_cache_key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| format!("prompt_cache:{}", stable_hash_hex(value.as_bytes())))
        })
        .or_else(|| {
            latest_user_summary_from_input(input)
                .map(|summary| format!("input:{}", stable_hash_hex(summary.as_bytes())))
        })
        .unwrap_or_else(|| format!("request:{}", stable_hash_hex(input.to_string().as_bytes())))
}

pub(super) fn client_handoff_output_items(input: &Value) -> Vec<ClientHandoffOutputItem> {
    let items = input
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    items
        .iter()
        .filter(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call_output")
                    | Some("custom_tool_call_output")
                    | Some("tool_search_output")
            )
        })
        .filter_map(|item| {
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("tool_call_id"))
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)?
                .to_owned();
            let output = item
                .get("output")
                .or_else(|| item.get("content"))
                .or_else(|| item.get("result"))
                .unwrap_or(&Value::Null);
            let (failed, failure_summary) = client_handoff_output_failure(output);
            Some(ClientHandoffOutputItem {
                call_id,
                failed,
                failure_summary,
            })
        })
        .collect()
}

fn client_handoff_output_failure(output: &Value) -> (bool, Option<String>) {
    if output.get("ok").and_then(Value::as_bool) == Some(false)
        || output.get("error").is_some()
        || output
            .get("status")
            .and_then(Value::as_str)
            .map(|status| {
                let status = status.trim().to_ascii_lowercase();
                matches!(status.as_str(), "failed" | "error" | "cancelled")
            })
            .unwrap_or(false)
    {
        return (true, Some(content_to_text(output)));
    }
    let text = content_to_text(output);
    let normalized = text.trim().to_ascii_lowercase();
    let failed = normalized.starts_with("error:")
        || normalized.starts_with("failed:")
        || normalized.contains("apply_patch verification failed")
        || normalized.contains("tool execution failed")
        || normalized.contains("traceback (most recent call last)");
    (failed, failed.then_some(text))
}

fn compact_client_handoff_failure_summary(value: &str) -> Option<String> {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_USAGE_SEGMENT_SUMMARY_CHARS {
        return Some(text.to_owned());
    }
    Some(format!(
        "{}...",
        text.chars()
            .take(MAX_USAGE_SEGMENT_SUMMARY_CHARS.saturating_sub(1))
            .collect::<String>()
    ))
}
