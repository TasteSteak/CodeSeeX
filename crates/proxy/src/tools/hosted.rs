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
) -> bool {
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
    let mut detail = json!({
        "id": request_id,
        "call_id": call.id,
        "name": call.name,
        "iteration": iteration,
        "ok": result.get("ok").and_then(Value::as_bool),
        "summary": summarize_tool_result_for_log(result),
        "result_size": crate::diagnostics::tool_result_size_summary(result)
    });
    if crate::tools::ownership::is_web_search_tool(&call.name) {
        if let Some(object) = detail.as_object_mut() {
            object.insert(
                "web_search".to_owned(),
                json!({
                    "source_plan": result.get("source_plan").cloned().unwrap_or(Value::Null),
                    "source_order": result.get("source_order").cloned().unwrap_or(Value::Null),
                    "sources_attempted": result.get("sources_attempted").cloned().unwrap_or(Value::Null),
                    "sources_deprioritized": result.get("sources_deprioritized").cloned().unwrap_or(Value::Null),
                    "source_health": compact_web_source_health_array(result.get("source_health")),
                    "source_diagnostics": compact_web_source_diagnostic_array(result.get("source_diagnostics")),
                    "fallback_errors": compact_web_source_diagnostic_array(result.get("fallback_errors")),
                    "browser_fallback": compact_browser_fallback(result.get("browser_fallback"))
                }),
            );
        }
    }
    detail
}

pub(crate) fn model_replay_tool_result(call: &ChatToolCall, result: &Value) -> String {
    if crate::tools::ownership::is_web_search_tool(&call.name) {
        return compact_web_search_result_for_model(result).to_string();
    }
    let text = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_owned());
    let text = redact_inline_data_urls(&text);
    if text.chars().count() <= MAX_TOOL_RESULT_REPLAY_CHARS {
        return text;
    }
    json!({
        "ok": result.get("ok").cloned().unwrap_or(Value::Null),
        "tool": call.name,
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
        "candidate_count": result.get("candidate_count").cloned().unwrap_or(Value::Null),
        "opened_count": result.get("opened_count").cloned().unwrap_or(Value::Null),
        "failure_count": result.get("failure_count").cloned().unwrap_or(Value::Null),
        "low_confidence": result.get("low_confidence").cloned().unwrap_or(Value::Null),
        "low_confidence_fallback": result.get("low_confidence_fallback").cloned().unwrap_or(Value::Null),
        "evidence": compact_web_evidence_array(result.get("evidence")),
        "evidence_count": result.get("evidence_count").cloned().unwrap_or(Value::Null),
        "results": compact_web_result_array(result.get("results")),
        "candidates": compact_web_result_array(result.get("candidates")),
        "low_confidence_candidates": compact_web_result_array(result.get("low_confidence_candidates")),
        "opened_results": compact_web_result_array(result.get("opened_results")),
        "auto_opened": result.get("auto_opened").cloned().unwrap_or(Value::Null),
        "auto_open_targets": result.get("auto_open_targets").cloned().unwrap_or(Value::Null),
        "auto_opened_count": result.get("auto_opened_count").cloned().unwrap_or(Value::Null),
        "auto_open_failed_count": result.get("auto_open_failed_count").cloned().unwrap_or(Value::Null),
        "failed_results": compact_web_diagnostic_array(result.get("failed_results")),
        "auto_open_failed_results": compact_web_diagnostic_array(result.get("auto_open_failed_results")),
        "source_plan": result.get("source_plan").cloned().unwrap_or(Value::Null),
        "source_order": result.get("source_order").cloned().unwrap_or(Value::Null),
        "sources_attempted": result.get("sources_attempted").cloned().unwrap_or(Value::Null),
        "sources_deprioritized": result.get("sources_deprioritized").cloned().unwrap_or(Value::Null),
        "source_health": compact_web_source_health_array(result.get("source_health")),
        "source_diagnostics": compact_web_source_diagnostic_array(result.get("source_diagnostics")),
        "fallback_errors": compact_web_source_diagnostic_array(result.get("fallback_errors")),
        "error": result.get("error").cloned().unwrap_or(Value::Null),
        "message": result.get("message").cloned().unwrap_or(Value::Null),
        "open_ids": result.get("open_ids").cloned().unwrap_or(Value::Null),
        "unresolved_ids": result.get("unresolved_ids").cloned().unwrap_or(Value::Null),
        "truncated": result.get("truncated").cloned().unwrap_or(Value::Null),
        "codeseex_compacted_for_model": true
    })
}

fn compact_web_source_health_array(value: Option<&Value>) -> Value {
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
                    "reachable": item.get("reachable").cloned().unwrap_or(Value::Null),
                    "latency_ms": item.get("latency_ms").cloned().unwrap_or(Value::Null),
                    "status": item.get("status").cloned().unwrap_or(Value::Null),
                    "error": item.get("error").cloned().unwrap_or(Value::Null),
                    "age_ms": item.get("age_ms").cloned().unwrap_or(Value::Null)
                })
            })
            .collect(),
    )
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
                    "usable_result_count": item.get("usable_result_count").cloned().unwrap_or(Value::Null),
                    "max_score": item.get("max_score").cloned().unwrap_or(Value::Null)
                })
            })
            .collect(),
    )
}

fn compact_web_result_array(value: Option<&Value>) -> Value {
    let Some(items) = value.and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };
    Value::Array(items.iter().take(8).map(compact_web_result_item).collect())
}

fn compact_web_evidence_array(value: Option<&Value>) -> Value {
    let Some(items) = value.and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        items
            .iter()
            .take(4)
            .map(|item| {
                json!({
                    "id": item.get("id").cloned().unwrap_or(Value::Null),
                    "title": item.get("title").cloned().unwrap_or(Value::Null),
                    "url": item.get("url").cloned().unwrap_or(Value::Null),
                    "status": item.get("status").cloned().unwrap_or(Value::Null),
                    "snippet": item.get("snippet").cloned().unwrap_or(Value::Null),
                    "content_excerpt": item.get("content_excerpt").cloned().unwrap_or(Value::Null),
                    "content_chars": item.get("content_chars").cloned().unwrap_or(Value::Null),
                    "source": item.get("source").cloned().unwrap_or(Value::Null),
                    "opened": item.get("opened").cloned().unwrap_or(Value::Null),
                    "truncated": item.get("truncated").cloned().unwrap_or(Value::Null)
                })
            })
            .collect(),
    )
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
                    "url": item.get("url").cloned().unwrap_or(Value::Null),
                    "status": item.get("status").cloned().unwrap_or(Value::Null),
                    "error": item.get("error").cloned().unwrap_or(Value::Null),
                    "message": item.get("message").cloned().unwrap_or(Value::Null)
                })
            })
            .collect(),
    )
}

fn compact_browser_fallback(value: Option<&Value>) -> Value {
    let Some(item) = value else {
        return Value::Null;
    };
    json!({
        "ok": item.get("ok").cloned().unwrap_or(Value::Null),
        "renderer": item.get("renderer").cloned().unwrap_or(Value::Null),
        "error": item.get("error").cloned().unwrap_or(Value::Null),
        "message": item.get("message").cloned().unwrap_or(Value::Null),
        "browser": item.get("browser").cloned().unwrap_or(Value::Null),
        "diagnostics": item.get("_diagnostics").map(compact_browser_diagnostics).unwrap_or(Value::Null)
    })
}

fn compact_browser_diagnostics(item: &Value) -> Value {
    json!({
        "renderer": item.get("renderer").cloned().unwrap_or(Value::Null),
        "browser": item.get("browser").cloned().unwrap_or(Value::Null),
        "profile_isolated": item.get("profile_isolated").cloned().unwrap_or(Value::Null),
        "shared_cookies": item.get("shared_cookies").cloned().unwrap_or(Value::Null),
        "extensions_disabled": item.get("extensions_disabled").cloned().unwrap_or(Value::Null),
        "headless": item.get("headless").cloned().unwrap_or(Value::Null),
        "exit_status": item.get("exit_status").cloned().unwrap_or(Value::Null),
        "stdout_bytes": item.get("stdout_bytes").cloned().unwrap_or(Value::Null),
        "stdout_truncated": item.get("stdout_truncated").cloned().unwrap_or(Value::Null),
        "stderr_bytes": item.get("stderr_bytes").cloned().unwrap_or(Value::Null),
        "stderr_truncated": item.get("stderr_truncated").cloned().unwrap_or(Value::Null),
        "readable_text": item.get("readable_text").cloned().unwrap_or(Value::Null)
    })
}

fn compact_web_result_item(item: &Value) -> Value {
    let content_chars = item
        .get("content")
        .or_else(|| item.get("text"))
        .and_then(Value::as_str)
        .map(|text| text.chars().count())
        .unwrap_or(0);
    json!({
        "id": item.get("id").cloned().unwrap_or(Value::Null),
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "url": item.get("url").cloned().unwrap_or(Value::Null),
        "snippet": item.get("snippet").cloned().unwrap_or(Value::Null),
        "source": item.get("source").cloned().unwrap_or(Value::Null),
        "status": item.get("status").cloned().unwrap_or(Value::Null),
        "opened": item.get("opened").cloned().unwrap_or(Value::Null),
        "content_chars": content_chars
    })
}

pub(crate) fn summarize_tool_result_for_log(result: &Value) -> String {
    if let Some(summary) = semantic_tool_result_summary(result) {
        return summary;
    }
    let text = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_owned());
    compact_line(&redact_inline_data_urls(&text), 360)
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
    let deprioritized = joined_string_array(result.get("sources_deprioritized"));
    let fallback_errors = result
        .get("fallback_errors")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let source_diagnostics = result
        .get("source_diagnostics")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    format!(
        "web_search {stage} ok={ok} candidates={count} evidence={evidence_count} sources=[{sources}] deprioritized=[{deprioritized}] fallback_errors={fallback_errors} source_diagnostics={source_diagnostics}"
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
    fn model_replay_web_search_uses_compact_summary() {
        let huge = "WEB_SEARCH_BODY_".repeat(2_000);
        let replay = model_replay_tool_result(
            &call("web_search"),
            &json!({
                "ok": true,
                "stage": "open",
                "mode": "open",
                "results": [{
                    "id": "cand_weather",
                    "title": "Weather",
                    "url": "https://example.com/weather",
                    "snippet": "Rain later",
                    "content": huge
                }]
            }),
        );

        assert!(replay.contains("cand_weather"));
        assert!(replay.contains("https://example.com/weather"));
        assert!(replay.chars().count() <= 2_500);
        assert!(!replay.contains("WEB_SEARCH_BODY_WEB_SEARCH_BODY"));
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
                "source_diagnostics": [{
                    "source": "bing_html",
                    "ok": true,
                    "error": "filtered_low_confidence",
                    "result_count": 2,
                    "usable_result_count": 0,
                    "max_score": 0.08
                }],
                "fallback_errors": [{
                    "source": "duckduckgo_lite",
                    "error": "request_failed",
                    "message": "timeout"
                }]
            }),
        );
        let replay_json: Value = serde_json::from_str(&replay).expect("replay json");

        assert_eq!(replay_json["sources_attempted"][0], "bing_html");
        assert_eq!(
            replay_json["source_diagnostics"][0]["error"],
            "filtered_low_confidence"
        );
        assert_eq!(
            replay_json["fallback_errors"][0]["source"],
            "duckduckgo_lite"
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
            "sources_deprioritized": ["duckduckgo_lite"],
            "fallback_errors": [{ "source": "bing_html", "error": "empty_results" }]
        }));

        assert_eq!(
            summary,
            "web_search search ok=false candidates=0 evidence=0 sources=[bing_html] deprioritized=[duckduckgo_lite] fallback_errors=1 source_diagnostics=0"
        );
    }
}
