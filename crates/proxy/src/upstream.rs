use codeseex_core::codex_auth::read_codex_auth_api_key;
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
    resolve_authorization_header_with_direct_key(upstream, inbound_auth, payload, || {
        read_codex_auth_api_key(false)
    })
}

fn resolve_authorization_header_with_direct_key<F>(
    upstream: &UpstreamConfig,
    inbound_auth: Option<&str>,
    payload: &Value,
    direct_key: F,
) -> Option<String>
where
    F: FnOnce() -> Option<String>,
{
    let can_use_inbound = !payload_looks_like_codex_app_request(payload);
    let configured_auth = upstream.api_key.as_deref().and_then(format_bearer_header);
    let inbound_auth = inbound_auth.and_then(format_bearer_header);
    if can_use_inbound {
        if let Some(auth) = inbound_auth {
            return Some(auth);
        }
    }
    direct_key()
        .and_then(|value| format_bearer_header(&value))
        .or(configured_auth)
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
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(None),
                Some("Bearer inbound-key"),
                &serde_json::json!({
                    "client_metadata": {
                        "x-codex-installation-id": "codex-install"
                    },
                    "prompt_cache_key": "thread"
                }),
                || None
            )
            .as_deref(),
            None
        );
    }

    #[test]
    fn inbound_authorization_can_authenticate_plain_external_clients() {
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(None),
                Some("Bearer inbound-key"),
                &serde_json::json!({ "input": "private smoke" }),
                || None
            )
            .as_deref(),
            Some("Bearer inbound-key")
        );
    }

    #[test]
    fn configured_key_accepts_raw_or_bearer_form() {
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(Some("configured-key")),
                None,
                &serde_json::json!({}),
                || None
            )
            .as_deref(),
            Some("Bearer configured-key")
        );
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(Some("Bearer configured-key")),
                None,
                &serde_json::json!({}),
                || None
            )
            .as_deref(),
            Some("Bearer configured-key")
        );
    }

    #[test]
    fn direct_codex_auth_key_can_authenticate_codex_app_payloads() {
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(None),
                Some("Bearer inbound-key"),
                &serde_json::json!({
                    "client_metadata": {
                        "x-codex-installation-id": "codex-install"
                    }
                }),
                || Some("direct-key".to_owned())
            )
            .as_deref(),
            Some("Bearer direct-key")
        );
    }

    #[test]
    fn direct_codex_auth_key_precedes_configured_fallback_for_codex_app_payloads() {
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(Some("configured-key")),
                Some("Bearer inbound-key"),
                &serde_json::json!({
                    "client_metadata": {
                        "x-codex-installation-id": "codex-install"
                    }
                }),
                || Some("direct-key".to_owned())
            )
            .as_deref(),
            Some("Bearer direct-key")
        );
    }

    #[test]
    fn inbound_authorization_precedes_configured_fallback_for_plain_external_clients() {
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(Some("configured-key")),
                Some("Bearer inbound-key"),
                &serde_json::json!({ "input": "private smoke" }),
                || Some("direct-key".to_owned())
            )
            .as_deref(),
            Some("Bearer inbound-key")
        );
    }
}
