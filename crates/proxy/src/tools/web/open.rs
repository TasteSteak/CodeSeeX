use codeseex_core::context::redact_inline_data_urls;
use codeseex_core::NetworkProxyMode;
use futures_util::future::join_all;
use serde_json::{json, Value};

use super::candidates::{
    candidate_id_for, open_diagnostic_item, open_result_item, open_summary_item,
};
use super::extract::{
    bytes_have_binary_markers, clean_visible_text, decode_text_bytes, extract_html_title,
    extract_markdown_title, html_to_text, is_textual_content_type, markdown_to_text,
    response_looks_like_html, response_looks_like_markdown, truncate_chars,
};
use super::net::{
    no_redirect_client, read_limited_response_bytes, request_error_message, user_agent,
};
use super::safety::{
    normalize_candidate_url, url_path_looks_blocked_resource, validate_public_web_url,
    validate_web_url_network,
};
use super::{MAX_OPEN_TARGETS, MAX_TEXT_CHARS};

const MAX_REDIRECTS: usize = 5;

pub(super) async fn many(
    proxy_mode: NetworkProxyMode,
    urls: &[String],
    open_ids: &[String],
    unresolved_ids: &[String],
) -> Value {
    if urls.is_empty() {
        return json!({
            "ok": false,
            "error": if unresolved_ids.is_empty() { "missing_url" } else { "unknown_candidate_ids" },
            "message": "web_search mode=open requires url/urls/open_urls or resolvable open_ids.",
            "open_ids": open_ids,
            "unresolved_ids": unresolved_ids
        });
    }

    let web_client = no_redirect_client(proxy_mode);
    let opened = join_all(
        urls.iter()
            .take(MAX_OPEN_TARGETS)
            .map(|url| one(&web_client, url)),
    )
    .await;
    let mut opened_results = Vec::new();
    let mut opened_summaries = Vec::new();
    for item in &opened {
        if item.get("ok").and_then(Value::as_bool) == Some(true) {
            opened_results.push(open_result_item(item));
            opened_summaries.push(open_summary_item(item));
        }
    }
    let failed_results = opened
        .iter()
        .filter(|item| item.get("ok").and_then(Value::as_bool) != Some(true))
        .map(open_diagnostic_item)
        .collect::<Vec<_>>();
    let opened_diagnostics = opened.iter().map(open_diagnostic_item).collect::<Vec<_>>();
    json!({
        "_diagnostics": {
            "opened_count": opened_results.len(),
            "failure_count": failed_results.len(),
            "truncated": urls.len() > MAX_OPEN_TARGETS
        },
        "ok": !opened_results.is_empty(),
        "stage": "open",
        "mode": "open",
        "open_urls": urls,
        "open_ids": open_ids,
        "unresolved_ids": unresolved_ids,
        "results": opened_results.clone(),
        "opened_results": opened_summaries,
        "opened_count": opened_results.len(),
        "failed_results": failed_results.clone(),
        "failure_count": failed_results.len(),
        "message": if opened_results.is_empty() && !failed_results.is_empty() { "No page yielded readable text. Inspect failed_results/opened for HTTP status, content type, redirect location, bytes, and error details." } else { "" },
        "opened": opened_diagnostics,
        "truncated": urls.len() > MAX_OPEN_TARGETS
    })
}

async fn one(web_client: &reqwest::Client, raw_url: &str) -> Value {
    let normalized_url =
        normalize_candidate_url(raw_url).unwrap_or_else(|| raw_url.trim().to_owned());
    let Ok(url) = reqwest::Url::parse(&normalized_url) else {
        return json!({ "ok": false, "error": "invalid_url", "url": raw_url });
    };
    let mut current_url = url.clone();
    let mut redirects = Vec::new();
    let response = loop {
        if url_path_looks_blocked_resource(current_url.path()) {
            return json!({
                "ok": false,
                "error": "blocked_resource_type",
                "url": current_url.as_str(),
                "message": "Binary, font, image, media, archive, and PDF resources are not opened by web_search.",
                "redirects": redirects
            });
        }

        if let Err(message) = validate_public_web_url(&current_url) {
            return json!({ "ok": false, "error": "blocked_url", "url": current_url.as_str(), "message": message, "redirects": redirects });
        }
        if let Err(message) = validate_web_url_network(&current_url).await {
            return json!({ "ok": false, "error": "blocked_url", "url": current_url.as_str(), "message": message, "redirects": redirects });
        }

        let response = match web_client
            .get(current_url.clone())
            .header(reqwest::header::USER_AGENT, user_agent())
            .header(
                reqwest::header::ACCEPT,
                "text/html,text/plain,application/json,application/xml;q=0.9,*/*;q=0.5",
            )
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return json!({ "ok": false, "error": "request_failed", "url": current_url.as_str(), "message": request_error_message(&error), "redirects": redirects });
            }
        };
        let status = response.status().as_u16();
        if !(300..400).contains(&status) {
            break response;
        }
        let redirect_location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let Some(location) = redirect_location else {
            return json!({
                "ok": false,
                "error": "redirect_without_location",
                "url": current_url.as_str(),
                "status": status,
                "redirects": redirects
            });
        };
        if redirects.len() >= MAX_REDIRECTS {
            return json!({
                "ok": false,
                "error": "too_many_redirects",
                "url": current_url.as_str(),
                "status": status,
                "location": location,
                "redirects": redirects
            });
        }
        let Ok(next_url) = current_url.join(&location) else {
            return json!({
                "ok": false,
                "error": "invalid_redirect_location",
                "url": current_url.as_str(),
                "status": status,
                "location": location,
                "redirects": redirects
            });
        };
        redirects.push(json!({
            "from": current_url.as_str(),
            "status": status,
            "location": location,
            "to": next_url.as_str()
        }));
        current_url = next_url;
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !content_type.is_empty() && !is_textual_content_type(&content_type) {
        return json!({
            "ok": false,
            "error": "non_text_response",
            "url": current_url.as_str(),
            "status": status,
            "content_type": content_type,
            "redirects": redirects
        });
    }

    let (bytes, byte_truncated) = read_limited_response_bytes(response).await;
    if bytes_have_binary_markers(&bytes) {
        return json!({
            "ok": false,
            "error": "binary_response",
            "url": current_url.as_str(),
            "status": status,
            "content_type": content_type,
            "bytes": bytes.len(),
            "redirects": redirects
        });
    }
    let (raw_text, encoding, had_decode_errors) = decode_text_bytes(&bytes, &content_type);
    let is_html = response_looks_like_html(&content_type, &raw_text);
    let is_markdown = !is_html && response_looks_like_markdown(&content_type, current_url.as_str());
    let title = if is_html {
        extract_html_title(&raw_text)
    } else if is_markdown {
        extract_markdown_title(&raw_text)
    } else {
        None
    };
    let text = if is_html {
        html_to_text(&raw_text)
    } else if is_markdown {
        markdown_to_text(&raw_text)
    } else {
        clean_visible_text(&raw_text)
    };
    let full_text_chars = text.chars().count();
    let text = truncate_chars(&redact_inline_data_urls(&text), MAX_TEXT_CHARS);
    let title_text = title.unwrap_or_else(|| url.as_str().to_owned());
    let raw_text_preview = truncate_chars(
        &redact_inline_data_urls(&clean_visible_text(&raw_text)),
        500,
    );
    if text.trim().is_empty() {
        return json!({
            "_diagnostics": {
                "status": status,
                "content_type": content_type,
                "bytes": bytes.len(),
                "encoding": encoding,
                "decode_errors": had_decode_errors,
                "redirects": redirects,
                "truncated": byte_truncated || full_text_chars > MAX_TEXT_CHARS,
                "readable_text": false
            },
            "ok": false,
            "stage": "open",
            "mode": "open",
            "error": "empty_text_content",
            "message": "The URL was fetched, but no readable text was extracted. The page may be empty, script-rendered, blocked, or unsupported by text-only web_search.",
            "id": candidate_id_for(url.as_str(), &title_text, raw_url, "open"),
            "url": current_url.as_str(),
            "requested_url": raw_url,
            "status": status,
            "content_type": content_type,
            "encoding": encoding,
            "decode_errors": had_decode_errors,
            "title": title_text,
            "raw_text_preview": raw_text_preview,
            "opened": false,
            "bytes": bytes.len(),
            "redirects": redirects,
            "truncated": byte_truncated || full_text_chars > MAX_TEXT_CHARS
        });
    }
    let ok = (200..400).contains(&status);
    let mut output = json!({
        "_diagnostics": {
            "status": status,
            "content_type": content_type,
            "bytes": bytes.len(),
            "encoding": encoding,
            "decode_errors": had_decode_errors,
            "redirects": redirects,
            "truncated": byte_truncated || full_text_chars > MAX_TEXT_CHARS,
            "readable_text": true
        },
        "ok": ok,
        "stage": "open",
        "mode": "open",
        "id": candidate_id_for(current_url.as_str(), &title_text, raw_url, "open"),
        "url": current_url.as_str(),
        "requested_url": raw_url,
        "status": status,
        "content_type": content_type,
        "encoding": encoding,
        "decode_errors": had_decode_errors,
        "title": title_text,
        "snippet": truncate_chars(&text, 500),
        "content": text.clone(),
        "text": text.clone(),
        "opened": true,
        "bytes": bytes.len(),
        "redirects": redirects,
        "truncated": byte_truncated || full_text_chars > MAX_TEXT_CHARS
    });
    if !ok {
        output["error"] = Value::String("http_status".to_owned());
        output["message"] = Value::String(format!("HTTP status {status} returned readable text."));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn many_reports_invalid_urls_as_failures() {
        let result = many(
            NetworkProxyMode::System,
            &["://bad-url".to_owned()],
            &[],
            &[],
        )
        .await;

        assert_eq!(result.get("ok").and_then(Value::as_bool), Some(false));
        assert_eq!(result.get("opened_count").and_then(Value::as_u64), Some(0));
        assert_eq!(result.get("failure_count").and_then(Value::as_u64), Some(1));
        assert_eq!(
            result
                .pointer("/failed_results/0/error")
                .and_then(Value::as_str),
            Some("invalid_url")
        );
        assert!(result
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("No page yielded readable text"));
    }
}
