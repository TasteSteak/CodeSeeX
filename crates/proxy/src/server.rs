use crate::app_state::ProxyState;
use crate::diagnostics::{
    client_tool_handoff_diagnostic_event, context_compile_diagnostic_event,
    retry_cache_diagnostic_event, upstream_call_usage_breakdown_event,
};
use crate::http_response::{
    json_error, json_response, passthrough_stream_with_completion, response_content_type_json,
    response_from_bytes, response_from_stream,
};
use crate::http_utils::{io_result, now_seconds};
use crate::manager_api::ensure_catalog;
use crate::response_sse::{
    custom_tool_call_sse_added, custom_tool_call_sse_done, function_call_sse_added,
    function_call_sse_done, generic_output_item_sse_events, hidden_reasoning_item_sse_events,
    message_item_sse_events, next_sequence, proxy_tool_call_sse_events, quote_thinking_delta,
    reasoning_done_sse_events, reasoning_response_item, sse_bytes, sse_data, stream_failed_event,
    streaming_message_done_sse_events, take_sse_frame, thinking_display_added_sse_events,
    thinking_display_delta_sse_event, thinking_display_done_sse_events, thinking_display_prefix,
    web_search_call_sse_events,
};
use crate::responses::compaction::build_compaction_item;
use crate::responses::context::{
    build_response_context, chat_messages_to_values, estimate_tokens_from_messages,
    estimate_tokens_from_text,
};
use crate::responses::conversion::{
    chat_completion_to_response, chat_completion_tool_calls_to_response, final_chat_turn_message,
    text_is_thinking_display_markdown,
};
use crate::responses::stream_tool_calls::{
    collect_streaming_tool_call_deltas, streaming_tool_calls, StreamingToolCallState,
    StreamingVisibleToolBridge,
};
use crate::responses::usage::{merge_response_usage, response_usage_from_chat_usage};
use crate::runtime_config::RuntimeConfigChangeKind;
use crate::text::compact_line;
use crate::tool_passthrough::ToolContext;
use crate::tools::chat_protocol::chat_tool_calls_to_assistant_message;
use crate::tools::coordinator::{complete_chat_with_tools, ToolLoopContext, ToolLoopResult};
use crate::tools::diagnostics::{
    attach_tool_loop_warning, ToolLoopDiagnostics, MAX_TOOL_LOOP_ITERATIONS,
};
use crate::tools::hosted::{
    execute_code_tools_concurrently, is_code_tool_executable, model_replay_tool_result,
    summarize_tool_result, summarize_tool_result_for_log, tool_fact_line,
};
use crate::tools::ownership::ChatToolCall;
use crate::tools::ownership::{
    is_web_search_tool, partition_tool_calls, proxy_executed_calls_in_order,
};
use crate::tools::registry::{
    dedupe_tool_definitions, enabled_tool_ids, normalized_tool_choice, tool_settings,
};
use crate::tools::response_items::{
    native_apply_patch_response_item_from_chat_call, proxy_visible_response_items,
    web_search_call_output_response_item,
};
use crate::upstream::payload::{
    codex_service_request_kind, normalize_chat_payload, request_is_codex_service,
    request_shape_diagnostic, resolve_upstream_model, CodexServiceRequestKind,
};
use crate::upstream::{codex_request_markers, CodexRequestMarkers};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use codeseex_core::context::request_looks_like_codex_full_context;
use codeseex_core::models::available_models;
use codeseex_core::protocol::ChatMessage;
use codeseex_core::{AppConfig, UserConfig};
use codeseex_store::{RequestStatus, Store};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

const RESPONSES_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const WEB_SEARCH_SOURCE_PROBE_DEBOUNCE_MS: u64 = 5_000;

pub async fn serve(config: AppConfig) -> anyhow::Result<()> {
    serve_with_shutdown(config, std::future::pending::<()>(), || {}).await
}

pub async fn serve_with_shutdown<F, L>(
    config: AppConfig,
    shutdown: F,
    on_listening: L,
) -> anyhow::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
    L: FnOnce() + Send + 'static,
{
    let effective_config = {
        let mut effective = config.clone();
        if let Ok(user_config) = UserConfig::read_from(&effective.config_path()) {
            effective.apply_user_config(user_config);
        }
        effective
    };
    let store = Store::open(&effective_config.data_dir).await?;
    let maintenance = store
        .run_maintenance(
            UserConfig::read_from(&effective_config.config_path())
                .unwrap_or_default()
                .log_retention_days(),
        )
        .await?;
    if maintenance.deleted_events > 0 {
        let _ = store
            .record_event(
                "info",
                "log_maintenance_completed",
                "CodeSeeX log maintenance completed.",
                Some(&json!({
                    "log_retention_days": maintenance.log_retention_days,
                    "deleted_log_files": maintenance.deleted_events
                })),
            )
            .await;
    }
    let recovered = store
        .recover_interrupted_requests("proxy_started_with_in_progress_checkpoint")
        .await?;
    if !recovered.is_empty() {
        let _ = store
            .record_event(
                "warn",
                "state_recovered_interrupted",
                "Recovered interrupted in-progress response checkpoints.",
                Some(&json!({
                    "interrupted_count": recovered.len(),
                    "response_ids": recovered.iter().take(20).collect::<Vec<_>>()
                })),
            )
            .await;
    }
    let state = ProxyState::new(config.clone(), store);
    let shutdown_store = state.store.clone();
    state.telemetry.emit_framework_started();

    ensure_catalog(&effective_config)?;

    let app = app_router(state.clone(), &effective_config);

    let listener =
        match TcpListener::bind((effective_config.host.as_str(), effective_config.port)).await {
            Ok(listener) => listener,
            Err(error) => {
                let _ = shutdown_store
                    .record_event(
                        "error",
                        "proxy_start_failed",
                        "CodeSeeX proxy failed to start.",
                        Some(&json!({
                            "host": effective_config.host.clone(),
                            "port": effective_config.port,
                            "error": error.to_string()
                        })),
                    )
                    .await;
                shutdown_store.close().await;
                return Err(error.into());
            }
        };
    let local_addr = listener.local_addr()?;
    let listener_base_url = proxy_base_url_for_listener(&effective_config, local_addr);
    let listener_detail = json!({
        "base_url": listener_base_url.clone(),
        "host": effective_config.host.clone(),
        "port": local_addr.port()
    });
    tracing::info!("CodeSeeX proxy listening on {}", listener_base_url);
    let _ = shutdown_store
        .record_event(
            "info",
            "proxy_started",
            "CodeSeeX proxy started.",
            Some(&listener_detail),
        )
        .await;
    let config_file_watcher = state
        .runtime_config
        .spawn_config_file_watcher(shutdown_store.clone());
    let system_proxy_watcher = state
        .runtime_config
        .spawn_system_proxy_watcher(shutdown_store.clone());
    let search_source_probe_changes = state.runtime_config.subscribe();
    let search_source_probe = spawn_web_search_source_probe_subscriber(
        state.runtime_config.clone(),
        search_source_probe_changes,
        shutdown_store.clone(),
        std::time::Duration::from_millis(WEB_SEARCH_SOURCE_PROBE_DEBOUNCE_MS),
        Arc::new(warm_search_sources_for_probe),
    );
    state.runtime_config.emit_proxy_startup();
    on_listening();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await;
    config_file_watcher.abort();
    system_proxy_watcher.abort();
    search_source_probe.abort();
    let _ = shutdown_store
        .record_event(
            "info",
            "proxy_stopped",
            "CodeSeeX proxy stopped.",
            Some(&listener_detail),
        )
        .await;
    shutdown_store.close().await;
    result?;
    Ok(())
}

fn spawn_web_search_source_probe_subscriber(
    runtime_config: crate::runtime_config::RuntimeConfigService,
    mut changes: tokio::sync::broadcast::Receiver<crate::runtime_config::RuntimeConfigChange>,
    store: Store,
    debounce: std::time::Duration,
    warm_sources: WarmSearchSourcesFn,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let mut change = match changes.recv().await {
                Ok(change) => change,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    let _ = store
                        .record_event(
                            "warn",
                            "web_search_source_probe_lagged",
                            "CodeSeeX web_search source probe skipped stale config events.",
                            Some(&json!({ "skipped": skipped })),
                        )
                        .await;
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if !change.has_kind(RuntimeConfigChangeKind::NetworkProxy) {
                continue;
            }
            let sleep = tokio::time::sleep(debounce);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    received = changes.recv() => {
                        let next = match received {
                            Ok(next) => next,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                let _ = store
                                    .record_event(
                                        "warn",
                                        "web_search_source_probe_lagged",
                                        "CodeSeeX web_search source probe skipped stale config events.",
                                        Some(&json!({ "skipped": skipped })),
                                    )
                                    .await;
                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        };
                        if next.has_kind(RuntimeConfigChangeKind::NetworkProxy) {
                            change = next;
                            sleep.as_mut().reset(tokio::time::Instant::now() + debounce);
                        }
                    }
                }
            }
            let snapshot = runtime_config.snapshot();
            if snapshot.network_proxy_signature != change.snapshot.network_proxy_signature {
                continue;
            }
            let diagnostic = (warm_sources)(snapshot.config.network_proxy).await;
            let _ = store
                .record_event(
                    "info",
                    "web_search_source_probe",
                    "CodeSeeX web_search source probe completed.",
                    Some(&web_search_source_probe_event_detail(
                        change.source.label(),
                        snapshot.network_proxy_signature.as_str(),
                        diagnostic,
                    )),
                )
                .await;
        }
    })
}

fn web_search_source_probe_event_detail(
    trigger: &str,
    network_proxy_signature: &str,
    diagnostic: Value,
) -> Value {
    json!({
        "trigger": trigger,
        "debounce_ms": WEB_SEARCH_SOURCE_PROBE_DEBOUNCE_MS,
        "network_proxy_signature": network_proxy_signature,
        "stage": diagnostic.get("stage").cloned().unwrap_or(Value::Null),
        "source_order": diagnostic.get("source_order").cloned().unwrap_or(Value::Null),
        "source_health": diagnostic.get("source_health").cloned().unwrap_or(Value::Null)
    })
}

type WarmSearchSourcesFuture = Pin<Box<dyn Future<Output = Value> + Send>>;
type WarmSearchSourcesFn = Arc<
    dyn Fn(codeseex_core::NetworkProxyMode) -> WarmSearchSourcesFuture + Send + Sync + 'static,
>;

fn warm_search_sources_for_probe(
    proxy_mode: codeseex_core::NetworkProxyMode,
) -> WarmSearchSourcesFuture {
    Box::pin(crate::tools::web::warm_search_sources(proxy_mode))
}

fn app_router(state: ProxyState, config: &AppConfig) -> Router {
    let manager_access = ManagerAccessPolicy {
        listener_host_is_local: authority_host_is_local(&config.host),
    };
    let manager_router = crate::manager_api::router()
        .route_layer(middleware::from_fn_with_state(manager_access, manager_local_request_guard));
    let v1_router = Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses/compact", post(responses_compact))
        .route("/v1/responses/{response_id}/cancel", post(cancel_response))
        .route("/v1/responses", post(responses))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_headers(Any)
                .allow_methods(Any),
        );
    Router::new()
        .merge(manager_router)
        .merge(v1_router)
        .layer(DefaultBodyLimit::max(RESPONSES_BODY_LIMIT_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn manager_local_request_guard(
    State(policy): State<ManagerAccessPolicy>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    if manager_request_is_local(policy, request.headers()) {
        return next.run(request).await;
    }
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "code": "manager_api_forbidden",
                "message": "CodeSeeX manager API only accepts local requests."
            }
        })),
    )
        .into_response()
}

#[derive(Debug, Clone, Copy)]
struct ManagerAccessPolicy {
    listener_host_is_local: bool,
}

fn manager_request_is_local(policy: ManagerAccessPolicy, headers: &HeaderMap) -> bool {
    if !policy.listener_host_is_local {
        return false;
    }
    if fetch_metadata_is_cross_site(headers) {
        return false;
    }
    let host_is_local = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(authority_host_is_local)
        .unwrap_or(false);
    let origin_is_local = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(origin_host_is_local)
        .unwrap_or(true);
    host_is_local && origin_is_local
}

fn fetch_metadata_is_cross_site(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
}

fn origin_host_is_local(origin: &str) -> bool {
    reqwest::Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(authority_host_is_local))
        .unwrap_or(false)
}

fn authority_host_is_local(authority: &str) -> bool {
    let host = authority_host(authority);
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn authority_host(authority: &str) -> &str {
    let trimmed = authority.trim();
    if let Some(rest) = trimmed.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(trimmed);
    }
    if trimmed.matches(':').count() == 1 {
        return trimmed.split(':').next().unwrap_or(trimmed);
    }
    trimmed
}

fn proxy_base_url_for_listener(config: &AppConfig, local_addr: SocketAddr) -> String {
    let port = local_addr.port();
    if port == config.port {
        return config.proxy_base_url();
    }
    format!("http://{}:{port}/v1", config.host)
}

fn tool_exposure_diagnostic(
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

fn service_lifecycle_for_kind(kind: CodexServiceRequestKind) -> Option<&'static str> {
    kind.is_service().then_some("service_ephemeral")
}

fn service_completion_diagnostic(kind: CodexServiceRequestKind) -> Option<Value> {
    kind.is_service().then(|| {
        json!({
            "codeseex_lifecycle": "service_ephemeral",
            "codeseex_service_kind": kind.label()
        })
    })
}

fn service_request_diagnostic(
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

fn request_has_codex_native_tool_surface(external_tool_context: &ToolContext) -> bool {
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

fn should_inject_codeseex_proxy_tools(
    _request: &Value,
    suppress_proxy_tools: bool,
    _external_tool_context: &ToolContext,
) -> bool {
    !suppress_proxy_tools
}

async fn immediate_previous_response_tool_call_ids(
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
struct CodexToolSearchBridgeDecision {
    injected: bool,
    reason: &'static str,
    markers: CodexRequestMarkers,
    upstream_had_tool_search: bool,
    codex_native_tool_surface: bool,
}

fn codex_tool_search_bridge_decision(
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

async fn models() -> impl IntoResponse {
    json_response(json!({
        "object": "list",
        "data": available_models().into_iter().map(|model| json!({
            "id": model.slug,
            "object": "model",
            "created": 0,
            "owned_by": "codeseex",
            "context_window": model.context_window
        })).collect::<Vec<_>>()
    }))
}

async fn chat_completions(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Json(mut payload): Json<Value>,
) -> impl IntoResponse {
    let id = Uuid::new_v4().to_string();
    let config = state.active_config();
    let original_payload = payload.clone();
    let requested_model = original_payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let service_kind = codex_service_request_kind(&original_payload);
    normalize_chat_payload(&config, &original_payload, &mut payload);
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if service_kind.is_service() {
        let _ = state
            .store
            .record_event(
                "info",
                "service_request_diagnostic",
                "CodeSeeX service request diagnostic.",
                Some(&service_request_diagnostic(
                    &id,
                    "/v1/chat/completions",
                    service_kind,
                    requested_model.as_deref(),
                    model.as_deref().unwrap_or_default(),
                    true,
                    &original_payload,
                )),
            )
            .await;
    }
    if let Err(error) = state
        .store
        .checkpoint_request(&id, None, model.as_deref(), &original_payload)
        .await
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_checkpoint_failed",
            error.to_string(),
        );
    }
    let _ = state
        .store
        .record_event(
            "info",
            "request_started",
            "Chat completion request started.",
            Some(&json!({
                "id": id,
                "endpoint": "/v1/chat/completions",
                "requested_model": requested_model.as_deref(),
                "model": model.as_deref()
            })),
        )
        .await;
    record_request_shape_diagnostic(
        &state.store,
        &id,
        "/v1/chat/completions",
        requested_model.as_deref(),
        model.as_deref().unwrap_or_default(),
        &original_payload,
    )
    .await;

    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(auth) = auth.as_deref() {
        codeseex_core::codex_auth::remember_authorization_header(auth);
    }
    let client = state.client();
    match crate::upstream::post_chat_completions(
        &client,
        &config.upstream,
        auth.as_deref(),
        Some(&original_payload),
        payload.clone(),
    )
    .await
    {
        Ok(response) => {
            let status = response.status();
            let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
            if status.is_success()
                && content_type
                    .as_ref()
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .contains("text/event-stream")
            {
                let stream = passthrough_stream_with_completion(
                    response,
                    state.store.clone(),
                    id.clone(),
                    service_completion_diagnostic(service_kind),
                );
                let _ = state
                    .store
                    .record_event(
                        "info",
                        "chat_stream_started",
                        "Streaming chat completion started.",
                        None,
                    )
                    .await;
                response_from_stream(status, content_type, Body::from_stream(stream))
            } else {
                match response.bytes().await {
                    Ok(bytes) => {
                        let body_json = serde_json::from_slice::<Value>(&bytes).ok();
                        let upstream_error = upstream_error_detail(body_json.as_ref(), &bytes);
                        let status_to_store = if status.is_success() {
                            RequestStatus::Completed
                        } else {
                            RequestStatus::Failed
                        };
                        let completion_diagnostic = service_completion_diagnostic(service_kind);
                        if let Err(error) = state
                            .store
                            .finish_request(
                                &id,
                                status_to_store,
                                body_json.as_ref(),
                                completion_diagnostic.as_ref(),
                            )
                            .await
                        {
                            if status.is_success() {
                                return json_error(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "state_finish_failed",
                                    error.to_string(),
                                );
                            }
                        }
                        let _ = state
                            .store
                            .record_event(
                                if status.is_success() { "info" } else { "error" },
                                if status.is_success() {
                                    "request_completed"
                                } else {
                                    "request_failed"
                                },
                                if status.is_success() {
                                    "Chat completion request completed."
                                } else {
                                    "Chat completion request failed."
                                },
                                Some(&if status.is_success() {
                                    let mut detail = request_completed_detail(
                                        &id,
                                        requested_model.as_deref(),
                                        model.as_deref(),
                                        service_lifecycle_for_kind(service_kind),
                                        body_json.as_ref(),
                                    );
                                    detail["status"] = json!(status.as_u16());
                                    detail
                                } else {
                                    json!({
                                        "id": id,
                                        "status": status.as_u16(),
                                        "requested_model": requested_model.as_deref(),
                                        "model": model.as_deref(),
                                        "upstream_error": upstream_error
                                    })
                                }),
                            )
                            .await;
                        response_from_bytes(status, content_type, bytes.to_vec())
                    }
                    Err(error) => {
                        let detail = json!({ "error": error.to_string() });
                        let _ = state
                            .store
                            .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                            .await;
                        let _ = state
                            .store
                            .record_event(
                                "error",
                                "request_failed",
                                "Failed to read upstream response body.",
                                Some(&json!({
                                    "id": id,
                                    "requested_model": requested_model.as_deref(),
                                    "model": model.as_deref(),
                                    "error": error.to_string()
                                })),
                            )
                            .await;
                        json_error(
                            StatusCode::BAD_GATEWAY,
                            "upstream_body_failed",
                            error.to_string(),
                        )
                    }
                }
            }
        }
        Err(error) => {
            let detail = json!({ "error": error.to_string() });
            let _ = state
                .store
                .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                .await;
            let _ = state
                .store
                .record_event(
                    "error",
                    "request_failed",
                    "Failed to connect to upstream.",
                    Some(&json!({
                        "id": id,
                        "requested_model": requested_model.as_deref(),
                        "model": model.as_deref(),
                        "error": error.to_string()
                    })),
                )
                .await;
            json_error(
                StatusCode::BAD_GATEWAY,
                "upstream_connection_failed",
                error.to_string(),
            )
        }
    }
}

async fn responses_compact(
    State(state): State<ProxyState>,
    Json(input): Json<Value>,
) -> impl IntoResponse {
    let id = response_id_from_input(&input);
    let previous = input.get("previous_response_id").and_then(Value::as_str);
    let config = state.active_config();
    let model = response_model_from_input(&config, &input);
    let started_at = now_seconds();
    if let Err(response) = ensure_new_response_id(&state, &id, previous).await {
        return response;
    }
    let previous_resolution = match resolve_previous_response_id(&state, previous).await {
        Ok(resolution) => resolution,
        Err(response) => return response,
    };
    record_previous_response_resolution_warning(&state, &id, &input, &previous_resolution).await;
    let previous_for_context = previous_resolution.resolved.as_deref();
    let runtime_context_storage =
        runtime_context_storage_diagnostic(&state, &input, previous_for_context).await;
    record_runtime_context_storage_events(&state, &id, &runtime_context_storage).await;

    if let Err(error) = state
        .store
        .checkpoint_request(&id, previous_for_context, Some(&model), &input)
        .await
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_checkpoint_failed",
            error.to_string(),
        );
    }
    let _ = state
        .store
        .record_event(
            "info",
            "context_compaction_started",
            "Context compaction requested.",
            Some(&json!({
                "id": id,
                "previous_response_id": previous,
                "resolved_previous_response_id": previous_for_context,
                "previous_response_resolution": previous_resolution.diagnostic(),
                "runtime_context_storage": runtime_context_storage.diagnostic()
            })),
        )
        .await;

    let compaction_id = format!("cmp_{}", Uuid::new_v4().simple());
    let built_context = build_response_context(&state, &input, previous_for_context).await;
    let compact = match build_compaction_item(
        &config,
        &compaction_id,
        &model,
        &built_context.messages,
        &built_context.tool_facts,
    ) {
        Ok(compact) => compact,
        Err(error) => {
            let detail = json!({ "error": error.to_string() });
            let _ = state
                .store
                .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                .await;
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "compaction_failed",
                error.to_string(),
            );
        }
    };
    let summary = compact.summary.clone();
    let output_item = compact.item.clone();
    let compact_turn_message = ChatMessage::text(
        "system",
        "CodeSeeX compacted this response. Rebuild later history from the compaction output payload, not from the original compact request input.",
    );
    if let Err(error) = state
        .store
        .replace_request_turn_messages(&id, &chat_messages_to_values(&[compact_turn_message]))
        .await
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_turn_messages_failed",
            error.to_string(),
        );
    }
    let response = json!({
        "id": id,
        "object": "response",
        "created_at": started_at,
        "model": model,
        "status": "completed",
        "error": Value::Null,
        "incomplete_details": Value::Null,
        "parallel_tool_calls": true,
        "output": [output_item],
        "usage": {
            "input_tokens": estimate_tokens_from_messages(&built_context.messages),
            "cached_input_tokens": 0,
            "cache_miss_input_tokens": estimate_tokens_from_messages(&built_context.messages),
            "input_tokens_details": { "cached_tokens": 0 },
            "output_tokens": estimate_tokens_from_text(&summary),
            "reasoning_output_tokens": 0,
            "output_tokens_details": { "reasoning_tokens": 0 },
            "total_tokens": estimate_tokens_from_messages(&built_context.messages) + estimate_tokens_from_text(&summary)
        }
    });
    let diagnostic = json!({
        "kind": "context_compaction",
        "context": built_context.diagnostic,
        "tool_fact_count": compact.payload.tool_fact_count,
        "retained_message_count": compact.payload.retained_message_count,
        "summary_chars": summary.chars().count(),
        "summary_tokens_estimate": estimate_tokens_from_text(&summary)
    });
    if let Err(error) = state
        .store
        .finish_request(
            &id,
            RequestStatus::Completed,
            Some(&response),
            Some(&diagnostic),
        )
        .await
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_finish_failed",
            error.to_string(),
        );
    }
    let _ = state
        .store
        .record_event(
            "info",
            "context_compaction_completed",
            "Context compaction completed.",
            Some(&json!({
                "id": id,
                "compaction_id": compaction_id,
                "message_count": built_context.messages.len(),
                "tool_fact_count": compact.payload.tool_fact_count,
                "summary_chars": summary.chars().count()
            })),
        )
        .await;
    let _ = state
        .store
        .record_event(
            "info",
            "context_compacted",
            "Context compacted.",
            Some(&json!({
                "id": id,
                "compaction_id": compaction_id,
                "message_count": built_context.messages.len()
            })),
        )
        .await;

    json_response(response)
}

async fn cancel_response(
    Path(response_id): Path<String>,
    State(state): State<ProxyState>,
) -> axum::response::Response {
    let active = cancel_streaming_response(&response_id);
    let interrupted = state
        .store
        .interrupt_request_if_in_progress(&response_id, "response cancelled by client")
        .await
        .unwrap_or(false);
    if active || interrupted {
        let _ = state
            .store
            .record_event(
                "info",
                "request_interrupted",
                "Streaming response cancelled.",
                Some(&json!({ "id": response_id })),
            )
            .await;
    }
    json_response(json!({
        "id": response_id,
        "object": "response",
        "status": "cancelled"
    }))
}

async fn responses(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> impl IntoResponse {
    let id = response_id_from_input(&input);
    let previous = input.get("previous_response_id").and_then(Value::as_str);
    let config = state.active_config();
    let requested_model = input
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let service_kind = codex_service_request_kind(&input);
    let model = response_model_from_input(&config, &input);
    if let Err(response) = ensure_new_response_id(&state, &id, previous).await {
        return response;
    }
    let previous_resolution = if service_kind.is_service() {
        PreviousResponseResolution::suppressed_service(previous)
    } else {
        match resolve_previous_response_id(&state, previous).await {
            Ok(resolution) => resolution,
            Err(response) => return response,
        }
    };
    record_previous_response_resolution_warning(&state, &id, &input, &previous_resolution).await;
    let previous_for_context = previous_resolution.resolved.as_deref();
    let runtime_context_storage =
        runtime_context_storage_diagnostic(&state, &input, previous_for_context).await;
    record_runtime_context_storage_events(&state, &id, &runtime_context_storage).await;
    if let Err(error) = state
        .store
        .checkpoint_request(&id, previous_for_context, Some(&model), &input)
        .await
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_checkpoint_failed",
            error.to_string(),
        );
    }
    let built_context = build_response_context(&state, &input, previous_for_context).await;
    let tool_execution_context = crate::tools::ToolExecutionContext::from_request(&input);
    let current_image_refs = built_context.current_image_refs.clone();
    let mut context_diagnostic = built_context.diagnostic.clone();
    if let Some(object) = context_diagnostic.as_object_mut() {
        object.insert(
            "tool_permissions".to_owned(),
            tool_execution_context.diagnostic(),
        );
        object.insert(
            "previous_response_resolution".to_owned(),
            previous_resolution.diagnostic(),
        );
        object.insert(
            "runtime_context_storage".to_owned(),
            runtime_context_storage.diagnostic(),
        );
    }
    let history_message_count = built_context.history_message_count;
    let stream_requested = input
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let auto_compaction = match build_automatic_compaction(&config, &input, &model, &built_context)
    {
        Ok(value) => value,
        Err(error) => {
            let detail = json!({ "error": error.to_string() });
            let _ = state
                .store
                .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                .await;
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "automatic_compaction_failed",
                error.to_string(),
            );
        }
    };
    let mut payload = json!({
        "model": model.clone(),
        "messages": if built_context.messages.is_empty() { vec![ChatMessage::text("user", "")] } else { built_context.messages },
        "stream": stream_requested
    });
    normalize_chat_payload(&config, &input, &mut payload);
    let suppress_tools_for_service = request_is_codex_service(&input);
    let mut external_tool_context =
        crate::tool_passthrough::ToolContext::from_request_tools(input.get("tools"));
    if !suppress_tools_for_service {
        let valid_previous_tool_call_ids =
            immediate_previous_response_tool_call_ids(&state, previous_for_context).await;
        external_tool_context
            .add_tool_search_output_tools(input.get("input"), &valid_previous_tool_call_ids);
    }
    let inject_codeseex_proxy_tools = should_inject_codeseex_proxy_tools(
        &input,
        suppress_tools_for_service,
        &external_tool_context,
    );
    let enabled_tools = if inject_codeseex_proxy_tools {
        enabled_tool_ids(&config)
    } else {
        Vec::new()
    };
    let tool_settings = tool_settings(&config);
    let community_tools = crate::community_tools::CommunityToolSet::load(
        &config.data_dir,
        &enabled_tools,
        &tool_settings,
    );
    let tool_search_bridge_decision = codex_tool_search_bridge_decision(
        &input,
        suppress_tools_for_service,
        &external_tool_context,
    );
    if !suppress_tools_for_service
        && tool_search_bridge_decision.upstream_had_tool_search
        && (tool_search_bridge_decision.markers.has_any()
            || tool_search_bridge_decision.codex_native_tool_surface)
    {
        external_tool_context.promote_codex_tool_search_tools();
    }
    if tool_search_bridge_decision.injected {
        external_tool_context.ensure_codex_tool_search_bridge();
    }
    let mut tools = if inject_codeseex_proxy_tools {
        crate::tools::upstream_tool_definitions(&enabled_tools)
    } else {
        Vec::new()
    };
    if inject_codeseex_proxy_tools {
        tools.extend(community_tools.definitions());
    }
    if !suppress_tools_for_service {
        tools.extend(external_tool_context.upstream_tools.clone());
    }
    let tools = dedupe_tool_definitions(tools);
    let _ = state
        .store
        .record_event(
            "debug",
            "tool_exposure_diagnostic",
            "CodeSeeX tool exposure diagnostic.",
            Some(&tool_exposure_diagnostic(
                &id,
                &external_tool_context,
                &tools,
                &tool_search_bridge_decision,
                &enabled_tools,
                inject_codeseex_proxy_tools,
                service_kind,
            )),
        )
        .await;
    if !tools.is_empty() {
        let tool_choice = normalized_tool_choice(input.get("tool_choice"), &tools);
        payload["tools"] = Value::Array(tools);
        if let Some(tool_choice) = tool_choice {
            payload["tool_choice"] = tool_choice;
        }
    }
    let upstream_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(&model)
        .to_owned();
    if service_kind.is_service() {
        let _ = state
            .store
            .record_event(
                "info",
                "service_request_diagnostic",
                "CodeSeeX service request diagnostic.",
                Some(&service_request_diagnostic(
                    &id,
                    "/v1/responses",
                    service_kind,
                    requested_model.as_deref(),
                    upstream_model.as_str(),
                    suppress_tools_for_service,
                    &input,
                )),
            )
            .await;
    }
    let current_turn_messages = if request_looks_like_codex_full_context(&input) {
        Vec::new()
    } else {
        chat_messages_to_values(&built_context.current_messages)
    };
    if let Err(error) = state
        .store
        .replace_request_turn_messages(&id, &current_turn_messages)
        .await
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_turn_messages_failed",
            error.to_string(),
        );
    }
    if let Err(error) = state
        .store
        .update_request_diagnostic(&id, &context_diagnostic)
        .await
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_diagnostic_failed",
            error.to_string(),
        );
    }
    let _ = state
        .store
        .record_event(
            "info",
            "request_started",
            "Responses request started.",
            Some(&json!({
                "id": id,
                "endpoint": "/v1/responses",
                "previous_response_id": previous,
                "resolved_previous_response_id": previous_for_context,
                "previous_response_resolution": previous_resolution.diagnostic(),
                "runtime_context_storage": runtime_context_storage.diagnostic(),
                "history_messages": history_message_count,
                "context": context_diagnostic,
                "requested_model": requested_model.as_deref(),
                "model": upstream_model.as_str()
            })),
        )
        .await;
    let runtime_context_storage_value = runtime_context_storage.diagnostic();
    let _ = state
        .store
        .record_event(
            "info",
            "context_compile_diagnostic",
            "CodeSeeX context compile diagnostic.",
            Some(&context_compile_diagnostic_event(
                &id,
                &context_diagnostic,
                &input,
                &runtime_context_storage_value,
            )),
        )
        .await;
    record_request_shape_diagnostic(
        &state.store,
        &id,
        "/v1/responses",
        requested_model.as_deref(),
        upstream_model.as_str(),
        &input,
    )
    .await;

    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(auth) = auth.as_deref() {
        codeseex_core::codex_auth::remember_authorization_header(auth);
    }
    let client = state.client();
    match crate::upstream::post_chat_completions(
        &client,
        &config.upstream,
        auth.as_deref(),
        Some(&input),
        payload.clone(),
    )
    .await
    {
        Ok(response) => {
            let status = response.status();
            if !status.is_success() {
                return match response.bytes().await {
                    Ok(bytes) => {
                        let body_json = serde_json::from_slice::<Value>(&bytes).ok();
                        let upstream_error = upstream_error_detail(body_json.as_ref(), &bytes);
                        let _ = state
                            .store
                            .finish_request(&id, RequestStatus::Failed, body_json.as_ref(), None)
                            .await;
                        let _ = state
                            .store
                            .record_event(
                                "info",
                                "retry_cache_diagnostic",
                                "CodeSeeX retry/cache diagnostic.",
                                Some(&retry_cache_diagnostic_event(
                                    &id,
                                    requested_model.as_deref(),
                                    Some(upstream_model.as_str()),
                                    &input,
                                    Some(&payload),
                                    "upstream_status_failed_initial",
                                )),
                            )
                            .await;
                        let _ = state
                            .store
                            .record_event(
                                "error",
                                "request_failed",
                                "Responses request failed.",
                                Some(&json!({
                                    "id": id,
                                    "status": status.as_u16(),
                                    "requested_model": requested_model.as_deref(),
                                    "model": upstream_model.as_str(),
                                    "upstream_error": upstream_error
                                })),
                            )
                            .await;
                        response_from_bytes(status, response_content_type_json(), bytes.to_vec())
                    }
                    Err(error) => {
                        let detail = json!({ "error": error.to_string() });
                        let _ = state
                            .store
                            .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                            .await;
                        let _ = state
                            .store
                            .record_event(
                                "info",
                                "retry_cache_diagnostic",
                                "CodeSeeX retry/cache diagnostic.",
                                Some(&retry_cache_diagnostic_event(
                                    &id,
                                    requested_model.as_deref(),
                                    Some(upstream_model.as_str()),
                                    &input,
                                    Some(&payload),
                                    "upstream_body_failed_initial",
                                )),
                            )
                            .await;
                        let _ = state
                            .store
                            .record_event(
                                "error",
                                "request_failed",
                                "Failed to read upstream response body.",
                                Some(&json!({
                                    "id": id,
                                    "requested_model": requested_model.as_deref(),
                                    "model": upstream_model.as_str(),
                                    "error": error.to_string()
                                })),
                            )
                            .await;
                        json_error(
                            StatusCode::BAD_GATEWAY,
                            "upstream_body_failed",
                            error.to_string(),
                        )
                    }
                };
            }
            if stream_requested {
                return response_stream_from_chat(StreamingResponseParams {
                    response_id: id,
                    model: model.to_owned(),
                    requested_model,
                    response,
                    state: state.clone(),
                    config,
                    auth,
                    payload,
                    enabled_tools,
                    tool_execution_context,
                    community_tools: Arc::new(community_tools),
                    external_tool_context,
                    current_image_refs,
                    auto_compaction,
                    service_kind,
                    original_request: input,
                    context_diagnostic,
                    runtime_context_storage: runtime_context_storage_value,
                });
            }
            match response.json::<Value>().await {
                Ok(chat) => {
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "upstream_call_usage_breakdown",
                            "CodeSeeX upstream call usage breakdown.",
                            Some(&upstream_call_usage_breakdown_event(
                                &id,
                                "non_streaming_initial",
                                0,
                                &input,
                                &payload,
                                chat.get("usage"),
                                false,
                            )),
                        )
                        .await;
                    let client = state.client();
                    let tool_loop_context = ToolLoopContext {
                        client: &client,
                        store: &state.store,
                        config: &config,
                        auth: auth.as_deref(),
                        request_id: &id,
                        enabled_tools: &enabled_tools,
                        tool_context: &tool_execution_context,
                        community_tools: &community_tools,
                        external_tool_context: &external_tool_context,
                        current_image_refs: &current_image_refs,
                        original_request: &input,
                        context_diagnostic: &context_diagnostic,
                        runtime_context_storage: &runtime_context_storage_value,
                        requested_model: requested_model.as_deref(),
                        upstream_model: upstream_model.as_str(),
                    };
                    let tool_loop_result =
                        match complete_chat_with_tools(tool_loop_context, payload, chat).await {
                            Ok(result) => result,
                            Err(error) => {
                                let failed_response = failed_billable_response(
                                    &id,
                                    &model,
                                    "tool_loop_failed",
                                    &error.message,
                                    &error.usage,
                                );
                                let detail = json!({
                                    "error": error.message,
                                    "codeseex_lifecycle": "failed_billable"
                                });
                                let _ = state
                                    .store
                                    .finish_request(
                                        &id,
                                        RequestStatus::Failed,
                                        Some(&failed_response),
                                        Some(&detail),
                                    )
                                    .await;
                                let _ = state
                                    .store
                                    .record_event(
                                        "error",
                                        "request_failed",
                                        "Tool execution loop failed.",
                                        Some(&json!({ "id": id, "error": error.message })),
                                    )
                                    .await;
                                return json_error(
                                    StatusCode::BAD_GATEWAY,
                                    "tool_loop_failed",
                                    error.message,
                                );
                            }
                        };
                    let mut client_tool_handoff = false;
                    let mut mapped = match tool_loop_result {
                        ToolLoopResult::FinalChat(result) => {
                            if let Some(message) = final_chat_turn_message(&result.chat) {
                                if let Err(error) = state
                                    .store
                                    .append_request_turn_messages(&id, &[message])
                                    .await
                                {
                                    return json_error(
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        "state_turn_messages_failed",
                                        error.to_string(),
                                    );
                                }
                            }
                            let mut response = chat_completion_to_response(
                                &config,
                                &id,
                                &model,
                                result.chat,
                                show_thinking_enabled(&config),
                            );
                            response["usage"] = result.usage;
                            prepend_response_output_items(&mut response, result.response_items);
                            response
                        }
                        ToolLoopResult::ClientToolCalls(result) => {
                            client_tool_handoff = true;
                            let mut response = chat_completion_tool_calls_to_response(
                                &config,
                                &id,
                                &model,
                                result.chat,
                                &community_tools,
                                &external_tool_context,
                                show_thinking_enabled(&config),
                            );
                            response["usage"] = result.usage;
                            prepend_response_output_items(&mut response, result.response_items);
                            response
                        }
                    };
                    if append_auto_compaction_if_safe(&mut mapped, auto_compaction.as_ref()) {
                        let _ = state
                            .store
                            .record_event(
                                "info",
                                "context_compacted",
                                "Context compacted automatically.",
                                Some(&json!({ "id": id })),
                            )
                            .await;
                    }
                    let completion_diagnostic = if client_tool_handoff {
                        Some(json!({ "codeseex_lifecycle": "client_tool_handoff" }))
                    } else {
                        service_completion_diagnostic(service_kind)
                    };
                    if let Err(error) = state
                        .store
                        .finish_request(
                            &id,
                            RequestStatus::Completed,
                            Some(&mapped),
                            completion_diagnostic.as_ref(),
                        )
                        .await
                    {
                        return json_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "state_finish_failed",
                            error.to_string(),
                        );
                    }
                    let lifecycle = if client_tool_handoff {
                        Some("client_tool_handoff")
                    } else {
                        service_lifecycle_for_kind(service_kind)
                    };
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "request_completed",
                            "Responses request completed.",
                            Some(&request_completed_detail(
                                &id,
                                Some(model.as_str()),
                                mapped
                                    .get("model")
                                    .and_then(Value::as_str)
                                    .or(Some(model.as_str())),
                                lifecycle,
                                Some(&mapped),
                            )),
                        )
                        .await;
                    json_response(mapped)
                }
                Err(error) => {
                    let detail = json!({ "error": error.to_string() });
                    let _ = state
                        .store
                        .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                        .await;
                    let _ = state
                        .store
                        .record_event(
                            "error",
                            "request_failed",
                            "Failed to parse upstream response JSON.",
                            Some(&json!({ "id": id, "error": error.to_string() })),
                        )
                        .await;
                    json_error(
                        StatusCode::BAD_GATEWAY,
                        "upstream_json_failed",
                        error.to_string(),
                    )
                }
            }
        }
        Err(error) => {
            let detail = json!({ "error": error.to_string() });
            let _ = state
                .store
                .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                .await;
            let _ = state
                .store
                .record_event(
                    "info",
                    "retry_cache_diagnostic",
                    "CodeSeeX retry/cache diagnostic.",
                    Some(&retry_cache_diagnostic_event(
                        &id,
                        requested_model.as_deref(),
                        Some(upstream_model.as_str()),
                        &input,
                        Some(&payload),
                        "upstream_connection_failed_initial",
                    )),
                )
                .await;
            let _ = state
                .store
                .record_event(
                    "error",
                    "request_failed",
                    "Failed to connect to upstream.",
                    Some(&json!({
                        "id": id,
                        "requested_model": requested_model.as_deref(),
                        "model": upstream_model.as_str(),
                        "error": error.to_string()
                    })),
                )
                .await;
            json_error(
                StatusCode::BAD_GATEWAY,
                "upstream_connection_failed",
                error.to_string(),
            )
        }
    }
}

fn prepend_response_output_items(response: &mut Value, mut items: Vec<Value>) {
    if items.is_empty() {
        return;
    }
    let Some(output) = response.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };
    items.append(output);
    *output = items;
}

fn upstream_error_detail(body_json: Option<&Value>, bytes: &[u8]) -> Value {
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

fn request_completed_detail(
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

fn failed_billable_response(
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

fn response_id_from_input(input: &Value) -> String {
    input
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("resp_{}", Uuid::new_v4().simple()))
}

async fn ensure_new_response_id(
    state: &ProxyState,
    request_id: &str,
    previous: Option<&str>,
) -> Result<(), Response<Body>> {
    if previous
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|previous| previous == request_id)
    {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "invalid_response_id",
            "response id must not equal previous_response_id".to_owned(),
        ));
    }
    match state.store.response_status(request_id).await {
        Ok(Some(status)) => Err(json_error(
            StatusCode::CONFLICT,
            "duplicate_response_id",
            format!("response id '{request_id}' already exists with status {status:?}"),
        )),
        Ok(None) => Ok(()),
        Err(error) => Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_response_id_check_failed",
            error.to_string(),
        )),
    }
}

#[derive(Debug, Clone)]
struct PreviousResponseResolution {
    requested: Option<String>,
    resolved: Option<String>,
    kind: &'static str,
    status: Option<&'static str>,
    warning: Option<&'static str>,
}

impl PreviousResponseResolution {
    fn none() -> Self {
        Self {
            requested: None,
            resolved: None,
            kind: "none",
            status: None,
            warning: None,
        }
    }

    fn resolved(previous: &str) -> Self {
        Self {
            requested: Some(previous.to_owned()),
            resolved: Some(previous.to_owned()),
            kind: "resolved",
            status: Some("completed"),
            warning: None,
        }
    }

    fn missing(previous: &str) -> Self {
        Self {
            requested: Some(previous.to_owned()),
            resolved: None,
            kind: "missing",
            status: None,
            warning: Some("previous_response_id was not found in this CodeSeeX process"),
        }
    }

    fn non_completed(previous: &str, status: RequestStatus) -> Self {
        Self {
            requested: Some(previous.to_owned()),
            resolved: None,
            kind: "non_completed",
            status: Some(request_status_name(status)),
            warning: Some(
                "previous_response_id is not completed; local history replay was skipped",
            ),
        }
    }

    fn suppressed_service(previous: Option<&str>) -> Self {
        Self {
            requested: previous.map(str::to_owned),
            resolved: None,
            kind: "suppressed_service",
            status: None,
            warning: None,
        }
    }

    fn should_warn(&self, request: &Value) -> bool {
        self.warning.is_some() && !request_looks_like_codex_full_context(request)
    }

    fn diagnostic(&self) -> Value {
        json!({
            "requested": self.requested.as_deref(),
            "resolved": self.resolved.as_deref(),
            "kind": self.kind,
            "status": self.status,
            "warning": self.warning
        })
    }
}

fn request_status_name(status: RequestStatus) -> &'static str {
    match status {
        RequestStatus::InProgress => "in_progress",
        RequestStatus::Completed => "completed",
        RequestStatus::Failed => "failed",
        RequestStatus::Interrupted => "interrupted",
    }
}

async fn resolve_previous_response_id(
    state: &ProxyState,
    previous: Option<&str>,
) -> Result<PreviousResponseResolution, Response<Body>> {
    let Some(previous) = previous.filter(|value| !value.trim().is_empty()) else {
        return Ok(PreviousResponseResolution::none());
    };
    match state.store.response_status(previous).await {
        Ok(Some(RequestStatus::Completed)) => Ok(PreviousResponseResolution::resolved(previous)),
        Ok(Some(status)) => Ok(PreviousResponseResolution::non_completed(previous, status)),
        Ok(None) => Ok(PreviousResponseResolution::missing(previous)),
        Err(error) => Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_previous_response_check_failed",
            error.to_string(),
        )),
    }
}

async fn record_previous_response_resolution_warning(
    state: &ProxyState,
    id: &str,
    input: &Value,
    resolution: &PreviousResponseResolution,
) {
    if !resolution.should_warn(input) {
        return;
    }
    let _ = state
        .store
        .record_event(
            "warn",
            "previous_response_resolution_warning",
            "previous_response_id could not be used for local history replay.",
            Some(&json!({
                "id": id,
                "previous_response_resolution": resolution.diagnostic(),
                "requires_full_context_for_lossless_replay": true
            })),
        )
        .await;
}

#[derive(Debug, Clone, Default)]
struct RuntimeContextStorageDiagnostic {
    current_mode: &'static str,
    current_full_context_not_stored: bool,
    current_original_input_items: Option<usize>,
    current_original_input_hash: Option<String>,
    previous_full_context_not_stored: bool,
    previous_full_context_response_id: Option<String>,
    continuation_warning: bool,
}

impl RuntimeContextStorageDiagnostic {
    fn diagnostic(&self) -> Value {
        json!({
            "current": {
                "mode": self.current_mode,
                "full_context_not_stored": self.current_full_context_not_stored,
                "original_input_items": self.current_original_input_items,
                "original_input_hash": self.current_original_input_hash.as_deref()
            },
            "previous": {
                "full_context_not_stored": self.previous_full_context_not_stored,
                "response_id": self.previous_full_context_response_id.as_deref()
            },
            "continuation_warning": self.continuation_warning
        })
    }
}

async fn runtime_context_storage_diagnostic(
    state: &ProxyState,
    input: &Value,
    previous_for_context: Option<&str>,
) -> RuntimeContextStorageDiagnostic {
    let input_items = input.get("input").and_then(Value::as_array).map(Vec::len);
    let current_full_context_not_stored = request_looks_like_codex_full_context(input);
    let current_original_input_hash = current_full_context_not_stored.then(|| {
        stable_log_hash_hex(
            &serde_json::to_vec(input.get("input").unwrap_or(&Value::Null)).unwrap_or_default(),
        )
    });
    let mut diagnostic = RuntimeContextStorageDiagnostic {
        current_mode: if current_full_context_not_stored {
            "codex_full_context_not_stored"
        } else {
            "stored_runtime_context"
        },
        current_full_context_not_stored,
        current_original_input_items: input_items.filter(|_| current_full_context_not_stored),
        current_original_input_hash,
        previous_full_context_not_stored: false,
        previous_full_context_response_id: None,
        continuation_warning: false,
    };

    if let Some(previous) = previous_for_context {
        if let Some(response_id) = first_full_context_not_stored_response_id(state, previous).await
        {
            diagnostic.previous_full_context_not_stored = true;
            diagnostic.previous_full_context_response_id = Some(response_id);
            diagnostic.continuation_warning = !current_full_context_not_stored;
        }
    }
    diagnostic
}

async fn first_full_context_not_stored_response_id(
    state: &ProxyState,
    previous: &str,
) -> Option<String> {
    let chain = state
        .store
        .response_context_chain(previous, 10_000)
        .await
        .ok()?;
    chain
        .into_iter()
        .find(|record| response_has_full_context_not_stored_marker(&record.input))
        .map(|record| record.id)
}

fn response_has_full_context_not_stored_marker(input: &Value) -> bool {
    input
        .pointer("/_codeseex_runtime/mode")
        .and_then(Value::as_str)
        == Some("codex_full_context_not_stored")
}

fn stable_log_hash_hex(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

async fn record_runtime_context_storage_events(
    state: &ProxyState,
    id: &str,
    diagnostic: &RuntimeContextStorageDiagnostic,
) {
    if diagnostic.current_full_context_not_stored {
        let _ = state
            .store
            .record_event(
                "debug",
                "runtime_context_storage",
                "CodeSeeX did not duplicate Codex full-context input in runtime storage.",
                Some(&json!({
                    "id": id,
                    "runtime_context_storage": diagnostic.diagnostic()
                })),
            )
            .await;
    }
    if diagnostic.continuation_warning {
        let _ = state
            .store
            .record_event(
                "warn",
                "runtime_context_storage_warning",
                "Continuation references a prior full-context request whose original input was not duplicated in CodeSeeX runtime storage.",
                Some(&json!({
                    "id": id,
                    "runtime_context_storage": diagnostic.diagnostic(),
                    "requires_full_context_for_lossless_replay": true
                })),
            )
            .await;
    }
}

async fn record_request_shape_diagnostic(
    store: &Store,
    id: &str,
    endpoint: &str,
    requested_model: Option<&str>,
    model: &str,
    request: &Value,
) {
    let mut detail = request_shape_diagnostic(request);
    if let Some(object) = detail.as_object_mut() {
        object.insert("id".to_owned(), json!(id));
        object.insert("endpoint".to_owned(), json!(endpoint));
        if let Some(requested_model) = requested_model {
            object.insert("requested_model".to_owned(), json!(requested_model));
        }
        object.insert("model".to_owned(), json!(model));
    }
    let _ = store
        .record_event(
            "debug",
            "request_shape_diagnostic",
            "CodeSeeX request shape diagnostic.",
            Some(&detail),
        )
        .await;
}

fn response_model_from_input(config: &AppConfig, input: &Value) -> String {
    let requested = input
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    resolve_upstream_model(config, input, requested)
}

fn build_automatic_compaction(
    config: &AppConfig,
    request: &Value,
    model: &str,
    context: &crate::responses::context::BuiltResponseContext,
) -> anyhow::Result<Option<Value>> {
    let Some(threshold) = resolve_compact_threshold(request.get("context_management")) else {
        return Ok(None);
    };
    let estimated_tokens = estimate_tokens_from_messages(&context.messages);
    if estimated_tokens < threshold {
        return Ok(None);
    }
    let compaction_id = format!("cmp_{}", Uuid::new_v4().simple());
    let compact = build_compaction_item(
        config,
        &compaction_id,
        model,
        &context.messages,
        &context.tool_facts,
    )?;
    Ok(Some(compact.item))
}

fn resolve_compact_threshold(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    match value {
        Value::Null | Value::Bool(false) => None,
        Value::Number(number) => number.as_u64().filter(|threshold| *threshold > 0),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| resolve_compact_threshold(Some(item)))
            .find(|threshold| *threshold > 0),
        Value::Object(object) => {
            for key in [
                "compact_threshold",
                "threshold",
                "token_threshold",
                "max_tokens",
            ] {
                if let Some(threshold) = value_to_positive_u64(object.get(key)) {
                    return Some(threshold);
                }
            }
            object
                .get("compaction")
                .and_then(|value| resolve_compact_threshold(Some(value)))
        }
        _ => None,
    }
}

fn value_to_positive_u64(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64().filter(|value| *value > 0),
        Value::String(text) => text.trim().parse::<u64>().ok().filter(|value| *value > 0),
        _ => None,
    }
}

fn append_auto_compaction_if_safe(response: &mut Value, item: Option<&Value>) -> bool {
    let Some(item) = item else {
        return false;
    };
    let Some(output) = response.get_mut("output").and_then(Value::as_array_mut) else {
        return false;
    };
    if output
        .iter()
        .any(response_item_requires_client_tool_execution)
    {
        return false;
    }
    output.push(item.clone());
    true
}

fn response_item_requires_client_tool_execution(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("function_call") | Some("custom_tool_call")
    )
}

fn show_thinking_enabled(config: &AppConfig) -> bool {
    UserConfig::read_from(&config.config_path())
        .ok()
        .and_then(|user_config| user_config.ui.and_then(|ui| ui.show_thinking))
        .unwrap_or(true)
}

fn native_apply_patch_client_tool_sse_events(
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

fn external_client_tool_sse_events(
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

struct StreamingResponseParams {
    response_id: String,
    model: String,
    requested_model: Option<String>,
    response: reqwest::Response,
    state: ProxyState,
    config: AppConfig,
    auth: Option<String>,
    payload: Value,
    enabled_tools: Vec<String>,
    tool_execution_context: crate::tools::ToolExecutionContext,
    community_tools: Arc<crate::community_tools::CommunityToolSet>,
    external_tool_context: crate::tool_passthrough::ToolContext,
    current_image_refs: Vec<String>,
    auto_compaction: Option<Value>,
    service_kind: CodexServiceRequestKind,
    original_request: Value,
    context_diagnostic: Value,
    runtime_context_storage: Value,
}

type StreamingCancellationMap = BTreeMap<String, StreamingCancellation>;

static STREAMING_CANCELLATIONS: OnceLock<Mutex<StreamingCancellationMap>> = OnceLock::new();

#[derive(Clone)]
struct StreamingCancellation {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl StreamingCancellation {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

fn streaming_cancellations() -> &'static Mutex<StreamingCancellationMap> {
    STREAMING_CANCELLATIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn register_streaming_response(response_id: &str) -> StreamingCancellation {
    let cancelled = StreamingCancellation::new();
    if let Ok(mut active) = streaming_cancellations().lock() {
        active.insert(response_id.to_owned(), cancelled.clone());
    }
    cancelled
}

fn unregister_streaming_response(response_id: &str) {
    if let Ok(mut active) = streaming_cancellations().lock() {
        active.remove(response_id);
    }
}

fn cancel_streaming_response(response_id: &str) -> bool {
    let Ok(active) = streaming_cancellations().lock() else {
        return false;
    };
    let Some(cancelled) = active.get(response_id) else {
        return false;
    };
    cancelled.cancel();
    true
}

fn streaming_response_cancelled(cancelled: &StreamingCancellation) -> bool {
    cancelled.is_cancelled()
}

struct StreamingRequestGuard {
    store: codeseex_store::Store,
    response_id: String,
    cancelled: StreamingCancellation,
}

impl Drop for StreamingRequestGuard {
    fn drop(&mut self) {
        self.cancelled.cancel();
        unregister_streaming_response(&self.response_id);
        let store = self.store.clone();
        let response_id = self.response_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = store
                    .interrupt_request_if_in_progress(
                        &response_id,
                        "stream dropped before request completion",
                    )
                    .await;
            });
        }
    }
}

fn response_stream_from_chat(params: StreamingResponseParams) -> axum::response::Response {
    let StreamingResponseParams {
        response_id,
        model,
        requested_model,
        response,
        state,
        config,
        auth,
        mut payload,
        enabled_tools,
        tool_execution_context,
        community_tools,
        external_tool_context,
        current_image_refs,
        auto_compaction,
        service_kind,
        original_request,
        context_diagnostic,
        runtime_context_storage,
    } = params;
    let cancelled = register_streaming_response(&response_id);
    let guard = StreamingRequestGuard {
        store: state.store.clone(),
        response_id: response_id.clone(),
        cancelled: cancelled.clone(),
    };
    let stream: BoxStream<'static, Result<Bytes, std::io::Error>> = Box::pin(
        async_stream::try_stream! {
            let _stream_guard = guard;
            io_result(())?;
            macro_rules! stop_if_cancelled {
                ($reason:expr) => {{
                    if streaming_response_cancelled(&cancelled) {
                        let _ = state
                            .store
                            .interrupt_request_if_in_progress(&response_id, $reason)
                            .await;
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
                }};
            }
            let created_at = now_seconds();
            let mut sequence = 0_u64;
            let mut output_index = 0_u64;
            let mut output = Vec::new();
            let mut usage = response_usage_from_chat_usage(None);
            let mut current_payload = payload.clone();
            let mut next_response = Some(response);
            stop_if_cancelled!("response cancelled before streaming started");
            yield sse_bytes("response.created", json!({
                "type": "response.created",
                "response": {
                    "id": response_id,
                    "object": "response",
                    "created_at": created_at,
                    "model": model,
                    "status": "in_progress"
                },
                "sequence_number": next_sequence(&mut sequence)
            }));
            yield sse_bytes("response.in_progress", json!({
                "type": "response.in_progress",
                "response": {
                    "id": response_id,
                    "object": "response",
                    "created_at": created_at,
                    "model": model,
                    "status": "in_progress"
                },
                "sequence_number": next_sequence(&mut sequence)
            }));

            let visible_thinking_enabled = show_thinking_enabled(&config);
            let mut completed_tool_iterations = 0_u32;
            let mut tool_loop_diagnostics = ToolLoopDiagnostics::default();
            let mut thinking_title_emitted = false;
            while let Some(response) = next_response.take() {
                stop_if_cancelled!("response cancelled before streaming iteration");
                let iteration = completed_tool_iterations;
                let turn_item_id = format!("msg_{}", Uuid::new_v4().simple());
                let mut turn_output_index = None;
                let mut turn_output_open = false;
                let mut turn_output_closed = false;
                let mut turn_text = String::new();
                let mut turn_reasoning = String::new();
                let reasoning_item_id = format!("rs_{}", Uuid::new_v4().simple());
                let mut reasoning_output_index = None;
                let mut reasoning_open = false;
                let mut reasoning_closed = false;
                let thinking_item_id = format!("msg_{}", Uuid::new_v4().simple());
                let mut thinking_output_index = None;
                let mut thinking_open = false;
                let mut thinking_closed = false;
                let mut thinking_text = String::new();
                let mut thinking_at_line_start = true;
                let mut buffer = String::new();
                let mut output_done = false;
                let mut last_tool_index = 0_u64;
                let mut tool_states: BTreeMap<u64, StreamingToolCallState> = BTreeMap::new();
                let mut visible_tool_bridge = StreamingVisibleToolBridge::default();
                let mut upstream = response.bytes_stream();
                let mut iteration_usage = response_usage_from_chat_usage(None);

                macro_rules! close_reasoning_if_needed {
                    () => {{
                        if !reasoning_closed && !turn_reasoning.is_empty() {
                            if reasoning_open {
                                if let Some(current_output_index) = reasoning_output_index {
                                    let (bytes, item) = reasoning_done_sse_events(
                                        &config,
                                        &response_id,
                                        current_output_index,
                                        &reasoning_item_id,
                                        &turn_reasoning,
                                        &mut sequence,
                                    );
                                    yield bytes;
                                    output.push(item);
                                }
                            } else {
                                let item = reasoning_response_item(&config, &turn_reasoning, false);
                                let current_output_index = output_index;
                                output_index += 1;
                                yield hidden_reasoning_item_sse_events(
                                    &response_id,
                                    current_output_index,
                                    &item,
                                    &mut sequence,
                                );
                                output.push(item);
                            }
                            if thinking_open && !thinking_closed {
                                if let Some(current_output_index) = thinking_output_index {
                                    let (bytes, item) = thinking_display_done_sse_events(
                                        &response_id,
                                        current_output_index,
                                        &thinking_item_id,
                                        &thinking_text,
                                        &mut sequence,
                                    );
                                    yield bytes;
                                    output.push(item);
                                }
                                thinking_closed = true;
                            }
                            reasoning_closed = true;
                        }
                    }};
                }

                macro_rules! close_content_if_needed {
                    ($phase:expr) => {{
                        if turn_output_open && !turn_output_closed {
                            let current_output_index = turn_output_index.unwrap_or_default();
                            let (bytes, item) = streaming_message_done_sse_events(
                                &response_id,
                                current_output_index,
                                &turn_item_id,
                                &turn_text,
                                $phase,
                                &mut sequence,
                            );
                            yield bytes;
                            output.push(item);
                            turn_output_closed = true;
                        }
                    }};
                }

                loop {
                    let next_chunk = tokio::select! {
                        chunk = upstream.next() => chunk,
                        _ = cancelled.cancelled() => None,
                    };
                    stop_if_cancelled!("response cancelled while reading upstream stream");
                    let Some(chunk) = next_chunk else {
                        break;
                    };
                    let bytes = match chunk {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            let message = error.to_string();
                            let detail = json!({ "error": message });
                            let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                            let _ = state
                                .store
                                .record_event(
                                    "error",
                                    "request_failed",
                                    "Streaming response failed.",
                                    Some(&json!({ "id": response_id, "error": detail["error"] })),
                                )
                                .await;
                            yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "upstream_stream_failed", &detail["error"].to_string());
                            yield Bytes::from_static(b"data: [DONE]\n\n");
                            return;
                        }
                    };
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(frame) = take_sse_frame(&mut buffer) {
                        let Some(data) = sse_data(&frame) else { continue };
                        if data.trim() == "[DONE]" {
                            output_done = true;
                            break;
                        }
                        let Ok(parsed) = serde_json::from_str::<Value>(&data) else { continue };
                        if let Some(next_usage) = parsed.get("usage") {
                            let parsed_usage = response_usage_from_chat_usage(Some(next_usage));
                            iteration_usage = merge_response_usage(&iteration_usage, &parsed_usage);
                            usage = merge_response_usage(&usage, &parsed_usage);
                        }
                        let delta = parsed.pointer("/choices/0/delta").cloned().unwrap_or(Value::Null);
                        if let Some(reasoning) = delta
                            .get("reasoning_content")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty() && !reasoning_closed)
                        {
                            if !reasoning_open && !reasoning_closed && visible_thinking_enabled {
                                reasoning_open = true;
                                let current_output_index = output_index;
                                reasoning_output_index = Some(current_output_index);
                                output_index += 1;
                                yield sse_bytes("response.output_item.added", json!({
                                    "type": "response.output_item.added",
                                    "response_id": response_id,
                                    "output_index": current_output_index,
                                    "item": {
                                        "id": reasoning_item_id,
                                        "type": "reasoning",
                                        "status": "in_progress",
                                        "summary": []
                                    },
                                    "sequence_number": next_sequence(&mut sequence)
                                }));
                                yield sse_bytes("response.reasoning_summary_part.added", json!({
                                    "type": "response.reasoning_summary_part.added",
                                    "response_id": response_id,
                                    "item_id": reasoning_item_id,
                                    "output_index": current_output_index,
                                    "summary_index": 0,
                                    "part": { "type": "summary_text", "text": "" },
                                    "sequence_number": next_sequence(&mut sequence)
                                }));
                            }
                            if !thinking_open && !thinking_closed && visible_thinking_enabled {
                                thinking_open = true;
                                let current_output_index = output_index;
                                thinking_output_index = Some(current_output_index);
                                output_index += 1;
                                let thinking_prefix = if thinking_title_emitted {
                                    ""
                                } else {
                                    thinking_title_emitted = true;
                                    thinking_display_prefix()
                                };
                                thinking_text.push_str(thinking_prefix);
                                yield thinking_display_added_sse_events(
                                    &response_id,
                                    current_output_index,
                                    &thinking_item_id,
                                    thinking_prefix,
                                    &mut sequence,
                                );
                            }
                            turn_reasoning.push_str(reasoning);
                            if let Some(current_output_index) = reasoning_output_index {
                                yield sse_bytes("response.reasoning_summary_text.delta", json!({
                                    "type": "response.reasoning_summary_text.delta",
                                    "response_id": response_id,
                                    "item_id": reasoning_item_id,
                                    "output_index": current_output_index,
                                    "summary_index": 0,
                                    "delta": reasoning,
                                    "sequence_number": next_sequence(&mut sequence)
                                }));
                            }
                            if thinking_open && !thinking_closed {
                                if let Some(current_output_index) = thinking_output_index {
                                    let quoted = quote_thinking_delta(reasoning, &mut thinking_at_line_start);
                                    if !quoted.is_empty() {
                                        thinking_text.push_str(&quoted);
                                        yield thinking_display_delta_sse_event(
                                            &response_id,
                                            current_output_index,
                                            &thinking_item_id,
                                            &quoted,
                                            &mut sequence,
                                        );
                                    }
                                }
                            }
                        }
                        if let Some(content) = delta.get("content").and_then(Value::as_str).filter(|value| !value.is_empty()) {
                            if !turn_output_closed {
                                close_reasoning_if_needed!();
                                if !turn_output_open {
                                    turn_output_open = true;
                                    let current_output_index = output_index;
                                    turn_output_index = Some(current_output_index);
                                    output_index += 1;
                                    yield sse_bytes("response.output_item.added", json!({
                                        "type": "response.output_item.added",
                                        "response_id": response_id,
                                        "output_index": current_output_index,
                                        "item": {
                                            "id": turn_item_id,
                                        "type": "message",
                                        "status": "in_progress",
                                        "role": "assistant",
                                        "phase": "commentary",
                                        "content": []
                                    },
                                    "sequence_number": next_sequence(&mut sequence)
                                }));
                                    yield sse_bytes("response.content_part.added", json!({
                                        "type": "response.content_part.added",
                                        "response_id": response_id,
                                        "item_id": turn_item_id,
                                        "output_index": current_output_index,
                                        "content_index": 0,
                                        "part": { "type": "output_text", "text": "", "annotations": [] },
                                    "sequence_number": next_sequence(&mut sequence)
                                }));
                                }
                                turn_text.push_str(content);
                                let current_output_index = turn_output_index.unwrap_or_default();
                                yield sse_bytes("response.output_text.delta", json!({
                                    "type": "response.output_text.delta",
                                    "response_id": response_id,
                                    "item_id": turn_item_id,
                                    "output_index": current_output_index,
                                    "content_index": 0,
                                    "delta": content,
                                    "sequence_number": next_sequence(&mut sequence)
                                }));
                            }
                        }
                        let has_tool_delta = delta
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .map(|calls| !calls.is_empty())
                            .unwrap_or(false);
                        if has_tool_delta {
                            close_reasoning_if_needed!();
                            close_content_if_needed!("commentary");
                            for event in visible_tool_bridge.process_delta(
                                &response_id,
                                &delta,
                                &external_tool_context,
                                &mut output_index,
                                &mut sequence,
                            ) {
                                yield event;
                            }
                        }
                        collect_streaming_tool_call_deltas(&delta, &mut tool_states, &mut last_tool_index);
                    }
                    if output_done {
                        break;
                    }
                }

                stop_if_cancelled!("response cancelled after upstream stream");
                let _ = state
                    .store
                    .record_event(
                        "info",
                        "upstream_call_usage_breakdown",
                        "CodeSeeX upstream call usage breakdown.",
                        Some(&upstream_call_usage_breakdown_event(
                            &response_id,
                            "streaming_iteration",
                            iteration,
                            &original_request,
                            &current_payload,
                            Some(&iteration_usage),
                            false,
                        )),
                    )
                    .await;
                let tool_calls = streaming_tool_calls(tool_states);
                let message_phase = if tool_calls.is_empty() {
                    "final_answer"
                } else {
                    "commentary"
                };

                close_reasoning_if_needed!();
                close_content_if_needed!(message_phase);
                let _ = (reasoning_closed, thinking_closed, turn_output_closed);

                if tool_calls.is_empty() {
                    stop_if_cancelled!("response cancelled before final response persistence");
                    if !turn_text.trim().is_empty()
                        && !text_is_thinking_display_markdown(&turn_text)
                    {
                        let message = json!({
                            "role": "assistant",
                            "content": turn_text
                        });
                        if let Err(error) = state
                            .store
                            .append_request_turn_messages(&response_id, &[message])
                            .await
                        {
                            let detail = json!({ "error": error.to_string() });
                            let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                            yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "state_turn_messages_failed", &detail["error"].to_string());
                            yield Bytes::from_static(b"data: [DONE]\n\n");
                            return;
                        }
                    }
                    if !turn_output_open {
                        let empty_item_id = format!("msg_{}", Uuid::new_v4().simple());
                        let empty_output_index = output_index;
                        let item = json!({
                            "id": empty_item_id,
                            "type": "message",
                            "status": "completed",
                            "role": "assistant",
                            "phase": "final_answer",
                            "content": [{ "type": "output_text", "text": "", "annotations": [] }]
                        });
                        yield sse_bytes("response.output_item.added", json!({
                            "type": "response.output_item.added",
                            "response_id": response_id,
                            "output_index": empty_output_index,
                            "item": {
                                "id": empty_item_id,
                                "type": "message",
                                "status": "in_progress",
                                "role": "assistant",
                                "phase": "final_answer",
                                "content": []
                            },
                            "sequence_number": next_sequence(&mut sequence)
                        }));
                        yield sse_bytes("response.content_part.added", json!({
                            "type": "response.content_part.added",
                            "response_id": response_id,
                            "item_id": empty_item_id,
                            "output_index": empty_output_index,
                            "content_index": 0,
                            "part": { "type": "output_text", "text": "", "annotations": [] },
                            "sequence_number": next_sequence(&mut sequence)
                        }));
                        yield sse_bytes("response.output_text.done", json!({
                            "type": "response.output_text.done",
                            "response_id": response_id,
                            "item_id": empty_item_id,
                            "output_index": empty_output_index,
                            "content_index": 0,
                            "text": "",
                            "sequence_number": next_sequence(&mut sequence)
                        }));
                        yield sse_bytes("response.content_part.done", json!({
                            "type": "response.content_part.done",
                            "response_id": response_id,
                            "item_id": empty_item_id,
                            "output_index": empty_output_index,
                            "content_index": 0,
                            "part": item["content"][0],
                            "sequence_number": next_sequence(&mut sequence)
                        }));
                        yield sse_bytes("response.output_item.done", json!({
                            "type": "response.output_item.done",
                            "response_id": response_id,
                            "output_index": empty_output_index,
                            "item": item,
                            "sequence_number": next_sequence(&mut sequence)
                        }));
                        output.push(item);
                    }
                    if let Some(item) = auto_compaction.as_ref() {
                        let compaction_output_index = output_index;
                        yield generic_output_item_sse_events(
                            &response_id,
                            compaction_output_index,
                            item,
                            &mut sequence,
                        );
                        output.push(item.clone());
                        let _ = state
                            .store
                            .record_event(
                                "info",
                                "context_compacted",
                                "Context compacted automatically.",
                                Some(&json!({ "id": response_id })),
                            )
                        .await;
                    }
                    stop_if_cancelled!("response cancelled before final response completion");
                    let final_response = json!({
                        "id": response_id,
                        "object": "response",
                        "created_at": created_at,
                        "model": model,
                        "status": "completed",
                        "error": Value::Null,
                        "incomplete_details": Value::Null,
                        "parallel_tool_calls": true,
                        "output": output,
                        "usage": usage
                    });
                    let completion_diagnostic = service_completion_diagnostic(service_kind);
                    let _ = state
                        .store
                        .finish_request(
                            &response_id,
                            RequestStatus::Completed,
                            Some(&final_response),
                            completion_diagnostic.as_ref(),
                        )
                        .await;
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "request_completed",
                            "Streaming response completed.",
                            Some(&request_completed_detail(
                                &response_id,
                                Some(model.as_str()),
                                final_response
                                    .get("model")
                                    .and_then(Value::as_str)
                                    .or(Some(model.as_str())),
                                service_lifecycle_for_kind(service_kind),
                                Some(&final_response),
                            )),
                        )
                        .await;
                    yield sse_bytes("response.completed", json!({
                        "type": "response.completed",
                        "response": final_response,
                        "sequence_number": next_sequence(&mut sequence)
                    }));
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                }

                let all_tool_calls = tool_calls.clone();
                stop_if_cancelled!("response cancelled before tool partition");
                let partition = partition_tool_calls(
                    tool_calls,
                    &community_tools,
                    &external_tool_context,
                );
                let diagnostic = tool_loop_diagnostics.record_iteration(
                    iteration + 1,
                    &all_tool_calls,
                    &partition,
                );
                let _ = state
                    .store
                    .record_event(
                        "debug",
                        "tool_loop_iteration",
                        "CodeSeeX streaming tool loop iteration.",
                        Some(&json!({ "id": response_id, "diagnostic": diagnostic })),
                    )
                    .await;
                if let Some(unknown) = partition.unknown.first() {
                    let message = format!(
                        "tool '{}' is not available to CodeSeeX or Codex",
                        unknown.name
                    );
                    let detail = json!({ "error": message });
                    let _ = state
                        .store
                        .finish_request(&response_id, RequestStatus::Failed, None, Some(&detail))
                        .await;
                    yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "tool_loop_failed", &message);
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                }
                let proxy_executed_calls = proxy_executed_calls_in_order(&all_tool_calls, &partition);
                if let Some(disabled) = proxy_executed_calls.iter().find(|call| {
                    !is_code_tool_executable(&call.name, &enabled_tools, &community_tools)
                }) {
                        let message = format!(
                            "tool '{}' is not enabled or not executable by CodeSeeX",
                            disabled.name
                        );
                        let detail = json!({ "error": message });
                        let _ = state
                            .store
                            .finish_request(&response_id, RequestStatus::Failed, None, Some(&detail))
                            .await;
                        yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "tool_loop_failed", &message);
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                }
                let has_client_tools = partition.has_client_executed_calls();
                if has_client_tools && !partition.has_proxy_executed_calls() {
                    stop_if_cancelled!("response cancelled before client tool handoff");
                    let stored_assistant = chat_tool_calls_to_assistant_message(
                        &all_tool_calls,
                        &turn_text,
                        &turn_reasoning,
                    );
                    if let Err(error) = state
                        .store
                        .append_request_turn_messages(&response_id, &[stored_assistant])
                        .await
                    {
                        let detail = json!({ "error": error.to_string() });
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                        yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "state_turn_messages_failed", &detail["error"].to_string());
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
                    for call in &partition.native {
                        let (bytes, item) = native_apply_patch_client_tool_sse_events(
                            &response_id,
                            call,
                            &mut visible_tool_bridge,
                            &mut output_index,
                            &mut sequence,
                        );
                        yield bytes;
                        output.push(item);
                    }
                    for call in &partition.external {
                        let (bytes, item) = external_client_tool_sse_events(
                            &response_id,
                            call,
                            &external_tool_context,
                            &mut visible_tool_bridge,
                            &mut output_index,
                            &mut sequence,
                        );
                        yield bytes;
                        output.push(item);
                    }
                    stop_if_cancelled!("response cancelled before client tool handoff completion");
                    let final_response = json!({
                        "id": response_id,
                        "object": "response",
                        "created_at": created_at,
                        "model": model,
                        "status": "completed",
                        "error": Value::Null,
                        "incomplete_details": Value::Null,
                        "parallel_tool_calls": true,
                        "output": output,
                        "usage": usage
                    });
                    let diagnostic = json!({ "codeseex_lifecycle": "client_tool_handoff" });
                    let _ = state.store.finish_request(&response_id, RequestStatus::Completed, Some(&final_response), Some(&diagnostic)).await;
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "client_tool_handoff_diagnostic",
                            "CodeSeeX client tool handoff diagnostic.",
                            Some(&client_tool_handoff_diagnostic_event(
                                &response_id,
                                "streaming_tool_loop",
                                iteration,
                                &original_request,
                                &context_diagnostic,
                                &runtime_context_storage,
                                Some(&partition),
                                Some(&usage),
                            )),
                        )
                        .await;
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "request_completed",
                            "Streaming response completed.",
                            Some(&request_completed_detail(
                                &response_id,
                                Some(model.as_str()),
                                final_response
                                    .get("model")
                                    .and_then(Value::as_str)
                                    .or(Some(model.as_str())),
                                Some("client_tool_handoff"),
                                Some(&final_response),
                            )),
                        )
                        .await;
                    yield sse_bytes("response.completed", json!({
                        "type": "response.completed",
                        "response": final_response,
                        "sequence_number": next_sequence(&mut sequence)
                    }));
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                }
                if has_client_tools {
                    stop_if_cancelled!("response cancelled before mixed tool split");
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "mixed_tool_turn_split",
                            "Mixed CodeSeeX and native Codex tool calls were split; CodeSeeX tools will run first.",
                            Some(&json!({
                                "id": response_id,
                                "code_tools": partition.code.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                                "hosted_tools": partition.hosted.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                                "native_tools": partition.native.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                                "external_tools": partition.external.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                                "iteration": iteration + 1
                            })),
                        )
                        .await;
                }

                stop_if_cancelled!("response cancelled before proxy tool display");
                for item in proxy_visible_response_items(&proxy_executed_calls) {
                    let current_output_index = output_index;
                    output_index += 1;
                    match item.get("type").and_then(Value::as_str) {
                        Some("message") => {
                            yield message_item_sse_events(
                                &response_id,
                                current_output_index,
                                &item,
                                &mut sequence,
                            );
                        }
                        Some("web_search_call") => {
                            yield web_search_call_sse_events(
                                &response_id,
                                current_output_index,
                                &item,
                                &mut sequence,
                            );
                        }
                        Some("proxy_tool_call") => {
                            yield proxy_tool_call_sse_events(
                                &response_id,
                                current_output_index,
                                &item,
                                &mut sequence,
                            );
                        }
                        _ => {
                            yield generic_output_item_sse_events(
                                &response_id,
                                current_output_index,
                                &item,
                                &mut sequence,
                            );
                        }
                    }
                    output.push(item);
                }

                let stored_assistant = chat_tool_calls_to_assistant_message(
                    &all_tool_calls,
                    &turn_text,
                    &turn_reasoning,
                );
                if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
                    messages.push(chat_tool_calls_to_assistant_message(
                        &proxy_executed_calls,
                        &turn_text,
                        &turn_reasoning,
                    ));
                } else {
                    let detail = json!({ "error": "chat payload messages were not an array" });
                    let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                    yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "tool_loop_failed", "chat payload messages were not an array");
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                }
                let message_snapshot = payload
                    .get("messages")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for call in &proxy_executed_calls {
                    stop_if_cancelled!("response cancelled before proxy tool execution");
                    let _ = state
                        .store
                        .record_event(
                                "info",
                                "tool_call",
                                "CodeSeeX streaming tool requested.",
                            Some(&json!({
                                "id": response_id,
                                "call_id": call.id,
                                "name": call.name,
                                "iteration": iteration + 1
                            })),
                        )
                        .await;
                }
                let client = state.client();
                let executed_tools = tokio::select! {
                    result = execute_code_tools_concurrently(
                        &client,
                        &config,
                        &tool_execution_context,
                        &message_snapshot,
                        &current_image_refs,
                        &community_tools,
                        &proxy_executed_calls,
                    ) => Some(result),
                    _ = cancelled.cancelled() => None,
                };
                stop_if_cancelled!("response cancelled after proxy tool execution");
                let Some(executed_tools) = executed_tools else {
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                };
                let mut tool_messages = Vec::new();
                let mut repeated_failure_error = None;
                for executed in executed_tools {
                    let call = executed.call;
                    let mut result = executed.result;
                    if let Some(warning) = tool_loop_diagnostics.repeated_call_warning(&call) {
                        attach_tool_loop_warning(&mut result, &warning);
                        let _ = state
                            .store
                            .record_event(
                                "warn",
                                "tool_loop_repeated_call",
                                "CodeSeeX detected a repeated streaming tool call.",
                                Some(&json!({
                                    "id": response_id,
                                    "call_id": call.id,
                                    "name": call.name,
                                    "iteration": iteration + 1,
                                    "warning": warning
                                })),
                            )
                            .await;
                    }
                    let repeated_error = tool_loop_diagnostics
                        .record_tool_result_and_repeated_failure(&call, &result);
                    let result_text = model_replay_tool_result(&call, &result);
                    let fact = tool_fact_line(&call, &result);
                    if let Err(error) = state.store.append_request_tool_fact(&response_id, &fact).await {
                        let message = format!("failed to persist tool fact: {error}");
                        let detail = json!({ "error": message });
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                        yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "state_tool_fact_failed", &message);
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
                    let _ = state
                        .store
                        .record_event(
                                "info",
                                "tool_result",
                                "CodeSeeX streaming tool result returned.",
                            Some(&json!({
                                "id": response_id,
                                "call_id": call.id,
                                "name": call.name,
                                "iteration": iteration + 1,
                                "ok": result.get("ok").and_then(Value::as_bool),
                                "summary": summarize_tool_result_for_log(&result),
                                "result_size": crate::diagnostics::tool_result_size_summary(&result)
                            })),
                        )
                        .await;

                    if is_web_search_tool(&call.name) {
                        let replay_output = summarize_tool_result(&result);
                        let item = web_search_call_output_response_item(&call, &replay_output);
                        let result_output_index = output_index;
                        output_index += 1;
                        yield generic_output_item_sse_events(
                            &response_id,
                            result_output_index,
                            &item,
                            &mut sequence,
                        );
                        output.push(item);
                    }
                    let tool_message = json!({
                        "role": "tool",
                        "tool_call_id": call.id,
                        "content": result_text
                    });
                    tool_messages.push(tool_message.clone());
                    if let Some(error) = repeated_error {
                        repeated_failure_error = Some(error);
                    }
                    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
                        messages.push(tool_message);
                    } else {
                        let detail = json!({ "error": "chat payload messages were not an array after tool execution" });
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                        yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "tool_loop_failed", "chat payload messages were not an array after tool execution");
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
                }

                let mut stored_messages = vec![stored_assistant];
                stored_messages.extend(tool_messages);
                if let Err(error) = state
                    .store
                    .append_request_turn_messages(&response_id, &stored_messages)
                    .await
                {
                    let detail = json!({ "error": error.to_string() });
                    let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                    yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "state_turn_messages_failed", &detail["error"].to_string());
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                }
                stop_if_cancelled!("response cancelled after tool turn message persistence");
                if let Some(error) = repeated_failure_error {
                    let failed_response = failed_billable_response(
                        &response_id,
                        &model,
                        "tool_loop_repeated_failure",
                        &error,
                        &usage,
                    );
                    let detail = json!({
                        "error": error,
                        "codeseex_lifecycle": "failed_billable"
                    });
                    let _ = state.store.finish_request(&response_id, RequestStatus::Failed, Some(&failed_response), Some(&detail)).await;
                    let _ = state
                        .store
                        .record_event(
                            "warn",
                            "tool_loop_repeated_failure_stopped",
                            "CodeSeeX stopped repeated failing streaming tool calls.",
                            Some(&json!({
                                "id": response_id,
                                "iteration": iteration + 1,
                                "error": error
                            })),
                        )
                        .await;
                    yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "tool_loop_repeated_failure", &error);
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                }
                if completed_tool_iterations + 1 >= MAX_TOOL_LOOP_ITERATIONS {
                    let error = tool_loop_diagnostics.iteration_limit_error();
                    let failed_response = failed_billable_response(
                        &response_id,
                        &model,
                        "tool_loop_iteration_limit",
                        &error,
                        &usage,
                    );
                    let detail = json!({
                        "error": error,
                        "codeseex_lifecycle": "failed_billable"
                    });
                    let _ = state.store.finish_request(&response_id, RequestStatus::Failed, Some(&failed_response), Some(&detail)).await;
                    let _ = state
                        .store
                        .record_event(
                            "warn",
                            "tool_loop_iteration_limit_stopped",
                            "CodeSeeX stopped a streaming tool loop that exceeded the iteration limit.",
                            Some(&json!({
                                "id": response_id,
                                "iteration": completed_tool_iterations + 1,
                                "limit": MAX_TOOL_LOOP_ITERATIONS,
                                "error": error
                            })),
                        )
                        .await;
                    yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "tool_loop_iteration_limit", &error);
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                }

                if has_client_tools {
                    stop_if_cancelled!("response cancelled before native/external handoff");
                    for call in &partition.native {
                        let (bytes, item) = native_apply_patch_client_tool_sse_events(
                            &response_id,
                            call,
                            &mut visible_tool_bridge,
                            &mut output_index,
                            &mut sequence,
                        );
                        yield bytes;
                        output.push(item);
                    }
                    for call in &partition.external {
                        let (bytes, item) = external_client_tool_sse_events(
                            &response_id,
                            call,
                            &external_tool_context,
                            &mut visible_tool_bridge,
                            &mut output_index,
                            &mut sequence,
                        );
                        yield bytes;
                        output.push(item);
                    }
                    stop_if_cancelled!("response cancelled before native/external handoff completion");
                    let final_response = json!({
                        "id": response_id,
                        "object": "response",
                        "created_at": created_at,
                        "model": model,
                        "status": "completed",
                        "error": Value::Null,
                        "incomplete_details": Value::Null,
                        "parallel_tool_calls": true,
                        "output": output,
                        "usage": usage
                    });
                    let diagnostic = json!({ "codeseex_lifecycle": "client_tool_handoff" });
                    let _ = state.store.finish_request(&response_id, RequestStatus::Completed, Some(&final_response), Some(&diagnostic)).await;
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "client_tool_handoff_diagnostic",
                            "CodeSeeX client tool handoff diagnostic.",
                            Some(&client_tool_handoff_diagnostic_event(
                                &response_id,
                                "streaming_tool_loop",
                                iteration,
                                &original_request,
                                &context_diagnostic,
                                &runtime_context_storage,
                                Some(&partition),
                                Some(&usage),
                            )),
                        )
                        .await;
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "request_completed",
                            "Streaming response completed.",
                            Some(&request_completed_detail(
                                &response_id,
                                Some(model.as_str()),
                                final_response
                                    .get("model")
                                    .and_then(Value::as_str)
                                    .or(Some(model.as_str())),
                                Some("client_tool_handoff"),
                                Some(&final_response),
                            )),
                        )
                        .await;
                    yield sse_bytes("response.completed", json!({
                        "type": "response.completed",
                        "response": final_response,
                        "sequence_number": next_sequence(&mut sequence)
                    }));
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                }

                completed_tool_iterations += 1;
                stop_if_cancelled!("response cancelled before upstream continuation");
                current_payload = payload.clone();
                let client = state.client();
                let next_chat = tokio::select! {
                    result = crate::upstream::post_chat_completions(
                        &client,
                        &config.upstream,
                        auth.as_deref(),
                        Some(&original_request),
                        current_payload.clone(),
                    ) => Some(result),
                    _ = cancelled.cancelled() => None,
                };
                stop_if_cancelled!("response cancelled during upstream continuation");
                let Some(next_chat) = next_chat else {
                    yield Bytes::from_static(b"data: [DONE]\n\n");
                    return;
                };
                match next_chat {
                    Ok(next) if next.status().is_success() => {
                        next_response = Some(next);
                    }
                    Ok(next) => {
                        let status = next.status();
                        let body = next.text().await.unwrap_or_else(|error| error.to_string());
                        let message = format!("upstream returned {status} after streaming tool execution: {body}");
                        let detail = json!({ "error": message });
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                        let _ = state
                            .store
                            .record_event(
                                "info",
                                "retry_cache_diagnostic",
                                "CodeSeeX retry/cache diagnostic.",
                                Some(&retry_cache_diagnostic_event(
                                    &response_id,
                                    requested_model.as_deref(),
                                    Some(model.as_str()),
                                    &original_request,
                                    Some(&current_payload),
                                    "streaming_upstream_status_failed_after_tool",
                                )),
                            )
                            .await;
                        yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "upstream_after_tool_failed", &message);
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let detail = json!({ "error": message });
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                        let _ = state
                            .store
                            .record_event(
                                "info",
                                "retry_cache_diagnostic",
                                "CodeSeeX retry/cache diagnostic.",
                                Some(&retry_cache_diagnostic_event(
                                    &response_id,
                                    requested_model.as_deref(),
                                    Some(model.as_str()),
                                    &original_request,
                                    Some(&current_payload),
                                    "streaming_upstream_connection_failed_after_tool",
                                )),
                            )
                            .await;
                        yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "upstream_connection_failed", &message);
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
                }
            }
        },
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests;
