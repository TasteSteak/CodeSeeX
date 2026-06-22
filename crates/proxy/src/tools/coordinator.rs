use crate::diagnostics::{
    client_tool_handoff_diagnostic_event, retry_cache_diagnostic_event,
    upstream_call_usage_breakdown_event,
};
use crate::responses::usage::{merge_response_usage, response_usage_from_chat_usage};
use crate::tools::chat_protocol::{
    assistant_message_from_chat_tool_subset, chat_tool_calls,
    full_assistant_tool_message_from_chat, normalize_assistant_tool_message,
};
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
use crate::tools::response_items::{
    proxy_visible_response_items, web_search_call_output_response_item,
};
use crate::upstream::deepseek::{
    should_adapt_tool_protocol, tool_protocol::adapt_chat_tool_protocol,
};
use codeseex_core::AppConfig;
use codeseex_store::{ClientToolHandoffCall, Store};
use serde_json::{json, Value};

pub(crate) struct ToolLoopContext<'a> {
    pub(crate) client: &'a reqwest::Client,
    pub(crate) store: &'a Store,
    pub(crate) config: &'a AppConfig,
    pub(crate) auth: Option<&'a str>,
    pub(crate) local_access_token: Option<&'a str>,
    pub(crate) request_id: &'a str,
    pub(crate) enabled_tools: &'a [String],
    pub(crate) tool_context: &'a crate::tools::ToolExecutionContext,
    pub(crate) community_tools: &'a crate::community_tools::CommunityToolSet,
    pub(crate) external_tool_context: &'a crate::tool_passthrough::ToolContext,
    pub(crate) current_image_refs: &'a [String],
    pub(crate) original_request: &'a Value,
    pub(crate) context_diagnostic: &'a Value,
    pub(crate) runtime_context_storage: &'a Value,
    pub(crate) requested_model: Option<&'a str>,
    pub(crate) upstream_model: &'a str,
}

pub(crate) enum ToolLoopResult {
    FinalChat(ToolLoopResponse),
    ClientToolCalls(ToolLoopResponse),
}

pub(crate) struct ToolLoopError {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) usage: Value,
}

impl ToolLoopError {
    fn new(message: impl Into<String>, usage: &Value) -> Self {
        Self {
            code: "tool_loop_failed".to_owned(),
            message: message.into(),
            usage: usage.clone(),
        }
    }

    fn with_code(code: impl Into<String>, message: impl Into<String>, usage: &Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            usage: usage.clone(),
        }
    }
}

pub(crate) struct ToolLoopResponse {
    pub(crate) chat: Value,
    pub(crate) response_items: Vec<Value>,
    pub(crate) usage: Value,
}

async fn adapt_non_streaming_deepseek_protocol(
    context: &ToolLoopContext<'_>,
    chat: &mut Value,
    payload: &Value,
    phase: &'static str,
) {
    if !should_adapt_tool_protocol(&context.config.upstream, context.upstream_model) {
        return;
    }
    let allow_tool_calls = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| !tools.is_empty())
        .unwrap_or(false);
    let adaptation = adapt_chat_tool_protocol(chat, allow_tool_calls);
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
    let _ = context
        .store
        .record_event(
            "debug",
            event_type,
            "DeepSeek tool protocol content was handled in a non-streaming tool loop response.",
            Some(&json!({
                "id": context.request_id,
                "phase": phase,
                "adapted_tool_names": adaptation.adapted_tool_names,
                "blocked_channels": adaptation.blocked_channels,
                "parse_failed_channels": adaptation.parse_failed_channels
            })),
        )
        .await;
}

pub(crate) async fn complete_chat_with_tools(
    context: ToolLoopContext<'_>,
    mut payload: Value,
    mut chat: Value,
) -> Result<ToolLoopResult, ToolLoopError> {
    let mut completed_tool_iterations = 0_u32;
    let mut loop_diagnostics = ToolLoopDiagnostics::default();
    let mut response_items = Vec::new();
    let mut cumulative_usage = response_usage_from_chat_usage(chat.get("usage"));
    loop {
        let iteration = completed_tool_iterations;
        let tool_calls = chat_tool_calls(&chat);
        if tool_calls.is_empty() {
            return Ok(ToolLoopResult::FinalChat(ToolLoopResponse {
                chat,
                response_items,
                usage: cumulative_usage,
            }));
        }
        let partition = partition_tool_calls(
            tool_calls.clone(),
            context.community_tools,
            context.external_tool_context,
        );
        let diagnostic = loop_diagnostics.record_iteration(iteration + 1, &tool_calls, &partition);
        let _ = context
            .store
            .record_event(
                "debug",
                "tool_loop_iteration",
                "CodeSeeX tool loop iteration.",
                Some(&json!({ "id": context.request_id, "diagnostic": diagnostic })),
            )
            .await;
        if let Some(unknown) = partition.unknown.first() {
            return Err(ToolLoopError::new(
                format!(
                    "tool '{}' is not available to CodeSeeX or Codex",
                    unknown.name
                ),
                &cumulative_usage,
            ));
        }
        let proxy_executed_calls = proxy_executed_calls_in_order(&tool_calls, &partition);
        if let Some(disabled) = proxy_executed_calls.iter().find(|call| {
            !is_code_tool_executable(&call.name, context.enabled_tools, context.community_tools)
        }) {
            return Err(ToolLoopError::new(
                format!(
                    "tool '{}' is not enabled or not executable by CodeSeeX",
                    disabled.name
                ),
                &cumulative_usage,
            ));
        }
        let has_client_tools = partition.has_client_executed_calls();
        if has_client_tools && !partition.has_proxy_executed_calls() {
            if let Some(stop) = context
                .store
                .record_client_tool_handoff_calls(
                    context.request_id,
                    context.original_request,
                    &client_handoff_guard_calls(&partition),
                    Some(&cumulative_usage),
                )
                .await
                .map_err(|error| {
                    ToolLoopError::new(
                        format!("failed to evaluate client tool handoff guard: {error}"),
                        &cumulative_usage,
                    )
                })?
            {
                let _ = context
                    .store
                    .record_event(
                        "warn",
                        "client_tool_handoff_guard_diagnostic",
                        "CodeSeeX stopped repeated client tool handoffs.",
                        Some(&client_handoff_guard_diagnostic(context.request_id, &stop)),
                    )
                    .await;
                return Err(ToolLoopError::with_code(
                    stop.code,
                    stop.message,
                    &cumulative_usage,
                ));
            }
            let stored_assistant = full_assistant_tool_message_from_chat(&chat)
                .map_err(|error| ToolLoopError::new(error, &cumulative_usage))?;
            context
                .store
                .append_request_turn_messages(context.request_id, &[stored_assistant])
                .await
                .map_err(|error| {
                    ToolLoopError::new(
                        format!("failed to persist client tool turn message: {error}"),
                        &cumulative_usage,
                    )
                })?;
            let _ = context
                .store
                .record_event(
                    "info",
                    "client_tool_handoff_diagnostic",
                    "CodeSeeX client tool handoff diagnostic.",
                    Some(&client_tool_handoff_diagnostic_event(
                        context.request_id,
                        "non_streaming_tool_loop",
                        iteration,
                        context.original_request,
                        context.context_diagnostic,
                        context.runtime_context_storage,
                        Some(&partition),
                        chat.get("usage"),
                    )),
                )
                .await;
            return Ok(ToolLoopResult::ClientToolCalls(ToolLoopResponse {
                chat,
                response_items,
                usage: cumulative_usage,
            }));
        }
        if has_client_tools {
            let _ = context
                .store
                .record_event(
                    "info",
                    "mixed_tool_turn_split",
                    "Mixed CodeSeeX and native Codex tool calls were split; CodeSeeX tools will run first.",
                    Some(&json!({
                        "id": context.request_id,
                        "code_tools": partition.code.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        "hosted_tools": partition.hosted.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        "native_tools": partition.native.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        "external_tools": partition.external.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        "iteration": iteration + 1
                    })),
                )
                .await;
        }
        response_items.extend(proxy_visible_response_items(&proxy_executed_calls));
        let messages = payload
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| {
                ToolLoopError::new("chat payload messages were not an array", &cumulative_usage)
            })?;
        let stored_assistant = full_assistant_tool_message_from_chat(&chat)
            .map_err(|error| ToolLoopError::new(error, &cumulative_usage))?;
        let assistant_message = if has_client_tools {
            assistant_message_from_chat_tool_subset(&chat, &proxy_executed_calls)
        } else {
            chat.pointer("/choices/0/message")
                .cloned()
                .ok_or_else(|| {
                    ToolLoopError::new(
                        "tool call response did not include an assistant message",
                        &cumulative_usage,
                    )
                })
                .map(normalize_assistant_tool_message)?
        };
        messages.push(assistant_message);
        let message_snapshot = messages.clone();
        for call in &proxy_executed_calls {
            let _ = context
                .store
                .record_event(
                    "info",
                    "tool_call",
                    "CodeSeeX tool requested.",
                    Some(&json!({
                        "id": context.request_id,
                        "call_id": call.id,
                        "name": call.name,
                        "iteration": iteration + 1
                    })),
                )
                .await;
        }
        let executed_tools = execute_code_tools_concurrently(
            context.client,
            context.config,
            context.tool_context,
            &message_snapshot,
            context.current_image_refs,
            context.community_tools,
            &proxy_executed_calls,
        )
        .await;
        let mut tool_messages = Vec::new();
        let mut iteration_results = Vec::new();
        for executed in executed_tools {
            let call = executed.call;
            let mut result = executed.result;
            if let Some(warning) = loop_diagnostics.repeated_call_warning(&call) {
                attach_tool_loop_warning(&mut result, &warning);
                let _ = context
                    .store
                    .record_event(
                        "warn",
                        "tool_loop_repeated_call",
                        "CodeSeeX detected a repeated tool call.",
                        Some(&json!({
                            "id": context.request_id,
                            "call_id": call.id,
                            "name": call.name,
                            "iteration": iteration + 1,
                            "warning": warning
                        })),
                    )
                    .await;
            }
            let result_summary = summarize_tool_result(&result);
            let result_text = model_replay_tool_result(&call, &result);
            let fact = tool_fact_line(&call, &result);
            context
                .store
                .append_request_tool_fact(context.request_id, &fact)
                .await
                .map_err(|error| {
                    ToolLoopError::new(
                        format!("failed to persist tool fact: {error}"),
                        &cumulative_usage,
                    )
                })?;
            let _ = context
                .store
                .record_event(
                    "info",
                    "tool_result",
                    "CodeSeeX tool result returned.",
                    Some(&tool_result_event_detail(
                        context.request_id,
                        &call,
                        iteration + 1,
                        &result,
                    )),
                )
                .await;
            if is_web_search_tool(&call.name) {
                response_items.push(web_search_call_output_response_item(&call, &result_summary));
            }
            let tool_message = json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result_text
            });
            iteration_results.push((call, result));
            tool_messages.push(tool_message.clone());
            messages.push(tool_message);
        }
        let repeated_failure_stop = loop_diagnostics.record_tool_results_and_repeated_failure(
            iteration_results
                .iter()
                .map(|(call, result)| (call, result)),
        );
        let mut stored_messages = vec![stored_assistant];
        stored_messages.extend(tool_messages);
        context
            .store
            .append_request_turn_messages(context.request_id, &stored_messages)
            .await
            .map_err(|error| {
                ToolLoopError::new(
                    format!("failed to persist tool turn messages: {error}"),
                    &cumulative_usage,
                )
            })?;
        if let Some(stop) = loop_diagnostics.web_search_budget_stop() {
            let _ = context
                .store
                .record_event(
                    "warn",
                    "tool_loop_web_search_budget_stopped",
                    "CodeSeeX stopped repeated web_search calls.",
                    Some(&json!({
                        "id": context.request_id,
                        "iteration": iteration + 1,
                        "error": stop.message,
                        "recover_with_final_response": stop.recover_with_final_response
                    })),
                )
                .await;
            return recover_final_response_after_tool_loop_stop(
                context,
                payload,
                cumulative_usage,
                stop,
                completed_tool_iterations + 1,
                response_items,
            )
            .await;
        }
        if let Some(stop) = repeated_failure_stop {
            let _ = context
                .store
                .record_event(
                    "warn",
                    "tool_loop_repeated_failure_stopped",
                    "CodeSeeX stopped repeated failing tool calls.",
                    Some(&json!({
                        "id": context.request_id,
                        "iteration": iteration + 1,
                        "error": stop.message,
                        "recover_with_final_response": stop.recover_with_final_response
                    })),
                )
                .await;
            if stop.recover_with_final_response {
                return recover_final_response_after_tool_loop_stop(
                    context,
                    payload,
                    cumulative_usage,
                    stop,
                    completed_tool_iterations + 1,
                    response_items,
                )
                .await;
            }
            return Err(ToolLoopError::new(stop.message, &cumulative_usage));
        }
        if completed_tool_iterations + 1 >= MAX_TOOL_LOOP_ITERATIONS {
            let error = loop_diagnostics.iteration_limit_error();
            let _ = context
                .store
                .record_event(
                    "warn",
                    "tool_loop_iteration_limit_stopped",
                    "CodeSeeX stopped a tool loop that exceeded the iteration limit.",
                    Some(&json!({
                        "id": context.request_id,
                        "iteration": completed_tool_iterations + 1,
                        "limit": MAX_TOOL_LOOP_ITERATIONS,
                        "error": error
                    })),
                )
                .await;
            return Err(ToolLoopError::new(error, &cumulative_usage));
        }
        if has_client_tools {
            let _ = context
                .store
                .record_event(
                    "info",
                    "client_tool_handoff_diagnostic",
                    "CodeSeeX client tool handoff diagnostic.",
                    Some(&client_tool_handoff_diagnostic_event(
                        context.request_id,
                        "non_streaming_tool_loop",
                        iteration,
                        context.original_request,
                        context.context_diagnostic,
                        context.runtime_context_storage,
                        Some(&partition),
                        chat.get("usage"),
                    )),
                )
                .await;
            if let Some(stop) = context
                .store
                .record_client_tool_handoff_calls(
                    context.request_id,
                    context.original_request,
                    &client_handoff_guard_calls(&partition),
                    Some(&cumulative_usage),
                )
                .await
                .map_err(|error| {
                    ToolLoopError::new(
                        format!("failed to evaluate client tool handoff guard: {error}"),
                        &cumulative_usage,
                    )
                })?
            {
                let _ = context
                    .store
                    .record_event(
                        "warn",
                        "client_tool_handoff_guard_diagnostic",
                        "CodeSeeX stopped repeated client tool handoffs.",
                        Some(&client_handoff_guard_diagnostic(context.request_id, &stop)),
                    )
                    .await;
                return Err(ToolLoopError::with_code(
                    stop.code,
                    stop.message,
                    &cumulative_usage,
                ));
            }
            return Ok(ToolLoopResult::ClientToolCalls(ToolLoopResponse {
                chat,
                response_items,
                usage: cumulative_usage,
            }));
        }
        completed_tool_iterations += 1;
        let upstream_started = std::time::Instant::now();
        let response = match crate::upstream::post_chat_completions(
            context.client,
            &context.config.upstream,
            context.auth,
            context.local_access_token,
            Some(context.original_request),
            payload.clone(),
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                let _ = context
                    .store
                    .record_event(
                        "info",
                        "retry_cache_diagnostic",
                        "CodeSeeX retry/cache diagnostic.",
                        Some(&retry_cache_diagnostic_event(
                            context.request_id,
                            context.requested_model,
                            Some(context.upstream_model),
                            context.original_request,
                            Some(&payload),
                            "upstream_connection_failed_after_tool",
                        )),
                    )
                    .await;
                return Err(ToolLoopError::new(error.to_string(), &cumulative_usage));
            }
        };
        let status = response.status();
        if !status.is_success() {
            let _ = context
                .store
                .record_event(
                    "info",
                    "retry_cache_diagnostic",
                    "CodeSeeX retry/cache diagnostic.",
                    Some(&retry_cache_diagnostic_event(
                        context.request_id,
                        context.requested_model,
                        Some(context.upstream_model),
                        context.original_request,
                        Some(&payload),
                        "upstream_status_failed_after_tool",
                    )),
                )
                .await;
            let body = response
                .text()
                .await
                .unwrap_or_else(|error| error.to_string());
            return Err(ToolLoopError::new(
                format!("upstream returned {status} after tool execution: {body}"),
                &cumulative_usage,
            ));
        }
        chat = response
            .json::<Value>()
            .await
            .map_err(|error| ToolLoopError::new(error.to_string(), &cumulative_usage))?;
        adapt_non_streaming_deepseek_protocol(&context, &mut chat, &payload, "tool_loop").await;
        cumulative_usage = merge_response_usage(
            &cumulative_usage,
            &response_usage_from_chat_usage(chat.get("usage")),
        );
        let _ = context
            .store
            .record_event(
                "info",
                "upstream_call_usage_breakdown",
                "CodeSeeX upstream call usage breakdown.",
                Some(&upstream_call_usage_breakdown_event(
                    context.request_id,
                    "non_streaming_tool_continuation",
                    completed_tool_iterations,
                    context.original_request,
                    &payload,
                    chat.get("usage"),
                    Some(upstream_started.elapsed().as_millis() as u64),
                    false,
                )),
            )
            .await;
    }
}

fn client_handoff_guard_calls(
    partition: &crate::tools::ownership::ToolCallPartition,
) -> Vec<ClientToolHandoffCall> {
    partition
        .native
        .iter()
        .chain(partition.external.iter())
        .map(|call| ClientToolHandoffCall {
            call_id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        })
        .collect()
}

fn client_handoff_guard_diagnostic(
    request_id: &str,
    stop: &codeseex_store::ClientToolHandoffGuardStop,
) -> Value {
    let mut detail = stop.diagnostic();
    detail["id"] = json!(request_id);
    detail
}

async fn recover_final_response_after_tool_loop_stop(
    context: ToolLoopContext<'_>,
    mut payload: Value,
    cumulative_usage: Value,
    stop: ToolLoopStop,
    iteration: u32,
    response_items: Vec<Value>,
) -> Result<ToolLoopResult, ToolLoopError> {
    prepare_tool_loop_recovery_payload(&mut payload, &stop.message)
        .map_err(|message| ToolLoopError::new(message, &cumulative_usage))?;

    let upstream_started = std::time::Instant::now();
    let response = match crate::upstream::post_chat_completions(
        context.client,
        &context.config.upstream,
        context.auth,
        context.local_access_token,
        Some(context.original_request),
        payload.clone(),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = context
                .store
                .record_event(
                    "info",
                    "retry_cache_diagnostic",
                    "CodeSeeX retry/cache diagnostic.",
                    Some(&retry_cache_diagnostic_event(
                        context.request_id,
                        context.requested_model,
                        Some(context.upstream_model),
                        context.original_request,
                        Some(&payload),
                        "upstream_connection_failed_after_tool_loop_recovery",
                    )),
                )
                .await;
            return Err(ToolLoopError::new(error.to_string(), &cumulative_usage));
        }
    };
    let status = response.status();
    if !status.is_success() {
        let _ = context
            .store
            .record_event(
                "info",
                "retry_cache_diagnostic",
                "CodeSeeX retry/cache diagnostic.",
                Some(&retry_cache_diagnostic_event(
                    context.request_id,
                    context.requested_model,
                    Some(context.upstream_model),
                    context.original_request,
                    Some(&payload),
                    "upstream_status_failed_after_tool_loop_recovery",
                )),
            )
            .await;
        let body = response
            .text()
            .await
            .unwrap_or_else(|error| error.to_string());
        return Err(ToolLoopError::new(
            format!("upstream returned {status} during tool loop recovery: {body}"),
            &cumulative_usage,
        ));
    }
    let mut chat = response
        .json::<Value>()
        .await
        .map_err(|error| ToolLoopError::new(error.to_string(), &cumulative_usage))?;
    adapt_non_streaming_deepseek_protocol(&context, &mut chat, &payload, "tool_loop_recovery")
        .await;
    if !chat_tool_calls(&chat).is_empty() {
        return Err(ToolLoopError::new(
            "upstream returned tool calls during no-tool loop recovery",
            &cumulative_usage,
        ));
    }
    let usage = merge_response_usage(
        &cumulative_usage,
        &response_usage_from_chat_usage(chat.get("usage")),
    );
    let _ = context
        .store
        .record_event(
            "info",
            "upstream_call_usage_breakdown",
            "CodeSeeX upstream call usage breakdown.",
            Some(&upstream_call_usage_breakdown_event(
                context.request_id,
                "non_streaming_tool_loop_recovery",
                iteration,
                context.original_request,
                &payload,
                chat.get("usage"),
                Some(upstream_started.elapsed().as_millis() as u64),
                false,
            )),
        )
        .await;
    Ok(ToolLoopResult::FinalChat(ToolLoopResponse {
        chat,
        response_items,
        usage,
    }))
}
