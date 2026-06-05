use codeseex_core::config::UpstreamConfig;
use codeseex_core::urls::chat_completions_url;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

pub(crate) mod payload;

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

    if let Some(auth) = resolve_authorization_header(upstream, inbound_auth, &payload) {
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
    payload: &Value,
) -> Option<String> {
    upstream
        .api_key
        .as_deref()
        .and_then(format_bearer_header)
        .or_else(|| {
            (!payload_looks_like_codex_app_request(payload))
                .then(|| inbound_auth.and_then(format_bearer_header))
                .flatten()
        })
}

fn payload_looks_like_codex_app_request(payload: &Value) -> bool {
    payload.get("client_metadata").is_some()
        || payload.get("prompt_cache_key").is_some()
        || payload
            .pointer("/metadata/x-codex-installation-id")
            .is_some()
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
    fn inbound_authorization_is_not_forwarded_for_codex_app_payloads() {
        assert_eq!(
            resolve_authorization_header(
                &upstream_with_key(None),
                Some("Bearer inbound-key"),
                &serde_json::json!({
                    "client_metadata": {
                        "x-codex-installation-id": "codex-install"
                    },
                    "prompt_cache_key": "thread"
                })
            )
            .as_deref(),
            None
        );
    }

    #[test]
    fn inbound_authorization_can_authenticate_plain_external_clients() {
        assert_eq!(
            resolve_authorization_header(
                &upstream_with_key(None),
                Some("Bearer inbound-key"),
                &serde_json::json!({ "input": "private smoke" })
            )
            .as_deref(),
            Some("Bearer inbound-key")
        );
    }

    #[test]
    fn configured_key_accepts_raw_or_bearer_form() {
        assert_eq!(
            resolve_authorization_header(
                &upstream_with_key(Some("configured-key")),
                None,
                &serde_json::json!({})
            )
            .as_deref(),
            Some("Bearer configured-key")
        );
        assert_eq!(
            resolve_authorization_header(
                &upstream_with_key(Some("Bearer configured-key")),
                None,
                &serde_json::json!({})
            )
            .as_deref(),
            Some("Bearer configured-key")
        );
    }
}
