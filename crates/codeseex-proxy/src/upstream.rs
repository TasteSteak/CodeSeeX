use codeseex_core::codex_auth::read_codex_auth_api_key;
use codeseex_core::config::UpstreamConfig;
use codeseex_core::urls::chat_completions_url;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

pub async fn post_chat_completions(
    client: &reqwest::Client,
    upstream: &UpstreamConfig,
    inbound_auth: Option<&str>,
    payload: Value,
) -> Result<reqwest::Response, reqwest::Error> {
    let url = chat_completions_url(&upstream.base_url, upstream.official_v1_compat);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );

    if let Some(auth) = resolve_authorization_header(upstream, inbound_auth) {
        if let Ok(value) = HeaderValue::from_str(&auth) {
            headers.insert(AUTHORIZATION, value);
        }
    }

    client
        .post(url)
        .headers(headers)
        .json(&payload)
        .send()
        .await
}

fn resolve_authorization_header(
    upstream: &UpstreamConfig,
    inbound_auth: Option<&str>,
) -> Option<String> {
    inbound_auth
        .and_then(format_bearer_header)
        .or_else(|| upstream.api_key.as_deref().and_then(format_bearer_header))
        .or_else(|| {
            read_codex_auth_api_key()
                .as_deref()
                .and_then(format_bearer_header)
        })
}

fn format_bearer_header(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.to_ascii_lowercase().starts_with("bearer ") {
        Some(trimmed.to_owned())
    } else {
        Some(format!("Bearer {trimmed}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream_with_key(api_key: Option<&str>) -> UpstreamConfig {
        UpstreamConfig {
            base_url: "https://api.deepseek.com".to_owned(),
            official_v1_compat: true,
            api_key: api_key.map(str::to_owned),
            timeout_ms: 120_000,
        }
    }

    #[test]
    fn inbound_authorization_takes_precedence() {
        assert_eq!(
            resolve_authorization_header(
                &upstream_with_key(Some("configured-key")),
                Some("Bearer inbound-key")
            )
            .as_deref(),
            Some("Bearer inbound-key")
        );
    }

    #[test]
    fn configured_key_accepts_raw_or_bearer_form() {
        assert_eq!(
            resolve_authorization_header(&upstream_with_key(Some("configured-key")), None)
                .as_deref(),
            Some("Bearer configured-key")
        );
        assert_eq!(
            resolve_authorization_header(&upstream_with_key(Some("Bearer configured-key")), None)
                .as_deref(),
            Some("Bearer configured-key")
        );
    }
}
