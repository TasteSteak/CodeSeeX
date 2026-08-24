//! Official DeepSeek Responses transport.
//!
//! This module deliberately owns only the native wire boundary. It neither
//! rebuilds Codex history nor reads a Codex transcript: the client's `input`
//! remains authoritative. Requests that require a CodeSeeX-owned local tool
//! stay on the proven Chat compatibility path because their CodeSeeX-owned
//! executor is not part of the native provider wire contract. This is a
//! deterministic ownership route, not a silent fallback after provider error.

use super::*;
use crate::native_coordinator::{
    NativePendingContinuation, NativePendingError, PendingNativeToolGroup,
};
use crate::native_responses::{
    native_stream_finalization, native_tool_call_group_from_response, plan_native_tools,
    rewrite_provider_response_identity, NativeResponseSseRelay, NativeStreamFinalization,
    NativeToolCallGroup,
};
use crate::upstream::{SelectedUpstreamTransport, UpstreamTransportSelection};
use codeseex_core::config::UpstreamTransport;
use codeseex_core::config::WebSearchBackend;

pub(super) async fn dispatch_if_selected(
    state: &ProxyState,
    headers: &HeaderMap,
    input: &Value,
    config: &AppConfig,
    model: &str,
    requested_model: Option<&str>,
) -> Option<axum::response::Response> {
    match crate::upstream::select_transport(&config.upstream, model) {
        UpstreamTransportSelection::Selected(SelectedUpstreamTransport::ChatCompat) => {
            reject_official_web_search_on_chat_compat(state, input, config, model, requested_model)
                .await
        }
        UpstreamTransportSelection::Selected(SelectedUpstreamTransport::NativeResponses) => {
            try_native_responses(state, headers, input, config, model, requested_model).await
        }
        UpstreamTransportSelection::NativeResponsesUnavailable(reason) => Some(json_error(
            StatusCode::BAD_REQUEST,
            "native_responses_unavailable",
            reason.message(model),
        )),
    }
}

/// `official` is an ownership choice, not a request to use whichever search
/// happens to be reachable. Chat compatibility has only the CodeSeeX-hosted
/// web-search executor, so accepting an official-search request here would
/// silently run the wrong backend. Keep local search available, but fail this
/// explicit incompatible combination before the Chat lifecycle starts.
async fn reject_official_web_search_on_chat_compat(
    state: &ProxyState,
    input: &Value,
    config: &AppConfig,
    model: &str,
    requested_model: Option<&str>,
) -> Option<axum::response::Response> {
    if config.web_search_backend != WebSearchBackend::Official
        || !request_advertises_web_search(input)
    {
        return None;
    }

    let id = response_id_from_input(input);
    let detail = json!({
        "id": id,
        "transport": "chat_compat",
        "issue": "official_web_search_requires_verified_native_responses",
        "requested_model": requested_model,
        "model": model,
        "selected_web_search_backend": web_search_backend_label(config.web_search_backend),
        "fallback": "none"
    });
    let _ = state
        .store
        .record_event(
            "warn",
            "native_responses_compatibility_diagnostic",
            "Official web search requires the verified native Responses transport.",
            Some(&detail),
        )
        .await;
    Some(json_error(
        StatusCode::BAD_REQUEST,
        "official_web_search_incompatible",
            "DeepSeek official web search requires the native Responses transport. Select the local web-search backend to use Chat API compatibility.".to_owned(),
    ))
}

fn request_advertises_web_search(input: &Value) -> bool {
    input
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                matches!(
                    tool.get("type").and_then(Value::as_str),
                    Some("web_search" | "web_search_2025_08_26")
                ) || matches!(
                    tool.pointer("/function/name")
                        .or_else(|| tool.get("name"))
                        .and_then(Value::as_str),
                    Some("web_search" | "web_search_preview")
                )
            })
        })
}

async fn try_native_responses(
    state: &ProxyState,
    headers: &HeaderMap,
    input: &Value,
    config: &AppConfig,
    model: &str,
    requested_model: Option<&str>,
) -> Option<axum::response::Response> {
    let id = response_id_from_input(input);
    let stream_requested = input
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let requested_tools = input
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let plan = match plan_native_tools(&requested_tools, config.web_search_backend) {
        Ok(plan) => plan,
        Err(message) => {
            return native_incompatible_or_fallback(
                state,
                config,
                &id,
                requested_model,
                model,
                "tool_definition_incompatible",
                message,
                true,
            )
            .await;
        }
    };

    // Local CodeSeeX tools must not be converted into provider-owned or
    // client-owned calls by accident. Returning `None` for Auto is an
    // explicit, diagnosable use of the existing Chat implementation; explicit
    // native mode instead receives a readable incompatibility error.
    if plan.requires_local_execution {
        // Selecting provider-owned Web Search is an explicit ownership choice.
        // If the same request also advertises CodeSeeX-owned tools, Chat
        // compatibility could execute the original local web_search function
        // and silently defeat that choice. Refuse that mixed request until a
        // complete native local-tool streaming loop has been verified.
        let allow_auto_fallback = !(config.web_search_backend == WebSearchBackend::Official
            && plan.uses_official_web_search);
        return native_incompatible_or_fallback(
            state,
            config,
            &id,
            requested_model,
            model,
            "local_tool_execution_requires_chat_compat",
            "This request combines provider-owned official web search with a CodeSeeX-owned local tool. CodeSeeX did not silently replace either backend. Disable the local tool for this native request, or select the local web-search backend / chat compatibility.",
            allow_auto_fallback,
        )
        .await;
    }

    let previous = input.get("previous_response_id").and_then(Value::as_str);
    if let Err(response) = ensure_new_response_id(state, &id, previous).await {
        return Some(response);
    }
    let mut payload = match native_payload(input, model, &plan.tools) {
        Ok(payload) => payload,
        Err(message) => {
            return Some(json_error(
                StatusCode::BAD_REQUEST,
                "native_responses_input_invalid",
                message,
            ));
        }
    };
    let pending = match state.native_pending_tool_groups.continuation_for(input) {
        Ok(pending) => pending,
        Err(error) => return Some(native_pending_error_response(error)),
    };
    if let Some(continuation) = pending.as_ref() {
        payload["input"] = Value::Array(continuation.merged_input.clone());
    }

    if let Err(error) = state
        .store
        .checkpoint_request(&id, previous, Some(model), input)
        .await
    {
        return Some(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_checkpoint_failed",
            error.to_string(),
        ));
    }

    let service_kind = codex_service_request_kind(input);
    let diagnostic = native_transport_diagnostic(
        &id,
        requested_model,
        model,
        input,
        &payload,
        config.web_search_backend,
        pending.as_ref(),
    );
    let _ = state
        .store
        .update_request_diagnostic(&id, &diagnostic)
        .await;
    let _ = state
        .store
        .record_event(
            "info",
            "request_started",
            "Native Responses request started.",
            Some(&json!({
                "id": id,
                "endpoint": "/v1/responses",
                "transport": "native_responses",
                "requested_model": requested_model,
                "model": model,
                "web_search_backend": web_search_backend_label(config.web_search_backend)
            })),
        )
        .await;
    let _ = state
        .store
        .record_event(
            "info",
            "native_responses_transport_diagnostic",
            "CodeSeeX selected the native DeepSeek Responses transport.",
            Some(&diagnostic),
        )
        .await;
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
                    requested_model,
                    model,
                    true,
                    input,
                )),
            )
            .await;
    }
    record_request_shape_diagnostic(
        &state.store,
        &id,
        "/v1/responses",
        requested_model,
        model,
        input,
    )
    .await;
    record_cost_risk_diagnostic(&state.store, &id, "/v1/responses", input, Some(&payload)).await;

    let auth = upstream_authorization_from_headers(headers, &state.v1_access_token);
    if let Some(auth) = auth.as_deref() {
        codeseex_core::codex_auth::remember_authorization_header(auth);
    }
    let client = state.client();
    let started = std::time::Instant::now();
    let upstream = crate::upstream::post_responses(
        &client,
        &config.upstream,
        auth.as_deref(),
        Some(&state.v1_access_token),
        Some(input),
        payload.clone(),
    )
    .await;
    let response = match upstream {
        Ok(response) => response,
        Err(error) => {
            let detail = json!({
                "id": id,
                "transport": "native_responses",
                "error": error.to_string()
            });
            let _ = state
                .store
                .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                .await;
            let _ = state
                .store
                .record_event(
                    "error",
                    "request_failed",
                    "Failed to connect to native Responses upstream.",
                    Some(&detail),
                )
                .await;
            return Some(json_error(
                StatusCode::BAD_GATEWAY,
                "native_upstream_connection_failed",
                error.to_string(),
            ));
        }
    };
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let response_headers = response.headers().clone();
    if !status.is_success() {
        return Some(
            native_upstream_status_failure(
                state,
                &id,
                requested_model,
                model,
                status,
                response,
                pending.as_ref(),
            )
            .await,
        );
    }

    if stream_requested
        && content_type
            .as_ref()
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/event-stream"))
    {
        return Some(response_stream_from_native(NativeStreamingResponseParams {
            response_id: id,
            model: model.to_owned(),
            requested_model: requested_model.map(str::to_owned),
            response,
            state: state.clone(),
            original_request: input.clone(),
            payload,
            content_type,
            upstream_started: started,
            web_search_backend: config.web_search_backend,
            settle_pending_response_id: pending
                .as_ref()
                .map(|continuation| continuation.pending_response_id.clone()),
        }));
    }

    Some(
        native_non_streaming_response(
            state,
            &id,
            requested_model,
            model,
            input,
            payload,
            response,
            status,
            response_headers,
            started,
            config.web_search_backend,
            pending
                .as_ref()
                .map(|continuation| continuation.pending_response_id.as_str()),
        )
        .await,
    )
}

#[allow(clippy::too_many_arguments)]
async fn native_incompatible_or_fallback(
    state: &ProxyState,
    config: &AppConfig,
    id: &str,
    requested_model: Option<&str>,
    model: &str,
    issue: &str,
    message: impl Into<String>,
    allow_auto_fallback: bool,
) -> Option<axum::response::Response> {
    let message = message.into();
    let detail = json!({
        "id": id,
        "transport": "native_responses",
        "issue": issue,
        "requested_model": requested_model,
        "model": model,
        "selected_web_search_backend": web_search_backend_label(config.web_search_backend),
        "selection": if allow_auto_fallback && config.upstream.transport == UpstreamTransport::Auto { "compatibility_required_by_request" } else { "explicit_or_failed_closed" },
        "fallback": if config.upstream.transport == UpstreamTransport::Auto && allow_auto_fallback { "chat_compat" } else { "none" }
    });
    let _ = state
        .store
        .record_event(
            if config.upstream.transport == UpstreamTransport::Auto && allow_auto_fallback {
                "info"
            } else {
                "warn"
            },
            "native_responses_compatibility_diagnostic",
            "Native Responses request requires the compatibility path.",
            Some(&detail),
        )
        .await;
    if config.upstream.transport == UpstreamTransport::Auto && allow_auto_fallback {
        return None;
    }
    Some(json_error(
        StatusCode::BAD_REQUEST,
        "native_responses_incompatible",
        message,
    ))
}

fn native_payload(input: &Value, model: &str, tools: &[Value]) -> Result<Value, String> {
    let Some(mut object) = input.as_object().cloned() else {
        return Err("Native Responses requests must be JSON objects.".to_owned());
    };
    if !object.get("input").is_some_and(Value::is_array) {
        return Err(
            "Native Responses requires the authoritative Codex input item array; CodeSeeX did not reconstruct hidden history."
                .to_owned(),
        );
    }
    // These are local/caller lifecycle fields. DeepSeek Responses is
    // stateless, so forwarding them would incorrectly imply server-side
    // continuation. All authoritative replay items remain untouched.
    object.remove("id");
    object.remove("previous_response_id");
    object.insert("model".to_owned(), Value::String(model.to_owned()));
    if tools.is_empty() {
        object.remove("tools");
        object.remove("tool_choice");
    } else {
        object.insert("tools".to_owned(), Value::Array(tools.to_vec()));
    }
    object
        .entry("stream".to_owned())
        .or_insert(Value::Bool(false));
    Ok(Value::Object(object))
}

async fn native_upstream_status_failure(
    state: &ProxyState,
    id: &str,
    requested_model: Option<&str>,
    model: &str,
    status: reqwest::StatusCode,
    response: reqwest::Response,
    pending: Option<&NativePendingContinuation>,
) -> axum::response::Response {
    match response.bytes().await {
        Ok(bytes) => {
            let body_json = serde_json::from_slice::<Value>(&bytes).ok();
            let detail = json!({
                "id": id,
                "transport": "native_responses",
                "status": status.as_u16(),
                "requested_model": requested_model,
                "model": model,
                "upstream_error": upstream_error_detail(body_json.as_ref(), &bytes),
                "pending_tool_group_retained": pending.is_some()
            });
            let _ = state
                .store
                .finish_request(id, RequestStatus::Failed, body_json.as_ref(), Some(&detail))
                .await;
            let _ = state
                .store
                .record_event(
                    "error",
                    "request_failed",
                    "Native Responses request failed.",
                    Some(&detail),
                )
                .await;
            response_from_bytes(status, response_content_type_json(), bytes.to_vec())
        }
        Err(error) => {
            let detail = json!({
                "id": id,
                "transport": "native_responses",
                "status": status.as_u16(),
                "requested_model": requested_model,
                "model": model,
                "error": error.to_string(),
                "pending_tool_group_retained": pending.is_some()
            });
            let _ = state
                .store
                .finish_request(id, RequestStatus::Failed, None, Some(&detail))
                .await;
            let _ = state
                .store
                .record_event(
                    "error",
                    "request_failed",
                    "Failed to read native Responses error body.",
                    Some(&detail),
                )
                .await;
            json_error(
                StatusCode::BAD_GATEWAY,
                "native_upstream_body_failed",
                error.to_string(),
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn native_non_streaming_response(
    state: &ProxyState,
    id: &str,
    requested_model: Option<&str>,
    model: &str,
    input: &Value,
    payload: Value,
    response: reqwest::Response,
    status: reqwest::StatusCode,
    response_headers: HeaderMap,
    started: std::time::Instant,
    web_search_backend: WebSearchBackend,
    settle_pending_response_id: Option<&str>,
) -> axum::response::Response {
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            let detail = upstream_body_read_error_detail(
                id,
                requested_model,
                Some(model),
                status,
                &response_headers,
                &error,
            );
            let _ = state
                .store
                .finish_request(id, RequestStatus::Failed, None, Some(&detail))
                .await;
            return json_error(
                StatusCode::BAD_GATEWAY,
                "native_upstream_body_failed",
                error.to_string(),
            );
        }
    };
    let mut native = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => value,
        Err(error) => {
            let detail = upstream_json_parse_error_detail(
                id,
                requested_model,
                Some(model),
                status,
                &response_headers,
                bytes.len(),
                &error,
            );
            let _ = state
                .store
                .finish_request(id, RequestStatus::Failed, None, Some(&detail))
                .await;
            return json_error(
                StatusCode::BAD_GATEWAY,
                "native_upstream_json_failed",
                error.to_string(),
            );
        }
    };
    let provider_id = native.get("id").and_then(Value::as_str).map(str::to_owned);
    if let Some(provider_id) = provider_id.as_deref() {
        rewrite_provider_response_identity(&mut native, provider_id, id);
    }
    let tool_group = match native_tool_call_group_from_response(&native) {
        Ok(group) => group,
        Err(error) => {
            let detail = json!({
                "id": id,
                "transport": "native_responses",
                "error": error
            });
            let _ = state
                .store
                .finish_request(id, RequestStatus::Failed, None, Some(&detail))
                .await;
            let _ = state
                .store
                .record_event(
                    "error",
                    "native_tool_protocol_invalid",
                    "Native Responses returned an unsafe tool group.",
                    Some(&detail),
                )
                .await;
            return json_error(
                StatusCode::BAD_GATEWAY,
                "native_tool_protocol_invalid",
                error,
            );
        }
    };
    let response_completed = native.get("status").and_then(Value::as_str) == Some("completed");
    if response_completed {
        if let Some(group) = tool_group.as_ref() {
            if let Err(error) =
                retain_native_pending_tool_group(state, id, input, &payload, group).await
            {
                let detail = json!({ "id": id, "error": error.message() });
                let _ = state
                    .store
                    .finish_request(id, RequestStatus::Failed, None, Some(&detail))
                    .await;
                return native_pending_error_response(error);
            }
        }
        if let Some(pending_response_id) = settle_pending_response_id {
            state.native_pending_tool_groups.settle(pending_response_id);
        }
    }
    let status_to_store = if response_completed {
        RequestStatus::Completed
    } else {
        RequestStatus::Failed
    };
    let detail = json!({
        "transport": "native_responses",
        "web_search_backend": web_search_backend_label(web_search_backend),
        "provider_tool_calls": tool_group.as_ref().map(|group| group.calls.len()).unwrap_or(0)
    });
    let _ = state
        .store
        .record_event(
            "info",
            "upstream_call_usage_breakdown",
            "CodeSeeX upstream call usage breakdown.",
            Some(&upstream_call_usage_breakdown_event(
                id,
                "native_non_streaming",
                0,
                input,
                &payload,
                native.get("usage"),
                Some(started.elapsed().as_millis() as u64),
                false,
            )),
        )
        .await;
    let _ = state
        .store
        .finish_request(id, status_to_store, Some(&native), Some(&detail))
        .await;
    let _ = state
        .store
        .record_event(
            if status_to_store == RequestStatus::Completed {
                "info"
            } else {
                "error"
            },
            if status_to_store == RequestStatus::Completed {
                "request_completed"
            } else {
                "request_failed"
            },
            "Native Responses request finished.",
            Some(&request_completed_detail(
                id,
                requested_model,
                native.get("model").and_then(Value::as_str).or(Some(model)),
                Some("native_responses"),
                Some(&native),
            )),
        )
        .await;
    json_response(native)
}

struct NativeStreamingResponseParams {
    response_id: String,
    model: String,
    requested_model: Option<String>,
    response: reqwest::Response,
    state: ProxyState,
    original_request: Value,
    payload: Value,
    content_type: Option<HeaderValue>,
    upstream_started: std::time::Instant,
    web_search_backend: WebSearchBackend,
    settle_pending_response_id: Option<String>,
}

fn response_stream_from_native(params: NativeStreamingResponseParams) -> axum::response::Response {
    let NativeStreamingResponseParams {
        response_id,
        model,
        requested_model,
        response,
        state,
        original_request,
        payload,
        content_type,
        upstream_started,
        web_search_backend,
        settle_pending_response_id,
    } = params;
    let cancelled = register_streaming_response(&response_id);
    let guard =
        StreamingRequestGuard::new(state.store.clone(), response_id.clone(), cancelled.clone());
    let stream: BoxStream<'static, Result<Bytes, std::io::Error>> = Box::pin(
        async_stream::try_stream! {
            let _stream_guard = guard;
            let mut upstream = response.bytes_stream();
            let mut relay = NativeResponseSseRelay::new(response_id.clone());
            loop {
                tokio::select! {
                    _ = cancelled.cancelled() => {
                        let _ = state.store.interrupt_request_if_in_progress(
                            &response_id,
                            "native Responses stream cancelled by client",
                        ).await;
                        let _ = state.store.record_event(
                            "info",
                            "request_interrupted",
                            "Native Responses stream cancelled.",
                            Some(&json!({ "id": response_id, "transport": "native_responses" })),
                        ).await;
                        return;
                    }
                    next = upstream.next() => match next {
                        Some(Ok(bytes)) => {
                            for frame in relay.relay_bytes(&bytes) {
                                yield Bytes::from(frame);
                            }
                        }
                        Some(Err(error)) => {
                            let detail = json!({
                                "id": response_id,
                                "transport": "native_responses",
                                "error": error.to_string()
                            });
                            let _ = state.store.finish_request(
                                &response_id,
                                RequestStatus::Failed,
                                None,
                                Some(&detail),
                            ).await;
                            let _ = state.store.record_event(
                                "error",
                                "request_failed",
                                "Native Responses SSE body read failed.",
                                Some(&detail),
                            ).await;
                            // Native Responses has no compatible synthetic
                            // terminal frame. Close the client stream after
                            // recording the failure rather than injecting a
                            // Chat-style `[DONE]` or a made-up sequence.
                            return;
                        }
                        None => break,
                    }
                }
            }
            if let Some(remainder) = relay.finish() {
                yield Bytes::from(remainder);
            }
            let inspection = relay.inspection().clone();
            let mut finalization = native_stream_finalization(&inspection, streaming_response_cancelled(&cancelled));
            let mut provider_tool_calls = 0_usize;
            let mut pending_tool_group_retained = false;
            let mut tool_group_issue = None;
            if finalization == NativeStreamFinalization::Completed {
                if inspection.output_items_incomplete {
                    finalization = NativeStreamFinalization::Failed;
                    tool_group_issue = Some(
                        "Native Responses stream output items could not be retained as one bounded group."
                            .to_owned(),
                    );
                } else {
                    let native_output = json!({ "output": inspection.output_items.clone() });
                    match native_tool_call_group_from_response(&native_output) {
                        Ok(Some(group)) => {
                            provider_tool_calls = group.calls.len();
                            match retain_native_pending_tool_group(
                                &state,
                                &response_id,
                                &original_request,
                                &payload,
                                &group,
                            )
                            .await
                            {
                                Ok(()) => pending_tool_group_retained = true,
                                Err(error) => {
                                    finalization = NativeStreamFinalization::Failed;
                                    tool_group_issue = Some(error.message());
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            finalization = NativeStreamFinalization::Failed;
                            tool_group_issue = Some(error);
                        }
                    }
                }
            }
            let usage = inspection.final_usage.clone().unwrap_or(Value::Null);
            let stored_response = json!({
                "id": response_id,
                "object": "response",
                "model": model,
                "status": native_stream_status(finalization),
                "output": inspection.output_items,
                "usage": usage
            });
            let detail = json!({
                "transport": "native_responses",
                "web_search_backend": web_search_backend_label(web_search_backend),
                "terminal": native_terminal_label(finalization),
                "event_count": inspection.event_count,
                "sequence_count": inspection.sequence_count,
                "sequences_strictly_increasing": inspection.sequences_strictly_increasing,
                "saw_done_sentinel": inspection.saw_done_sentinel,
                "oversized_frame_ignored": inspection.oversized_frame_ignored,
                "provider_response_id_hash": inspection.provider_response_id_hash,
                "output_item_count": stored_response.get("output").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
                "output_items_bytes": inspection.output_items_bytes,
                "output_items_incomplete": inspection.output_items_incomplete,
                "provider_tool_calls": provider_tool_calls,
                "pending_tool_group_retained": pending_tool_group_retained,
                "tool_group_issue": tool_group_issue
            });
            let _ = state.store.record_event(
                "info",
                "upstream_call_usage_breakdown",
                "CodeSeeX upstream call usage breakdown.",
                Some(&upstream_call_usage_breakdown_event(
                    &response_id,
                    "native_streaming",
                    0,
                    &original_request,
                    &payload,
                    inspection.final_usage.as_ref(),
                    Some(upstream_started.elapsed().as_millis() as u64),
                    false,
                )),
            ).await;
            if let Some(issue) = detail.get("tool_group_issue").and_then(Value::as_str) {
                let _ = state.store.record_event(
                    "error",
                    "native_tool_protocol_invalid",
                    "Native Responses stream did not yield a safe complete tool group.",
                    Some(&json!({
                        "id": response_id,
                        "issue": issue,
                        "output_item_count": detail.get("output_item_count").cloned().unwrap_or(Value::Null),
                        "output_items_incomplete": detail.get("output_items_incomplete").cloned().unwrap_or(Value::Null)
                    })),
                ).await;
            }
            match finalization {
                NativeStreamFinalization::Completed => {
                    if let Some(pending_response_id) = settle_pending_response_id.as_deref() {
                        state.native_pending_tool_groups.settle(pending_response_id);
                    }
                    let _ = state.store.finish_request(
                        &response_id,
                        RequestStatus::Completed,
                        Some(&stored_response),
                        Some(&detail),
                    ).await;
                    let _ = state.store.record_event(
                        "info",
                        "request_completed",
                        "Native Responses stream completed.",
                        Some(&request_completed_detail(
                            &response_id,
                            requested_model.as_deref(),
                            Some(&model),
                            Some("native_responses"),
                            Some(&stored_response),
                        )),
                    ).await;
                }
                NativeStreamFinalization::Failed => {
                    let _ = state.store.finish_request(
                        &response_id,
                        RequestStatus::Failed,
                        Some(&stored_response),
                        Some(&detail),
                    ).await;
                    let _ = state.store.record_event(
                        "error",
                        "request_failed",
                        "Native Responses stream ended without completion.",
                        Some(&detail),
                    ).await;
                }
                NativeStreamFinalization::Interrupted => {
                    let _ = state.store.interrupt_request_if_in_progress(
                        &response_id,
                        "native Responses stream ended after cancellation",
                    ).await;
                }
            }
        },
    );
    response_from_stream(
        reqwest::StatusCode::OK,
        content_type.or_else(|| Some(HeaderValue::from_static("text/event-stream"))),
        Body::from_stream(stream),
    )
}

fn native_pending_error_response(error: NativePendingError) -> axum::response::Response {
    json_error(StatusCode::BAD_REQUEST, error.code(), error.message())
}

/// Registers exactly the provider output group that Codex observed. This
/// state stays only in RAM and is used solely to reject partial/out-of-order
/// client tool outputs on the immediate full-replay continuation.
async fn retain_native_pending_tool_group(
    state: &ProxyState,
    response_id: &str,
    original_request: &Value,
    payload: &Value,
    group: &NativeToolCallGroup,
) -> Result<(), NativePendingError> {
    let authoritative_input = payload
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let pending = PendingNativeToolGroup::new(
        response_id,
        original_request,
        authoritative_input,
        group.provider_output.clone(),
        group.provider_output.clone(),
        Vec::new(),
        group.calls.clone(),
    );
    let pending_diagnostic = pending.diagnostic();
    state.native_pending_tool_groups.register(pending)?;
    let _ = state
        .store
        .record_event(
            "info",
            "native_pending_tool_group",
            "Native Responses retained a complete client tool group in RAM.",
            Some(&json!({ "id": response_id, "group": pending_diagnostic })),
        )
        .await;
    Ok(())
}

fn native_transport_diagnostic(
    id: &str,
    requested_model: Option<&str>,
    model: &str,
    input: &Value,
    payload: &Value,
    backend: WebSearchBackend,
    pending: Option<&NativePendingContinuation>,
) -> Value {
    json!({
        "id": id,
        "transport": "native_responses",
        "requested_model": requested_model,
        "model": model,
        "web_search_backend": web_search_backend_label(backend),
        "request": {
            "input_items": input.get("input").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "tool_count": input.get("tools").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "stream": input.get("stream").and_then(Value::as_bool).unwrap_or(false),
            "has_previous_response_id": input.get("previous_response_id").is_some(),
            "has_prompt_cache_key": input.get("prompt_cache_key").is_some()
        },
        "payload": {
            "input_items": payload.get("input").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "tool_count": payload.get("tools").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "previous_response_id_forwarded": payload.get("previous_response_id").is_some()
        },
        "pending_continuation": pending.map(|value| json!({
            "client_output_count": value.client_output_count,
            "local_output_count": value.local_output_count
        }))
    })
}

fn native_stream_status(finalization: NativeStreamFinalization) -> &'static str {
    match finalization {
        NativeStreamFinalization::Completed => "completed",
        NativeStreamFinalization::Failed => "failed",
        NativeStreamFinalization::Interrupted => "cancelled",
    }
}

fn native_terminal_label(finalization: NativeStreamFinalization) -> &'static str {
    match finalization {
        NativeStreamFinalization::Completed => "completed",
        NativeStreamFinalization::Failed => "failed_or_incomplete",
        NativeStreamFinalization::Interrupted => "interrupted",
    }
}

fn web_search_backend_label(backend: WebSearchBackend) -> &'static str {
    match backend {
        WebSearchBackend::Local => "local",
        WebSearchBackend::Official => "official",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_coordinator::PendingNativeToolGroup;
    use crate::native_responses::{NativeToolCall, NativeToolCallKind};
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use codeseex_core::config::UpstreamConfig;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use uuid::Uuid;

    #[derive(Clone, Default)]
    struct Capture {
        requests: Arc<Mutex<Vec<Value>>>,
    }

    fn temp_data_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codeseex-native-runtime-{label}-{}",
            Uuid::new_v4().simple()
        ))
    }

    fn config_for_fake(data_dir: PathBuf, address: std::net::SocketAddr) -> AppConfig {
        AppConfig {
            data_dir,
            upstream: UpstreamConfig {
                base_url: format!("http://{address}"),
                official_v1_compat: false,
                transport: UpstreamTransport::NativeResponses,
                api_key: Some("native-test-key".to_owned()),
                timeout_ms: 30_000,
            },
            web_search_backend: WebSearchBackend::Official,
            ..Default::default()
        }
    }

    async fn fake_native_response(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        capture.requests.lock().expect("capture lock").push(payload);
        Json(json!({
            "id": "provider_resp_1",
            "object": "response",
            "model": "deepseek-v4-flash",
            "status": "completed",
            "output": [],
            "usage": {
                "input_tokens": 11,
                "input_tokens_details": { "cached_tokens": 8 },
                "output_tokens": 2,
                "total_tokens": 13
            }
        }))
    }

    async fn fake_native_sse(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        capture.requests.lock().expect("capture lock").push(payload);
        let bytes = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":1,\"response\":{\"id\":\"provider_stream_1\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{\"id\":\"provider_stream_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":7,\"input_tokens_details\":{\"cached_tokens\":5},\"output_tokens\":1,\"total_tokens\":8}}}\n\n"
        );
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            bytes.to_owned(),
        )
            .into_response()
    }

    async fn fake_native_sse_failed(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        capture.requests.lock().expect("capture lock").push(payload);
        let bytes = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":1,\"response\":{\"id\":\"provider_stream_failed_1\"}}\n\n",
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"sequence_number\":2,\"response\":{\"id\":\"provider_stream_failed_1\",\"status\":\"failed\"}}\n\n"
        );
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            bytes.to_owned(),
        )
            .into_response()
    }

    async fn fake_native_sse_client_tool_group(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        capture.requests.lock().expect("capture lock").push(payload);
        let bytes = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":1,\"response\":{\"id\":\"provider_stream_tool_1\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":2,\"response_id\":\"provider_stream_tool_1\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_stream_1\",\"call_id\":\"call_stream_shell\",\"name\":\"shell_command\",\"arguments\":\"{}\",\"status\":\"completed\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":3,\"response_id\":\"provider_stream_tool_1\",\"item\":{\"type\":\"custom_tool_call\",\"id\":\"ctc_stream_1\",\"call_id\":\"call_stream_patch\",\"name\":\"apply_patch\",\"input\":\"*** Begin Patch\\n*** End Patch\",\"status\":\"completed\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":4,\"response\":{\"id\":\"provider_stream_tool_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":7,\"output_tokens\":1,\"total_tokens\":8}}}\n\n"
        );
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            bytes.to_owned(),
        )
            .into_response()
    }

    async fn fake_native_client_tool_turn(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        let call_count = {
            let mut requests = capture.requests.lock().expect("capture lock");
            requests.push(payload);
            requests.len()
        };
        if call_count == 1 {
            return Json(json!({
                "id": "provider_tool_turn_1",
                "object": "response",
                "model": "deepseek-v4-flash",
                "status": "completed",
                "output": [{
                    "type": "function_call",
                    "id": "fc_native_1",
                    "call_id": "call_native_1",
                    "name": "shell_command",
                    "arguments": "{\"command\":\"echo native\"}",
                    "status": "completed"
                }],
                "usage": { "input_tokens": 3, "output_tokens": 1, "total_tokens": 4 }
            }));
        }
        Json(json!({
            "id": "provider_tool_turn_2",
            "object": "response",
            "model": "deepseek-v4-flash",
            "status": "completed",
            "output": [],
            "usage": { "input_tokens": 5, "output_tokens": 2, "total_tokens": 7 }
        }))
    }

    async fn fake_native_client_tool_turn_failed(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        let call_count = {
            let mut requests = capture.requests.lock().expect("capture lock");
            requests.push(payload);
            requests.len()
        };
        if call_count == 1 {
            return Json(json!({
                "id": "provider_tool_failed_1",
                "object": "response",
                "model": "deepseek-v4-flash",
                "status": "completed",
                "output": [{
                    "type": "function_call",
                    "id": "fc_native_failed_1",
                    "call_id": "call_native_failed_1",
                    "name": "shell_command",
                    "arguments": "{}",
                    "status": "completed"
                }]
            }));
        }
        Json(json!({
            "id": "provider_tool_failed_2",
            "object": "response",
            "model": "deepseek-v4-flash",
            "status": "failed",
            "output": []
        }))
    }

    fn request(id: &str, stream: bool, tools: Value) -> Value {
        json!({
            "id": id,
            "model": "deepseek-v4-flash",
            "stream": stream,
            "prompt_cache_key": "native-test-thread",
            "previous_response_id": "local_previous_only",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "private text stays in the input" }]
            }],
            "tools": tools
        })
    }

    #[tokio::test]
    async fn native_non_streaming_rewrites_only_response_id_and_normalizes_official_search() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_response))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("non-streaming");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store.clone());
        let input = request(
            "resp_native_non_stream",
            false,
            json!([
                { "type": "web_search_2025_08_26" }
            ]),
        );

        let response = try_native_responses(
            &state,
            &HeaderMap::new(),
            &input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("native route should handle request");
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let output = serde_json::from_slice::<Value>(&body).unwrap();

        assert_eq!(output["id"], "resp_native_non_stream");
        assert_eq!(output["usage"]["input_tokens_details"]["cached_tokens"], 8);
        assert_eq!(
            store
                .response_status("resp_native_non_stream")
                .await
                .unwrap(),
            Some(RequestStatus::Completed)
        );
        let captured = capture.requests.lock().expect("capture lock");
        assert_eq!(captured.len(), 1);
        assert!(captured[0].get("id").is_none());
        assert!(captured[0].get("previous_response_id").is_none());
        assert_eq!(captured[0]["input"], input["input"]);
        assert_eq!(captured[0]["tools"], json!([{ "type": "web_search" }]));
        drop(captured);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn native_stream_preserves_provider_sequence_and_never_appends_done() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_sse))
            .with_state(capture);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("streaming");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store.clone());
        let input = request("resp_native_stream", true, json!([]));
        let response = try_native_responses(
            &state,
            &HeaderMap::new(),
            &input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("native route should handle request");
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(output.contains("resp_native_stream"));
        assert!(!output.contains("provider_stream_1"));
        assert!(output.contains("\"sequence_number\":1"));
        assert!(output.contains("\"sequence_number\":2"));
        assert!(!output.contains("[DONE]"));
        assert_eq!(
            store.response_status("resp_native_stream").await.unwrap(),
            Some(RequestStatus::Completed)
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn native_stream_registers_complete_client_tool_group_before_partial_replay() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_sse_client_tool_group))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("streaming-tool-continuation");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let tools = json!([
            { "type": "function", "function": { "name": "shell_command", "parameters": { "type": "object" } } },
            { "type": "function", "function": { "name": "apply_patch", "parameters": { "type": "object" } } }
        ]);
        let first_input = request("resp_native_stream_tool_first", true, tools.clone());
        let first = try_native_responses(
            &state,
            &HeaderMap::new(),
            &first_input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("streaming native response");
        let body = axum::body::to_bytes(first.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stream = String::from_utf8(body.to_vec()).unwrap();
        assert!(stream.contains("call_stream_shell"));
        assert!(stream.contains("call_stream_patch"));
        assert_eq!(state.native_pending_tool_groups.pending_count(), 1);

        let shell_call = json!({
            "type": "function_call",
            "id": "fc_stream_1",
            "call_id": "call_stream_shell",
            "name": "shell_command",
            "arguments": "{}",
            "status": "completed"
        });
        let patch_call = json!({
            "type": "custom_tool_call",
            "id": "ctc_stream_1",
            "call_id": "call_stream_patch",
            "name": "apply_patch",
            "input": "*** Begin Patch\n*** End Patch",
            "status": "completed"
        });
        let mut continuation = request("resp_native_stream_tool_second", false, tools);
        continuation["previous_response_id"] = json!("resp_native_stream_tool_first");
        continuation["input"] = json!([
            first_input["input"][0].clone(),
            shell_call,
            patch_call,
            { "type": "function_call_output", "call_id": "call_stream_shell", "output": "done" }
        ]);
        let partial = try_native_responses(
            &state,
            &HeaderMap::new(),
            &continuation,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("partial replay must be rejected locally");
        assert_eq!(partial.status(), StatusCode::BAD_REQUEST);
        assert_eq!(state.native_pending_tool_groups.pending_count(), 1);
        assert_eq!(capture.requests.lock().expect("capture lock").len(), 1);

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn native_non_streaming_client_tool_continuation_reconstructs_one_complete_group() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_client_tool_turn))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("tool-continuation");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let tool = json!({
            "type": "function",
            "name": "shell_command",
            "description": "Codex-owned test tool",
            "parameters": { "type": "object", "properties": {} }
        });
        let mut first_input = request("resp_native_tool_first", false, json!([tool]));
        first_input["instructions"] = json!("initial native instructions");
        let first = try_native_responses(
            &state,
            &HeaderMap::new(),
            &first_input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("first native response");
        let first_body = axum::body::to_bytes(first.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let first_response = serde_json::from_slice::<Value>(&first_body).unwrap();
        assert_eq!(first_response["id"], "resp_native_tool_first");
        assert_eq!(state.native_pending_tool_groups.pending_count(), 1);

        let provider_call = json!({
            "type": "function_call",
            "id": "fc_native_1",
            "call_id": "call_native_1",
            "name": "shell_command",
            "arguments": "{\"command\":\"echo native\"}",
            "status": "completed"
        });
        let mut continuation = request(
            "resp_native_tool_second",
            false,
            json!([{
                "type": "function",
                "name": "shell_command",
                "description": "Codex-owned test tool",
                "parameters": { "type": "object", "properties": {} }
            }]),
        );
        continuation["previous_response_id"] = json!("resp_native_tool_first");
        continuation["stream"] = json!(true);
        continuation["instructions"] = json!("current authoritative instructions");
        continuation["input"] = json!([
            first_input["input"][0].clone(),
            provider_call,
            {
                "type": "function_call_output",
                "call_id": "call_native_1",
                "output": "native tool completed"
            }
        ]);
        let second = try_native_responses(
            &state,
            &HeaderMap::new(),
            &continuation,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("continuation native response");
        let second_body = axum::body::to_bytes(second.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let second_response = serde_json::from_slice::<Value>(&second_body).unwrap();

        assert_eq!(second_response["id"], "resp_native_tool_second");
        assert_eq!(state.native_pending_tool_groups.pending_count(), 0);
        let captured = capture.requests.lock().expect("capture lock");
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[1]["input"], continuation["input"]);
        assert!(captured[1].get("previous_response_id").is_none());
        assert_eq!(captured[1]["stream"], true);
        assert_eq!(
            captured[1]["instructions"],
            "current authoritative instructions"
        );
        drop(captured);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn failed_non_streaming_continuation_keeps_prior_tool_group_pending() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_client_tool_turn_failed))
            .with_state(capture);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("failed-tool-continuation");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let tool = json!({
            "type": "function",
            "name": "shell_command",
            "parameters": { "type": "object", "properties": {} }
        });
        let first_input = request("resp_native_failed_first", false, json!([tool.clone()]));
        let first = try_native_responses(
            &state,
            &HeaderMap::new(),
            &first_input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("first native response");
        let _ = axum::body::to_bytes(first.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(state.native_pending_tool_groups.pending_count(), 1);

        let mut continuation = request("resp_native_failed_second", false, json!([tool]));
        continuation["previous_response_id"] = json!("resp_native_failed_first");
        continuation["input"] = json!([
            first_input["input"][0].clone(),
            {
                "type": "function_call",
                "id": "fc_native_failed_1",
                "call_id": "call_native_failed_1",
                "name": "shell_command",
                "arguments": "{}",
                "status": "completed"
            },
            {
                "type": "function_call_output",
                "call_id": "call_native_failed_1",
                "output": "done"
            }
        ]);
        let second = try_native_responses(
            &state,
            &HeaderMap::new(),
            &continuation,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("failed native response is relayed");
        let body = axum::body::to_bytes(second.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["status"],
            "failed"
        );
        assert_eq!(state.native_pending_tool_groups.pending_count(), 1);

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn failed_streaming_continuation_keeps_prior_tool_group_pending() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_sse_failed))
            .with_state(capture);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("failed-streaming-tool-continuation");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let tool = json!({
            "type": "function",
            "name": "shell_command",
            "parameters": { "type": "object", "properties": {} }
        });
        let first_input = request(
            "resp_native_stream_pending_first",
            false,
            json!([tool.clone()]),
        );
        let provider_call = json!({
            "type": "function_call",
            "id": "fc_native_stream_pending_1",
            "call_id": "call_native_stream_pending_1",
            "name": "shell_command",
            "arguments": "{}",
            "status": "completed"
        });
        state
            .native_pending_tool_groups
            .register(PendingNativeToolGroup::new(
                "resp_native_stream_pending_first",
                &first_input,
                first_input["input"].as_array().unwrap().clone(),
                vec![provider_call.clone()],
                vec![provider_call.clone()],
                Vec::new(),
                vec![NativeToolCall {
                    call_id: "call_native_stream_pending_1".to_owned(),
                    name: "shell_command".to_owned(),
                    input: "{}".to_owned(),
                    kind: NativeToolCallKind::Function,
                }],
            ))
            .unwrap();
        let mut continuation = request("resp_native_stream_pending_second", true, json!([tool]));
        continuation["previous_response_id"] = json!("resp_native_stream_pending_first");
        continuation["input"] = json!([
            first_input["input"][0].clone(),
            provider_call,
            {
                "type": "function_call_output",
                "call_id": "call_native_stream_pending_1",
                "output": "done"
            }
        ]);
        let response = try_native_responses(
            &state,
            &HeaderMap::new(),
            &continuation,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("streaming failure is relayed");
        let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(state.native_pending_tool_groups.pending_count(), 1);

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn local_search_auto_falls_back_without_contacting_native_upstream() {
        let data_dir = temp_data_dir("local-fallback");
        let mut config = AppConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        config.upstream.transport = UpstreamTransport::Auto;
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let input = request(
            "resp_native_local",
            true,
            json!([
                { "type": "web_search" }
            ]),
        );

        assert!(
            try_native_responses(
                &state,
                &HeaderMap::new(),
                &input,
                &config,
                "deepseek-v4-flash",
                Some("deepseek-v4-flash"),
            )
            .await
            .is_none(),
            "Auto must retain local web_search through chat_compat"
        );
        assert_eq!(
            state
                .store
                .response_status("resp_native_local")
                .await
                .unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn official_backend_without_a_search_tool_allows_local_tool_chat_fallback() {
        let data_dir = temp_data_dir("official-no-search-local-tool-fallback");
        let mut config = AppConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        config.upstream.transport = UpstreamTransport::Auto;
        config.web_search_backend = WebSearchBackend::Official;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let input = request(
            "resp_native_official_no_search",
            true,
            json!([{ "type": "function", "function": { "name": "workspace_search", "parameters": { "type": "object" } } }]),
        );

        assert!(
            try_native_responses(
                &state,
                &HeaderMap::new(),
                &input,
                &config,
                "deepseek-v4-flash",
                Some("deepseek-v4-flash"),
            )
            .await
            .is_none(),
            "official mode must not block unrelated local tools when no search was requested"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn official_search_never_silently_falls_back_to_local_when_other_local_tools_are_present()
    {
        let data_dir = temp_data_dir("official-search-mixed-tools");
        let mut config = AppConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        config.upstream.transport = UpstreamTransport::Auto;
        config.web_search_backend = WebSearchBackend::Official;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let input = request(
            "resp_native_official_search_mixed",
            true,
            json!([
                { "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } },
                { "type": "function", "function": { "name": "workspace_search", "parameters": { "type": "object" } } }
            ]),
        );

        let response = try_native_responses(
            &state,
            &HeaderMap::new(),
            &input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("official/local mixed tools must return a controlled error");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            state
                .store
                .response_status("resp_native_official_search_mixed")
                .await
                .unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn official_search_is_rejected_before_chat_compat_can_run_local_search() {
        let data_dir = temp_data_dir("official-search-chat-compat");
        let mut config = AppConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        config.upstream.transport = UpstreamTransport::ChatCompat;
        config.web_search_backend = WebSearchBackend::Official;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store.clone());
        let input = request(
            "resp_official_search_chat_compat",
            true,
            json!([{ "type": "function", "function": { "name": "web_search_preview", "parameters": { "type": "object" } } }]),
        );

        let response = dispatch_if_selected(
            &state,
            &HeaderMap::new(),
            &input,
            &config,
            "deepseek-v4-pro",
            Some("deepseek-v4-pro"),
        )
        .await
        .expect("official search must not fall through to chat compatibility");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["error"]["code"],
            "official_web_search_incompatible"
        );
        assert_eq!(
            store
                .response_status("resp_official_search_chat_compat")
                .await
                .unwrap(),
            None,
            "the Chat lifecycle never started"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn local_search_still_enters_chat_compat_when_native_is_unavailable() {
        let data_dir = temp_data_dir("local-search-chat-compat");
        let mut config = AppConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        config.upstream.transport = UpstreamTransport::ChatCompat;
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let input = request(
            "resp_local_search_chat_compat",
            true,
            json!([{ "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } }]),
        );

        assert!(
            dispatch_if_selected(
                &state,
                &HeaderMap::new(),
                &input,
                &config,
                "deepseek-v4-pro",
                Some("deepseek-v4-pro"),
            )
            .await
            .is_none(),
            "local search must retain the existing Chat compatibility path"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
