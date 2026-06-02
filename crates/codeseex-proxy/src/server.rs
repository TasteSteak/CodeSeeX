use crate::app_state::ProxyState;
use crate::http_response::{
    json_error, passthrough_stream_with_completion, response_content_type_json,
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
use crate::responses::context::{
    build_response_context, chat_messages_to_values, deterministic_compaction_summary,
    estimate_tokens_from_messages, estimate_tokens_from_text,
};
use crate::responses::conversion::{
    chat_completion_to_response, chat_completion_tool_calls_to_response, final_chat_turn_message,
};
use crate::responses::stream_tool_calls::{
    collect_streaming_tool_call_deltas, streaming_tool_calls, StreamingToolCallState,
};
use crate::responses::usage::{merge_response_usage, response_usage_from_chat_usage};
use crate::text::compact_line;
use crate::tools::chat_protocol::chat_tool_calls_to_assistant_message;
use crate::tools::coordinator::{complete_chat_with_tools, ToolLoopContext, ToolLoopResult};
use crate::tools::diagnostics::ToolLoopDiagnostics;
use crate::tools::hosted::{
    execute_code_tool, is_code_tool_executable, summarize_tool_result, tool_fact_line,
};
use crate::tools::ownership::{partition_tool_calls, proxy_executed_calls_in_order};
use crate::tools::registry::{
    dedupe_tool_definitions, enabled_tool_ids, normalized_tool_choice, tool_settings,
};
use crate::tools::response_items::{
    native_apply_patch_response_item_from_chat_call, proxy_visible_response_items,
};
use crate::upstream::payload::normalize_chat_payload;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use codeseex_core::context::redact_inline_data_urls;
use codeseex_core::models::available_models;
use codeseex_core::protocol::ChatMessage;
use codeseex_core::{AppConfig, UserConfig};
use codeseex_store::{RequestStatus, Store};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

pub async fn serve(config: AppConfig) -> anyhow::Result<()> {
    let store = Store::open(&config.database_path()).await?;
    let timeout = std::time::Duration::from_millis(config.upstream.timeout_ms);
    let state = ProxyState {
        config: Arc::new(config.clone()),
        client: reqwest::Client::builder().timeout(timeout).build()?,
        store,
    };

    ensure_catalog(&config)?;

    let app = Router::new()
        .merge(crate::manager_api::router())
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses/compact", post(responses_compact))
        .route("/v1/responses", post(responses))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_headers(Any)
                .allow_methods(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = TcpListener::bind((config.host.as_str(), config.port)).await?;
    tracing::info!(
        "CodeSeeX Next proxy listening on {}",
        config.proxy_base_url()
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn models() -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": available_models().into_iter().map(|model| json!({
            "id": model.slug,
            "object": "model",
            "created": 0,
            "owned_by": "codeseex-next",
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
    normalize_chat_payload(&config, &original_payload, &mut payload);
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
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
                "requested_model": requested_model,
                "model": model
            })),
        )
        .await;

    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    match crate::upstream::post_chat_completions(
        &state.client,
        &config.upstream,
        auth.as_deref(),
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
                let stream =
                    passthrough_stream_with_completion(response, state.store.clone(), id.clone());
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
                        if let Err(error) = state
                            .store
                            .finish_request(&id, status_to_store, body_json.as_ref(), None)
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
                                Some(&json!({
                                    "id": id,
                                    "status": status.as_u16(),
                                    "upstream_error": if status.is_success() { Value::Null } else { upstream_error }
                                })),
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
                                Some(&json!({ "id": id, "error": error.to_string() })),
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
                    Some(&json!({ "id": id, "error": error.to_string() })),
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
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("resp_{}", Uuid::new_v4().simple()));
    let previous = input.get("previous_response_id").and_then(Value::as_str);
    let model = input
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("deepseek-v4-pro");
    let started_at = now_seconds();

    if let Err(error) = state
        .store
        .checkpoint_request(&id, previous, Some(model), &input)
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
            Some(&json!({ "id": id, "previous_response_id": previous })),
        )
        .await;

    let built_context = build_response_context(&state, &input, previous).await;
    let summary = deterministic_compaction_summary(&built_context.messages);
    let compaction_id = format!("cmp_{}", Uuid::new_v4().simple());
    let output_item = json!({
        "id": compaction_id,
        "type": "compaction",
        "status": "completed",
        "summary": [{ "type": "summary_text", "text": summary }],
        "content": [{ "type": "output_text", "text": summary }]
    });
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

    Json(response).into_response()
}

async fn responses(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> impl IntoResponse {
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("resp_{}", Uuid::new_v4().simple()));
    let previous = input.get("previous_response_id").and_then(Value::as_str);
    let model = input
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("deepseek-v4-pro");
    let built_context = build_response_context(&state, &input, previous).await;
    let context_diagnostic = built_context.diagnostic.clone();
    let history_message_count = built_context.history_message_count;
    let stream_requested = input
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut payload = json!({
        "model": model,
        "messages": if built_context.messages.is_empty() { vec![ChatMessage::text("user", "")] } else { built_context.messages },
        "stream": stream_requested
    });
    let config = state.active_config();
    normalize_chat_payload(&config, &input, &mut payload);
    let enabled_tools = enabled_tool_ids(&config);
    let tool_settings = tool_settings(&config);
    let community_tools = crate::community_tools::CommunityToolSet::load(
        &config.data_dir,
        &enabled_tools,
        &tool_settings,
    );
    let external_tool_context =
        crate::tool_passthrough::ToolContext::from_request_tools(input.get("tools"));
    let mut tools = crate::tools::executable_tool_definitions(&enabled_tools);
    tools.extend(community_tools.definitions());
    tools.extend(external_tool_context.upstream_tools.clone());
    let tools = dedupe_tool_definitions(tools);
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
        .unwrap_or(model)
        .to_owned();
    if let Err(error) = state
        .store
        .checkpoint_request(&id, previous, Some(&upstream_model), &input)
        .await
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_checkpoint_failed",
            error.to_string(),
        );
    }
    if let Err(error) = state
        .store
        .replace_request_turn_messages(
            &id,
            &chat_messages_to_values(&built_context.current_messages),
        )
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
                "history_messages": history_message_count,
                "context": context_diagnostic,
                "requested_model": model,
                "model": upstream_model
            })),
        )
        .await;

    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    match crate::upstream::post_chat_completions(
        &state.client,
        &config.upstream,
        auth.as_deref(),
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
                                "error",
                                "request_failed",
                                "Responses request failed.",
                                Some(&json!({
                                    "id": id,
                                    "status": status.as_u16(),
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
                                "error",
                                "request_failed",
                                "Failed to read upstream response body.",
                                Some(&json!({ "id": id, "error": error.to_string() })),
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
                    response,
                    state: state.clone(),
                    config,
                    auth,
                    payload,
                    enabled_tools,
                    community_tools: Arc::new(community_tools),
                    external_tool_context,
                });
            }
            match response.json::<Value>().await {
                Ok(chat) => {
                    let tool_loop_context = ToolLoopContext {
                        client: &state.client,
                        store: &state.store,
                        config: &config,
                        auth: auth.as_deref(),
                        request_id: &id,
                        enabled_tools: &enabled_tools,
                        community_tools: &community_tools,
                        external_tool_context: &external_tool_context,
                    };
                    let tool_loop_result =
                        match complete_chat_with_tools(tool_loop_context, payload, chat).await {
                            Ok(result) => result,
                            Err(error) => {
                                let detail = json!({ "error": error });
                                let _ = state
                                    .store
                                    .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                                    .await;
                                let _ = state
                                    .store
                                    .record_event(
                                        "error",
                                        "request_failed",
                                        "Tool execution loop failed.",
                                        Some(&json!({ "id": id, "error": error })),
                                    )
                                    .await;
                                return json_error(
                                    StatusCode::BAD_GATEWAY,
                                    "tool_loop_failed",
                                    error,
                                );
                            }
                        };
                    let mapped = match tool_loop_result {
                        ToolLoopResult::FinalChat(chat) => {
                            if let Some(message) = final_chat_turn_message(&chat) {
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
                            chat_completion_to_response(
                                &id,
                                model,
                                chat,
                                show_thinking_enabled(&config),
                            )
                        }
                        ToolLoopResult::ClientToolCalls(chat) => {
                            chat_completion_tool_calls_to_response(
                                &id,
                                model,
                                chat,
                                &community_tools,
                                &external_tool_context,
                                show_thinking_enabled(&config),
                            )
                        }
                    };
                    if let Err(error) = state
                        .store
                        .finish_request(&id, RequestStatus::Completed, Some(&mapped), None)
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
                            "request_completed",
                            "Responses request completed.",
                            Some(&json!({ "id": id })),
                        )
                        .await;
                    Json(mapped).into_response()
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
                    "error",
                    "request_failed",
                    "Failed to connect to upstream.",
                    Some(&json!({ "id": id, "error": error.to_string() })),
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

fn upstream_error_detail(body_json: Option<&Value>, bytes: &[u8]) -> Value {
    let message = body_json
        .and_then(|body| body.pointer("/error/message").and_then(Value::as_str))
        .or_else(|| body_json.and_then(|body| body.get("message").and_then(Value::as_str)))
        .map(str::to_owned)
        .unwrap_or_else(|| compact_line(&String::from_utf8_lossy(bytes), 2_000));
    json!({
        "message": message,
        "body": compact_line(&String::from_utf8_lossy(bytes), 4_000)
    })
}

fn show_thinking_enabled(config: &AppConfig) -> bool {
    UserConfig::read_from(&config.config_path())
        .ok()
        .and_then(|user_config| user_config.ui.and_then(|ui| ui.show_thinking))
        .unwrap_or(true)
}

struct StreamingResponseParams {
    response_id: String,
    model: String,
    response: reqwest::Response,
    state: ProxyState,
    config: AppConfig,
    auth: Option<String>,
    payload: Value,
    enabled_tools: Vec<String>,
    community_tools: Arc<crate::community_tools::CommunityToolSet>,
    external_tool_context: crate::tool_passthrough::ToolContext,
}

fn response_stream_from_chat(params: StreamingResponseParams) -> axum::response::Response {
    let StreamingResponseParams {
        response_id,
        model,
        response,
        state,
        config,
        auth,
        mut payload,
        enabled_tools,
        community_tools,
        external_tool_context,
    } = params;
    let stream: BoxStream<'static, Result<Bytes, std::io::Error>> = Box::pin(
        async_stream::try_stream! {
            io_result(())?;
            let created_at = now_seconds();
            let mut sequence = 0_u64;
            let mut output_index = 0_u64;
            let mut output = Vec::new();
            let mut usage = response_usage_from_chat_usage(None);
            let mut next_response = Some(response);

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
                let mut upstream = response.bytes_stream();

                macro_rules! close_reasoning_if_needed {
                    () => {{
                        if !reasoning_closed && !turn_reasoning.is_empty() {
                            if reasoning_open {
                                if let Some(current_output_index) = reasoning_output_index {
                                    let (bytes, item) = reasoning_done_sse_events(
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
                                let item = reasoning_response_item(&turn_reasoning, false);
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

                while let Some(chunk) = upstream.next().await {
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
                            usage = merge_response_usage(
                                &usage,
                                &response_usage_from_chat_usage(Some(next_usage)),
                            );
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
                        }
                        collect_streaming_tool_call_deltas(&delta, &mut tool_states, &mut last_tool_index);
                    }
                    if output_done {
                        break;
                    }
                }

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
                    if !turn_text.trim().is_empty() {
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
                    let _ = state.store.finish_request(&response_id, RequestStatus::Completed, Some(&final_response), None).await;
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "request_completed",
                            "Streaming response completed.",
                            Some(&json!({ "id": response_id })),
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
                        "tool '{}' is not available to CodeSeeX Next or Codex",
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
                            "tool '{}' is not enabled or not executable by CodeSeeX Next",
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
                        let item = native_apply_patch_response_item_from_chat_call(call);
                        let call_output_index = output_index;
                        output_index += 1;
                        yield custom_tool_call_sse_added(&response_id, call_output_index, &item, &mut sequence);
                        if let Some(input) = item.get("input").and_then(Value::as_str).filter(|value| !value.is_empty()) {
                            yield sse_bytes("response.custom_tool_call_input.delta", json!({
                                "type": "response.custom_tool_call_input.delta",
                                "response_id": response_id,
                                "item_id": item["id"],
                                "output_index": call_output_index,
                                "delta": input,
                                "sequence_number": next_sequence(&mut sequence)
                            }));
                        }
                        yield custom_tool_call_sse_done(&response_id, call_output_index, &item, &mut sequence);
                        output.push(item);
                    }
                    for call in &partition.external {
                        let item = external_tool_context.response_item_from_chat_call(call);
                        let call_output_index = output_index;
                        output_index += 1;
                        yield function_call_sse_added(&response_id, call_output_index, &item, &mut sequence);
                        if !call.arguments.is_empty() {
                            yield sse_bytes("response.function_call_arguments.delta", json!({
                                "type": "response.function_call_arguments.delta",
                                "response_id": response_id,
                                "item_id": item["id"],
                                "output_index": call_output_index,
                                "delta": call.arguments,
                                "sequence_number": next_sequence(&mut sequence)
                            }));
                        }
                        yield function_call_sse_done(&response_id, call_output_index, &item, &mut sequence);
                        output.push(item);
                    }
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
                    let _ = state.store.finish_request(&response_id, RequestStatus::Completed, Some(&final_response), None).await;
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "request_completed",
                            "Streaming response completed with native external tool call.",
                            Some(&json!({ "id": response_id })),
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

                for call in &proxy_executed_calls {
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
                    let result = execute_code_tool(&state.client, &community_tools, call).await;
                    let result_text = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_owned());
                    let result_text = redact_inline_data_urls(&result_text);
                    let fact = tool_fact_line(call, &result);
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
                                "summary": summarize_tool_result(&result)
                            })),
                        )
                        .await;

                    let tool_message = json!({
                        "role": "tool",
                        "tool_call_id": call.id,
                        "content": result_text
                    });
                    if let Err(error) = state
                        .store
                        .append_request_turn_messages(&response_id, std::slice::from_ref(&tool_message))
                        .await
                    {
                        let detail = json!({ "error": error.to_string() });
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                        yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "state_turn_messages_failed", &detail["error"].to_string());
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
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

                if has_client_tools {
                    for call in &partition.native {
                        let item = native_apply_patch_response_item_from_chat_call(call);
                        let call_output_index = output_index;
                        output_index += 1;
                        yield custom_tool_call_sse_added(&response_id, call_output_index, &item, &mut sequence);
                        if let Some(input) = item.get("input").and_then(Value::as_str).filter(|value| !value.is_empty()) {
                            yield sse_bytes("response.custom_tool_call_input.delta", json!({
                                "type": "response.custom_tool_call_input.delta",
                                "response_id": response_id,
                                "item_id": item["id"],
                                "output_index": call_output_index,
                                "delta": input,
                                "sequence_number": next_sequence(&mut sequence)
                            }));
                        }
                        yield custom_tool_call_sse_done(&response_id, call_output_index, &item, &mut sequence);
                        output.push(item);
                    }
                    for call in &partition.external {
                        let item = external_tool_context.response_item_from_chat_call(call);
                        let call_output_index = output_index;
                        output_index += 1;
                        yield function_call_sse_added(&response_id, call_output_index, &item, &mut sequence);
                        if !call.arguments.is_empty() {
                            yield sse_bytes("response.function_call_arguments.delta", json!({
                                "type": "response.function_call_arguments.delta",
                                "response_id": response_id,
                                "item_id": item["id"],
                                "output_index": call_output_index,
                                "delta": call.arguments,
                                "sequence_number": next_sequence(&mut sequence)
                            }));
                        }
                        yield function_call_sse_done(&response_id, call_output_index, &item, &mut sequence);
                        output.push(item);
                    }
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
                    let _ = state.store.finish_request(&response_id, RequestStatus::Completed, Some(&final_response), None).await;
                    let _ = state
                        .store
                        .record_event(
                            "info",
                            "request_completed",
                            "Streaming response completed after CodeSeeX and native/external tool split.",
                            Some(&json!({ "id": response_id })),
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
                match crate::upstream::post_chat_completions(
                    &state.client,
                    &config.upstream,
                    auth.as_deref(),
                    payload.clone(),
                )
                .await
                {
                    Ok(next) if next.status().is_success() => {
                        next_response = Some(next);
                    }
                    Ok(next) => {
                        let status = next.status();
                        let body = next.text().await.unwrap_or_else(|error| error.to_string());
                        let message = format!("upstream returned {status} after streaming tool execution: {body}");
                        let detail = json!({ "error": message });
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
                        yield stream_failed_event(&response_id, &model, created_at, &mut sequence, "upstream_after_tool_failed", &message);
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let detail = json!({ "error": message });
                        let _ = state.store.finish_request(&response_id, RequestStatus::Failed, None, Some(&detail)).await;
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
mod tests {
    use super::*;
    use crate::responses::context::{
        response_history_messages, response_output_tool_call_messages,
    };
    use crate::tools::chat_protocol::assistant_message_from_chat_tool_subset;
    use crate::tools::ownership::ChatToolCall;
    use codeseex_core::config::UpstreamConfig;
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeUpstreamState {
        requests: Arc<Mutex<Vec<Value>>>,
    }

    fn test_config(data_dir: PathBuf) -> AppConfig {
        AppConfig {
            data_dir,
            ..Default::default()
        }
    }

    fn test_config_with_upstream(data_dir: PathBuf, fake_addr: SocketAddr) -> AppConfig {
        AppConfig {
            data_dir,
            upstream: UpstreamConfig {
                base_url: format!("http://{fake_addr}"),
                official_v1_compat: false,
                api_key: Some("test-key".to_owned()),
                timeout_ms: 30_000,
            },
            ..Default::default()
        }
    }

    async fn fake_streaming_chat_completions(
        State(state): State<FakeUpstreamState>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        let request_index = {
            let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
            requests.push(payload);
            requests.len()
        };
        let body = if request_index == 1 {
            concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"need directory\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_ls\",\"type\":\"function\",\"function\":{\"name\":\"list_directory\",\"arguments\":\"{\\\"path\\\":\\\".\\\"}\"}}]}}]}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
        } else {
            concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"after tool\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"directory checked\"}}],\"usage\":{\"prompt_tokens\":10,\"prompt_cache_hit_tokens\":4,\"completion_tokens\":2,\"total_tokens\":12}}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
        };
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(body))
            .expect("fake upstream response should build")
    }

    async fn fake_mixed_streaming_chat_completions(
        State(state): State<FakeUpstreamState>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        let request_index = {
            let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
            requests.push(payload);
            requests.len()
        };
        let body = if request_index == 1 {
            concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"need directory first\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
                "{\"index\":0,\"id\":\"call_ls\",\"type\":\"function\",\"function\":{\"name\":\"list_directory\",\"arguments\":\"{\\\"path\\\":\\\".\\\"}\"}},",
                "{\"index\":1,\"id\":\"call_js\",\"type\":\"function\",\"function\":{\"name\":\"js\",\"arguments\":\"{\\\"code\\\":\\\"1+1\\\"}\"}}",
                "]}}]}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
        } else {
            concat!(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_js_2\",\"type\":\"function\",\"function\":{\"name\":\"js\",\"arguments\":\"{\\\"code\\\":\\\"1+1\\\"}\"}}]}}]}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
        };
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(body))
            .expect("fake upstream response should build")
    }

    async fn fake_apply_patch_streaming_chat_completions(
        State(state): State<FakeUpstreamState>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        let has_tool_result = payload
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| {
                messages.iter().any(|message| {
                    message.get("role").and_then(Value::as_str) == Some("tool")
                        && message.get("tool_call_id").and_then(Value::as_str) == Some("call_patch")
                })
            })
            .unwrap_or(false);
        {
            let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
            requests.push(payload);
        }
        let body = if has_tool_result {
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"patch-ok\"}}]}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
        } else {
            concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"patch the file natively\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_patch\",\"type\":\"function\",\"function\":{\"name\":\"apply_patch\",\"arguments\":\"{\\\"patch\\\":\\\"*** Begin Patch\\\\n*** Add File: target/codeseex-next-apply-patch-streaming-test/hello.txt\\\\n+hello\\\\n*** End Patch\\\"}\"}}]}}]}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
        };
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(body))
            .expect("fake upstream response should build")
    }

    async fn fake_reasoning_then_content_streaming_chat_completions(
        State(state): State<FakeUpstreamState>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        {
            let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
            requests.push(payload);
        }
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think before answering\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"final answer\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(body))
            .expect("fake upstream response should build")
    }

    async fn fake_final_chat_completions(
        State(state): State<FakeUpstreamState>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        {
            let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
            requests.push(payload);
        }
        Json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "tool result acknowledged" }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13 }
        }))
        .into_response()
    }

    #[test]
    fn maps_chat_usage_to_responses_usage_shape() {
        let usage = json!({
            "prompt_tokens": 100,
            "prompt_cache_hit_tokens": 60,
            "completion_tokens": 20,
            "completion_tokens_details": { "reasoning_tokens": 7 },
            "total_tokens": 120
        });

        let mapped = response_usage_from_chat_usage(Some(&usage));

        assert_eq!(mapped["input_tokens"], 100);
        assert_eq!(mapped["cached_input_tokens"], 60);
        assert_eq!(mapped["cache_miss_input_tokens"], 40);
        assert_eq!(mapped["input_tokens_details"]["cached_tokens"], 60);
        assert_eq!(mapped["output_tokens"], 20);
        assert_eq!(mapped["reasoning_output_tokens"], 7);
        assert_eq!(mapped["output_tokens_details"]["reasoning_tokens"], 7);
        assert_eq!(mapped["total_tokens"], 120);
    }

    #[test]
    fn mapped_response_keeps_codex_completion_metadata() {
        let chat = json!({
            "choices": [{
                "message": { "role": "assistant", "content": "ok" }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        });

        let response = chat_completion_to_response("resp_test", "deepseek-v4-pro", chat, true);

        assert_eq!(response["status"], "completed");
        assert_eq!(response["error"], Value::Null);
        assert_eq!(response["incomplete_details"], Value::Null);
        assert_eq!(response["parallel_tool_calls"], true);
        assert_eq!(response["usage"]["input_tokens"], 10);
        assert_eq!(response["usage"]["output_tokens"], 2);
    }

    #[test]
    fn chat_payload_forwards_codex_generation_parameters() {
        let config = test_config(std::env::temp_dir().join(format!(
            "codeseex-next-payload-params-test-{}",
            Uuid::new_v4().simple()
        )));
        let request = json!({
            "temperature": 0.7,
            "top_p": 0.8,
            "max_output_tokens": 1234,
            "text": { "format": { "type": "json_schema" } },
            "reasoning": { "effort": "xhigh" }
        });
        let mut payload = json!({
            "model": "deepseek-v4-pro",
            "messages": [],
            "stream": true
        });

        normalize_chat_payload(&config, &request, &mut payload);

        assert_eq!(payload["temperature"], json!(0.7));
        assert_eq!(payload["top_p"], json!(0.8));
        assert_eq!(payload["max_tokens"], json!(1234));
        assert_eq!(payload["response_format"], json!({ "type": "json_object" }));
        assert_eq!(payload["thinking"], json!({ "type": "enabled" }));
        assert_eq!(payload["stream_options"], json!({ "include_usage": true }));
    }

    #[test]
    fn configured_temperature_overrides_request_temperature() {
        let config = AppConfig {
            temperature: codeseex_core::models::TemperaturePreset::Strict,
            ..Default::default()
        };
        let mut payload = json!({
            "model": "deepseek-v4-pro",
            "messages": [],
            "stream": false
        });

        normalize_chat_payload(&config, &json!({ "temperature": 1.5 }), &mut payload);

        assert_eq!(payload["temperature"], json!(0.0));
    }

    #[test]
    fn tool_choice_none_is_not_rewritten_to_auto() {
        let tools = vec![json!({
            "type": "function",
            "function": { "name": "read_file_range" }
        })];

        assert_eq!(
            normalized_tool_choice(Some(&json!("none")), &tools),
            Some(json!("none"))
        );
        assert_eq!(
            normalized_tool_choice(
                Some(&json!({ "type": "function", "function": { "name": "read_file_range" } })),
                &tools
            ),
            Some(json!({ "type": "function", "function": { "name": "read_file_range" } }))
        );
    }

    #[test]
    fn streaming_tool_loop_preserves_deepseek_reasoning_content() {
        let tool_calls = vec![ChatToolCall {
            id: "call_abc".to_owned(),
            name: "list_directory".to_owned(),
            arguments: r#"{"path":"."}"#.to_owned(),
        }];

        let message =
            chat_tool_calls_to_assistant_message(&tool_calls, "", "look up the directory first");

        assert_eq!(message["role"], "assistant");
        assert_eq!(message["content"], "");
        assert_eq!(message["reasoning_content"], "look up the directory first");
        assert_eq!(message["tool_calls"][0]["id"], "call_abc");
        assert_eq!(
            message["tool_calls"][0]["function"]["name"],
            "list_directory"
        );
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"."}"#
        );
    }

    #[test]
    fn mixed_native_and_code_tool_replay_keeps_only_executed_code_tools() {
        let chat = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "inspect before patching",
                    "tool_calls": [
                        {
                            "id": "call_code",
                            "type": "function",
                            "function": {
                                "name": "list_directory",
                                "arguments": "{\"path\":\".\"}"
                            }
                        },
                        {
                            "id": "call_patch",
                            "type": "function",
                            "function": {
                                "name": "apply_patch",
                                "arguments": "{\"patch\":\"*** Begin Patch\\n*** End Patch\"}"
                            }
                        }
                    ]
                }
            }]
        });
        let code_tool_calls = vec![ChatToolCall {
            id: "call_code".to_owned(),
            name: "list_directory".to_owned(),
            arguments: r#"{"path":"."}"#.to_owned(),
        }];

        let assistant = assistant_message_from_chat_tool_subset(&chat, &code_tool_calls);

        assert_eq!(assistant["reasoning_content"], "inspect before patching");
        let calls = assistant["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call_code");
        assert_eq!(calls[0]["function"]["name"], "list_directory");
    }

    #[test]
    fn internal_code_tools_keep_codeseex_ownership_without_client_function_calls() {
        let chat = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_internal",
                            "type": "function",
                            "function": {
                                "name": "list_directory",
                                "arguments": "{\"path\":\".\"}"
                            }
                        },
                        {
                            "id": "call_external",
                            "type": "function",
                            "function": {
                                "name": "js",
                                "arguments": "{\"code\":\"1+1\"}"
                            }
                        }
                    ]
                }
            }]
        });
        let tool_context = crate::tool_passthrough::ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "function": {
                    "name": "list_directory",
                    "description": "A colliding client-side tool that must not override CodeSeeX internal ownership.",
                    "parameters": { "type": "object", "properties": {} }
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "js",
                    "description": "Run JavaScript.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }
        ])));

        let response = chat_completion_tool_calls_to_response(
            "resp_test",
            "deepseek-v4-pro",
            chat,
            &crate::community_tools::CommunityToolSet::default(),
            &tool_context,
            true,
        );
        let output = response["output"].as_array().unwrap();

        assert!(output.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("proxy_tool_call")
                && item.get("name").and_then(Value::as_str) == Some("list_directory")
        }));
        assert!(output.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("name").and_then(Value::as_str) == Some("js")
                && item.get("call_id").and_then(Value::as_str) == Some("call_external")
        }));
        assert!(!output.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("name").and_then(Value::as_str) == Some("list_directory")
        }));
    }

    #[test]
    fn internal_code_tools_do_not_trigger_external_passthrough() {
        let tool_calls = vec![ChatToolCall {
            id: "call_internal".to_owned(),
            name: "workspace_search".to_owned(),
            arguments: r#"{"query":"needle"}"#.to_owned(),
        }];
        let community_tools = crate::community_tools::CommunityToolSet::default();
        let external_tool_context =
            crate::tool_passthrough::ToolContext::from_request_tools(Some(&json!([
                {
                    "type": "function",
                    "function": {
                        "name": "workspace_search",
                        "description": "A colliding client-side tool.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "js",
                        "description": "Run JavaScript.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                }
            ])));

        let partition = partition_tool_calls(tool_calls, &community_tools, &external_tool_context);
        assert_eq!(partition.code.len(), 1);
        assert!(partition.external.is_empty());
        assert!(partition.unknown.is_empty());
    }

    #[test]
    fn web_search_maps_to_native_response_item_not_proxy_tool_item() {
        let chat = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_web",
                        "type": "function",
                        "function": {
                            "name": "web_search",
                            "arguments": "{\"query\":\"today weather\"}"
                        }
                    }]
                }
            }]
        });
        let response = chat_completion_tool_calls_to_response(
            "resp_web",
            "deepseek-v4-pro",
            chat,
            &crate::community_tools::CommunityToolSet::default(),
            &crate::tool_passthrough::ToolContext::default(),
            true,
        );
        let output = response["output"].as_array().unwrap();

        assert!(output.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("web_search_call")
                && item.get("call_id").and_then(Value::as_str) == Some("call_web")
        }));
        assert!(!output.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("proxy_tool_call")
                && item.get("name").and_then(Value::as_str) == Some("web_search")
        }));
        let web_item = output
            .iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
            .unwrap();
        let mut sequence = 0;
        let events = String::from_utf8(
            web_search_call_sse_events("resp_web", 0, web_item, &mut sequence).to_vec(),
        )
        .unwrap();
        assert!(events.contains("response.web_search_call.searching"));
        assert!(!events.contains("proxy_tool_call"));
    }

    #[test]
    fn proxy_tool_call_sse_uses_in_progress_then_completed_lifecycle() {
        let item = json!({
            "id": "ptc_test",
            "type": "proxy_tool_call",
            "status": "completed",
            "call_id": "call_test",
            "name": "list_directory",
            "arguments": "{\"path\":\".\"}"
        });
        let mut sequence = 0;
        let events = String::from_utf8(
            proxy_tool_call_sse_events("resp_tool", 0, &item, &mut sequence).to_vec(),
        )
        .unwrap();

        let added = events
            .find("response.output_item.added")
            .expect("proxy tool call should emit added");
        let in_progress = events
            .find("\"status\":\"in_progress\"")
            .expect("added proxy tool call should be in progress");
        let done = events
            .find("response.output_item.done")
            .expect("proxy tool call should emit done");
        let completed = events
            .find("\"status\":\"completed\"")
            .expect("done proxy tool call should be completed");
        assert!(added < done, "{events}");
        assert!(in_progress < completed, "{events}");
    }

    #[test]
    fn proxy_visible_items_preserve_tool_order_while_grouping_proxy_usage() {
        let calls = vec![
            ChatToolCall {
                id: "call_ls_1".to_owned(),
                name: "list_directory".to_owned(),
                arguments: r#"{"path":"."}"#.to_owned(),
            },
            ChatToolCall {
                id: "call_web".to_owned(),
                name: "web_search".to_owned(),
                arguments: r#"{"query":"CodeSeeX"}"#.to_owned(),
            },
            ChatToolCall {
                id: "call_ls_2".to_owned(),
                name: "workspace_search".to_owned(),
                arguments: r#"{"query":"needle"}"#.to_owned(),
            },
        ];

        let items = proxy_visible_response_items(&calls);
        let types = items
            .iter()
            .map(|item| item.get("type").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>();

        assert_eq!(
            types,
            vec![
                "message",
                "proxy_tool_call",
                "web_search_call",
                "message",
                "proxy_tool_call"
            ]
        );
        assert_eq!(items[1]["call_id"], "call_ls_1");
        assert_eq!(items[2]["call_id"], "call_web");
        assert_eq!(items[4]["call_id"], "call_ls_2");
    }

    #[tokio::test]
    async fn streaming_closes_thinking_before_final_content() {
        let fake_state = FakeUpstreamState::default();
        let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let fake_addr = fake_listener.local_addr().unwrap();
        let fake_app = Router::new()
            .route(
                "/chat/completions",
                post(fake_reasoning_then_content_streaming_chat_completions),
            )
            .with_state(fake_state);
        tokio::spawn(async move {
            axum::serve(fake_listener, fake_app).await.unwrap();
        });

        let data_dir = std::env::temp_dir().join(format!(
            "codeseex-next-thinking-order-test-{}",
            Uuid::new_v4().simple()
        ));
        let config = test_config_with_upstream(data_dir, fake_addr);
        let store = Store::open(&config.database_path()).await.unwrap();
        let proxy_state = ProxyState {
            config: Arc::new(config),
            client: reqwest::Client::new(),
            store,
        };
        let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_app = Router::new()
            .route("/v1/responses", post(responses))
            .with_state(proxy_state);
        tokio::spawn(async move {
            axum::serve(proxy_listener, proxy_app).await.unwrap();
        });

        let body = reqwest::Client::new()
            .post(format!("http://{proxy_addr}/v1/responses"))
            .json(&json!({
                "id": "resp_stream_thinking_order",
                "model": "deepseek-v4-pro",
                "stream": true,
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "answer once" }]
                }]
            }))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        let reasoning_done = body
            .find("response.reasoning_summary_text.done")
            .expect("reasoning should be closed");
        let thinking_done = body
            .find("\"codeseex_display_only\":\"thinking_markdown\"")
            .expect("thinking display item should be emitted");
        let content_delta = body
            .find("\"delta\":\"final answer\"")
            .expect("final content should stream");
        assert!(reasoning_done < content_delta, "{body}");
        assert!(thinking_done < content_delta, "{body}");
    }

    #[tokio::test]
    async fn streaming_internal_tools_execute_inside_proxy_without_client_function_call() {
        let fake_state = FakeUpstreamState::default();
        let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let fake_addr = fake_listener.local_addr().unwrap();
        let fake_app = Router::new()
            .route("/chat/completions", post(fake_streaming_chat_completions))
            .with_state(fake_state.clone());
        tokio::spawn(async move {
            axum::serve(fake_listener, fake_app).await.unwrap();
        });

        let data_dir = std::env::temp_dir().join(format!(
            "codeseex-next-streaming-test-{}",
            Uuid::new_v4().simple()
        ));
        let config = test_config_with_upstream(data_dir, fake_addr);
        let store = Store::open(&config.database_path()).await.unwrap();
        let proxy_state = ProxyState {
            config: Arc::new(config),
            client: reqwest::Client::new(),
            store,
        };
        let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_app = Router::new()
            .route("/v1/responses", post(responses))
            .with_state(proxy_state);
        tokio::spawn(async move {
            axum::serve(proxy_listener, proxy_app).await.unwrap();
        });

        let body = reqwest::Client::new()
            .post(format!("http://{proxy_addr}/v1/responses"))
            .json(&json!({
                "id": "resp_stream_internal_tool",
                "model": "deepseek-v4-pro",
                "stream": true,
                "input": [
                    {
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "list files then answer" }]
                    }
                ]
            }))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert!(
            body.contains("response.reasoning_summary_text.delta"),
            "{body}"
        );
        assert!(body.contains("DeepSeek Thinking"), "{body}");
        assert!(body.contains("codeseex_display_only"), "{body}");
        assert_eq!(
            body.matches("\"delta\":\"**DeepSeek Thinking**\\n\"")
                .count(),
            1,
            "{body}"
        );
        assert!(body.contains("after tool"), "{body}");
        assert!(body.contains("已使用工具 `list_directory`"), "{body}");
        assert!(body.contains("\"type\":\"proxy_tool_call\""), "{body}");
        assert!(body.contains("directory checked"), "{body}");
        assert!(
            !body.contains("response.function_call_arguments.delta"),
            "{body}"
        );
        assert!(!body.contains("\"type\":\"function_call\""), "{body}");
        assert!(!body.contains("unsupported call"), "{body}");
        let reasoning_done = body
            .find("response.reasoning_summary_text.done")
            .expect("reasoning should close before tool display");
        let thinking_added = body
            .find("\"codeseex_display_only\":\"thinking_markdown\"")
            .expect("thinking display should be emitted before tool display");
        let proxy_tool = body
            .find("\"type\":\"proxy_tool_call\"")
            .expect("proxy tool item should be emitted");
        assert!(reasoning_done < proxy_tool, "{body}");
        assert!(thinking_added < proxy_tool, "{body}");

        let requests = fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .clone();
        assert_eq!(requests.len(), 2);
        let second_messages = requests[1]["messages"].as_array().unwrap();
        let assistant_tool_message = second_messages
            .iter()
            .find(|message| {
                message.get("role").and_then(Value::as_str) == Some("assistant")
                    && message.get("tool_calls").is_some()
            })
            .expect("second upstream request should include assistant tool call message");
        assert_eq!(
            assistant_tool_message["reasoning_content"],
            "need directory"
        );
        assert!(second_messages.iter().any(|message| {
            message.get("role").and_then(Value::as_str) == Some("tool")
                && message.get("tool_call_id").and_then(Value::as_str) == Some("call_ls")
        }));
    }

    #[tokio::test]
    async fn current_input_tool_outputs_replay_as_chat_tool_protocol() {
        let fake_state = FakeUpstreamState::default();
        let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let fake_addr = fake_listener.local_addr().unwrap();
        let fake_app = Router::new()
            .route("/chat/completions", post(fake_final_chat_completions))
            .with_state(fake_state.clone());
        tokio::spawn(async move {
            axum::serve(fake_listener, fake_app).await.unwrap();
        });

        let data_dir = std::env::temp_dir().join(format!(
            "codeseex-next-current-tool-replay-test-{}",
            Uuid::new_v4().simple()
        ));
        let config = test_config_with_upstream(data_dir, fake_addr);
        let store = Store::open(&config.database_path()).await.unwrap();
        let proxy_state = ProxyState {
            config: Arc::new(config),
            client: reqwest::Client::new(),
            store,
        };
        let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_app = Router::new()
            .route("/v1/responses", post(responses))
            .with_state(proxy_state);
        tokio::spawn(async move {
            axum::serve(proxy_listener, proxy_app).await.unwrap();
        });

        let response = reqwest::Client::new()
            .post(format!("http://{proxy_addr}/v1/responses"))
            .json(&json!({
                "id": "resp_current_tool_replay",
                "model": "deepseek-v4-pro",
                "stream": false,
                "input": [
                    {
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "test a shell tool" }]
                    },
                    {
                        "type": "reasoning",
                        "summary": [{ "type": "summary_text", "text": "create the test file first" }]
                    },
                    {
                        "type": "function_call",
                        "call_id": "call_shell",
                        "name": "shell_command",
                        "arguments": "{\"command\":\"echo ok\"}"
                    },
                    {
                        "type": "function_call_output",
                        "call_id": "call_shell",
                        "output": "Exit code: 0\nOutput:\nok\n"
                    },
                    {
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "continue" }]
                    }
                ],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "description": "Run shell command.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                }]
            }))
            .send()
            .await
            .unwrap();

        assert!(response.status().is_success());
        let requests = fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .clone();
        assert_eq!(requests.len(), 1);
        let messages = requests[0]["messages"].as_array().unwrap();
        let assistant = messages
            .iter()
            .find(|message| {
                message.get("role").and_then(Value::as_str) == Some("assistant")
                    && message.get("tool_calls").is_some()
            })
            .expect("upstream should receive the prior assistant tool call");
        assert_eq!(assistant["reasoning_content"], "create the test file first");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["name"],
            "shell_command"
        );
        assert!(messages.iter().any(|message| {
            message.get("role").and_then(Value::as_str) == Some("tool")
                && message.get("tool_call_id").and_then(Value::as_str) == Some("call_shell")
                && message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains("ok")
        }));
    }

    #[tokio::test]
    async fn previous_response_history_pairs_tool_outputs_with_parent_calls() {
        let data_dir = std::env::temp_dir().join(format!(
            "codeseex-next-history-tool-pair-test-{}",
            Uuid::new_v4().simple()
        ));
        let config = test_config(data_dir);
        let store = Store::open(&config.database_path()).await.unwrap();
        let state = ProxyState {
            config: Arc::new(config),
            client: reqwest::Client::new(),
            store,
        };

        state
            .store
            .checkpoint_request(
                "resp_parent",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "run a shell command" }]
                    }]
                }),
            )
            .await
            .unwrap();
        state
            .store
            .finish_request(
                "resp_parent",
                RequestStatus::Completed,
                Some(&json!({
                    "output": [{
                        "type": "function_call",
                        "call_id": "call_shell",
                        "name": "shell_command",
                        "arguments": "{\"command\":\"echo ok\"}"
                    }]
                })),
                None,
            )
            .await
            .unwrap();
        state
            .store
            .checkpoint_request(
                "resp_child",
                Some("resp_parent"),
                Some("deepseek-v4-pro"),
                &json!({
                    "input": [{
                        "type": "function_call_output",
                        "call_id": "call_shell",
                        "output": "Exit code: 0\nOutput:\nok\n"
                    }]
                }),
            )
            .await
            .unwrap();
        state
            .store
            .finish_request(
                "resp_child",
                RequestStatus::Completed,
                Some(&json!({
                    "output": [{
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "done" }]
                    }]
                })),
                None,
            )
            .await
            .unwrap();

        let messages = response_history_messages(&state, Some("resp_child")).await;

        assert!(messages.iter().any(|message| {
            message.role == "assistant"
                && message
                    .tool_calls
                    .as_ref()
                    .and_then(|calls| calls.first())
                    .and_then(|call| call.pointer("/function/name"))
                    .and_then(Value::as_str)
                    == Some("shell_command")
        }));
        assert!(messages.iter().any(|message| {
            message.role == "tool"
                && message.tool_call_id.as_deref() == Some("call_shell")
                && message.content.contains("ok")
        }));
        assert!(messages
            .last()
            .map(|message| message.role == "assistant" && message.content == "done")
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn previous_response_history_prefers_persisted_turn_messages() {
        let data_dir = std::env::temp_dir().join(format!(
            "codeseex-next-turn-message-history-test-{}",
            Uuid::new_v4().simple()
        ));
        let config = test_config(data_dir);
        let store = Store::open(&config.database_path()).await.unwrap();
        let state = ProxyState {
            config: Arc::new(config),
            client: reqwest::Client::new(),
            store,
        };

        state
            .store
            .checkpoint_request(
                "resp_turn",
                None,
                Some("deepseek-v4-pro"),
                &json!({"input":"ignored once turn messages exist"}),
            )
            .await
            .unwrap();
        state
            .store
            .replace_request_turn_messages(
                "resp_turn",
                &[
                    json!({"role":"user","content":"list files"}),
                    json!({
                        "role":"assistant",
                        "content":"",
                        "reasoning_content":"need directory first",
                        "tool_calls":[{
                            "id":"call_ls",
                            "type":"function",
                            "function":{"name":"list_directory","arguments":"{\"path\":\".\"}"}
                        }]
                    }),
                    json!({"role":"tool","tool_call_id":"call_ls","content":"Cargo.toml"}),
                    json!({"role":"assistant","content":"I saw Cargo.toml."}),
                ],
            )
            .await
            .unwrap();
        state
            .store
            .finish_request(
                "resp_turn",
                RequestStatus::Completed,
                Some(&json!({
                    "output": [{
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "fallback must not duplicate" }]
                    }]
                })),
                None,
            )
            .await
            .unwrap();

        let messages = response_history_messages(&state, Some("resp_turn")).await;
        assert_eq!(messages.len(), 4);
        assert_eq!(
            messages[1].reasoning_content.as_deref(),
            Some("need directory first")
        );
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("call_ls"));
        assert_eq!(messages[3].content, "I saw Cargo.toml.");
    }

    #[tokio::test]
    async fn streaming_mixed_internal_and_external_tools_runs_internal_first() {
        let fake_state = FakeUpstreamState::default();
        let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let fake_addr = fake_listener.local_addr().unwrap();
        let fake_app = Router::new()
            .route(
                "/chat/completions",
                post(fake_mixed_streaming_chat_completions),
            )
            .with_state(fake_state.clone());
        tokio::spawn(async move {
            axum::serve(fake_listener, fake_app).await.unwrap();
        });

        let data_dir = std::env::temp_dir().join(format!(
            "codeseex-next-mixed-streaming-test-{}",
            Uuid::new_v4().simple()
        ));
        let config = test_config_with_upstream(data_dir, fake_addr);
        let store = Store::open(&config.database_path()).await.unwrap();
        let proxy_state = ProxyState {
            config: Arc::new(config),
            client: reqwest::Client::new(),
            store,
        };
        let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_app = Router::new()
            .route("/v1/responses", post(responses))
            .with_state(proxy_state);
        tokio::spawn(async move {
            axum::serve(proxy_listener, proxy_app).await.unwrap();
        });

        let body = reqwest::Client::new()
            .post(format!("http://{proxy_addr}/v1/responses"))
            .json(&json!({
                "id": "resp_stream_mixed_tool",
                "model": "deepseek-v4-pro",
                "stream": true,
                "input": [
                    {
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "use local files and js" }]
                    }
                ],
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "list_directory",
                            "description": "Colliding client tool that must not take internal ownership.",
                            "parameters": { "type": "object", "properties": {} }
                        }
                    },
                    {
                        "type": "function",
                        "function": {
                            "name": "js",
                            "description": "External JavaScript tool.",
                            "parameters": { "type": "object", "properties": {} }
                        }
                    }
                ]
            }))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert!(
            body.contains("response.function_call_arguments.delta"),
            "{body}"
        );
        assert!(body.contains("\"name\":\"js\""), "{body}");
        assert!(body.contains("已使用工具 `list_directory`"), "{body}");
        assert!(body.contains("\"type\":\"proxy_tool_call\""), "{body}");
        assert!(
            !body.contains(
                "\"name\":\"list_directory\",\"status\":\"completed\",\"type\":\"function_call\""
            ),
            "{body}"
        );
        assert!(!body.contains("unsupported call"), "{body}");

        let requests = fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .clone();
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn streaming_apply_patch_returns_native_custom_tool_call() {
        let fake_state = FakeUpstreamState::default();
        let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let fake_addr = fake_listener.local_addr().unwrap();
        let fake_app = Router::new()
            .route(
                "/chat/completions",
                post(fake_apply_patch_streaming_chat_completions),
            )
            .with_state(fake_state.clone());
        tokio::spawn(async move {
            axum::serve(fake_listener, fake_app).await.unwrap();
        });

        let data_dir = std::env::temp_dir().join(format!(
            "codeseex-next-apply-patch-streaming-test-{}",
            Uuid::new_v4().simple()
        ));
        let patch_dir = PathBuf::from("target").join("codeseex-next-apply-patch-streaming-test");
        std::fs::create_dir_all(&patch_dir).expect("create ignored apply_patch test directory");
        let patch_file = patch_dir.join("hello.txt");
        let _ = std::fs::remove_file(&patch_file);
        let config = test_config_with_upstream(data_dir, fake_addr);
        let store = Store::open(&config.database_path()).await.unwrap();
        let proxy_state = ProxyState {
            config: Arc::new(config),
            client: reqwest::Client::new(),
            store,
        };
        let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_app = Router::new()
            .route("/v1/responses", post(responses))
            .with_state(proxy_state);
        tokio::spawn(async move {
            axum::serve(proxy_listener, proxy_app).await.unwrap();
        });

        let body = reqwest::Client::new()
            .post(format!("http://{proxy_addr}/v1/responses"))
            .json(&json!({
                "id": "resp_stream_apply_patch",
                "model": "deepseek-v4-pro",
                "stream": true,
                "input": [
                    {
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "patch a file" }]
                    }
                ]
            }))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert!(body.contains("\"type\":\"custom_tool_call\""), "{body}");
        assert!(body.contains("\"name\":\"apply_patch\""), "{body}");
        assert!(body.contains("*** Begin Patch"), "{body}");
        assert!(!body.contains("patch-ok"), "{body}");
        assert!(body.contains("encrypted_content"), "{body}");
        let thinking_done = body
            .find("response.output_text.done")
            .expect("thinking display should close before native tool completion");
        let completed = body
            .find("response.completed")
            .expect("stream should complete");
        assert!(thinking_done < completed, "{body}");
        assert!(
            !patch_file.exists(),
            "proxy must not execute native apply_patch"
        );
        let _ = std::fs::remove_file(&patch_file);

        let requests = fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .clone();
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn reconstructed_tool_call_history_keeps_reasoning_content_field() {
        let reasoning = "read the file before answering";
        let response = json!({
            "output": [
                reasoning_response_item(reasoning, false),
                {
                    "type": "function_call",
                    "call_id": "call_prev",
                    "name": "read_file_range",
                    "arguments": "{\"path\":\"README.md\"}"
                }
            ]
        });

        let messages = response_output_tool_call_messages(&response);
        let serialized = serde_json::to_value(&messages[0]).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(serialized["role"], "assistant");
        assert_eq!(serialized["content"], "");
        assert_eq!(serialized["reasoning_content"], reasoning);
        assert_eq!(serialized["tool_calls"][0]["id"], "call_prev");
        assert_eq!(
            serialized["tool_calls"][0]["function"]["name"],
            "read_file_range"
        );
    }

    #[test]
    fn reconstructed_custom_apply_patch_history_uses_patch_argument() {
        let response = json!({
            "output": [
                {
                    "type": "custom_tool_call",
                    "call_id": "call_patch",
                    "name": "apply_patch",
                    "input": "*** Begin Patch\n*** Add File: hi.txt\n+hi\n*** End Patch"
                }
            ]
        });

        let messages = response_output_tool_call_messages(&response);
        let serialized = serde_json::to_value(&messages[0]).unwrap();
        let arguments = serialized["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        let parsed = serde_json::from_str::<Value>(arguments).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(serialized["tool_calls"][0]["id"], "call_patch");
        assert_eq!(
            serialized["tool_calls"][0]["function"]["name"],
            "apply_patch"
        );
        assert!(parsed["patch"]
            .as_str()
            .unwrap()
            .contains("*** Begin Patch"));
    }
}
