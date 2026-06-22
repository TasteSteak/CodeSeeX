use crate::app_state::ProxyState;
use crate::responses::context::estimate_tokens_from_text;
use crate::tool_passthrough::ToolContext;
use crate::upstream::codex_request_markers;
use crate::upstream::deepseek::{
    should_adapt_tool_protocol,
    tool_protocol::{adapt_chat_tool_protocol, DeepSeekChatToolProtocolAdaptation},
};
use crate::upstream::payload::{request_shape_diagnostic, CodexServiceRequestKind};
use codeseex_core::context::request_looks_like_codex_full_context;
use codeseex_core::AppConfig;
use codeseex_store::{RequestStatus, Store};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) fn tool_exposure_diagnostic(
    request_id: &str,
    external_tool_context: &ToolContext,
    upstream_tools: &[Value],
    bridge_decision: &CodexToolSearchBridgeDecision,
    codeseex_enabled_tools: &[String],
    codeseex_base_tools_injected: bool,
    service_kind: CodexServiceRequestKind,
) -> Value {
    let upstream_names = upstream_tool_names(upstream_tools);
    let configurable_tools_disabled_by_config =
        codeseex_base_tools_injected && codeseex_enabled_tools.is_empty();
    let expected_codeseex_tools = if codeseex_base_tools_injected {
        upstream_tool_names(&crate::tools::upstream_tool_definitions(
            codeseex_enabled_tools,
        ))
    } else {
        Vec::new()
    };
    let missing_expected_codeseex_tools = expected_codeseex_tools
        .iter()
        .filter_map(|name| {
            (!upstream_names.iter().any(|upstream| upstream == name)).then(|| name.to_owned())
        })
        .collect::<Vec<_>>();
    let enabled_codeseex_tools_missing = !service_kind.is_service()
        && codeseex_base_tools_injected
        && !expected_codeseex_tools.is_empty()
        && !missing_expected_codeseex_tools.is_empty();
    json!({
        "id": request_id,
        "incoming_tool_items": external_tool_context.request_tool_items(),
        "discovered_tool_items": external_tool_context.discovered_tool_items(),
        "codeseex_enabled_tools": limited_tool_names(codeseex_enabled_tools.to_vec()),
        "codeseex_expected_upstream_tools": limited_tool_names(expected_codeseex_tools),
        "codeseex_base_tools_injected": codeseex_base_tools_injected,
        "configurable_tools_disabled_by_config": configurable_tools_disabled_by_config,
        "missing_expected_codeseex_tools": limited_tool_names(missing_expected_codeseex_tools),
        "warning": enabled_codeseex_tools_missing
            .then_some("enabled_codeseex_tools_missing_from_upstream_payload"),
        "external_callable_tools": limited_tool_names(external_tool_context.source_names()),
        "external_upstream_tools": limited_tool_names(external_tool_context.upstream_names()),
        "external_tool_budget": external_tool_context.external_tool_budget_diagnostic(),
        "final_upstream_tools": limited_tool_names(upstream_names.clone()),
        "codex_request_markers": {
            "client_metadata": bridge_decision.markers.client_metadata,
            "prompt_cache_key": bridge_decision.markers.prompt_cache_key,
            "metadata_installation_id": bridge_decision.markers.metadata_installation_id
        },
        "tool_search_bridge": {
            "injected": bridge_decision.injected,
            "reason": bridge_decision.reason,
            "suppressed_by_service_kind": service_kind.is_service().then_some(service_kind.label()),
            "has_tool_search_tool": upstream_names.iter().any(|name| name == "tool_search_tool"),
            "has_tool_search": upstream_names.iter().any(|name| name == "tool_search"),
            "upstream_had_tool_search": bridge_decision.upstream_had_tool_search,
            "codex_native_tool_surface": bridge_decision.codex_native_tool_surface
        },
        "interesting_tools": interesting_tool_names(&upstream_names)
    })
}

pub(super) fn service_lifecycle_for_kind(kind: CodexServiceRequestKind) -> Option<&'static str> {
    kind.is_service().then_some("service_ephemeral")
}

pub(super) fn service_completion_diagnostic(kind: CodexServiceRequestKind) -> Option<Value> {
    kind.is_service().then(|| {
        json!({
            "codeseex_lifecycle": "service_ephemeral",
            "codeseex_service_kind": kind.label()
        })
    })
}

pub(super) fn service_request_diagnostic(
    id: &str,
    endpoint: &str,
    kind: CodexServiceRequestKind,
    requested_model: Option<&str>,
    upstream_model: &str,
    tools_suppressed: bool,
    request: &Value,
) -> Value {
    let shape = request_shape_diagnostic(request);
    json!({
        "id": id,
        "endpoint": endpoint,
        "kind": kind.label(),
        "route": {
            "requested_model": requested_model,
            "model": upstream_model
        },
        "tools_suppressed": tools_suppressed,
        "thinking_disabled": true,
        "lifecycle": "service_ephemeral",
        "signals": shape.get("service_signals").cloned().unwrap_or(Value::Null),
        "estimated_text_chars": shape.get("estimated_text_chars").cloned().unwrap_or(Value::Null),
        "input_items": shape.get("input_items").cloned().unwrap_or(Value::Null),
        "max_output_tokens": shape.get("max_output_tokens").cloned().unwrap_or(Value::Null)
    })
}

pub(super) async fn record_cost_risk_diagnostic(
    store: &Store,
    id: &str,
    endpoint: &str,
    request: &Value,
    upstream_payload: Option<&Value>,
) {
    const HIGH_TEXT_CHARS: u64 = 200_000;
    const HIGH_INPUT_ITEMS: u64 = 80;
    const HIGH_MESSAGE_TOKENS: u64 = 120_000;

    let shape = request_shape_diagnostic(request);
    let estimated_text_chars = shape
        .get("estimated_text_chars")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let input_items = shape
        .get("input_items")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let message_tokens = upstream_payload
        .map(|payload| estimate_tokens_from_text(&payload.to_string()))
        .unwrap_or(0);
    let full_context = request_looks_like_codex_full_context(request);
    let warnings = [
        (
            "high_estimated_text_chars",
            estimated_text_chars > HIGH_TEXT_CHARS,
        ),
        ("high_input_items", input_items > HIGH_INPUT_ITEMS),
        (
            "high_upstream_message_tokens",
            message_tokens > HIGH_MESSAGE_TOKENS,
        ),
        ("codex_full_context", full_context),
    ]
    .into_iter()
    .filter_map(|(name, active)| active.then_some(name))
    .collect::<Vec<_>>();
    if warnings.is_empty() {
        return;
    }
    let detail = json!({
        "id": id,
        "endpoint": endpoint,
        "mode": "warn_only",
        "warnings": warnings,
        "estimated_text_chars": estimated_text_chars,
        "input_items": input_items,
        "estimated_upstream_message_tokens": message_tokens,
        "codex_full_context": full_context
    });
    let _ = store
        .record_event(
            "warn",
            "cost_risk_diagnostic",
            "CodeSeeX observed a high token/cost risk request before upstream dispatch.",
            Some(&detail),
        )
        .await;
    let _ = store
        .update_request_diagnostic(
            id,
            &json!({
                "cost_risk_warning": detail
            }),
        )
        .await;
}

pub(super) async fn adapt_deepseek_chat_tool_protocol_for_non_streaming(
    store: &Store,
    request_id: &str,
    config: &AppConfig,
    upstream_model: &str,
    chat: &mut Value,
    allow_tool_calls: bool,
    phase: &'static str,
) {
    if !should_adapt_tool_protocol(&config.upstream, upstream_model) {
        return;
    }
    let adaptation = adapt_chat_tool_protocol(chat, allow_tool_calls);
    record_deepseek_chat_tool_protocol_diagnostic(store, request_id, phase, &adaptation).await;
}

async fn record_deepseek_chat_tool_protocol_diagnostic(
    store: &Store,
    request_id: &str,
    phase: &'static str,
    adaptation: &DeepSeekChatToolProtocolAdaptation,
) {
    if !adaptation.changed() {
        return;
    }
    let event_type = if !adaptation.adapted_tool_names.is_empty() {
        "deepseek_tool_protocol_adapted"
    } else if !adaptation.blocked_channels.is_empty() {
        "deepseek_tool_protocol_blocked"
    } else {
        "deepseek_tool_protocol_parse_failed"
    };
    let _ = store
        .record_event(
            "debug",
            event_type,
            "DeepSeek tool protocol content was handled in a non-streaming response.",
            Some(&json!({
                "id": request_id,
                "phase": phase,
                "adapted_tool_names": adaptation.adapted_tool_names,
                "blocked_channels": adaptation.blocked_channels,
                "parse_failed_channels": adaptation.parse_failed_channels
            })),
        )
        .await;
}

pub(super) fn payload_tools_available(payload: &Value) -> bool {
    payload
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| !tools.is_empty())
        .unwrap_or(false)
}

fn upstream_tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .or_else(|| tool.get("name"))
                .or_else(|| tool.get("type"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn limited_tool_names(names: Vec<String>) -> Value {
    const MAX_TOOL_NAMES: usize = 120;
    let total = names.len();
    let shown = names.into_iter().take(MAX_TOOL_NAMES).collect::<Vec<_>>();
    let shown_count = shown.len();
    json!({
        "count": total,
        "names": shown,
        "omitted": total.saturating_sub(shown_count)
    })
}

fn interesting_tool_names(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower.contains("tool_search")
                || lower.contains("spawn_agent")
                || lower.contains("agent")
                || lower.contains("thread")
                || lower.contains("computer")
                || lower.contains("automation")
        })
        .cloned()
        .collect()
}

pub(crate) fn request_has_codex_native_tool_surface(external_tool_context: &ToolContext) -> bool {
    external_tool_context.has_any_response_tool(&[
        "apply_patch",
        "shell_command",
        "view_image",
        "request_user_input",
        "list_mcp_resources",
        "list_mcp_resource_templates",
        "read_mcp_resource",
        "js",
        "js_reset",
        "js_add_node_module_dir",
        "load_workspace_dependencies",
        "create_goal",
        "update_goal",
    ])
}

pub(super) fn should_inject_codeseex_proxy_tools(
    _request: &Value,
    suppress_proxy_tools: bool,
    _external_tool_context: &ToolContext,
) -> bool {
    !suppress_proxy_tools
}

pub(super) async fn immediate_previous_response_tool_call_ids(
    state: &ProxyState,
    previous: Option<&str>,
) -> BTreeSet<String> {
    let Some(previous) = previous else {
        return BTreeSet::new();
    };
    let Ok(chain) = state.store.response_context_chain(previous, 1).await else {
        return BTreeSet::new();
    };
    chain
        .last()
        .filter(|record| record.status == RequestStatus::Completed)
        .and_then(|record| record.response.get("output").and_then(Value::as_array))
        .into_iter()
        .flat_map(|items| items.iter())
        .filter(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call") | Some("custom_tool_call") | Some("tool_search_call")
            )
        })
        .filter_map(response_item_call_id)
        .map(str::to_owned)
        .collect()
}

fn response_item_call_id(item: &Value) -> Option<&str> {
    item.get("call_id")
        .or_else(|| item.get("tool_call_id"))
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CodexToolSearchBridgeDecision {
    pub(super) injected: bool,
    pub(super) reason: &'static str,
    pub(super) markers: crate::upstream::CodexRequestMarkers,
    pub(super) upstream_had_tool_search: bool,
    pub(super) codex_native_tool_surface: bool,
}

pub(super) fn codex_tool_search_bridge_decision(
    request: &Value,
    suppress_proxy_tools: bool,
    external_tool_context: &ToolContext,
) -> CodexToolSearchBridgeDecision {
    let markers = codex_request_markers(request);
    let upstream_had_tool_search = external_tool_context.has_response_tool("tool_search_tool")
        || external_tool_context.has_response_tool("tool_search");
    let codex_native_tool_surface = request_has_codex_native_tool_surface(external_tool_context);

    let (injected, reason) = if suppress_proxy_tools {
        (false, "suppressed_service_request")
    } else if upstream_had_tool_search {
        (false, "already_present")
    } else if markers.has_any() {
        (true, "codex_request_marker")
    } else if codex_native_tool_surface {
        (true, "codex_native_tool_surface")
    } else {
        (false, "not_codex_request")
    };

    CodexToolSearchBridgeDecision {
        injected,
        reason,
        markers,
        upstream_had_tool_search,
        codex_native_tool_surface,
    }
}
