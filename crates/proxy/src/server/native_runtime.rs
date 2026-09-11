//! Native DeepSeek Responses transport.
//!
//! This module deliberately owns only the native wire boundary. It neither
//! rebuilds Codex history nor reads a Codex transcript: the client's `input`
//! remains authoritative. The native transport is the default for every
//! upstream; Chat API compatibility is selected only when the user enables it.
//! CodeSeeX-hosted tools (local web search) are executed inside this transport
//! by the hosted tool loop; the native route never borrows the Chat
//! compatibility executor, so tool ownership never changes silently.

use super::*;
use crate::native_coordinator::{
    NativeInjectedItems, NativePendingContinuation, NativePendingError, PendingNativeToolGroup,
};
use crate::native_responses::{
    append_complete_native_tool_group, native_stream_finalization,
    native_tool_call_group_from_response, native_tool_output_item, plan_native_tools,
    present_reasoning_summary_in_response, reconcile_grouped_tool_namespaces,
    repair_provider_tool_schemas, rewrite_provider_response_identity, NativeResponseSseRelay,
    NativeResponseStreamInspection, NativeResponseTerminal, NativeStreamFinalization,
    NativeToolCall, NativeToolCallGroup, NativeToolPlan,
};
use crate::upstream::SelectedUpstreamTransport;
use codeseex_core::config::WebSearchBackend;
use super::response_helpers::show_thinking_enabled;

pub(super) async fn dispatch_if_selected(
    state: &ProxyState,
    headers: &HeaderMap,
    input: &Value,
    config: &AppConfig,
    model: &str,
    requested_model: Option<&str>,
) -> Option<axum::response::Response> {
    match crate::upstream::select_transport(&config.upstream) {
        SelectedUpstreamTransport::ChatCompat => {
            reject_official_web_search_on_chat_compat(state, input, config, model, requested_model)
                .await
        }
        SelectedUpstreamTransport::NativeResponses => {
            try_native_responses(state, headers, input, config, model, requested_model).await
        }
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
            return Some(native_incompatible(
                state,
                config,
                &id,
                requested_model,
                model,
                "tool_definition_incompatible",
                message,
            )
            .await);
        }
    };

    // CodeSeeX-hosted tools (for example local web search) are executed inside
    // the native transport itself. The native route never hands a request to
    // the Chat compatibility path, so the two APIs stay independent and tool
    // ownership never changes silently. A request that mixes provider-owned
    // official search with a hosted tool has no single owner and stays
    // fail-closed. Base workspace tools stay native because the Codex client
    // executes them itself.
    if plan.requires_local_execution {
        if config.web_search_backend == WebSearchBackend::Official && plan.uses_official_web_search
        {
            return Some(
                native_incompatible(
                    state,
                    config,
                    &id,
                    requested_model,
                    model,
                    "mixed_official_web_search_and_hosted_tool",
                    "This request combines provider-owned official web search with a CodeSeeX-hosted tool. CodeSeeX did not silently replace either backend. Select Chat API compatibility for this request.",
                )
                .await,
            );
        }
        return Some(
            native_hosted_tool_loop(NativeHostedToolLoopParams {
                state,
                headers,
                input,
                config,
                model,
                requested_model,
                plan: &plan,
            })
            .await,
        );
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
        Err(error) => {
            record_native_continuation_rejection(state, &error).await;
            return Some(native_pending_error_response(error));
        }
    };
    if let Some(continuation) = pending.as_ref() {
        payload["input"] = Value::Array(continuation.merged_input.clone());
    }
    let reconciled_namespaces = reconcile_grouped_tool_namespaces(&mut payload);
    record_namespace_reconciliation(state, &id, &reconciled_namespaces).await;
    let repaired_schemas = repair_provider_tool_schemas(&mut payload);
    record_tool_schema_repair(state, &id, &repaired_schemas).await;

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
    let managed_key = crate::secrets::upstream_api_key(&config);
    let passthrough = crate::upstream::UpstreamPassthrough::from_headers(headers);
    crate::upstream::remember_passthrough(&passthrough);
    let started = std::time::Instant::now();
    let upstream = crate::upstream::post_responses(
        &client,
        &config.upstream,
        crate::upstream::UpstreamAuthRequest {
            inbound: auth.as_deref(),
            local_access_token: Some(&state.v1_access_token),
            managed_key: managed_key.as_deref(),
            passthrough,
        },
        Some(input),
        native_upstream_payload(&payload, should_adapt_tool_protocol(&config.upstream, model)),
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
            present_reasoning_summary: show_thinking_enabled(config),
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
            show_thinking_enabled(config),
            pending
                .as_ref()
                .map(|continuation| continuation.pending_response_id.as_str()),
        )
        .await,
    )
}

/// One CodeSeeX-hosted call the native transport can execute itself.
fn native_hosted_call_is_local(call: &NativeToolCall, config: &AppConfig) -> bool {
    crate::tools::ownership::is_web_search_tool(&call.name)
        && config.web_search_backend != WebSearchBackend::Official
}

/// Drains a native SSE body into memory while rewriting the narrow provider
/// response-id boundary. The hosted loop needs the complete output group before
/// it can decide whether a turn is final or carries a hosted tool call, so the
/// final turn reaches the client only after that decision.
async fn buffer_native_sse(
    response: reqwest::Response,
    response_id: &str,
    present_reasoning_summary: bool,
) -> Result<(Vec<u8>, NativeResponseStreamInspection), reqwest::Error> {
    use futures_util::StreamExt;

    let mut upstream = response.bytes_stream();
    let mut relay = NativeResponseSseRelay::new(response_id.to_owned())
        .with_reasoning_summary_presentation(present_reasoning_summary);
    let mut buffered = Vec::new();
    while let Some(next) = upstream.next().await {
        let chunk = next?;
        for frame in relay.relay_bytes(&chunk) {
            buffered.extend_from_slice(&frame);
        }
    }
    if let Some(remainder) = relay.finish() {
        buffered.extend_from_slice(&remainder);
    }
    let inspection = relay.inspection().clone();
    Ok((buffered, inspection))
}

struct NativeHostedToolLoopParams<'a> {
    state: &'a ProxyState,
    headers: &'a HeaderMap,
    input: &'a Value,
    config: &'a AppConfig,
    model: &'a str,
    requested_model: Option<&'a str>,
    plan: &'a NativeToolPlan,
}

/// Executes CodeSeeX-hosted tools (local web search) inside the native
/// Responses transport. The native route owns its own tool loop and never
/// defers to the Chat compatibility path, so the two APIs stay independent and
/// a request can never change tool ownership silently. Every provider tool
/// group is executed in full and replayed as one complete native continuation.
async fn native_hosted_tool_loop(
    params: NativeHostedToolLoopParams<'_>,
) -> axum::response::Response {
    let NativeHostedToolLoopParams {
        state,
        headers,
        input,
        config,
        model,
        requested_model,
        plan,
    } = params;
    let id = response_id_from_input(input);
    let previous = input.get("previous_response_id").and_then(Value::as_str);

    let mut payload = match native_payload(input, model, &plan.tools) {
        Ok(payload) => payload,
        Err(message) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "native_responses_input_invalid",
                message,
            );
        }
    };
    let pending = match state.native_pending_tool_groups.continuation_for(input) {
        Ok(pending) => pending,
        Err(error) => {
            record_native_continuation_rejection(state, &error).await;
            return native_pending_error_response(error);
        }
    };
    if let Some(continuation) = pending.as_ref() {
        payload["input"] = Value::Array(continuation.merged_input.clone());
    }
    let reconciled_namespaces = reconcile_grouped_tool_namespaces(&mut payload);
    record_namespace_reconciliation(state, &id, &reconciled_namespaces).await;
    let repaired_schemas = repair_provider_tool_schemas(&mut payload);
    record_tool_schema_repair(state, &id, &repaired_schemas).await;
    if let Err(error) = state
        .store
        .checkpoint_request(&id, previous, Some(model), input)
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
            "Native Responses request started with the hosted tool loop.",
            Some(&json!({
                "id": id,
                "endpoint": "/v1/responses",
                "transport": "native_responses",
                "tool_loop": "native_hosted",
                "requested_model": requested_model,
                "model": model,
                "web_search_backend": web_search_backend_label(config.web_search_backend)
            })),
        )
        .await;

    let auth = upstream_authorization_from_headers(headers, &state.v1_access_token);
    if let Some(auth) = auth.as_deref() {
        codeseex_core::codex_auth::remember_authorization_header(auth);
    }
    let client = state.client();
    let managed_key = crate::secrets::upstream_api_key(config);
    let passthrough = crate::upstream::UpstreamPassthrough::from_headers(headers);
    crate::upstream::remember_passthrough(&passthrough);
    let tool_context = crate::tools::ToolExecutionContext::from_request(input);
    let mut tool_messages: Vec<Value> = input
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut authoritative_input = payload
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Codex never sees a hosted round CodeSeeX executes itself, so the rounds
    // are kept apart from the client-visible anchor. They are replayed at the
    // offset in that anchor where CodeSeeX injected them.
    let client_input_len = input
        .get("input")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let mut injected_items: Vec<NativeInjectedItems> = pending
        .as_ref()
        .map(|continuation| continuation.injected_items.clone())
        .unwrap_or_default();

    let max_iterations = crate::tools::diagnostics::MAX_TOOL_LOOP_ITERATIONS;
    let mut iteration = 0_u32;
    loop {
        iteration += 1;
        let started = std::time::Instant::now();
        let upstream = crate::upstream::post_responses(
            &client,
            &config.upstream,
            crate::upstream::UpstreamAuthRequest {
                inbound: auth.as_deref(),
                local_access_token: Some(&state.v1_access_token),
                managed_key: managed_key.as_deref(),
                passthrough: passthrough.clone(),
            },
            Some(input),
            native_upstream_payload(&payload, should_adapt_tool_protocol(&config.upstream, model)),
        )
        .await;
        let response = match upstream {
            Ok(response) => response,
            Err(error) => {
                let detail = json!({
                    "id": id,
                    "transport": "native_responses",
                    "tool_loop": "native_hosted",
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
                return json_error(
                    StatusCode::BAD_GATEWAY,
                    "native_upstream_connection_failed",
                    error.to_string(),
                );
            }
        };
        let status = response.status();
        let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
        let response_headers = response.headers().clone();
        if !status.is_success() {
            return native_upstream_status_failure(
                state,
                &id,
                requested_model,
                model,
                status,
                response,
                pending.as_ref(),
            )
            .await;
        }
        // The pending continuation this request consumed has now been accepted
        // upstream, so it must not stay registered: a stale group would make the
        // next replay ambiguous. A failed dispatch keeps it for a retry.
        if let Some(continuation) = pending.as_ref() {
            state
                .native_pending_tool_groups
                .settle(&continuation.pending_response_id);
        }
        let is_sse = content_type
            .as_ref()
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/event-stream"));
        let (body, output_items, completed, usage) = if is_sse {
            match buffer_native_sse(response, &id, show_thinking_enabled(config)).await {
                Ok((bytes, inspection)) => {
                    if inspection.output_items_incomplete {
                        let detail = json!({
                            "id": id,
                            "transport": "native_responses",
                            "tool_loop": "native_hosted",
                            "issue": "stream_output_items_incomplete"
                        });
                        let _ = state
                            .store
                            .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                            .await;
                        return json_error(
                            StatusCode::BAD_GATEWAY,
                            "native_tool_protocol_invalid",
                            "Native Responses stream did not yield a safe complete output group."
                                .to_owned(),
                        );
                    }
                    let completed = matches!(
                        inspection.terminal,
                        Some(NativeResponseTerminal::Completed)
                    );
                    (
                        bytes,
                        inspection.output_items,
                        completed,
                        inspection.final_usage,
                    )
                }
                Err(error) => {
                    let detail = upstream_body_read_error_detail(
                        &id,
                        requested_model,
                        Some(model),
                        status,
                        &response_headers,
                        &error,
                    );
                    let _ = state
                        .store
                        .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                        .await;
                    return json_error(
                        StatusCode::BAD_GATEWAY,
                        "native_upstream_body_failed",
                        error.to_string(),
                    );
                }
            }
        } else {
            let bytes = match response.bytes().await {
                Ok(bytes) => bytes,
                Err(error) => {
                    let detail = upstream_body_read_error_detail(
                        &id,
                        requested_model,
                        Some(model),
                        status,
                        &response_headers,
                        &error,
                    );
                    let _ = state
                        .store
                        .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                        .await;
                    return json_error(
                        StatusCode::BAD_GATEWAY,
                        "native_upstream_body_failed",
                        error.to_string(),
                    );
                }
            };
            let native = match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => value,
                Err(error) => {
                    let detail = upstream_json_parse_error_detail(
                        &id,
                        requested_model,
                        Some(model),
                        status,
                        &response_headers,
                        bytes.len(),
                        &error,
                    );
                    let _ = state
                        .store
                        .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                        .await;
                    return json_error(
                        StatusCode::BAD_GATEWAY,
                        "native_upstream_json_failed",
                        error.to_string(),
                    );
                }
            };
            let completed = native.get("status").and_then(Value::as_str) == Some("completed");
            let output_items = native
                .get("output")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            (bytes.to_vec(), output_items, completed, native.get("usage").cloned())
        };

        let tool_group =
            match native_tool_call_group_from_response(&json!({ "output": output_items })) {
                Ok(group) => group,
                Err(error) => {
                    let detail = json!({
                        "id": id,
                        "transport": "native_responses",
                        "tool_loop": "native_hosted",
                        "error": error
                    });
                    let _ = state
                        .store
                        .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                        .await;
                    return json_error(
                        StatusCode::BAD_GATEWAY,
                        "native_tool_protocol_invalid",
                        error,
                    );
                }
            };

        let Some(group) = tool_group else {
            let status_to_store = if completed {
                RequestStatus::Completed
            } else {
                RequestStatus::Failed
            };
            let detail = json!({
                "transport": "native_responses",
                "tool_loop": "native_hosted",
                "web_search_backend": web_search_backend_label(config.web_search_backend),
                "provider_tool_calls": 0
            });
            let _ = state
                .store
                .record_event(
                    "info",
                    "upstream_call_usage_breakdown",
                    "CodeSeeX upstream call usage breakdown.",
                    Some(&upstream_call_usage_breakdown_event(
                        &id,
                        "native_hosted_loop",
                        iteration,
                        input,
                        &payload,
                        usage.as_ref(),
                        Some(started.elapsed().as_millis() as u64),
                        false,
                    )),
                )
                .await;
            let _ = state
                .store
                .finish_request(&id, status_to_store, None, Some(&detail))
                .await;
            return native_provider_turn_response(body, is_sse, &id);
        };

        // Codex always declares its local web search tool, so a request that
        // selected the CodeSeeX-hosted backend enters this loop even when the
        // provider only asks for Codex-owned tools. A group without a single
        // CodeSeeX hosted call belongs to the client: the provider turn is
        // forwarded unchanged and retained for the continuation check, exactly
        // as the non-hosted native path forwards it. CodeSeeX never executes a
        // client-owned tool itself.
        let hosted_calls = group
            .calls
            .iter()
            .filter(|call| native_hosted_call_is_local(call, config))
            .count();
        if hosted_calls == 0 {
            return native_hosted_client_tool_group(NativeHostedClientToolGroupParams {
                state,
                config,
                id: &id,
                group: &group,
                input,
                payload: &payload,
                body,
                is_sse,
                completed,
                usage: usage.as_ref(),
                iteration,
                started,
                injected_items,
                pending: pending.as_ref(),
            })
            .await;
        }
        // A group that mixes hosted and client-owned calls has no single owner.
        // The native transport fails closed instead of handing either side to
        // the other transport.
        if hosted_calls != group.calls.len() {
            let hosted = group
                .calls
                .iter()
                .filter(|call| native_hosted_call_is_local(call, config))
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>();
            let client_owned = group
                .calls
                .iter()
                .filter(|call| !native_hosted_call_is_local(call, config))
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>();
            return native_incompatible(
                state,
                config,
                &id,
                requested_model,
                model,
                "mixed_hosted_and_client_tool_group",
                format!(
                    "Native Responses returned a mixed tool group (hosted: {hosted:?}, client-owned: {client_owned:?}). CodeSeeX does not split tool ownership between transports."
                ),
            )
            .await;
        }

        if iteration >= max_iterations {
            let message = format!(
                "Native hosted tool loop exceeded {max_iterations} iterations; CodeSeeX stopped the loop to avoid unbounded execution."
            );
            let detail = json!({
                "id": id,
                "transport": "native_responses",
                "tool_loop": "native_hosted",
                "iterations": iteration,
                "error": message
            });
            let _ = state
                .store
                .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                .await;
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "native_tool_loop_iteration_limit",
                message,
            );
        }

        let mut outputs = Vec::with_capacity(group.calls.len());
        for call in &group.calls {
            let _ = state
                .store
                .record_event(
                    "info",
                    "tool_call",
                    "CodeSeeX tool requested in the native hosted tool loop.",
                    Some(&json!({
                        "id": id,
                        "call_id": call.call_id,
                        "name": call.name,
                        "iteration": iteration,
                        "transport": "native_responses"
                    })),
                )
                .await;
            let result = crate::tools::execute_tool_with_client(
                &client,
                config,
                &tool_context,
                &tool_messages,
                &[],
                &call.name,
                &call.input,
            )
            .await;
            let replay = crate::tools::hosted::model_replay_tool_result_for(&call.name, &result);
            outputs.push(native_tool_output_item(call, replay.clone()));
            let _ = state
                .store
                .record_event(
                    "info",
                    "tool_result",
                    "CodeSeeX tool result in the native hosted tool loop.",
                    Some(&crate::tools::hosted::tool_result_event_detail_for(
                        &id,
                        &call.call_id,
                        &call.name,
                        iteration,
                        &result,
                    )),
                )
                .await;
            tool_messages.push(json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": call.call_id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": call.input }
                }]
            }));
            tool_messages.push(json!({
                "role": "tool",
                "tool_call_id": call.call_id,
                "content": replay
            }));
        }
        let next_input =
            match append_complete_native_tool_group(&authoritative_input, &group, &outputs) {
                Ok(next) => next,
                Err(error) => {
                    let detail = json!({
                        "id": id,
                        "transport": "native_responses",
                        "tool_loop": "native_hosted",
                        "error": error
                    });
                    let _ = state
                        .store
                        .finish_request(&id, RequestStatus::Failed, None, Some(&detail))
                        .await;
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "native_tool_continuation_failed",
                        error,
                    );
                }
            };
        authoritative_input = next_input.clone();
        payload["input"] = Value::Array(next_input);
        let mut executed_round = group.provider_output.clone();
        executed_round.extend(outputs.iter().cloned());
        injected_items.push(NativeInjectedItems {
            offset: client_input_len,
            items: executed_round,
        });
    }
}

struct NativeHostedClientToolGroupParams<'a> {
    state: &'a ProxyState,
    config: &'a AppConfig,
    id: &'a str,
    group: &'a NativeToolCallGroup,
    input: &'a Value,
    payload: &'a Value,
    body: Vec<u8>,
    is_sse: bool,
    completed: bool,
    usage: Option<&'a Value>,
    iteration: u32,
    started: std::time::Instant,
    /// Hosted rounds this loop already executed before it handed the retained
    /// provider group back to Codex. Codex never saw them, so the coordinator
    /// replays them from the offset inside the client-visible anchor where they
    /// were injected.
    injected_items: Vec<NativeInjectedItems>,
    pending: Option<&'a NativePendingContinuation>,
}

/// Hands one provider turn that only carries Codex-owned tool calls back to the
/// client. The hosted loop must not execute those calls itself, so the turn is
/// forwarded to Codex and retained in RAM: the client runs the call it owns and
/// the retained group proves that the replay which follows is complete and in
/// order before CodeSeeX sends it upstream.
async fn native_hosted_client_tool_group(
    params: NativeHostedClientToolGroupParams<'_>,
) -> axum::response::Response {
    let NativeHostedClientToolGroupParams {
        state,
        config,
        id,
        group,
        input,
        payload,
        body,
        is_sse,
        completed,
        usage,
        iteration,
        started,
        injected_items,
        pending,
    } = params;
    if completed {
        // The client only ever saw this turn, so the retained group is anchored
        // on the client-visible input. Items CodeSeeX injected earlier in this
        // same hosted loop stay out of that anchor: the client cannot replay
        // what it never received.
        let authoritative_input = input
            .get("input")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Err(error) = retain_native_pending_tool_group(
            state,
            id,
            input,
            &authoritative_input,
            injected_items,
            group,
        )
        .await
        {
            let detail = json!({ "id": id, "error": error.message() });
            let _ = state
                .store
                .finish_request(id, RequestStatus::Failed, None, Some(&detail))
                .await;
            return native_pending_error_response(error);
        }
        if let Some(continuation) = pending {
            state
                .native_pending_tool_groups
                .settle(&continuation.pending_response_id);
        }
    }
    let status_to_store = if completed {
        RequestStatus::Completed
    } else {
        RequestStatus::Failed
    };
    let detail = json!({
        "transport": "native_responses",
        "tool_loop": "native_hosted",
        "client_tool_group": group
            .calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>(),
        "web_search_backend": web_search_backend_label(config.web_search_backend),
        "provider_tool_calls": group.calls.len()
    });
    let _ = state
        .store
        .record_event(
            "info",
            "upstream_call_usage_breakdown",
            "CodeSeeX upstream call usage breakdown.",
            Some(&upstream_call_usage_breakdown_event(
                id,
                "native_hosted_loop",
                iteration,
                input,
                payload,
                usage,
                Some(started.elapsed().as_millis() as u64),
                false,
            )),
        )
        .await;
    let _ = state
        .store
        .finish_request(id, status_to_store, None, Some(&detail))
        .await;
    native_provider_turn_response(body, is_sse, id)
}

/// Returns one buffered provider turn to the client. The native transport
/// forwards the provider body and only narrows the response identity it exposes.
fn native_provider_turn_response(
    body: Vec<u8>,
    is_sse: bool,
    id: &str,
) -> axum::response::Response {
    if is_sse {
        return response_from_bytes(
            reqwest::StatusCode::OK,
            Some(HeaderValue::from_static("text/event-stream")),
            body,
        );
    }
    let mut native = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(_) => {
            return response_from_bytes(
                reqwest::StatusCode::OK,
                response_content_type_json(),
                body,
            );
        }
    };
    let provider_id = native.get("id").and_then(Value::as_str).map(str::to_owned);
    if let Some(provider_id) = provider_id.as_deref() {
        rewrite_provider_response_identity(&mut native, provider_id, id);
    }
    json_response(native)
}

/// Codex replays `tool_search_output` items verbatim, so one namespace can
/// arrive twice with a different nested tool list. The provider rejects a
/// repeated namespace name with `Duplicate namespace name`, and the native
/// transport now merges the repetition instead of failing the whole turn. The
/// event keeps that repair visible in the log.
async fn record_namespace_reconciliation(state: &ProxyState, id: &str, merged: &[String]) {
    if merged.is_empty() {
        return;
    }
    let _ = state
        .store
        .record_event(
            "info",
            "native_tool_namespace_reconciled",
            "CodeSeeX merged repeated tool namespaces so the provider accepts the replayed tool list.",
            Some(&json!({
                "id": id,
                "transport": "native_responses",
                "merged_namespaces": merged,
                "reason": "provider_requires_unique_namespace_names"
            })),
        )
        .await;
}

/// The provider validates every forwarded declaration, so a function whose
/// parameter schema has no `type` fails the whole turn. The repair keeps the
/// tool's own schema and only declares it as an object schema, and the event
/// keeps that repair visible in the log.
async fn record_tool_schema_repair(state: &ProxyState, id: &str, repaired: &[String]) {
    if repaired.is_empty() {
        return;
    }
    let _ = state
        .store
        .record_event(
            "info",
            "native_tool_schema_repaired",
            "CodeSeeX completed a tool schema so the provider accepts the forwarded declaration.",
            Some(&json!({
                "id": id,
                "transport": "native_responses",
                "repaired_functions": repaired,
                "reason": "provider_requires_object_typed_parameters"
            })),
        )
        .await;
}

async fn native_incompatible(
    state: &ProxyState,
    config: &AppConfig,
    id: &str,
    requested_model: Option<&str>,
    model: &str,
    issue: &str,
    message: impl Into<String>,
) -> axum::response::Response {
    let message = message.into();
    let detail = json!({
        "id": id,
        "transport": "native_responses",
        "issue": issue,
        "requested_model": requested_model,
        "model": model,
        "selected_web_search_backend": web_search_backend_label(config.web_search_backend),
        "selection": "explicit_or_failed_closed",
        "fallback": "none"
    });
    let _ = state
        .store
        .record_event(
            "warn",
            "native_responses_compatibility_diagnostic",
            "Native Responses request is incompatible with the selected transport.",
            Some(&detail),
        )
        .await;
    json_error(
        StatusCode::BAD_REQUEST,
        "native_responses_incompatible",
        message,
    )
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

/// The upstream boundary for the client-facing reasoning presentation.
///
/// The native relay presents provider `reasoning_text` as the summary Codex
/// renders. DeepSeek's thinking mode then requires the text back in
/// `reasoning_text` on the next call, so the presentation is undone here before
/// a replay reaches upstream. The mapping is read from the item itself, so it
/// holds for every replayed item, including the ones Codex re-serializes
/// without the provider item id.
fn native_upstream_payload(payload: &Value, restore_reasoning_text: bool) -> Value {
    let mut payload = payload.clone();
    if !restore_reasoning_text {
        return payload;
    }
    let Some(items) = payload.get_mut("input").and_then(Value::as_array_mut) else {
        return payload;
    };
    for item in items.iter_mut() {
        restore_reasoning_text_field(item);
    }
    payload
}

/// Puts a presented summary back into the provider's own `reasoning_text`
/// content and drops the summary CodeSeeX added, so a thinking-mode replay
/// always carries the text DeepSeek requires.
fn restore_reasoning_text_field(item: &mut Value) {
    if !item.is_object() || item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return;
    }
    let summary_text = item
        .get("summary")
        .and_then(Value::as_array)
        .map(|parts| parts.iter().map(summary_part_text).collect::<String>())
        .unwrap_or_default();
    let content_text = item
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|part| {
                    part.get("type").and_then(Value::as_str) == Some("reasoning_text")
                })
                .map(summary_part_text)
                .collect::<String>()
        })
        .unwrap_or_default();
    if content_text.is_empty() {
        if summary_text.is_empty() {
            return;
        }
        item["content"] = json!([{ "type": "reasoning_text", "text": summary_text }]);
        item["summary"] = Value::Array(Vec::new());
        return;
    }
    // Keep a provider-authored summary when it is not the presentation CodeSeeX
    // added; only an exact duplicate of the reasoning text is dropped.
    if content_text == summary_text {
        item["summary"] = Value::Array(Vec::new());
    }
}

fn summary_part_text(part: &Value) -> String {
    part.get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
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
    present_reasoning_summary: bool,
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
            let authoritative_input = payload
                .get("input")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // This path never executes a hosted round itself, so the stored
            // group carried no CodeSeeX-injected input.
            if let Err(error) = retain_native_pending_tool_group(
                state,
                id,
                input,
                &authoritative_input,
                Vec::new(),
                group,
            )
            .await
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
    // Only the client copy carries the reasoning presentation: the retained
    // group and the stored response must keep the provider's own item shape.
    let mut client_response = native;
    if present_reasoning_summary {
        present_reasoning_summary_in_response(&mut client_response);
    }
    json_response(client_response)
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
    present_reasoning_summary: bool,
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
        present_reasoning_summary,
        settle_pending_response_id,
    } = params;
    let cancelled = register_streaming_response(&response_id);
    let guard =
        StreamingRequestGuard::new(state.store.clone(), response_id.clone(), cancelled.clone());
    let stream: BoxStream<'static, Result<Bytes, std::io::Error>> = Box::pin(
        async_stream::try_stream! {
            let _stream_guard = guard;
            let mut upstream = response.bytes_stream();
            let mut relay = NativeResponseSseRelay::new(response_id.clone())
                .with_reasoning_summary_presentation(present_reasoning_summary);
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
                            // The streamed path never executes a hosted round
                            // itself, so the stored group carried none either.
                            let authoritative_input = payload
                                .get("input")
                                .and_then(Value::as_array)
                                .cloned()
                                .unwrap_or_default();
                            match retain_native_pending_tool_group(
                                &state,
                                &response_id,
                                &original_request,
                                &authoritative_input,
                                Vec::new(),
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

/// Every rejected native continuation is recorded, not only the ones that carry
/// a structured mismatch detail. Otherwise a client sees a bare 400 and the
/// event log stays empty, which leaves an operator nothing to act on.
async fn record_native_continuation_rejection(state: &ProxyState, error: &NativePendingError) {
    let message = error.message();
    let detail = json!({
        "code": error.code(),
        "message": message,
        "diagnostic": error.diagnostic(),
    });
    let _ = state
        .store
        .record_event(
            "warn",
            "native_pending_continuation_rejected",
            &message,
            Some(&detail),
        )
        .await;
}

/// Registers exactly the provider output group that Codex observed, plus the
/// hosted rounds CodeSeeX executed for it. This state stays only in RAM and is
/// used solely to reject partial/out-of-order client tool outputs on the
/// immediate full-replay continuation. The caller supplies the input prefix the
/// client is expected to replay, so it must be the client-visible history and
/// never a payload CodeSeeX extended locally; injected rounds travel separately
/// so the anchor stays client-visible.
async fn retain_native_pending_tool_group(
    state: &ProxyState,
    response_id: &str,
    original_request: &Value,
    authoritative_input: &[Value],
    injected_items: Vec<NativeInjectedItems>,
    group: &NativeToolCallGroup,
) -> Result<(), NativePendingError> {
    let pending = PendingNativeToolGroup::new(
        response_id,
        original_request,
        authoritative_input.to_vec(),
        injected_items,
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
            "local_output_count": value.local_output_count,
            "injected_item_count": value
                .injected_items
                .iter()
                .map(|segment| segment.items.len())
                .sum::<usize>()
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
    use codeseex_core::config::UpstreamTransport;
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
                transport: UpstreamTransport::NativeResponses,
                credential: Default::default(),
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

    /// A provider turn that streams its thinking the way DeepSeek does: as
    /// `reasoning_text` content, with an empty `summary`.
    async fn fake_native_sse_reasoning(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        capture.requests.lock().expect("capture lock").push(payload);
        let bytes = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":1,\"response\":{\"id\":\"provider_reasoning_1\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"sequence_number\":2,\"response_id\":\"provider_reasoning_1\",\"output_index\":0,\"item\":{\"id\":\"rs_provider\",\"type\":\"reasoning\",\"status\":\"in_progress\",\"content\":[],\"summary\":[]}}\n\n",
            "event: response.content_part.added\n",
            "data: {\"type\":\"response.content_part.added\",\"sequence_number\":3,\"response_id\":\"provider_reasoning_1\",\"item_id\":\"rs_provider\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"reasoning_text\",\"text\":\"\"}}\n\n",
            "event: response.reasoning_text.delta\n",
            "data: {\"type\":\"response.reasoning_text.delta\",\"sequence_number\":4,\"response_id\":\"provider_reasoning_1\",\"item_id\":\"rs_provider\",\"output_index\":0,\"content_index\":0,\"delta\":\"step one \"}\n\n",
            "event: response.reasoning_text.delta\n",
            "data: {\"type\":\"response.reasoning_text.delta\",\"sequence_number\":5,\"response_id\":\"provider_reasoning_1\",\"item_id\":\"rs_provider\",\"output_index\":0,\"content_index\":0,\"delta\":\"step two\"}\n\n",
            "event: response.reasoning_text.done\n",
            "data: {\"type\":\"response.reasoning_text.done\",\"sequence_number\":6,\"response_id\":\"provider_reasoning_1\",\"item_id\":\"rs_provider\",\"output_index\":0,\"content_index\":0,\"text\":\"step one step two\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":7,\"response_id\":\"provider_reasoning_1\",\"output_index\":0,\"item\":{\"id\":\"rs_provider\",\"type\":\"reasoning\",\"status\":\"completed\",\"content\":[{\"type\":\"reasoning_text\",\"text\":\"step one step two\"}],\"summary\":[],\"encrypted_content\":\"blob\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":8,\"response_id\":\"provider_reasoning_1\",\"output_index\":1,\"item\":{\"id\":\"msg_provider\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"42\",\"annotations\":[]}]}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":9,\"response\":{\"id\":\"provider_reasoning_1\",\"status\":\"completed\",\"output\":[{\"id\":\"rs_provider\",\"type\":\"reasoning\",\"content\":[{\"type\":\"reasoning_text\",\"text\":\"step one step two\"}],\"summary\":[],\"encrypted_content\":\"blob\"},{\"id\":\"msg_provider\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"42\",\"annotations\":[]}]}],\"usage\":{\"input_tokens\":7,\"output_tokens\":2,\"total_tokens\":9}}}\n\n"
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

    async fn fake_native_codex_app_tool_turn(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        capture.requests.lock().expect("capture lock").push(payload);
        Json(json!({
            "id": "provider_codex_app_turn_1",
            "object": "response",
            "model": "deepseek-v4-flash",
            "status": "completed",
            "output": [{
                "type": "function_call",
                "id": "fc_codex_app_1",
                "call_id": "call_read_thread",
                "name": "read_thread",
                "arguments": "{\"threadId\":\"01a08b18\"}",
                "status": "completed"
            }],
            "usage": { "input_tokens": 3, "output_tokens": 1, "total_tokens": 4 }
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

    async fn fake_native_hosted_tool_turn(
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
                "id": "provider_hosted_turn_1",
                "object": "response",
                "model": "deepseek-v4-flash",
                "status": "completed",
                "output": [{
                    "type": "function_call",
                    "id": "fc_hosted_1",
                    "call_id": "call_hosted_1",
                    "name": "web_search",
                    "arguments": "not-json",
                    "status": "completed"
                }],
                "usage": { "input_tokens": 3, "output_tokens": 1, "total_tokens": 4 }
            }));
        }
        Json(json!({
            "id": "provider_hosted_turn_2",
            "object": "response",
            "model": "deepseek-v4-flash",
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done via native hosted loop" }]
            }],
            "usage": { "input_tokens": 5, "output_tokens": 2, "total_tokens": 7 }
        }))
    }

    async fn fake_native_hosted_then_client_tool_turn(
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
                "id": "provider_hosted_first_1",
                "object": "response",
                "model": "deepseek-v4-flash",
                "status": "completed",
                "output": [{
                    "type": "function_call",
                    "id": "fc_hosted_search_1",
                    "call_id": "call_hosted_search",
                    "name": "web_search",
                    "arguments": "not-json",
                    "status": "completed"
                }],
                "usage": { "input_tokens": 3, "output_tokens": 1, "total_tokens": 4 }
            }));
        }
        if call_count == 2 {
            return Json(json!({
                "id": "provider_hosted_first_2",
                "object": "response",
                "model": "deepseek-v4-flash",
                "status": "completed",
                "output": [{
                    "type": "function_call",
                    "id": "fc_hosted_shell_1",
                    "call_id": "call_hosted_shell",
                    "name": "shell_command",
                    "arguments": "{}",
                    "status": "completed"
                }],
                "usage": { "input_tokens": 5, "output_tokens": 1, "total_tokens": 6 }
            }));
        }
        Json(json!({
            "id": "provider_hosted_first_3",
            "object": "response",
            "model": "deepseek-v4-flash",
            "status": "completed",
            "output": [],
            "usage": { "input_tokens": 7, "output_tokens": 1, "total_tokens": 8 }
        }))
    }

    async fn fake_native_sse_hosted_tool_turn(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        let call_count = {
            let mut requests = capture.requests.lock().expect("capture lock");
            requests.push(payload);
            requests.len()
        };
        let bytes = if call_count == 1 {
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"sequence_number\":1,\"response\":{\"id\":\"provider_sse_hosted_1\"}}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"sequence_number\":2,\"response_id\":\"provider_sse_hosted_1\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_sse_hosted_1\",\"call_id\":\"call_sse_hosted_1\",\"name\":\"web_search\",\"arguments\":\"not-json\",\"status\":\"completed\"}}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"sequence_number\":3,\"response\":{\"id\":\"provider_sse_hosted_1\",\"status\":\"completed\"}}\n\n"
            )
        } else {
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"sequence_number\":1,\"response\":{\"id\":\"provider_sse_hosted_2\"}}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"sequence_number\":2,\"response_id\":\"provider_sse_hosted_2\",\"item\":{\"type\":\"message\",\"id\":\"msg_sse_1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"final\"}]}}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"sequence_number\":3,\"response\":{\"id\":\"provider_sse_hosted_2\",\"status\":\"completed\"}}\n\n"
            )
        };
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            bytes.to_owned(),
        )
            .into_response()
    }

    async fn fake_native_mixed_tool_turn(
        State(capture): State<Capture>,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        capture.requests.lock().expect("capture lock").push(payload);
        Json(json!({
            "id": "provider_mixed_turn_1",
            "object": "response",
            "model": "deepseek-v4-flash",
            "status": "completed",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc_mixed_hosted",
                    "call_id": "call_mixed_hosted",
                    "name": "web_search",
                    "arguments": "not-json",
                    "status": "completed"
                },
                {
                    "type": "function_call",
                    "id": "fc_mixed_client",
                    "call_id": "call_mixed_client",
                    "name": "shell_command",
                    "arguments": "{}",
                    "status": "completed"
                }
            ]
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
    async fn native_dispatch_merges_replayed_namespace_duplicates_before_dispatch() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_response))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("namespace-merge");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store.clone());
        let namespace = |tools: Value| {
            json!({
                "type": "namespace",
                "name": "codex_app",
                "description": "Tools provided by the Codex app.",
                "tools": tools
            })
        };
        let mut input = request(
            "resp_native_namespace_merge",
            false,
            json!([namespace(json!([
                { "type": "function", "name": "fork_thread", "parameters": { "type": "object" } }
            ]))]),
        );
        input["input"] = json!([
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "replayed namespace" }]
            },
            {
                "type": "tool_search_output",
                "call_id": "call_discovered",
                "tools": [namespace(json!([
                    { "type": "function", "name": "read_thread", "parameters": { "type": "object" } }
                ]))]
            }
        ]);

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
        let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();

        let captured = capture.requests.lock().expect("capture lock");
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0]["tools"].as_array().unwrap().len(), 1);
        let declared = captured[0]["tools"][0]["tools"].as_array().unwrap();
        assert_eq!(declared.len(), 2);
        assert_eq!(declared[0]["name"], "fork_thread");
        assert_eq!(declared[1]["name"], "read_thread");
        assert_eq!(captured[0]["input"][1]["tools"], json!([]));
        drop(captured);

        let (events, _) = store.recent_events(20, None).await.unwrap();
        assert!(events.iter().any(|event| {
            event.event_type == "native_tool_namespace_reconciled"
                && event
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.pointer("/merged_namespaces/0"))
                    .and_then(Value::as_str)
                    == Some("codex_app")
        }));
        let _ = std::fs::remove_dir_all(data_dir);
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
    async fn native_stream_presents_reasoning_as_a_summary_and_keeps_the_provider_shape() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_sse_reasoning))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("reasoning-mirror");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);

        let first = try_native_responses(
            &state,
            &HeaderMap::new(),
            &request("resp_native_reasoning", true, json!([])),
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("native streaming response");
        let body = axum::body::to_bytes(first.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stream = String::from_utf8(body.to_vec()).unwrap();

        // Codex receives the thinking as the summary it renders, and never as
        // raw reasoning content it would drop.
        assert!(stream.contains("event: response.reasoning_summary_part.added"));
        assert!(stream.contains("event: response.reasoning_summary_text.delta"));
        assert!(stream.contains("event: response.reasoning_summary_text.done"));
        assert!(!stream.contains("event: response.reasoning_text.delta"));
        assert!(!stream.contains("\"type\":\"reasoning_text\""));
        assert!(stream
            .contains("\"summary\":[{\"text\":\"step one step two\",\"type\":\"summary_text\"}]"));
        assert!(!stream.contains("[DONE]"));

        // Codex replays the item as a summary on the following turn.
        let mut replay = request("resp_native_reasoning_second", true, json!([]));
        replay["input"] = json!([
            { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "again" }] },
            {
                "type": "reasoning",
                "id": "rs_provider",
                "summary": [{ "type": "summary_text", "text": "step one step two" }],
                "content": null,
                "encrypted_content": "blob"
            }
        ]);
        let second = try_native_responses(
            &state,
            &HeaderMap::new(),
            &replay,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("second native response");
        let _ = axum::body::to_bytes(second.into_body(), 1024 * 1024)
            .await
            .unwrap();

        // The presentation is undone: upstream gets the provider's own shape.
        let requests = capture.requests.lock().expect("capture lock");
        let forwarded = requests.last().expect("second upstream request");
        let replayed_reasoning = forwarded["input"]
            .as_array()
            .expect("forwarded input array")
            .iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
            .expect("the replayed reasoning item still travels");
        assert_eq!(replayed_reasoning["summary"], json!([]));
        assert_eq!(
            replayed_reasoning["content"][0]["type"],
            json!("reasoning_text")
        );
        assert_eq!(
            replayed_reasoning["content"][0]["text"],
            json!("step one step two")
        );
        assert_eq!(replayed_reasoning["encrypted_content"], json!("blob"));
        drop(requests);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn sub_agent_thread_sharing_the_parent_anchor_dispatches_as_a_new_conversation() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_sse_client_tool_group))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("sub-agent-anchor");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let tools = json!([
            { "type": "function", "function": { "name": "shell_command", "parameters": { "type": "object" } } },
            { "type": "function", "function": { "name": "apply_patch", "parameters": { "type": "object" } } }
        ]);

        let parent = request("resp_parent_turn", true, tools.clone());
        let parent_response = try_native_responses(
            &state,
            &HeaderMap::new(),
            &parent,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("parent turn");
        let _ = axum::body::to_bytes(parent_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(state.native_pending_tool_groups.pending_count(), 1);

        // A spawned thread reuses the parent's prompt_cache_key but is its own
        // conversation: it answers none of the parent's pending tool calls.
        let mut child = request("resp_child_turn", true, tools);
        child["previous_response_id"] = Value::Null;
        child["input"] = json!([{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "child prompt" }]
        }]);
        let child_response = try_native_responses(
            &state,
            &HeaderMap::new(),
            &child,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("child turn");
        assert_ne!(child_response.status(), StatusCode::BAD_REQUEST);
        let _ = axum::body::to_bytes(child_response.into_body(), 1024 * 1024)
            .await
            .unwrap();

        assert_eq!(
            capture.requests.lock().expect("capture lock").len(),
            2,
            "the sub-agent turn must reach upstream instead of failing closed"
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
                Vec::new(),
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
    async fn codex_normalized_client_tool_replay_still_dispatches_the_retained_group() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_response))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("normalized-client-tool-replay");
        let config = config_for_fake(data_dir.clone(), address);
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let tool = json!({
            "type": "function",
            "name": "exec_command",
            "parameters": { "type": "object", "properties": {} }
        });
        let first_input = request("resp_native_normalized_first", false, json!([tool.clone()]));
        // DeepSeek returns bare UUID item ids and a `status` on function calls. Codex
        // drops both before it replays history, so the continuation check must accept
        // that normalized copy.
        let provider_message = json!({
            "type": "message",
            "id": "c53e120a-544c-49fc-8314-6ebf26c2a122",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "working" }]
        });
        let provider_call = json!({
            "type": "function_call",
            "id": "2997568c-4abb-4010-999b-7a914321067a",
            "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
            "name": "exec_command",
            "arguments": "{\"cmd\":\"pwsh\"}",
            "status": "completed"
        });
        state
            .native_pending_tool_groups
            .register(PendingNativeToolGroup::new(
                "resp_native_normalized_first",
                &first_input,
                first_input["input"].as_array().unwrap().clone(),
                Vec::new(),
                vec![provider_message.clone(), provider_call.clone()],
                vec![provider_message, provider_call],
                Vec::new(),
                vec![NativeToolCall {
                    call_id: "call_00_mDEXZuYtEFUSlPgouz1p8384".to_owned(),
                    name: "exec_command".to_owned(),
                    input: "{\"cmd\":\"pwsh\"}".to_owned(),
                    kind: NativeToolCallKind::Function,
                }],
            ))
            .unwrap();
        let mut continuation = request("resp_native_normalized_second", false, json!([tool]));
        continuation["previous_response_id"] = json!("resp_native_normalized_first");
        continuation["input"] = json!([
            first_input["input"][0].clone(),
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "working" }]
            },
            {
                "type": "function_call",
                "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"pwsh\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
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
        .expect("the normalized Codex replay must still continue");
        assert_eq!(response.status(), StatusCode::OK);
        let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            state.native_pending_tool_groups.pending_count(),
            0,
            "a dispatched continuation settles the retained group"
        );
        let requests = capture.requests.lock().expect("capture lock");
        let sent = requests
            .last()
            .expect("the continuation reached the provider");
        let items = sent["input"].as_array().expect("forwarded input");
        assert!(
            items.iter().any(|item| {
                item.get("id").and_then(Value::as_str)
                    == Some("2997568c-4abb-4010-999b-7a914321067a")
            }),
            "upstream must keep CodeSeeX's stored provider item, not the client's normalized copy"
        );

        let _ = std::fs::remove_dir_all(data_dir);
    }
    #[tokio::test]
    async fn hosted_local_search_stays_on_native_and_never_defers_to_chat_compat() {
        let data_dir = temp_data_dir("local-hosted-native");
        let mut config = AppConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        config.upstream.transport = UpstreamTransport::NativeResponses;
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let input = request(
            "resp_native_local",
            true,
            json!([
                { "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } }
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
        .expect("CodeSeeX-hosted local search must stay on the native transport");
        assert_ne!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "the native transport must not reject its own hosted tool"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn codex_provider_search_declaration_stays_on_native_when_local_search_is_selected() {
        let data_dir = temp_data_dir("provider-search-native");
        let mut config = AppConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        config.upstream.transport = UpstreamTransport::NativeResponses;
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        // The real Codex client advertises provider-native search even when the
        // user selected CodeSeeX local search; that must not be a dead end.
        let input = request(
            "resp_native_provider_search",
            true,
            json!([{ "type": "web_search", "external_web_access": true }]),
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
        .expect("a provider-native search declaration must stay on the native transport");
        assert_ne!(response.status(), StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn native_hosted_tool_loop_executes_local_search_without_chat_compat() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_hosted_tool_turn))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("hosted-loop");
        let mut config = config_for_fake(data_dir.clone(), address);
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let input = request(
            "resp_native_hosted_loop",
            false,
            json!([
                { "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } }
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
        .expect("the hosted tool loop owns the native response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let native: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(native["id"], "resp_native_hosted_loop");
        assert_eq!(native["status"], "completed");

        let requests = capture.requests.lock().expect("capture lock");
        assert_eq!(
            requests.len(),
            2,
            "the hosted loop must continue upstream instead of handing the request to Chat compatibility"
        );
        assert_eq!(
            requests[1]["tools"][0]["name"],
            "web_search",
            "the second native request must keep the native tool declaration"
        );
        let continuation = requests[1]["input"]
            .as_array()
            .expect("second request input array");
        assert!(
            continuation.iter().any(|item| {
                item.get("type").and_then(Value::as_str) == Some("function_call_output")
                    && item.get("call_id").and_then(Value::as_str) == Some("call_hosted_1")
            }),
            "the second native request must carry the executed hosted tool output"
        );
        drop(requests);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn native_hosted_tool_loop_streams_the_final_turn_after_executing_search() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_sse_hosted_tool_turn))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("hosted-loop-stream");
        let mut config = config_for_fake(data_dir.clone(), address);
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let input = request(
            "resp_native_hosted_stream",
            true,
            json!([
                { "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } }
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
        .expect("the hosted tool loop owns the native response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("resp_native_hosted_stream"),
            "the streamed final turn must carry the local response id"
        );
        assert!(
            !text.contains("provider_sse_hosted_2"),
            "the provider identity must not leak past the relay"
        );

        let requests = capture.requests.lock().expect("capture lock");
        assert_eq!(requests.len(), 2);
        let continuation = requests[1]["input"]
            .as_array()
            .expect("second request input array");
        assert!(continuation.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some("call_sse_hosted_1")
        }));
        drop(requests);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn native_hosted_loop_hands_client_owned_tool_groups_back_to_codex() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_client_tool_turn))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("hosted-loop-client-group");
        let mut config = config_for_fake(data_dir.clone(), address);
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let tools = json!([
            { "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } },
            { "type": "function", "name": "shell_command", "parameters": { "type": "object" } }
        ]);
        let input = request("resp_native_hosted_client_group", false, tools);

        let response = try_native_responses(
            &state,
            &HeaderMap::new(),
            &input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("the hosted loop owns the native response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let native: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(native["id"], "resp_native_hosted_client_group");
        assert_eq!(
            native["output"][0]["call_id"], "call_native_1",
            "the Codex-owned tool call must reach the client unchanged"
        );
        assert_eq!(
            capture.requests.lock().expect("capture lock").len(),
            1,
            "a client-owned tool group must never be executed or re-dispatched by CodeSeeX"
        );
        assert_eq!(
            state.native_pending_tool_groups.pending_count(),
            1,
            "the handed-back group must stay retained for the continuation check"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn native_hosted_loop_merges_replayed_namespaces_and_hands_codex_tools_back() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_codex_app_tool_turn))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("hosted-loop-namespace-handoff");
        let mut config = config_for_fake(data_dir.clone(), address);
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let namespace = |tools: Value| {
            json!({
                "type": "namespace",
                "name": "codex_app",
                "description": "Tools provided by the Codex app.",
                "tools": tools
            })
        };
        let mut input = request(
            "resp_native_hosted_namespace_handoff",
            false,
            json!([
                { "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } },
                namespace(json!([
                    { "type": "function", "name": "read_thread", "parameters": { "type": "object" } }
                ]))
            ]),
        );
        input["input"] = json!([
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "open the thread" }]
            },
            {
                "type": "tool_search_output",
                "call_id": "call_discovered",
                "tools": [namespace(json!([
                    { "type": "function", "name": "fork_thread", "parameters": { "type": "object" } },
                    {
                        "type": "function",
                        "name": "automation_update",
                        "parameters": {
                            "oneOf": [{ "$ref": "#/$defs/__schema0" }],
                            "$defs": { "__schema0": { "type": "object", "properties": {} } }
                        }
                    }
                ]))]
            }
        ]);

        let response = try_native_responses(
            &state,
            &HeaderMap::new(),
            &input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("the hosted loop owns the native response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let native: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(native["output"][0]["name"], "read_thread");

        let requests = capture.requests.lock().expect("capture lock");
        assert_eq!(
            requests.len(),
            1,
            "the replayed namespace and the Codex-owned tool call must be handled locally"
        );
        let tools = requests[0]["tools"].as_array().expect("dispatched tools");
        let namespaces = tools
            .iter()
            .filter(|tool| {
                tool.get("type").and_then(Value::as_str) == Some("namespace")
                    && tool.get("name").and_then(Value::as_str) == Some("codex_app")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            namespaces.len(),
            1,
            "the provider rejects a repeated namespace name, so the replay must be merged"
        );
        let nested = namespaces[0]["tools"]
            .as_array()
            .expect("merged namespace tools");
        assert!(
            nested
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some("fork_thread")),
            "the nested tools of the replayed declaration must survive the merge"
        );
        let automation = nested
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some("automation_update"))
            .expect("the deferred app tool must survive the merge");
        assert_eq!(
            automation["parameters"]["type"], "object",
            "the provider rejects a declaration whose parameter schema has no object type"
        );
        assert_eq!(
            automation["parameters"]["oneOf"][0]["$ref"], "#/$defs/__schema0",
            "the union schema itself must stay intact"
        );
        drop(requests);
        assert_eq!(
            state.native_pending_tool_groups.pending_count(),
            1,
            "the Codex-owned tool group must stay retained for the continuation check"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }
    #[tokio::test]
    async fn native_hosted_loop_accepts_the_client_replay_of_a_handed_back_group() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_client_tool_turn))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("hosted-loop-client-group-replay");
        let mut config = config_for_fake(data_dir.clone(), address);
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let tools = json!([
            { "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } },
            { "type": "function", "name": "shell_command", "parameters": { "type": "object" } }
        ]);
        let input = request(
            "resp_native_hosted_client_group_replay",
            false,
            tools.clone(),
        );

        let first = try_native_responses(
            &state,
            &HeaderMap::new(),
            &input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("the hosted loop owns the native response");
        assert_eq!(first.status(), StatusCode::OK);
        let _ = axum::body::to_bytes(first.into_body(), 1024 * 1024)
            .await
            .unwrap();

        let mut continuation = request(
            "resp_native_hosted_client_group_replay_second",
            false,
            tools,
        );
        continuation["previous_response_id"] = json!("resp_native_hosted_client_group_replay");
        continuation["input"] = json!([
            input["input"][0].clone(),
            {
                "type": "function_call",
                "id": "fc_native_1",
                "call_id": "call_native_1",
                "name": "shell_command",
                "arguments": "{\"command\":\"echo native\"}",
                "status": "completed"
            },
            { "type": "function_call_output", "call_id": "call_native_1", "output": "native output" }
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
        .expect("the client replay of the handed-back group must continue upstream");
        assert_eq!(second.status(), StatusCode::OK);
        let body = axum::body::to_bytes(second.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let native: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            native["id"],
            "resp_native_hosted_client_group_replay_second"
        );

        let requests = capture.requests.lock().expect("capture lock");
        assert_eq!(
            requests.len(),
            2,
            "the continuation must be dispatched upstream exactly once"
        );
        let replayed = requests[1]["input"]
            .as_array()
            .expect("second request input array");
        assert!(
            replayed.iter().any(|item| {
                item.get("type").and_then(Value::as_str) == Some("function_call_output")
                    && item.get("call_id").and_then(Value::as_str) == Some("call_native_1")
            }),
            "the continuation must carry the client tool output Codex produced"
        );
        drop(requests);
        assert_eq!(
            state.native_pending_tool_groups.pending_count(),
            0,
            "the consumed continuation must be settled and the final turn retains nothing"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }
    #[tokio::test]
    async fn native_mixed_hosted_and_client_tool_group_fails_closed() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_mixed_tool_turn))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("hosted-loop-mixed");
        let mut config = config_for_fake(data_dir.clone(), address);
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let input = request(
            "resp_native_mixed",
            false,
            json!([
                { "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } }
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
        .expect("the native transport owns the response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            capture.requests.lock().expect("capture lock").len(),
            1,
            "a mixed tool group must never trigger a second upstream call or a transport switch"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn official_backend_without_a_search_tool_stays_on_native() {
        let data_dir = temp_data_dir("official-no-search-local-tool-fallback");
        let mut config = AppConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        config.upstream.transport = UpstreamTransport::NativeResponses;
        config.web_search_backend = WebSearchBackend::Official;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let input = request(
            "resp_native_official_no_search",
            true,
            json!([{ "type": "function", "function": { "name": "workspace_search", "parameters": { "type": "object" } } }]),
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
        .expect("native transport must stay selected for workspace tools");
        assert_ne!(response.status(), StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn official_search_with_workspace_tools_stays_on_native() {
        let data_dir = temp_data_dir("official-search-mixed-tools");
        let mut config = AppConfig {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        config.upstream.transport = UpstreamTransport::NativeResponses;
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
        .expect("provider search with workspace tools must stay on native");
        assert_ne!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            state
                .store
                .response_status("resp_native_official_search_mixed")
                .await
                .unwrap(),
            Some(codeseex_store::RequestStatus::Failed)
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
    async fn local_search_enters_chat_compat_when_explicitly_configured() {
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

    #[tokio::test]
    async fn native_hosted_loop_replays_its_executed_round_in_the_next_continuation() {
        let capture = Capture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/responses", post(fake_native_hosted_then_client_tool_turn))
            .with_state(capture.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let data_dir = temp_data_dir("hosted-loop-executed-round");
        let mut config = config_for_fake(data_dir.clone(), address);
        config.web_search_backend = WebSearchBackend::Local;
        let store = Store::open(&data_dir).await.unwrap();
        let state = ProxyState::for_test(config.clone(), store);
        let tools = json!([
            { "type": "function", "function": { "name": "web_search", "parameters": { "type": "object" } } },
            { "type": "function", "name": "shell_command", "parameters": { "type": "object" } }
        ]);
        let input = request("resp_native_hosted_executed_round", false, tools.clone());

        let first = try_native_responses(
            &state,
            &HeaderMap::new(),
            &input,
            &config,
            "deepseek-v4-flash",
            Some("deepseek-v4-flash"),
        )
        .await
        .expect("the hosted loop owns the native response");
        assert_eq!(first.status(), StatusCode::OK);
        let _ = axum::body::to_bytes(first.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            state.native_pending_tool_groups.pending_count(),
            1,
            "the executed search must not stop the handed-back group from being retained"
        );

        let mut continuation = request("resp_native_hosted_executed_round_second", false, tools);
        continuation["previous_response_id"] = json!("resp_native_hosted_executed_round");
        continuation["input"] = json!([
            input["input"][0].clone(),
            {
                "type": "function_call",
                "id": "fc_hosted_shell_1",
                "call_id": "call_hosted_shell",
                "name": "shell_command",
                "arguments": "{}",
                "status": "completed"
            },
            { "type": "function_call_output", "call_id": "call_hosted_shell", "output": "native output" }
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
        .expect("the client replay of the handed-back group must continue upstream");
        assert_eq!(second.status(), StatusCode::OK);
        let _ = axum::body::to_bytes(second.into_body(), 1024 * 1024)
            .await
            .unwrap();

        let requests = capture.requests.lock().expect("capture lock");
        assert_eq!(
            requests.len(),
            3,
            "the executed round and the handed-back group each dispatch exactly once"
        );
        let replayed = requests[2]["input"]
            .as_array()
            .expect("third request input array");
        let search_call = replayed
            .iter()
            .position(|item| {
                item.get("type").and_then(Value::as_str) == Some("function_call")
                    && item.get("call_id").and_then(Value::as_str) == Some("call_hosted_search")
            })
            .expect("the executed search call must be replayed upstream");
        let search_output = replayed
            .iter()
            .position(|item| {
                item.get("type").and_then(Value::as_str) == Some("function_call_output")
                    && item.get("call_id").and_then(Value::as_str) == Some("call_hosted_search")
            })
            .expect("the executed search output must be replayed upstream");
        let shell_call = replayed
            .iter()
            .position(|item| {
                item.get("type").and_then(Value::as_str) == Some("function_call")
                    && item.get("call_id").and_then(Value::as_str) == Some("call_hosted_shell")
            })
            .expect("the retained client call must still be replayed");
        assert!(
            search_call < search_output && search_output < shell_call,
            "the round CodeSeeX executed must stay before the group it was executed for: {replayed:?}"
        );
        drop(requests);
        assert_eq!(
            state.native_pending_tool_groups.pending_count(),
            0,
            "the consumed continuation must be settled"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn upstream_payload_restores_the_provider_reasoning_shape() {
        let client_payload = json!({
            "model": "deepseek-v4-flash",
            "input": [
                {
                    "type": "reasoning",
                    "id": "rs_presented",
                    "summary": [{ "type": "summary_text", "text": "think" }],
                    "content": null,
                    "encrypted_content": "blob"
                },
                {
                    "type": "reasoning",
                    "id": "rs_provider_summary",
                    "summary": [{ "type": "summary_text", "text": "provider summary" }],
                    "content": [{ "type": "reasoning_text", "text": "different" }]
                },
                {
                    "type": "reasoning",
                    "id": "rs_already_native",
                    "summary": [{ "type": "summary_text", "text": "same" }],
                    "content": [{ "type": "reasoning_text", "text": "same" }]
                },
                { "type": "message", "role": "user", "content": "hi" }
            ]
        });

        let upstream = native_upstream_payload(&client_payload, true);

        // A presented item goes back to the shape DeepSeek's thinking mode
        // requires on the next call.
        assert_eq!(upstream["input"][0]["summary"], json!([]));
        assert_eq!(upstream["input"][0]["content"][0]["text"], json!("think"));
        assert_eq!(
            upstream["input"][0]["content"][0]["type"],
            json!("reasoning_text")
        );
        assert_eq!(upstream["input"][0]["encrypted_content"], json!("blob"));
        // A provider-authored summary that is not a duplicate is left alone.
        assert_eq!(
            upstream["input"][1]["summary"][0]["text"],
            json!("provider summary")
        );
        // An exact duplicate of the reasoning text is dropped.
        assert_eq!(upstream["input"][2]["summary"], json!([]));
        assert_eq!(upstream["input"][2]["content"][0]["text"], json!("same"));
        assert_eq!(upstream["input"][3]["content"], json!("hi"));
        // The client-visible copy keeps the presentation.
        assert_eq!(
            client_payload["input"][0]["summary"][0]["text"],
            json!("think")
        );

        // A non-DeepSeek upstream is never rewritten.
        let untouched = native_upstream_payload(&client_payload, false);
        assert_eq!(
            untouched["input"][0]["summary"][0]["text"],
            json!("think")
        );
        assert_eq!(untouched["input"][0]["content"], Value::Null);
    }
}
