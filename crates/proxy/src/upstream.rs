use codeseex_core::codex_auth::read_codex_auth_api_key;
use codeseex_core::config::{UpstreamConfig, UpstreamCredentialSource, UpstreamTransport};
use codeseex_core::urls::{chat_completions_url, is_official_deepseek_url, responses_url};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use std::sync::{Mutex, OnceLock};
use url::Url;

pub(crate) mod deepseek;
pub(crate) mod payload;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectedUpstreamTransport {
    NativeResponses,
    ChatCompat,
}

/// The native Responses transport is the default for every upstream. Chat
/// compatibility is selected only when the user explicitly configures it; the
/// proxy never decides between transports based on endpoint identity.
pub(crate) fn select_transport(upstream: &UpstreamConfig) -> SelectedUpstreamTransport {
    match upstream.transport {
        UpstreamTransport::ChatCompat => SelectedUpstreamTransport::ChatCompat,
        UpstreamTransport::NativeResponses => SelectedUpstreamTransport::NativeResponses,
    }
}

/// Diagnostic marker describing which credential reached the upstream. It is
/// never a secret: it only says where the Authorization header came from.
pub(crate) const CREDENTIAL_SOURCE_HEADER: &str = "x-codeseex-credential-source";

/// Client identity headers a Codex relay may use to recognize its own client.
///
/// CodeSeeX is a router: it neither invents nor rewrites these values, it only
/// forwards what the client already sent. Dropping them made relays answer
/// `401 unauthorized client detected` even though the credential was valid.
#[derive(Clone, Default)]
pub(crate) struct UpstreamPassthrough {
    originator: Option<String>,
    user_agent: Option<String>,
    session_id: Option<String>,
    conversation_id: Option<String>,
}

impl UpstreamPassthrough {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Self {
        let read = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Self {
            originator: read("originator"),
            user_agent: read("user-agent"),
            session_id: read("session_id"),
            conversation_id: read("conversation_id"),
        }
    }

    fn entries(&self) -> [(&'static str, Option<&str>); 4] {
        [
            ("originator", self.originator.as_deref()),
            ("user-agent", self.user_agent.as_deref()),
            ("session_id", self.session_id.as_deref()),
            ("conversation_id", self.conversation_id.as_deref()),
        ]
    }

    /// Applies the forwarded identity headers to an outbound request. Call it
    /// after tool-specific headers so a real client value wins when present.
    pub(crate) fn apply_to(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        for (name, value) in self.entries() {
            let Some(value) = value else { continue };
            request = request.header(name, value);
        }
        request
    }
}

static CACHED_PASSTHROUGH: OnceLock<Mutex<Option<UpstreamPassthrough>>> = OnceLock::new();

/// Remembers the most recent client identity headers so tools that run outside
/// the request path (Vision) can present the same caller to the upstream relay.
pub(crate) fn remember_passthrough(passthrough: &UpstreamPassthrough) {
    if let Ok(mut slot) = CACHED_PASSTHROUGH.get_or_init(|| Mutex::new(None)).lock() {
        *slot = Some(passthrough.clone());
    }
}

/// Returns the last remembered identity headers, or an empty set before the
/// client has sent any request.
pub(crate) fn cached_passthrough() -> UpstreamPassthrough {
    CACHED_PASSTHROUGH
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_default()
}

/// Credential inputs available for one upstream request.
#[derive(Clone, Default)]
pub(crate) struct UpstreamAuthRequest<'a> {
    /// Authorization the client sent to the local proxy.
    pub inbound: Option<&'a str>,
    /// CodeSeeX's own v1 access token. It is a local-only credential and is
    /// never forwarded upstream.
    pub local_access_token: Option<&'a str>,
    /// Key the user stored for the upstream inside CodeSeeX (OS credential
    /// store).
    pub managed_key: Option<&'a str>,
    /// Client identity headers forwarded verbatim to the upstream.
    pub passthrough: UpstreamPassthrough,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedUpstreamAuth {
    pub header: Option<String>,
    pub source: &'static str,
}

impl ResolvedUpstreamAuth {
    fn new(header: Option<String>, source: &'static str) -> Self {
        match header {
            Some(header) => Self {
                header: Some(header),
                source,
            },
            None => Self {
                header: None,
                source: "none",
            },
        }
    }
}

impl std::fmt::Debug for UpstreamAuthRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpstreamAuthRequest")
            .field("inbound", &self.inbound.is_some())
            .field("local_access_token", &self.local_access_token.is_some())
            .field("managed_key", &self.managed_key.is_some())
            .finish()
    }
}

pub(crate) fn upstream_is_official(upstream: &UpstreamConfig) -> bool {
    Url::parse(&upstream.base_url)
        .ok()
        .as_ref()
        .is_some_and(is_official_deepseek_url)
}

fn codex_app_isolation_applies(upstream: &UpstreamConfig, payload: &Value) -> bool {
    upstream_is_official(upstream) && payload_looks_like_codex_app_request(payload)
}

fn inbound_credential<'a>(request: &UpstreamAuthRequest<'a>) -> Option<String> {
    request
        .inbound
        .filter(|value| {
            !authorization_matches_local_access_token(request.local_access_token, value)
        })
        .and_then(format_bearer_header)
}

fn configured_credential(
    upstream: &UpstreamConfig,
    request: &UpstreamAuthRequest<'_>,
) -> Option<String> {
    upstream
        .api_key
        .as_deref()
        .filter(|value| !local_access_token_matches(request.local_access_token, value))
        .and_then(format_bearer_header)
}

fn managed_credential(request: &UpstreamAuthRequest<'_>) -> Option<String> {
    request
        .managed_key
        .filter(|value| !local_access_token_matches(request.local_access_token, value))
        .and_then(format_bearer_header)
}

fn codex_auth_credential(
    request: &UpstreamAuthRequest<'_>,
    direct_key: &dyn Fn() -> Option<String>,
) -> Option<String> {
    direct_key()
        .filter(|value| !local_access_token_matches(request.local_access_token, value))
        .and_then(|value| format_bearer_header(&value))
}

/// Resolves the Authorization header for one upstream request.
///
/// `Auto` keeps the historical isolation for the official endpoint (a Codex
/// App shaped payload never forwards the client's own credential, because the
/// official endpoint expects the account key). Custom endpoints instead
/// forward whatever the client sent: on a relay, the key the user typed into
/// the client is the only credential that can work there, and dropping it was
/// the root cause of `401 unauthorized client` after switching upstreams.
pub(crate) fn resolve_upstream_authorization(
    upstream: &UpstreamConfig,
    request: UpstreamAuthRequest<'_>,
    payload: &Value,
    direct_key: &dyn Fn() -> Option<String>,
) -> ResolvedUpstreamAuth {
    match upstream.credential {
        UpstreamCredentialSource::Request => {
            ResolvedUpstreamAuth::new(inbound_credential(&request), "request")
        }
        UpstreamCredentialSource::Env => {
            ResolvedUpstreamAuth::new(configured_credential(upstream, &request), "env")
        }
        UpstreamCredentialSource::Secret => {
            ResolvedUpstreamAuth::new(managed_credential(&request), "secret")
        }
        UpstreamCredentialSource::CodexAuth => {
            ResolvedUpstreamAuth::new(codex_auth_credential(&request, direct_key), "codex_auth")
        }
        UpstreamCredentialSource::Auto => {
            if !codex_app_isolation_applies(upstream, payload) {
                if let Some(header) = inbound_credential(&request) {
                    return ResolvedUpstreamAuth::new(Some(header), "request");
                }
            }
            if let Some(header) = managed_credential(&request) {
                return ResolvedUpstreamAuth::new(Some(header), "secret");
            }
            if let Some(header) = configured_credential(upstream, &request) {
                return ResolvedUpstreamAuth::new(Some(header), "env");
            }
            ResolvedUpstreamAuth::new(codex_auth_credential(&request, direct_key), "codex_auth")
        }
    }
}

async fn send_upstream_request(
    client: &reqwest::Client,
    url: &str,
    upstream: &UpstreamConfig,
    request: UpstreamAuthRequest<'_>,
    auth_context_payload: Option<&Value>,
    payload: Value,
) -> reqwest::Result<reqwest::Response> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    // Forward the client's own identity headers before anything else so a
    // relay sees the same caller it would have seen without CodeSeeX in front.
    for (name, value) in request.passthrough.entries() {
        let (Some(value), Ok(name)) = (value, HeaderName::from_bytes(name.as_bytes())) else {
            continue;
        };
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    }

    let auth_payload = auth_context_payload.unwrap_or(&payload);
    let resolved = resolve_upstream_authorization(upstream, request, auth_payload, &|| {
        read_codex_auth_api_key(false)
    });
    if let Some(value) = resolved
        .header
        .as_deref()
        .and_then(|auth| HeaderValue::from_str(auth).ok())
    {
        headers.insert(AUTHORIZATION, value);
    }
    if let Ok(value) = HeaderValue::from_str(resolved.source) {
        headers.insert(CREDENTIAL_SOURCE_HEADER, value);
    }

    client
        .post(url)
        .headers(headers)
        .json(&payload)
        .send()
        .await
}

pub async fn post_chat_completions(
    client: &reqwest::Client,
    upstream: &UpstreamConfig,
    auth: UpstreamAuthRequest<'_>,
    auth_context_payload: Option<&Value>,
    payload: Value,
) -> Result<reqwest::Response, reqwest::Error> {
    let url = chat_completions_url(&upstream.base_url);
    send_upstream_request(client, &url, upstream, auth, auth_context_payload, payload).await
}

pub async fn post_responses(
    client: &reqwest::Client,
    upstream: &UpstreamConfig,
    auth: UpstreamAuthRequest<'_>,
    auth_context_payload: Option<&Value>,
    payload: Value,
) -> anyhow::Result<reqwest::Response> {
    let url = responses_url(&upstream.base_url)?;
    Ok(send_upstream_request(client, &url, upstream, auth, auth_context_payload, payload).await?)
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
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};
    use codeseex_core::models::MODEL_FLASH;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    #[derive(Clone, Default)]
    struct NativeRequestCapture {
        headers: Arc<Mutex<Option<HeaderMap>>>,
        payload: Arc<Mutex<Option<Value>>>,
    }

    async fn fake_native_responses(
        State(capture): State<NativeRequestCapture>,
        headers: HeaderMap,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        *capture.headers.lock().expect("headers mutex") = Some(headers);
        *capture.payload.lock().expect("payload mutex") = Some(payload);
        Json(serde_json::json!({ "object": "response", "status": "completed" }))
    }

    fn upstream_with_key(api_key: Option<&str>) -> UpstreamConfig {
        UpstreamConfig {
            base_url: "https://api.deepseek.com".to_owned(),
            transport: UpstreamTransport::NativeResponses,
            credential: UpstreamCredentialSource::Auto,
            api_key: api_key.map(str::to_owned),
            timeout_ms: 120_000,
        }
    }

    fn custom_upstream(api_key: Option<&str>) -> UpstreamConfig {
        UpstreamConfig {
            base_url: "https://relay.example.com/v1".to_owned(),
            ..upstream_with_key(api_key)
        }
    }

    fn resolve_for_test(
        upstream: &UpstreamConfig,
        inbound: Option<&str>,
        local_access_token: Option<&str>,
        payload: &Value,
        direct_key: impl Fn() -> Option<String>,
    ) -> Option<String> {
        resolve_upstream_authorization(
            upstream,
            UpstreamAuthRequest {
                inbound,
                local_access_token,
                managed_key: None,
                passthrough: Default::default(),
            },
            payload,
            &direct_key,
        )
        .header
    }

    fn resolve_with_managed_key(
        upstream: &UpstreamConfig,
        inbound: Option<&str>,
        managed_key: Option<&str>,
        payload: &Value,
    ) -> ResolvedUpstreamAuth {
        resolve_upstream_authorization(
            upstream,
            UpstreamAuthRequest {
                inbound,
                local_access_token: None,
                managed_key,
                passthrough: Default::default(),
            },
            payload,
            &|| None,
        )
    }

    #[test]
    fn custom_endpoint_forwards_client_authorization_for_codex_app_payloads() {
        let payload = serde_json::json!({
            "client_metadata": { "x-codex-installation-id": "codex-install" },
            "prompt_cache_key": "thread"
        });
        assert_eq!(
            resolve_for_test(
                &custom_upstream(None),
                Some("Bearer relay-key"),
                None,
                &payload,
                || None
            )
            .as_deref(),
            Some("Bearer relay-key")
        );
    }

    #[test]
    fn explicit_credential_sources_are_pinned() {
        let payload = serde_json::json!({ "input": "hello" });
        let request = UpstreamAuthRequest {
            inbound: Some("Bearer inbound-key"),
            local_access_token: None,
            managed_key: Some("managed-key"),
            passthrough: Default::default(),
        };
        let env_upstream = UpstreamConfig {
            credential: UpstreamCredentialSource::Env,
            api_key: Some("configured-key".to_owned()),
            ..custom_upstream(None)
        };
        assert_eq!(
            resolve_upstream_authorization(&env_upstream, request.clone(), &payload, &|| Some(
                "direct-key".to_owned()
            ))
            .source,
            "env"
        );
        let secret_upstream = UpstreamConfig {
            credential: UpstreamCredentialSource::Secret,
            api_key: Some("configured-key".to_owned()),
            ..custom_upstream(None)
        };
        let resolved =
            resolve_upstream_authorization(&secret_upstream, request.clone(), &payload, &|| {
                Some("direct-key".to_owned())
            });
        assert_eq!(resolved.source, "secret");
        assert_eq!(resolved.header.as_deref(), Some("Bearer managed-key"));
        let codex_auth_upstream = UpstreamConfig {
            credential: UpstreamCredentialSource::CodexAuth,
            api_key: Some("configured-key".to_owned()),
            ..custom_upstream(None)
        };
        let resolved = resolve_upstream_authorization(
            &codex_auth_upstream,
            request.clone(),
            &payload,
            &|| Some("direct-key".to_owned()),
        );
        assert_eq!(resolved.source, "codex_auth");
        assert_eq!(resolved.header.as_deref(), Some("Bearer direct-key"));
        let request_only = UpstreamConfig {
            credential: UpstreamCredentialSource::Request,
            api_key: Some("configured-key".to_owned()),
            ..custom_upstream(None)
        };
        let resolved = resolve_upstream_authorization(&request_only, request, &payload, &|| None);
        assert_eq!(resolved.source, "request");
        assert_eq!(resolved.header.as_deref(), Some("Bearer inbound-key"));
    }

    #[test]
    fn managed_secret_credential_wins_before_ambient_sources() {
        let payload = serde_json::json!({ "input": "hello" });
        let upstream = UpstreamConfig {
            api_key: Some("configured-key".to_owned()),
            ..custom_upstream(None)
        };
        let resolved = resolve_with_managed_key(&upstream, None, Some("managed-key"), &payload);
        assert_eq!(resolved.source, "secret");
        assert_eq!(resolved.header.as_deref(), Some("Bearer managed-key"));
    }

    #[test]
    fn missing_credential_reports_none_source() {
        let payload = serde_json::json!({ "input": "hello" });
        let resolved = resolve_with_managed_key(&custom_upstream(None), None, None, &payload);
        assert_eq!(resolved.source, "none");
        assert!(resolved.header.is_none());
    }

    #[test]
    fn official_endpoint_still_isolates_codex_app_credentials() {
        let payload = serde_json::json!({ "prompt_cache_key": "thread" });
        assert_eq!(
            resolve_for_test(
                &upstream_with_key(None),
                Some("Bearer relay-key"),
                None,
                &payload,
                || None
            ),
            None
        );
        assert!(!upstream_is_official(&custom_upstream(None)));
    }

    #[tokio::test]
    async fn native_post_uses_responses_path_auth_and_exact_json_payload() {
        let capture = NativeRequestCapture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/openai/v1/responses", post(fake_native_responses))
            .with_state(capture.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let upstream = UpstreamConfig {
            base_url: format!("http://{address}/openai/v1"),
            transport: UpstreamTransport::NativeResponses,
            api_key: Some("native-test-key".to_owned()),
            ..upstream_with_key(None)
        };
        let payload = serde_json::json!({
            "model": MODEL_FLASH,
            "stream": true,
            "input": [{ "type": "message", "role": "user", "content": [] }]
        });
        let response = post_responses(
            &reqwest::Client::new(),
            &upstream,
            UpstreamAuthRequest::default(),
            Some(&payload),
            payload.clone(),
        )
        .await
        .unwrap();

        assert!(response.status().is_success());
        let headers = capture
            .headers
            .lock()
            .expect("headers mutex")
            .clone()
            .expect("captured headers");
        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer native-test-key")
        );
        assert!(headers
            .get(ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/event-stream")));
        assert_eq!(
            capture.payload.lock().expect("payload mutex").as_ref(),
            Some(&payload)
        );
    }

    #[test]
    fn passthrough_keeps_only_non_empty_identity_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));
        headers.insert("user-agent", HeaderValue::from_static("codex_cli_rs/0.7.1"));
        headers.insert("session_id", HeaderValue::from_static("   "));
        let passthrough = UpstreamPassthrough::from_headers(&headers);
        let entries = passthrough.entries();
        assert_eq!(entries[0], ("originator", Some("codex_cli_rs")));
        assert_eq!(entries[1], ("user-agent", Some("codex_cli_rs/0.7.1")));
        assert_eq!(entries[2], ("session_id", None));
        assert_eq!(entries[3], ("conversation_id", None));
    }

    #[test]
    fn apply_to_forwards_non_empty_identity_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));
        headers.insert("user-agent", HeaderValue::from_static("codex_cli_rs/0.7.1"));
        headers.insert("session_id", HeaderValue::from_static("session-123"));
        headers.insert("conversation_id", HeaderValue::from_static("conv-456"));
        let passthrough = UpstreamPassthrough::from_headers(&headers);

        let request = passthrough
            .apply_to(reqwest::Client::new().post("https://example.test"))
            .build()
            .expect("request builds");

        assert_eq!(request.headers().get("originator").unwrap(), "codex_cli_rs");
        assert_eq!(
            request.headers().get("user-agent").unwrap(),
            "codex_cli_rs/0.7.1"
        );
        assert_eq!(request.headers().get("session_id").unwrap(), "session-123");
        assert_eq!(
            request.headers().get("conversation_id").unwrap(),
            "conv-456"
        );
    }

    #[test]
    fn apply_to_keeps_tool_headers_when_no_client_value() {
        let passthrough = UpstreamPassthrough::default();
        let request = passthrough
            .apply_to(
                reqwest::Client::new()
                    .post("https://example.test")
                    .header("user-agent", "CodeSeeX Vision"),
            )
            .build()
            .expect("request builds");

        assert_eq!(
            request.headers().get("user-agent").unwrap(),
            "CodeSeeX Vision"
        );
        assert!(request.headers().get("originator").is_none());
    }

    #[tokio::test]
    async fn post_forwards_client_identity_headers_verbatim() {
        let capture = NativeRequestCapture::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/openai/v1/responses", post(fake_native_responses))
            .with_state(capture.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let upstream = UpstreamConfig {
            base_url: format!("http://{address}/openai/v1"),
            transport: UpstreamTransport::NativeResponses,
            api_key: Some("native-test-key".to_owned()),
            ..upstream_with_key(None)
        };
        let mut inbound = HeaderMap::new();
        inbound.insert("originator", HeaderValue::from_static("codex_cli_rs"));
        inbound.insert("user-agent", HeaderValue::from_static("codex_cli_rs/0.7.1"));
        inbound.insert("session_id", HeaderValue::from_static("session-123"));
        let payload = serde_json::json!({
            "model": MODEL_FLASH,
            "input": [{ "type": "message", "role": "user", "content": [] }]
        });
        let response = post_responses(
            &reqwest::Client::new(),
            &upstream,
            UpstreamAuthRequest {
                passthrough: UpstreamPassthrough::from_headers(&inbound),
                ..Default::default()
            },
            Some(&payload),
            payload.clone(),
        )
        .await
        .unwrap();

        assert!(response.status().is_success());
        let headers = capture
            .headers
            .lock()
            .expect("headers mutex")
            .clone()
            .expect("captured headers");
        assert_eq!(
            headers
                .get("originator")
                .and_then(|value| value.to_str().ok()),
            Some("codex_cli_rs")
        );
        assert_eq!(
            headers
                .get("user-agent")
                .and_then(|value| value.to_str().ok()),
            Some("codex_cli_rs/0.7.1")
        );
        assert_eq!(
            headers
                .get("session_id")
                .and_then(|value| value.to_str().ok()),
            Some("session-123")
        );
        assert!(headers.get("conversation_id").is_none());
    }

    #[test]
    fn inbound_authorization_is_not_forwarded_for_codex_app_payloads() {
        assert_eq!(
            resolve_for_test(
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
            resolve_for_test(
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
            resolve_for_test(
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
            resolve_for_test(
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
            resolve_for_test(
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
            resolve_for_test(
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
            resolve_for_test(
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
            resolve_for_test(
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
            resolve_for_test(
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

    #[test]
    fn native_responses_is_the_default_transport_for_any_upstream() {
        assert_eq!(
            select_transport(&upstream_with_key(None)),
            SelectedUpstreamTransport::NativeResponses
        );
        assert_eq!(
            select_transport(&UpstreamConfig {
                base_url: "https://relay.example.com/v1".to_owned(),
                ..upstream_with_key(None)
            }),
            SelectedUpstreamTransport::NativeResponses
        );
    }

    #[test]
    fn chat_compat_is_selected_only_when_explicitly_configured() {
        assert_eq!(
            select_transport(&UpstreamConfig {
                transport: UpstreamTransport::ChatCompat,
                ..upstream_with_key(None)
            }),
            SelectedUpstreamTransport::ChatCompat
        );
        assert_eq!(
            select_transport(&UpstreamConfig {
                base_url: "http://127.0.0.1:9000/v1".to_owned(),
                transport: UpstreamTransport::ChatCompat,
                ..upstream_with_key(None)
            }),
            SelectedUpstreamTransport::ChatCompat
        );
    }
}
