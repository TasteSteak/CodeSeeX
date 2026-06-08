use crate::text::compact_line;
use crate::tools::ownership::ChatToolCall;
use codeseex_core::context::redact_inline_data_urls;
use serde_json::Value;

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

pub(crate) fn summarize_tool_result_for_log(result: &Value) -> String {
    if let Some(summary) = semantic_tool_result_summary(result) {
        return summary;
    }
    let text = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_owned());
    compact_line(&redact_inline_data_urls(&text), 360)
}

fn semantic_tool_result_summary(result: &Value) -> Option<String> {
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
    if let Some(stage) = result.get("stage").and_then(Value::as_str) {
        if result.get("mode").and_then(Value::as_str) == Some("search") {
            let count = result
                .get("candidate_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            return Some(format!("web_search {stage} ok: {count} candidates"));
        }
    }
    result
        .get("summary")
        .and_then(Value::as_str)
        .map(|value| compact_line(value, 240))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
