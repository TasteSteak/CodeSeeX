use crate::app_state::ProxyState;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

pub(crate) const CODESEEX_V1_ACCESS_TOKEN_HEADER: &str = "x-codeseex-token";

pub(super) async fn v1_local_request_guard(
    State(state): State<ProxyState>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let cors = v1_cors_context(request.headers());
    if request.method() == Method::OPTIONS && v1_preflight_is_allowed(request.headers()) {
        return v1_preflight_response(cors);
    }
    let access = ManagerAccessPolicy {
        listener_host_is_local: authority_host_is_local(&state.active_config().host),
    };
    if v1_request_is_allowed(access, request.headers(), &state.v1_access_token) {
        let mut response = next.run(request).await;
        apply_v1_cors_headers(response.headers_mut(), cors);
        return response;
    }
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "code": "v1_api_forbidden",
                "message": "CodeSeeX /v1 only accepts local requests or requests with the local access token."
            }
        })),
    )
        .into_response()
}

pub(super) async fn v1_options() -> axum::response::Response {
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Clone)]
struct V1CorsContext {
    origin: Option<HeaderValue>,
    request_headers: Option<HeaderValue>,
}

fn v1_cors_context(headers: &HeaderMap) -> V1CorsContext {
    V1CorsContext {
        origin: headers.get(header::ORIGIN).cloned(),
        request_headers: headers.get(header::ACCESS_CONTROL_REQUEST_HEADERS).cloned(),
    }
}

fn v1_preflight_is_allowed(headers: &HeaderMap) -> bool {
    if headers.get(header::ORIGIN).is_none() {
        return false;
    }
    let method_allowed = headers
        .get(header::ACCESS_CONTROL_REQUEST_METHOD)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("GET") || value.eq_ignore_ascii_case("POST"))
        .unwrap_or(false);
    if !method_allowed {
        return false;
    }
    headers
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(|header| header.trim().to_ascii_lowercase())
                .any(|header| {
                    header == CODESEEX_V1_ACCESS_TOKEN_HEADER || header == "authorization"
                })
        })
        .unwrap_or(false)
}

fn v1_preflight_response(cors: V1CorsContext) -> axum::response::Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    apply_v1_cors_headers(response.headers_mut(), cors);
    response
}

fn apply_v1_cors_headers(headers: &mut HeaderMap, cors: V1CorsContext) {
    let Some(origin) = cors.origin else {
        return;
    };
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,POST,OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        cors.request_headers.unwrap_or_else(|| {
            HeaderValue::from_static("authorization,content-type,x-codeseex-token")
        }),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    headers.append(header::VARY, HeaderValue::from_static("Origin"));
    headers.append(
        header::VARY,
        HeaderValue::from_static("Access-Control-Request-Headers"),
    );
    headers.append(
        header::VARY,
        HeaderValue::from_static("Access-Control-Request-Method"),
    );
}

pub(super) async fn manager_local_request_guard(
    State(policy): State<ManagerAccessPolicy>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    if manager_request_is_local(policy, request.headers()) {
        return next.run(request).await;
    }
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "code": "manager_api_forbidden",
                "message": "CodeSeeX manager API only accepts local requests."
            }
        })),
    )
        .into_response()
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ManagerAccessPolicy {
    pub(super) listener_host_is_local: bool,
}

fn manager_request_is_local(policy: ManagerAccessPolicy, headers: &HeaderMap) -> bool {
    if !policy.listener_host_is_local {
        return false;
    }
    if fetch_metadata_is_cross_site(headers) {
        return false;
    }
    let host_is_local = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(authority_host_is_local)
        .unwrap_or(false);
    let origin_is_local = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(origin_host_is_local)
        .unwrap_or(true);
    host_is_local && origin_is_local
}

fn v1_request_is_allowed(
    policy: ManagerAccessPolicy,
    headers: &HeaderMap,
    access_token: &str,
) -> bool {
    if v1_access_token_matches(headers, access_token) {
        return true;
    }
    manager_request_is_local(policy, headers)
}

fn v1_access_token_matches(headers: &HeaderMap, access_token: &str) -> bool {
    if access_token.trim().is_empty() {
        return false;
    }
    if headers
        .get(CODESEEX_V1_ACCESS_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| constant_time_eq(value.trim().as_bytes(), access_token.as_bytes()))
    {
        return true;
    }
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_token)
        .is_some_and(|value| constant_time_eq(value.as_bytes(), access_token.as_bytes()))
}

fn bearer_token(value: &str) -> Option<&str> {
    let text = value.trim();
    let (scheme, rest) = text.split_once(char::is_whitespace)?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| rest.trim())
        .filter(|token| !token.is_empty())
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

pub(super) fn upstream_authorization_from_headers(
    headers: &HeaderMap,
    access_token: &str,
) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .filter(|value| match bearer_token(value) {
            Some(token) => !constant_time_eq(token.as_bytes(), access_token.as_bytes()),
            None => true,
        })
        .map(str::to_owned)
}

fn fetch_metadata_is_cross_site(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
}

fn origin_host_is_local(origin: &str) -> bool {
    reqwest::Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(authority_host_is_local))
        .unwrap_or(false)
}

pub(super) fn authority_host_is_local(authority: &str) -> bool {
    let host = authority_host(authority);
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn authority_host(authority: &str) -> &str {
    let trimmed = authority.trim();
    if let Some(rest) = trimmed.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(trimmed);
    }
    if trimmed.matches(':').count() == 1 {
        return trimmed.split(':').next().unwrap_or(trimmed);
    }
    trimmed
}
