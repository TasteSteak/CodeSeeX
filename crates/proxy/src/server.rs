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
    generic_output_item_sse_events, hidden_reasoning_item_sse_events, message_item_sse_events,
    next_sequence, proxy_tool_call_sse_events, quote_thinking_delta, reasoning_done_sse_events,
    reasoning_response_item, sse_bytes, sse_data, stream_failed_event,
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
    collect_streaming_tool_call_deltas, insert_streaming_tool_calls, streaming_tool_calls,
    StreamingToolCallState, StreamingVisibleToolBridge,
};
use crate::responses::usage::{merge_response_usage, response_usage_from_chat_usage};
use crate::tools::chat_protocol::{chat_tool_calls, chat_tool_calls_to_assistant_message};
use crate::tools::coordinator::{complete_chat_with_tools, ToolLoopContext, ToolLoopResult};
use crate::tools::diagnostics::{
    attach_tool_loop_warning, prepare_tool_loop_recovery_payload, ToolLoopDiagnostics,
    ToolLoopStop, MAX_TOOL_LOOP_ITERATIONS,
};
use crate::tools::hosted::{
    execute_code_tools_concurrently, is_code_tool_executable, model_replay_tool_result,
    summarize_tool_result, tool_fact_line, tool_result_event_detail,
};
use crate::tools::ownership::{
    is_web_search_tool, partition_tool_calls, proxy_executed_calls_in_order,
};
use crate::tools::registry::{
    dedupe_tool_definitions, enabled_tool_ids, normalized_tool_choice, tool_settings,
};
use crate::tools::response_items::{
    proxy_visible_response_items, web_search_call_output_response_item,
};
use crate::upstream::deepseek::{
    should_adapt_tool_protocol, tool_protocol::DeepSeekStreamToolAdapter,
};
use crate::upstream::payload::{
    codex_service_request_kind, normalize_chat_payload, request_is_codex_service,
    CodexServiceRequestKind,
};
mod access;
mod request_diagnostics;
mod response_helpers;
mod response_lifecycle;
mod router;
mod search_probe;
mod streaming_state;
use access::upstream_authorization_from_headers;
#[cfg(test)]
pub(crate) use access::CODESEEX_V1_ACCESS_TOKEN_HEADER;
use axum::body::{Body, Bytes};
#[cfg(test)]
use axum::extract::DefaultBodyLimit;
use axum::extract::{Path, State};
#[cfg(test)]
use axum::http::Method;
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode};
use axum::response::IntoResponse;
#[cfg(test)]
use axum::routing::post;
use axum::Json;
#[cfg(test)]
use axum::Router;
use codeseex_core::context::request_looks_like_codex_full_context;
use codeseex_core::models::available_models;
use codeseex_core::protocol::ChatMessage;
use codeseex_core::{AppConfig, UserConfig};
use codeseex_store::{RequestStatus, Store};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
#[cfg(test)]
pub(crate) use request_diagnostics::request_has_codex_native_tool_surface;
use request_diagnostics::{
    adapt_deepseek_chat_tool_protocol_for_non_streaming, codex_tool_search_bridge_decision,
    immediate_previous_response_tool_call_ids, payload_tools_available,
    record_cost_risk_diagnostic, service_completion_diagnostic, service_lifecycle_for_kind,
    service_request_diagnostic, should_inject_codeseex_proxy_tools, tool_exposure_diagnostic,
};
use response_helpers::{
    client_handoff_guard_terminal_diagnostic, client_handoff_guard_terminal_response,
    client_handoff_guard_terminal_sse, external_client_tool_sse_events, failed_billable_response,
    native_apply_patch_client_tool_sse_events, prepend_response_output_items,
    record_apply_patch_input_micro_repair_diagnostic,
    record_apply_patch_input_micro_repair_diagnostics, record_client_tool_handoff_guard_stop,
    request_completed_detail, response_id_from_input, show_thinking_enabled,
    upstream_body_read_error_detail, upstream_error_detail, upstream_json_parse_error_detail,
};
#[cfg(test)]
pub(crate) use response_lifecycle::resolve_compact_threshold;
use response_lifecycle::{
    append_auto_compaction_if_safe, build_automatic_compaction, client_handoff_guard_calls,
    ensure_new_response_id, record_previous_response_resolution_warning,
    record_request_shape_diagnostic, record_runtime_context_storage_events,
    resolve_previous_response_id, resolve_prompt_cache_session_anchor, response_model_from_input,
    runtime_context_storage_diagnostic, PreviousResponseResolution,
};
use router::app_router;
use search_probe::spawn_default_web_search_source_probe_subscriber;
#[cfg(test)]
use search_probe::spawn_web_search_source_probe_subscriber;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use streaming_state::{
    cancel_streaming_response, register_streaming_response, streaming_response_cancelled,
    StreamingRequestGuard,
};
use tokio::net::TcpListener;
use uuid::Uuid;

const RESPONSES_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const STORED_FULL_CONTEXT_TURN_CONTENT_CHARS: usize = 32 * 1024;

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
    let search_source_probe = spawn_default_web_search_source_probe_subscriber(
        state.runtime_config.clone(),
        search_source_probe_changes,
        shutdown_store.clone(),
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

fn proxy_base_url_for_listener(config: &AppConfig, local_addr: SocketAddr) -> String {
    let port = local_addr.port();
    if port == config.port {
        return config.proxy_base_url();
    }
    format!("http://{}:{port}/v1", config.host)
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
    record_cost_risk_diagnostic(
        &state.store,
        &id,
        "/v1/chat/completions",
        &original_payload,
        Some(&payload),
    )
    .await;

    let auth = upstream_authorization_from_headers(&headers, &state.v1_access_token);
    if let Some(auth) = auth.as_deref() {
        codeseex_core::codex_auth::remember_authorization_header(auth);
    }
    let client = state.client();
    match crate::upstream::post_chat_completions(
        &client,
        &config.upstream,
        auth.as_deref(),
        Some(&state.v1_access_token),
        Some(&original_payload),
        payload.clone(),
    )
    .await
    {
        Ok(response) => {
            let status = response.status();
            let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
            let response_headers = response.headers().clone();
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
                        let detail = upstream_body_read_error_detail(
                            &id,
                            requested_model.as_deref(),
                            model.as_deref(),
                            status,
                            &response_headers,
                            &error,
                        );
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
                                Some(&detail),
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
    } else if let Some(anchor) = resolve_prompt_cache_session_anchor(&state, &input).await {
        anchor
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
    if let Some(limit) = built_context.upstream_context_limit.as_ref() {
        let detail = json!({
            "error": "authoritative Codex replay exceeds the configured upstream context window",
            "context_limit": limit.clone(),
            "context": built_context.diagnostic.clone()
        });
        let _ = state
            .store
            .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
            .await;
        let _ = state
            .store
            .record_event(
                "warn",
                "context_limit_exceeded",
                "Authoritative Codex replay exceeds the upstream context window.",
                Some(&json!({ "id": id, "context_limit": limit.clone() })),
            )
            .await;
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "context_limit_exceeded",
            "The Codex context exceeds the configured upstream window. Compact the Codex thread and retry; CodeSeeX did not truncate the replay.".to_owned(),
        );
    }
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
        full_context_current_turn_messages(&built_context.current_messages)
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
    record_cost_risk_diagnostic(&state.store, &id, "/v1/responses", &input, Some(&payload)).await;

    if let Some(stop) = match state
        .store
        .settle_client_tool_handoff_outputs(&id, &input)
        .await
    {
        Ok(stop) => stop,
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "client_tool_handoff_guard_failed",
                error.to_string(),
            );
        }
    } {
        let terminal_response = client_handoff_guard_terminal_response(
            &id,
            &model,
            &stop.code,
            &stop.message,
            &Value::Null,
        );
        let detail = client_handoff_guard_terminal_diagnostic(&stop);
        let _ = state
            .store
            .finish_request(
                &id,
                RequestStatus::Failed,
                Some(&terminal_response),
                Some(&detail),
            )
            .await;
        record_client_tool_handoff_guard_stop(&state.store, &id, &stop).await;
        let _ = state
            .store
            .record_event(
                "error",
                "request_failed",
                "CodeSeeX stopped repeated client tool handoffs.",
                Some(&json!({
                    "id": id,
                    "requested_model": requested_model.as_deref(),
                    "model": upstream_model.as_str(),
                    "error": stop.message.clone()
                })),
            )
            .await;
        if stream_requested {
            let mut sequence = 0_u64;
            let mut bytes = client_handoff_guard_terminal_sse(
                &id,
                &model,
                &stop.code,
                &stop.message,
                &Value::Null,
                &mut sequence,
            )
            .to_vec();
            bytes.extend_from_slice(b"data: [DONE]\n\n");
            return response_from_bytes(
                reqwest::StatusCode::OK,
                Some(HeaderValue::from_static("text/event-stream")),
                bytes,
            );
        }
        return json_response(terminal_response);
    }

    if let Some(stop) = match state
        .store
        .client_tool_handoff_guard_preflight(&id, &input)
        .await
    {
        Ok(stop) => stop,
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "client_tool_handoff_guard_failed",
                error.to_string(),
            );
        }
    } {
        let terminal_response = client_handoff_guard_terminal_response(
            &id,
            &model,
            &stop.code,
            &stop.message,
            &Value::Null,
        );
        let detail = client_handoff_guard_terminal_diagnostic(&stop);
        let _ = state
            .store
            .finish_request(
                &id,
                RequestStatus::Failed,
                Some(&terminal_response),
                Some(&detail),
            )
            .await;
        record_client_tool_handoff_guard_stop(&state.store, &id, &stop).await;
        let _ = state
            .store
            .record_event(
                "warn",
                "request_failed",
                "CodeSeeX ended a repeated client tool handoff before another upstream call.",
                Some(&request_completed_detail(
                    &id,
                    Some(model.as_str()),
                    Some(model.as_str()),
                    Some("failed_billable"),
                    Some(&terminal_response),
                )),
            )
            .await;
        if stream_requested {
            let mut sequence = 0_u64;
            let mut bytes = client_handoff_guard_terminal_sse(
                &id,
                &model,
                &stop.code,
                &stop.message,
                &Value::Null,
                &mut sequence,
            )
            .to_vec();
            bytes.extend_from_slice(b"data: [DONE]\n\n");
            return response_from_bytes(
                reqwest::StatusCode::OK,
                Some(HeaderValue::from_static("text/event-stream")),
                bytes,
            );
        }
        return json_response(terminal_response);
    }

    let auth = upstream_authorization_from_headers(&headers, &state.v1_access_token);
    if let Some(auth) = auth.as_deref() {
        codeseex_core::codex_auth::remember_authorization_header(auth);
    }
    let client = state.client();
    let upstream_started = std::time::Instant::now();
    match crate::upstream::post_chat_completions(
        &client,
        &config.upstream,
        auth.as_deref(),
        Some(&state.v1_access_token),
        Some(&input),
        payload.clone(),
    )
    .await
    {
        Ok(response) => {
            let status = response.status();
            let response_headers = response.headers().clone();
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
                        let detail = upstream_body_read_error_detail(
                            &id,
                            requested_model.as_deref(),
                            Some(upstream_model.as_str()),
                            status,
                            &response_headers,
                            &error,
                        );
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
                                Some(&detail),
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
            match response.bytes().await {
                Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                    Ok(mut chat) => {
                        adapt_deepseek_chat_tool_protocol_for_non_streaming(
                            &state.store,
                            &id,
                            &config,
                            upstream_model.as_str(),
                            &mut chat,
                            payload_tools_available(&payload),
                            "non_streaming_initial",
                        )
                        .await;
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
                                    Some(upstream_started.elapsed().as_millis() as u64),
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
                            local_access_token: Some(&state.v1_access_token),
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
                        let tool_loop_result = match complete_chat_with_tools(
                            tool_loop_context,
                            payload,
                            chat,
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(error) => {
                                let error_code = error.code.clone();
                                let failed_response = failed_billable_response(
                                    &id,
                                    &model,
                                    &error_code,
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
                                    &error_code,
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
                                let tool_calls = chat_tool_calls(&result.chat);
                                let partition = partition_tool_calls(
                                    tool_calls,
                                    &community_tools,
                                    &external_tool_context,
                                );
                                record_apply_patch_input_micro_repair_diagnostics(
                                    &state.store,
                                    &id,
                                    &partition.native,
                                )
                                .await;
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
                        let detail = upstream_json_parse_error_detail(
                            &id,
                            requested_model.as_deref(),
                            Some(upstream_model.as_str()),
                            status,
                            &response_headers,
                            bytes.len(),
                            &error,
                        );
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
                                Some(&detail),
                            )
                            .await;
                        json_error(
                            StatusCode::BAD_GATEWAY,
                            "upstream_json_failed",
                            error.to_string(),
                        )
                    }
                },
                Err(error) => {
                    let detail = upstream_body_read_error_detail(
                        &id,
                        requested_model.as_deref(),
                        Some(upstream_model.as_str()),
                        status,
                        &response_headers,
                        &error,
                    );
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
                            Some(&detail),
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

fn full_context_current_turn_messages(messages: &[ChatMessage]) -> Vec<Value> {
    let current_turn = messages
        .iter()
        .cloned()
        .map(compact_turn_message_for_runtime)
        .collect::<Vec<_>>();
    chat_messages_to_values(&current_turn)
}

fn compact_turn_message_for_runtime(mut message: ChatMessage) -> ChatMessage {
    message.content = truncate_chars(&message.content, STORED_FULL_CONTEXT_TURN_CONTENT_CHARS);
    message.reasoning_content = None;
    message
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    format!(
        "{prefix}...[truncated chars={} bytes={}]",
        text.chars().count(),
        text.len()
    )
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
    let guard =
        StreamingRequestGuard::new(state.store.clone(), response_id.clone(), cancelled.clone());
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
            let adapt_deepseek_tool_protocol =
                should_adapt_tool_protocol(&config.upstream, &model);
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
                let mut provider_content_tool_adapter = DeepSeekStreamToolAdapter::default();
                let mut provider_reasoning_tool_adapter = DeepSeekStreamToolAdapter::default();
                let mut visible_tool_bridge = StreamingVisibleToolBridge::default();
                let iteration_started = std::time::Instant::now();
                let mut upstream = response.bytes_stream();
                let mut iteration_usage = response_usage_from_chat_usage(None);
                let mut iteration_usage_chunks = 0_u32;

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

                macro_rules! emit_content_delta {
                    ($content:expr) => {{
                        let content = $content;
                        if !content.is_empty() && !turn_output_closed {
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
                    }};
                }

                macro_rules! emit_reasoning_delta {
                    ($reasoning:expr) => {{
                        let reasoning = $reasoning;
                        if !reasoning.is_empty() && !reasoning_closed {
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
                    }};
                }

                macro_rules! handle_provider_tool_content {
                    ($chunk:expr, $tools_available:expr, $channel:expr) => {{
                        let tool_content = $chunk;
                        let channel = $channel;
                        if channel == "reasoning_content" {
                            emit_reasoning_delta!(&tool_content.visible_text);
                        } else {
                            emit_content_delta!(&tool_content.visible_text);
                        }
                        if !tool_content.tool_calls.is_empty() {
                            let tool_names = tool_content
                                .tool_calls
                                .iter()
                                .map(|call| call.name.clone())
                                .collect::<Vec<_>>();
                            let _ = state
                                .store
                                .record_event(
                                    "info",
                                    "deepseek_tool_protocol_adapted",
                                    "DeepSeek tool protocol content was adapted into standard tool calls.",
                                    Some(&json!({
                                        "id": response_id,
                                        "iteration": iteration + 1,
                                        "channel": channel,
                                        "tool_count": tool_content.tool_calls.len(),
                                        "tool_names": tool_names
                                    })),
                                )
                                .await;
                            insert_streaming_tool_calls(
                                tool_content.tool_calls,
                                &mut tool_states,
                                &mut last_tool_index,
                            );
                        }
                        if tool_content.blocked {
                            let _ = state
                                .store
                                .record_event(
                                    "warn",
                                    "deepseek_tool_protocol_blocked",
                                    "DeepSeek tool protocol content was blocked because this upstream turn has no tools.",
                                    Some(&json!({
                                        "id": response_id,
                                        "iteration": iteration + 1,
                                        "channel": channel,
                                        "tools_available": $tools_available
                                    })),
                                )
                                .await;
                        }
                        if tool_content.parse_failed {
                            let _ = state
                                .store
                                .record_event(
                                    "warn",
                                    "deepseek_tool_protocol_parse_failed",
                                    "DeepSeek tool protocol content could not be parsed.",
                                    Some(&json!({
                                        "id": response_id,
                                        "iteration": iteration + 1,
                                        "channel": channel,
                                        "tools_available": $tools_available
                                    })),
                                )
                                .await;
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
                            usage = merge_response_usage(&usage, &iteration_usage);
                            let _ = state
                                .store
                                .record_event(
                                    "info",
                                    "upstream_call_usage_breakdown",
                                    "CodeSeeX upstream call usage breakdown.",
                                    Some(&upstream_call_usage_breakdown_event(
                                        &response_id,
                                        "streaming_iteration_failed",
                                        iteration,
                                        &original_request,
                                        &current_payload,
                                        Some(&iteration_usage),
                                        Some(iteration_started.elapsed().as_millis() as u64),
                                        false,
                                    )),
                                )
                                .await;
                            let failed_response = failed_billable_response(
                                &response_id,
                                &model,
                                "upstream_stream_failed",
                                &message,
                                &usage,
                            );
                            let detail = json!({
                                "error": message,
                                "codeseex_lifecycle": "failed_billable"
                            });
                            let _ = state
                                .store
                                .finish_request(
                                    &response_id,
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
                            iteration_usage_chunks = iteration_usage_chunks.saturating_add(1);
                            iteration_usage = parsed_usage;
                        }
                        let delta = parsed.pointer("/choices/0/delta").cloned().unwrap_or(Value::Null);
                        if let Some(reasoning) = delta
                            .get("reasoning_content")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty() && !reasoning_closed)
                        {
                            if adapt_deepseek_tool_protocol {
                                let tools_available = payload
                                    .get("tools")
                                    .and_then(Value::as_array)
                                    .map(|tools| !tools.is_empty())
                                    .unwrap_or(false);
                                let tool_content =
                                    provider_reasoning_tool_adapter.push(reasoning, tools_available);
                                handle_provider_tool_content!(
                                    tool_content,
                                    tools_available,
                                    "reasoning_content"
                                );
                            } else {
                                emit_reasoning_delta!(reasoning);
                            }
                        }
                        if let Some(content) = delta.get("content").and_then(Value::as_str).filter(|value| !value.is_empty()) {
                            if adapt_deepseek_tool_protocol {
                                let tools_available = payload
                                    .get("tools")
                                    .and_then(Value::as_array)
                                    .map(|tools| !tools.is_empty())
                                    .unwrap_or(false);
                                let tool_content =
                                    provider_content_tool_adapter.push(content, tools_available);
                                handle_provider_tool_content!(
                                    tool_content,
                                    tools_available,
                                    "content"
                                );
                            } else {
                                emit_content_delta!(content);
                            }
                        }
                        let has_tool_delta = delta
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .map(|calls| !calls.is_empty())
                            .unwrap_or(false);
                        if has_tool_delta {
                            let tools_available = payload
                                .get("tools")
                                .and_then(Value::as_array)
                                .map(|tools| !tools.is_empty())
                                .unwrap_or(false);
                            if tools_available {
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
                                collect_streaming_tool_call_deltas(
                                    &delta,
                                    &mut tool_states,
                                    &mut last_tool_index,
                                );
                            } else {
                                let tool_count = delta
                                    .get("tool_calls")
                                    .and_then(Value::as_array)
                                    .map(Vec::len)
                                    .unwrap_or(0);
                                let _ = state
                                    .store
                                    .record_event(
                                        "warn",
                                        "streaming_tool_call_blocked",
                                        "Streaming tool call deltas were blocked because this upstream turn has no tools.",
                                        Some(&json!({
                                            "id": response_id,
                                            "iteration": iteration + 1,
                                            "tool_count": tool_count
                                        })),
                                    )
                                    .await;
                            }
                        }
                    }
                    if output_done {
                        break;
                    }
                }

                if adapt_deepseek_tool_protocol {
                    let tools_available = payload
                        .get("tools")
                        .and_then(Value::as_array)
                        .map(|tools| !tools.is_empty())
                        .unwrap_or(false);
                    let tool_content = provider_reasoning_tool_adapter.finish(tools_available);
                    handle_provider_tool_content!(
                        tool_content,
                        tools_available,
                        "reasoning_content"
                    );
                    let tool_content = provider_content_tool_adapter.finish(tools_available);
                    handle_provider_tool_content!(tool_content, tools_available, "content");
                }

                stop_if_cancelled!("response cancelled after upstream stream");
                usage = merge_response_usage(&usage, &iteration_usage);
                if iteration_usage_chunks > 1 {
                    let _ = state
                        .store
                        .record_event(
                            "debug",
                            "upstream_usage_chunk_diagnostic",
                            "CodeSeeX observed multiple usage chunks in one streaming upstream iteration.",
                            Some(&json!({
                                "id": response_id,
                                "iteration": iteration,
                                "usage_chunks": iteration_usage_chunks,
                                "merge_mode": "last_chunk_per_iteration"
                            })),
                        )
                        .await;
                }
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
                            Some(iteration_started.elapsed().as_millis() as u64),
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
                    if let Some(stop) = match state
                        .store
                        .record_client_tool_handoff_calls(
                            &response_id,
                            &original_request,
                            &client_handoff_guard_calls(&partition),
                            Some(&usage),
                        )
                        .await
                    {
                        Ok(stop) => stop,
                        Err(error) => {
                            let detail = json!({ "error": error.to_string() });
                            let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                            yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "client_tool_handoff_guard_failed", &detail["error"].to_string());
                            yield Bytes::from_static(b"data: [DONE]\n\n");
                            return;
                        }
                    } {
                        record_client_tool_handoff_guard_stop(&state.store, &response_id, &stop).await;
                        let terminal_response = client_handoff_guard_terminal_response(
                            &response_id,
                            &model,
                            &stop.code,
                            &stop.message,
                            &usage,
                        );
                        let detail = client_handoff_guard_terminal_diagnostic(&stop);
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, Some(&terminal_response), Some(&detail)).await;
                        yield client_handoff_guard_terminal_sse(
                            &response_id,
                            &model,
                            &stop.code,
                            &stop.message,
                            &usage,
                            &mut sequence,
                        );
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
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
                        record_apply_patch_input_micro_repair_diagnostic(
                            &state.store,
                            &response_id,
                            call,
                        )
                        .await;
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
                let mut repeated_failure_stop = None;
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
                            Some(&tool_result_event_detail(
                                &response_id,
                                &call,
                                iteration + 1,
                                &result,
                            )),
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
                    if let Some(stop) = repeated_error {
                        repeated_failure_stop = Some(stop);
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
                if let Some(stop) = tool_loop_diagnostics.web_search_budget_stop() {
                    let _ = state
                        .store
                        .record_event(
                            "warn",
                            "tool_loop_web_search_budget_stopped",
                            "CodeSeeX stopped repeated streaming web_search calls.",
                            Some(&json!({
                                "id": response_id,
                                "iteration": iteration + 1,
                                "error": stop.message,
                                "recover_with_final_response": stop.recover_with_final_response
                            })),
                        )
                        .await;
                    match recover_streaming_tool_loop_with_final_response(
                        &state,
                        &config,
                        auth.as_deref(),
                        &original_request,
                        requested_model.as_deref(),
                        &model,
                        &response_id,
                        &mut payload,
                        &stop,
                        "web_search budget",
                    )
                    .await {
                        Ok(recovery) => {
                            completed_tool_iterations += 1;
                            current_payload = payload.clone();
                            next_response = Some(recovery);
                            continue;
                        }
                        Err(error) => {
                            let failed_response = failed_billable_response(
                                &response_id,
                                &model,
                                "tool_loop_web_search_budget_recovery_failed",
                                &error,
                                &usage,
                            );
                            let detail = json!({
                                "error": error,
                                "codeseex_lifecycle": "failed_billable"
                            });
                            let _ = state.store.finish_request(&response_id, RequestStatus::Failed, Some(&failed_response), Some(&detail)).await;
                            yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "tool_loop_web_search_budget_recovery_failed", &error);
                            yield Bytes::from_static(b"data: [DONE]\n\n");
                            return;
                        }
                    }
                }
                if let Some(stop) = repeated_failure_stop {
                    let _ = state
                        .store
                        .record_event(
                            "warn",
                            "tool_loop_repeated_failure_stopped",
                            "CodeSeeX stopped repeated failing streaming tool calls.",
                            Some(&json!({
                                "id": response_id,
                                "iteration": iteration + 1,
                                "error": stop.message,
                                "recover_with_final_response": stop.recover_with_final_response
                            })),
                        )
                        .await;
                    if stop.recover_with_final_response {
                        match recover_streaming_tool_loop_with_final_response(
                            &state,
                            &config,
                            auth.as_deref(),
                            &original_request,
                            requested_model.as_deref(),
                            &model,
                            &response_id,
                            &mut payload,
                            &stop,
                            "repeated failure",
                        )
                        .await {
                            Ok(recovery) => {
                                completed_tool_iterations += 1;
                                current_payload = payload.clone();
                                next_response = Some(recovery);
                                continue;
                            }
                            Err(error) => {
                                let failed_response = failed_billable_response(
                                    &response_id,
                                    &model,
                                    "tool_loop_repeated_failure_recovery_failed",
                                    &error,
                                    &usage,
                                );
                                let detail = json!({
                                    "error": error,
                                    "codeseex_lifecycle": "failed_billable"
                                });
                                let _ = state.store.finish_request(&response_id, RequestStatus::Failed, Some(&failed_response), Some(&detail)).await;
                                yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "tool_loop_repeated_failure_recovery_failed", &error);
                                yield Bytes::from_static(b"data: [DONE]\n\n");
                                return;
                            }
                        }
                    }
                    let error = stop.message;
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
                    if let Some(stop) = match state
                        .store
                        .record_client_tool_handoff_calls(
                            &response_id,
                            &original_request,
                            &client_handoff_guard_calls(&partition),
                            Some(&usage),
                        )
                        .await
                    {
                        Ok(stop) => stop,
                        Err(error) => {
                            let detail = json!({ "error": error.to_string() });
                            let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                            yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "client_tool_handoff_guard_failed", &detail["error"].to_string());
                            yield Bytes::from_static(b"data: [DONE]\n\n");
                            return;
                        }
                    } {
                        record_client_tool_handoff_guard_stop(&state.store, &response_id, &stop).await;
                        let terminal_response = client_handoff_guard_terminal_response(
                            &response_id,
                            &model,
                            &stop.code,
                            &stop.message,
                            &usage,
                        );
                        let detail = client_handoff_guard_terminal_diagnostic(&stop);
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, Some(&terminal_response), Some(&detail)).await;
                        yield client_handoff_guard_terminal_sse(
                            &response_id,
                            &model,
                            &stop.code,
                            &stop.message,
                            &usage,
                            &mut sequence,
                        );
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
                    for call in &partition.native {
                        record_apply_patch_input_micro_repair_diagnostic(
                            &state.store,
                            &response_id,
                            call,
                        )
                        .await;
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
                        Some(&state.v1_access_token),
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

#[allow(clippy::too_many_arguments)]
async fn recover_streaming_tool_loop_with_final_response(
    state: &ProxyState,
    config: &codeseex_core::AppConfig,
    auth: Option<&str>,
    original_request: &Value,
    requested_model: Option<&str>,
    model: &str,
    response_id: &str,
    payload: &mut Value,
    stop: &ToolLoopStop,
    phase: &'static str,
) -> Result<reqwest::Response, String> {
    prepare_tool_loop_recovery_payload(payload, &stop.message)
        .map_err(|message| message.to_owned())?;
    let client = state.client();
    let response = match crate::upstream::post_chat_completions(
        &client,
        &config.upstream,
        auth,
        Some(&state.v1_access_token),
        Some(original_request),
        payload.clone(),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = state
                .store
                .record_event(
                    "info",
                    "retry_cache_diagnostic",
                    "CodeSeeX retry/cache diagnostic.",
                    Some(&retry_cache_diagnostic_event(
                        response_id,
                        requested_model,
                        Some(model),
                        original_request,
                        Some(payload),
                        "streaming_upstream_connection_failed_after_tool_loop_recovery",
                    )),
                )
                .await;
            return Err(error.to_string());
        }
    };
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| error.to_string());
    let _ = state
        .store
        .record_event(
            "info",
            "retry_cache_diagnostic",
            "CodeSeeX retry/cache diagnostic.",
            Some(&retry_cache_diagnostic_event(
                response_id,
                requested_model,
                Some(model),
                original_request,
                Some(payload),
                "streaming_upstream_status_failed_after_tool_loop_recovery",
            )),
        )
        .await;
    Err(format!(
        "upstream returned {status} during streaming {phase} recovery: {body}"
    ))
}

#[cfg(test)]
mod tests;
