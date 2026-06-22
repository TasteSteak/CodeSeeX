use codeseex_core::codex_auth::read_codex_auth_api_key;
use codeseex_core::config::UpstreamConfig;
use codeseex_core::urls::chat_completions_url;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

pub(crate) mod deepseek;
pub(crate) mod payload;

pub async fn post_chat_completions(
    client: &reqwest::Client,
    upstream: &UpstreamConfig,
    inbound_auth: Option<&str>,
    local_access_token: Option<&str>,
    auth_context_payload: Option<&Value>,
    payload: Value,
) -> Result<reqwest::Response, reqwest::Error> {
    let url = chat_completions_url(&upstream.base_url, upstream.official_v1_compat);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );

    let auth_payload = auth_context_payload.unwrap_or(&payload);
    if let Some(auth) =
        resolve_authorization_header(upstream, inbound_auth, local_access_token, auth_payload)
    {
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
    local_access_token: Option<&str>,
    payload: &Value,
) -> Option<String> {
    resolve_authorization_header_with_direct_key(
        upstream,
        inbound_auth,
        local_access_token,
        payload,
        || read_codex_auth_api_key(false),
    )
}

fn resolve_authorization_header_with_direct_key<F>(
    upstream: &UpstreamConfig,
    inbound_auth: Option<&str>,
    local_access_token: Option<&str>,
    payload: &Value,
    direct_key: F,
) -> Option<String>
where
    F: FnOnce() -> Option<String>,
{
    let can_use_inbound = !payload_looks_like_codex_app_request(payload);
    let configured_auth = upstream
        .api_key
        .as_deref()
        .filter(|value| !local_access_token_matches(local_access_token, value))
        .and_then(format_bearer_header);
    let inbound_auth = inbound_auth
        .filter(|value| !authorization_matches_local_access_token(local_access_token, value))
        .and_then(format_bearer_header);
    if can_use_inbound {
        if let Some(auth) = inbound_auth {
            return Some(auth);
        }
    }
    configured_auth.or_else(|| {
        direct_key()
            .filter(|value| !local_access_token_matches(local_access_token, value))
            .and_then(|value| format_bearer_header(&value))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexRequestMarkers {
    pub client_metadata: bool,
    pub prompt_cache_key: bool,
    pub metadata_installation_id: bool,
}

impl CodexRequestMarkers {
    pub(crate) fn has_any(self) -> bool {
        self.client_metadata || self.prompt_cache_key || self.metadata_installation_id
    }
}

pub(crate) fn codex_request_markers(payload: &Value) -> CodexRequestMarkers {
    CodexRequestMarkers {
        client_metadata: payload.get("client_metadata").is_some(),
        prompt_cache_key: payload.get("prompt_cache_key").is_some(),
        metadata_installation_id: payload
            .pointer("/metadata/x-codex-installation-id")
            .is_some(),
    }
}

pub(crate) fn payload_looks_like_codex_app_request(payload: &Value) -> bool {
    codex_request_markers(payload).has_any()
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

fn authorization_matches_local_access_token(local_access_token: Option<&str>, value: &str) -> bool {
    let Some(token) = local_access_token else {
        return false;
    };
    let Some(auth_token) = api_key_from_authorization(value) else {
        return false;
    };
    constant_time_eq(auth_token.trim().as_bytes(), token.trim().as_bytes())
}

fn local_access_token_matches(local_access_token: Option<&str>, value: &str) -> bool {
    let Some(token) = local_access_token else {
        return false;
    };
    if constant_time_eq(value.trim().as_bytes(), token.trim().as_bytes()) {
        return true;
    }
    api_key_from_authorization(value)
        .map(|auth_token| constant_time_eq(auth_token.trim().as_bytes(), token.trim().as_bytes()))
        .unwrap_or(false)
}

fn api_key_from_authorization(value: &str) -> Option<String> {
    let normalized = format_bearer_header(value)?;
    Some(
        normalized
            .trim_start_matches(|ch: char| ch.is_ascii_whitespace())
            .strip_prefix("Bearer ")
            .or_else(|| normalized.strip_prefix("bearer "))
            .unwrap_or(&normalized)
            .trim()
            .to_owned(),
    )
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
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
                None,
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
                None,
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
                None,
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
    fn configured_key_precedes_direct_codex_auth_for_codex_app_payloads() {
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(Some("configured-key")),
                Some("Bearer inbound-key"),
                None,
                &serde_json::json!({
                    "client_metadata": {
                        "x-codex-installation-id": "codex-install"
                    }
                }),
                || Some("direct-key".to_owned())
            )
            .as_deref(),
            Some("Bearer configured-key")
        );
    }

    #[test]
    fn inbound_authorization_precedes_configured_fallback_for_plain_external_clients() {
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(Some("configured-key")),
                Some("Bearer inbound-key"),
                None,
                &serde_json::json!({ "input": "private smoke" }),
                || Some("direct-key".to_owned())
            )
            .as_deref(),
            Some("Bearer inbound-key")
        );
    }

    #[test]
    fn codex_request_markers_are_detected_from_native_fields() {
        let markers = codex_request_markers(&serde_json::json!({
            "client_metadata": {},
            "prompt_cache_key": "thread-full-context",
            "metadata": {
                "x-codex-installation-id": "codex-install"
            }
        }));

        assert!(markers.client_metadata);
        assert!(markers.prompt_cache_key);
        assert!(markers.metadata_installation_id);
        assert!(markers.has_any());
    }

    #[test]
    fn local_access_token_is_not_forwarded_as_upstream_auth() {
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(None),
                Some("Bearer csx_local_token"),
                Some("csx_local_token"),
                &serde_json::json!({ "input": "private smoke" }),
                || None
            )
            .as_deref(),
            None
        );
        assert_eq!(
            resolve_authorization_header_with_direct_key(
                &upstream_with_key(Some("Bearer csx_local_token")),
                None,
                Some("csx_local_token"),
                &serde_json::json!({ "input": "private smoke" }),
                || Some("Bearer csx_local_token".to_owned())
            )
            .as_deref(),
            None
        );
    }
}
