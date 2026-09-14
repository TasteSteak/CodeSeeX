use crate::text::compact_line;
use crate::tools::ownership::ChatToolCall;
use codeseex_core::context::redact_inline_data_urls;
use serde_json::{json, Value};

const MAX_TOOL_RESULT_REPLAY_CHARS: usize = 12_000;

pub(crate) struct ExecutedCodeTool {
    pub(crate) call: ChatToolCall,
    pub(crate) result: Value,
}

pub(crate) fn is_code_tool_executable(
    name: &str,
    enabled_tools: &[String],
    community_tools: &crate::community_tools::CommunityToolSet,
    local_web_search_enabled: bool,
) -> bool {
    // Tool definitions are only the first ownership boundary. An upstream may
    // still emit an undeclared tool call, so do not let the system-hosted
    // allowlist re-enable local search after the user selected the official
    // provider-owned backend.
    if crate::tools::ownership::is_web_search_tool(name) && !local_web_search_enabled {
        return false;
    }
    crate::tools::is_executable_tool_enabled(name, enabled_tools)
        || community_tools.is_executable_tool(name)
}

pub(crate) async fn execute_code_tool(
    client: &reqwest::Client,
    config: &codeseex_core::AppConfig,
    tool_context: &crate::tools::ToolExecutionContext,
    messages: &[Value],
    current_image_refs: &[String],
    community_tools: &crate::community_tools::CommunityToolSet,
    call: &ChatToolCall,
) -> Value {
    if let Some(result) = community_tools.execute(&call.name, &call.arguments).await {
        return result;
    }
    crate::tools::execute_tool_with_client(
        client,
        config,
        tool_context,
        messages,
        current_image_refs,
        &call.name,
        &call.arguments,
    )
    .await
}

pub(crate) async fn execute_code_tools_concurrently(
    client: &reqwest::Client,
    config: &codeseex_core::AppConfig,
    tool_context: &crate::tools::ToolExecutionContext,
    messages: &[Value],
    current_image_refs: &[String],
    community_tools: &crate::community_tools::CommunityToolSet,
    calls: &[ChatToolCall],
) -> Vec<ExecutedCodeTool> {
    futures_util::future::join_all(calls.iter().cloned().map(|call| async move {
        let result = execute_code_tool(
            client,
            config,
            tool_context,
            messages,
            current_image_refs,
            community_tools,
            &call,
        )
        .await;
        ExecutedCodeTool { call, result }
    }))
    .await
}

pub(crate) fn tool_fact_line(call: &ChatToolCall, result: &Value) -> String {
    format!(
        "tool={} call_id={} arguments={} ok={} result={}",
        call.name,
        call.id,
        compact_line(&redact_inline_data_urls(&call.arguments), 800),
        result
            .get("ok")
            .and_then(Value::as_bool)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
        summarize_tool_result(result)
    )
}

pub(crate) fn summarize_tool_result(result: &Value) -> String {
    let text = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_owned());
    compact_line(&redact_inline_data_urls(&text), 2400)
}

pub(crate) fn tool_result_event_detail(
    request_id: &str,
    call: &ChatToolCall,
    iteration: u32,
    result: &Value,
) -> Value {
    tool_result_event_detail_for(request_id, &call.id, &call.name, iteration, result)
}

/// Protocol-agnostic form. The native Responses transport logs tool results
/// without borrowing the Chat tool-call type, so the two API paths never share
/// a tool-call representation.
pub(crate) fn tool_result_event_detail_for(
    request_id: &str,
    call_id: &str,
    name: &str,
    iteration: u32,
    result: &Value,
) -> Value {
    let mut detail = json!({
        "id": request_id,
        "call_id": call_id,
        "name": name,
        "iteration": iteration,
        "ok": result.get("ok").and_then(Value::as_bool),
        "summary": summarize_tool_result_for_log(result),
        "result_size": crate::diagnostics::tool_result_size_summary(result)
    });
    if crate::tools::ownership::is_web_search_tool(name) {
        if let Some(object) = detail.as_object_mut() {
            object.insert(
                "web_search".to_owned(),
                json!({
                    "sources_attempted": result.get("sources_attempted").cloned().unwrap_or(Value::Null),
                    "sources_skipped": result.get("sources_skipped").cloned().unwrap_or(Value::Null),
                    "sources": compact_web_source_diagnostic_array(result.get("sources")),
                    "quality": result.get("quality").cloned().unwrap_or(Value::Null)
                }),
            );
        }
    }
    if matches!(name, "vision_analyze" | "vision_generate" | "image_gen") {
        if let Some(object) = detail.as_object_mut() {
            object.insert(
                "vision".to_owned(),
                json!({
                    "provider": result.get("provider").cloned().unwrap_or(Value::Null),
                    "backend": result.get("backend").cloned().unwrap_or(Value::Null),
                    "model": result.get("model").cloned().unwrap_or(Value::Null),
                    "transport": result.get("endpoint_kind").cloned().unwrap_or(Value::Null),
                    "image_count": result.get("image_count").cloned().unwrap_or(Value::Null),
                    "image_detail": result.get("image_detail").cloned().unwrap_or(Value::Null),
                    "duration_ms": result.get("duration_ms").cloned().unwrap_or(Value::Null),
                    "usage": result.get("usage").cloned().unwrap_or(Value::Null)
                }),
            );
        }
    }
    detail
}

pub(crate) fn model_replay_tool_result(call: &ChatToolCall, result: &Value) -> String {
    model_replay_tool_result_for(&call.name, result)
}

/// Protocol-agnostic form. The native Responses transport replays tool results
/// without borrowing the Chat tool-call type.
pub(crate) fn model_replay_tool_result_for(name: &str, result: &Value) -> String {
    if crate::tools::ownership::is_web_search_tool(name) {
        return compact_web_search_result_for_model(result).to_string();
    }
    let text = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_owned());
    let text = redact_inline_data_urls(&text);
    if text.chars().count() <= MAX_TOOL_RESULT_REPLAY_CHARS {
        return text;
    }
    json!({
        "ok": result.get("ok").cloned().unwrap_or(Value::Null),
        "tool": name,
        "codeseex_truncated_for_model": true,
        "original_chars": text.chars().count(),
        "summary": summarize_tool_result(result)
    })
    .to_string()
}

fn compact_web_search_result_for_model(result: &Value) -> Value {
    json!({
        "ok": result.get("ok").cloned().unwrap_or(Value::Null),
        "stage": result.get("stage").cloned().unwrap_or(Value::Null),
        "mode": result.get("mode").cloned().unwrap_or(Value::Null),
        "queries": result.get("queries").cloned().unwrap_or(Value::Null),
        "candidate_count": result.get("candidate_count").cloned().unwrap_or(Value::Null),
        "candidates": compact_web_candidate_array(result.get("candidates")),
        "evidence": compact_web_evidence_array(result.get("evidence")),
        "evidence_count": result.get("evidence_count").cloned().unwrap_or(Value::Null),
        "opened": compact_web_opened_array(result.get("opened")),
        "opened_count": result.get("opened_count").cloned().unwrap_or(Value::Null),
        "quality": result.get("quality").cloned().unwrap_or(Value::Null),
        "sources": compact_web_source_diagnostic_array(result.get("sources")),
        "sources_attempted": result.get("sources_attempted").cloned().unwrap_or(Value::Null),
        "sources_skipped": result.get("sources_skipped").cloned().unwrap_or(Value::Null),
        "errors": compact_web_diagnostic_array(result.get("errors")),
        "payload_guard": result.get("payload_guard").cloned().unwrap_or(Value::Null),
        "unresolved_ids": result.get("unresolved_ids").cloned().unwrap_or(Value::Null),
        "per_query": result.get("per_query").cloned().unwrap_or(Value::Null),
        "next_action": result.get("next_action").cloned().unwrap_or(Value::Null),
        "error": result.get("error").cloned().unwrap_or(Value::Null),
        "message": result.get("message").cloned().unwrap_or(Value::Null),
        "truncated": result.get("truncated").cloned().unwrap_or(Value::Null),
        "codeseex_compacted_for_model": true
    })
}

fn compact_web_source_diagnostic_array(value: Option<&Value>) -> Value {
    let Some(items) = value.and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        items
            .iter()
            .take(8)
            .map(|item| {
                json!({
                    "source": item.get("source").cloned().unwrap_or(Value::Null),
                    "ok": item.get("ok").cloned().unwrap_or(Value::Null),
                    "status": item.get("status").cloned().unwrap_or(Value::Null),
                    "error": item.get("error").cloned().unwrap_or(Value::Null),
                    "result_count": item.get("result_count").cloned().unwrap_or(Value::Null),
                    "latency_ms": item.get("latency_ms").cloned().unwrap_or(Value::Null)
                })
            })
            .collect(),
    )
}

fn compact_web_candidate_array(value: Option<&Value>) -> Value {
    let Some(items) = value.and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        items
            .iter()
            .take(8)
            .map(|item| {
                json!({
                    "id": item.get("id").cloned().unwrap_or(Value::Null),
                    "title": item.get("title").cloned().unwrap_or(Value::Null),
                    "url": item.get("url").cloned().unwrap_or(Value::Null),
                    "snippet": item.get("snippet").cloned().unwrap_or(Value::Null),
                    "source": item.get("source").cloned().unwrap_or(Value::Null),
                    "rank": item.get("rank").cloned().unwrap_or(Value::Null),
                    "score": item.get("score").cloned().unwrap_or(Value::Null),
                    "relevance": item.get("relevance").cloned().unwrap_or(Value::Null),
                    "matched_sources": item.get("matched_sources").cloned().unwrap_or(Value::Null)
                })
            })
            .collect(),
    )
}

fn compact_web_opened_array(value: Option<&Value>) -> Value {
    let Some(items) = value.and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        items
            .iter()
            .take(8)
            .map(|item| {
                json!({
                    "id": item.get("id").cloned().unwrap_or(Value::Null),
                    "title": item.get("title").cloned().unwrap_or(Value::Null),
                    "url": item.get("url").cloned().unwrap_or(Value::Null),
                    "status": item.get("status").cloned().unwrap_or(Value::Null),
                    "chars": item.get("chars").cloned().unwrap_or(Value::Null),
                    "truncated": item.get("truncated").cloned().unwrap_or(Value::Null)
                })
            })
            .collect(),
    )
}

fn compact_web_evidence_array(value: Option<&Value>) -> Value {
    let Some(items) = value.and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        items
            .iter()
            .take(3)
            .map(|item| {
                json!({
                    "id": item.get("id").cloned().unwrap_or(Value::Null),
                    "title": item.get("title").cloned().unwrap_or(Value::Null),
                    "url": item.get("url").cloned().unwrap_or(Value::Null),
                    "status": item.get("status").cloned().unwrap_or(Value::Null),
                    "origin": item.get("origin").cloned().unwrap_or(Value::Null),
                    "excerpt": item
                        .get("excerpt")
                        .and_then(Value::as_str)
                        .map(|text| Value::String(bounded_text(text, MAX_MODEL_EVIDENCE_CHARS)))
                        .unwrap_or(Value::Null),
                    "links": compact_string_array(item.get("links"), 8),
                    "chars": item.get("chars").cloned().unwrap_or(Value::Null),
                    "omitted_chars": item.get("omitted_chars").cloned().unwrap_or(Value::Null),
                    "omitted_blocks": item.get("omitted_blocks").cloned().unwrap_or(Value::Null),
                    "confidence": item.get("confidence").cloned().unwrap_or(Value::Null),
                    "truncated": item.get("truncated").cloned().unwrap_or(Value::Null)
                })
            })
            .collect(),
    )
}

fn compact_string_array(value: Option<&Value>, max_items: usize) -> Value {
    let Some(items) = value.and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        items
            .iter()
            .filter_map(Value::as_str)
            .take(max_items)
            .map(|item| Value::String(item.to_owned()))
            .collect(),
    )
}

/// Upper bound on one evidence excerpt in the model-facing replay. The web
/// pipeline already bounds the rendered excerpt to its own budget; this adds a
/// little headroom so it never cuts content the pipeline deliberately kept, and
/// still caps a future producer that forgets to bound itself.
const MAX_MODEL_EVIDENCE_CHARS: usize = crate::tools::web::EVIDENCE_BUDGET_CHARS + 256;

fn bounded_text(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    format!("{prefix}...[truncated chars={count}]")
}

fn compact_web_diagnostic_array(value: Option<&Value>) -> Value {
    let Some(items) = value.and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        items
            .iter()
            .take(8)
            .map(|item| {
                json!({
                    "stage": item.get("stage").cloned().unwrap_or(Value::Null),
                    "code": item.get("code").cloned().unwrap_or(Value::Null),
                    "url": item.get("url").cloned().unwrap_or(Value::Null),
                    "status": item.get("status").cloned().unwrap_or(Value::Null),
                    "error": item.get("error").cloned().unwrap_or(Value::Null),
                    "message": item.get("message").cloned().unwrap_or(Value::Null)
                })
            })
            .collect(),
    )
}

pub(crate) fn summarize_tool_result_for_log(result: &Value) -> String {
    let summary = semantic_tool_result_summary(result).unwrap_or_else(|| {
        let text = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_owned());
        compact_line(&redact_inline_data_urls(&text), 360)
    });
    // Log summaries are persisted for days, so apply the same credential scrub
    // the model-facing path uses; tool output must not leak secrets into logs.
    crate::tools::redact_sensitive_text(&summary).0
}

fn semantic_tool_result_summary(result: &Value) -> Option<String> {
    if result.get("stage").and_then(Value::as_str).is_some()
        && result.get("mode").and_then(Value::as_str) == Some("search")
    {
        return Some(web_search_result_summary(result));
    }
    if result.get("ok").and_then(Value::as_bool) == Some(false) {
        return Some(failed_tool_summary(result));
    }
    match result
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "vision_analyze" => {
            let text = result.get("text").and_then(Value::as_str)?;
            Some(format!("vision_analyze ok: {}", compact_line(text, 180)))
        }
        "image_gen" | "vision_generate" => {
            let count = result
                .get("image_count")
                .and_then(Value::as_u64)
                .or_else(|| {
                    result
                        .get("images")
                        .and_then(Value::as_array)
                        .map(|items| items.len() as u64)
                })
                .unwrap_or(0);
            Some(format!(
                "{} ok: {} image{}",
                result
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("image_gen"),
                count,
                if count == 1 { "" } else { "s" }
            ))
        }
        _ => generic_success_summary(result),
    }
}

fn failed_tool_summary(result: &Value) -> String {
    let tool = result.get("tool").and_then(Value::as_str).unwrap_or("tool");
    let error = result
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("failed");
    let message = result
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(" - {}", compact_line(value, 180)))
        .unwrap_or_default();
    format!("{tool} failed: {error}{message}")
}

fn generic_success_summary(result: &Value) -> Option<String> {
    if let Some(path) = result.get("path").and_then(Value::as_str) {
        if let Some(entries) = result.get("entries").and_then(Value::as_array) {
            return Some(format!(
                "list_directory ok: {path} ({} entries)",
                entries.len()
            ));
        }
        if let Some(text) = result.get("text").and_then(Value::as_str) {
            let start = result.get("start").and_then(Value::as_u64).unwrap_or(0);
            let end = result.get("end").and_then(Value::as_u64).unwrap_or(0);
            return Some(format!(
                "read_file_range ok: {path} lines {start}-{end} ({} chars)",
                text.chars().count()
            ));
        }
    }
    if let Some(query) = result.get("query").and_then(Value::as_str) {
        if let Some(matches) = result.get("matches").and_then(Value::as_array) {
            return Some(format!(
                "workspace_search ok: {} matches for {}",
                matches.len(),
                compact_line(query, 80)
            ));
        }
    }
    if result.get("stage").and_then(Value::as_str).is_some()
        && result.get("mode").and_then(Value::as_str) == Some("search")
    {
        return Some(web_search_result_summary(result));
    }
    result
        .get("summary")
        .and_then(Value::as_str)
        .map(|value| compact_line(value, 240))
}

fn web_search_result_summary(result: &Value) -> String {
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let stage = result
        .get("stage")
        .and_then(Value::as_str)
        .unwrap_or("search");
    let count = result
        .get("candidate_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let evidence_count = result
        .get("evidence_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let sources = joined_string_array(result.get("sources_attempted"));
    let skipped = joined_string_array(result.get("sources_skipped"));
    let degraded = result
        .pointer("/quality/degraded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let low_confidence = result
        .pointer("/quality/low_confidence")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    format!(
        "web_search {stage} ok={ok} candidates={count} evidence={evidence_count} sources=[{sources}] skipped=[{skipped}] degraded={degraded} low_confidence={low_confidence}"
    )
}

fn joined_string_array(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(8)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str) -> ChatToolCall {
        ChatToolCall {
            id: format!("call_{name}"),
            name: name.to_owned(),
            arguments: "{}".to_owned(),
        }
    }

    #[test]
    fn official_backend_refuses_unsolicited_local_web_search_at_execution_boundary() {
        let community = crate::community_tools::CommunityToolSet::default();
        assert!(!is_code_tool_executable(
            "web_search",
            &[],
            &community,
            false
        ));
        assert!(!is_code_tool_executable(
            "web_search_preview",
            &[],
            &community,
            false
        ));
        assert!(is_code_tool_executable("web_search", &[], &community, true));
        assert!(is_code_tool_executable(
            "workspace_search",
            &["workspace_search".to_owned()],
            &community,
            false
        ));
    }

    #[test]
    fn model_replay_web_search_uses_compact_summary() {
        let huge = "WEB_SEARCH_BODY_".repeat(2_000);
        let replay = model_replay_tool_result(
            &call("web_search"),
            &json!({
                "ok": true,
                "stage": "open",
                "mode": "open",
                "opened": [{
                    "id": "cand_weather",
                    "title": "Weather",
                    "url": "https://example.com/weather",
                    "status": 200,
                    "chars": huge.len(),
                    "truncated": false
                }],
                "evidence": [{
                    "id": "cand_weather",
                    "title": "Weather",
                    "url": "https://example.com/weather",
                    "status": 200,
                    "origin": "open",
                    "excerpt": huge,
                    "chars": huge.len()
                }]
            }),
        );

        assert!(replay.contains("cand_weather"));
        assert!(replay.contains("https://example.com/weather"));
        // One 32k excerpt must come back bounded by the evidence budget, not by
        // its original size.
        assert!(
            replay.chars().count() <= MAX_MODEL_EVIDENCE_CHARS + 1_200,
            "replay was {} chars",
            replay.chars().count()
        );
        assert!(replay.contains("truncated chars="));
    }

    #[test]
    fn model_replay_web_search_failure_keeps_source_diagnostics() {
        let replay = model_replay_tool_result(
            &call("web_search"),
            &json!({
                "ok": false,
                "stage": "search",
                "mode": "search",
                "candidate_count": 0,
                "sources_attempted": ["bing_html", "duckduckgo_lite"],
                "sources": [{
                    "source": "bing_html",
                    "ok": true,
                    "error": "filtered_low_confidence",
                    "result_count": 2,
                    "latency_ms": 120
                }]
            }),
        );
        let replay_json: Value = serde_json::from_str(&replay).expect("replay json");

        assert_eq!(replay_json["sources_attempted"][0], "bing_html");
        assert_eq!(
            replay_json["sources"][0]["error"],
            "filtered_low_confidence"
        );
        assert!(replay.chars().count() <= 2_500);
    }

    #[test]
    fn model_replay_large_non_web_tool_is_bounded() {
        let replay = model_replay_tool_result(
            &call("large_tool"),
            &json!({
                "ok": true,
                "payload": "x".repeat(MAX_TOOL_RESULT_REPLAY_CHARS + 8_000)
            }),
        );
        let replay_json: Value = serde_json::from_str(&replay).expect("replay json");

        assert_eq!(replay_json["codeseex_truncated_for_model"], true);
        assert_eq!(replay_json["tool"], "large_tool");
        assert!(replay.chars().count() <= 3_000);
    }

    #[test]
    fn model_replay_small_non_web_tool_keeps_json() {
        let replay = model_replay_tool_result(
            &call("read_file_range"),
            &json!({
                "ok": true,
                "text": "important exact content"
            }),
        );

        assert!(replay.contains("important exact content"));
        assert!(!replay.contains("codeseex_truncated_for_model"));
    }

    #[test]
    fn log_summary_for_vision_is_user_level() {
        let summary = summarize_tool_result_for_log(&json!({
            "ok": true,
            "tool": "vision_analyze",
            "prompt_sent": "Describe the dominant color in one short phrase.",
            "text": "Bright red",
            "usage": { "total_tokens": 1234 }
        }));

        assert_eq!(summary, "vision_analyze ok: Bright red");
        assert!(!summary.contains("prompt_sent"));
        assert!(!summary.contains("usage"));
    }

    #[test]
    fn log_summary_redacts_inline_image_data_in_fallback() {
        let summary = summarize_tool_result_for_log(&json!({
            "ok": true,
            "image": "data:image/png;base64,AAAASECRETBBBB"
        }));

        assert!(!summary.contains("AAAASECRETBBBB"));
        assert!(summary.contains("[inline-data-url omitted"));
    }

    #[test]
    fn log_summary_for_failure_keeps_key_error() {
        let summary = summarize_tool_result_for_log(&json!({
            "ok": false,
            "tool": "vision_analyze",
            "error": "upstream_error",
            "message": "HTTP 502 from provider"
        }));

        assert_eq!(
            summary,
            "vision_analyze failed: upstream_error - HTTP 502 from provider"
        );
    }

    #[test]
    fn log_summary_for_failure_omits_empty_message() {
        let summary = summarize_tool_result_for_log(&json!({
            "ok": false,
            "tool": "image_gen",
            "error": "upstream_error",
            "message": ""
        }));

        assert_eq!(summary, "image_gen failed: upstream_error");
    }

    #[test]
    fn log_summary_for_web_search_failure_keeps_source_diagnostics() {
        let summary = summarize_tool_result_for_log(&json!({
            "ok": false,
            "stage": "search",
            "mode": "search",
            "candidate_count": 0,
            "sources_attempted": ["bing_html"],
            "sources_skipped": ["duckduckgo_lite"],
            "quality": { "degraded": true, "low_confidence": true }
        }));

        assert_eq!(
            summary,
            "web_search search ok=false candidates=0 evidence=0 sources=[bing_html] skipped=[duckduckgo_lite] degraded=true low_confidence=true"
        );
    }

    #[test]
    fn log_summary_redacts_credentials() {
        let auth = summarize_tool_result_for_log(&json!({
            "ok": true,
            "tool": "vision_analyze",
            "text": "Authorization: Bearer sk-abcdefghijklmnop"
        }));
        assert!(!auth.contains("sk-abcdefghijklmnop"), "{auth}");
        assert!(auth.contains("[REDACTED]"), "{auth}");

        let bare = summarize_tool_result_for_log(&json!({
            "ok": true,
            "notes": "leaked sk-abcdefghijklmnop here"
        }));
        assert!(!bare.contains("sk-abcdefghijklmnop"), "{bare}");
        assert!(bare.contains("[REDACTED]"), "{bare}");
    }
}
