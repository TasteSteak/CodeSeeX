use crate::app_state::ProxyState;
use crate::manager_service::{ManagerJsonResponse, ManagerRuntime};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use codeseex_core::{AppConfig, UserConfig};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
struct EventsQuery {
    limit: Option<u32>,
    before: Option<String>,
    cursor: Option<String>,
    after: Option<String>,
    audience: Option<String>,
    category: Option<String>,
    level: Option<String>,
    request_id: Option<String>,
    q: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageQuery {
    limit: Option<u32>,
    cursor: Option<String>,
    since_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct UsageSessionQuery {
    id: String,
}

pub(crate) fn router() -> Router<ProxyState> {
    Router::new()
        .route("/health", get(health))
        .route("/api/status", get(api_status))
        .route("/api/usage", get(api_usage))
        .route("/api/usage/session", get(api_usage_session))
        .route("/api/models", get(api_models))
        .route("/api/app-server", post(api_app_server))
        .route(
            "/api/codex-app/inject",
            post(api_codex_app_inject).get(api_codex_app_inject_get),
        )
        .route(
            "/api/codex-app/launch",
            post(api_codex_app_launch).get(api_codex_app_launch_get),
        )
        .route(
            "/codex-model-catalog",
            get(codex_model_catalog).post(codex_model_catalog),
        )
        .route("/codeseex/renderer-inject.js", get(renderer_inject_script))
        .route("/api/config", get(api_config).post(save_config))
        .route("/api/languages", get(api_languages))
        .route("/api/tools", get(api_tools))
        .route("/tool-assets/{tool_id}/{file}", get(tool_asset))
        .route("/api/app-info", get(api_app_info))
        .route("/api/update-check", get(api_update_check))
        .route("/api/release-notes", get(api_release_notes))
        .route("/api/deepseek/balance", get(api_balance))
        .route("/api/search-sources/health", get(search_sources_health))
        .route("/api/events", get(api_events))
        .route("/api/start", post(api_start))
        .route("/api/restart", post(api_restart))
        .route("/api/stop", post(api_stop))
        .route("/api/codex-adapter", get(generate_adapter))
        .route(
            "/api/codex-adapter/generate",
            post(generate_adapter).get(generate_adapter),
        )
        .route(
            "/api/codex-adapter/runtime",
            post(verify_codex_runtime).get(verify_codex_runtime),
        )
}

pub(crate) fn ensure_catalog(config: &AppConfig) -> anyhow::Result<()> {
    crate::manager_service::ensure_catalog(config)
}

async fn health(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/health", None, None)
            .await,
    )
}

async fn api_status(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/status", None, None)
            .await,
    )
}

async fn api_usage(
    State(state): State<ProxyState>,
    Query(query): Query<UsageQuery>,
) -> impl IntoResponse {
    let query = json!({
        "limit": query.limit,
        "cursor": query.cursor,
        "since_revision": query.since_revision
    });
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/usage", Some(&query), None)
            .await,
    )
}

async fn api_usage_session(
    State(state): State<ProxyState>,
    Query(query): Query<UsageSessionQuery>,
) -> impl IntoResponse {
    let query = json!({
        "id": query.id
    });
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/usage/session", Some(&query), None)
            .await,
    )
}

async fn api_models(
    State(state): State<ProxyState>,
    Query(query): Query<Value>,
) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/models", Some(&query), None)
            .await,
    )
}

async fn api_app_server(
    State(state): State<ProxyState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("POST", "/api/app-server", None, Some(&payload))
            .await,
    )
}

async fn codex_model_catalog(State(state): State<ProxyState>) -> impl IntoResponse {
    Json(crate::codex_app::codex_model_catalog_value(
        &state.active_config(),
    ))
}

async fn renderer_inject_script(State(state): State<ProxyState>) -> impl IntoResponse {
    let catalog = crate::codex_app::codex_model_catalog_value(&state.active_config());
    let script = crate::codex_app::renderer_inject_script(&catalog);
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        script,
    )
}

async fn api_codex_app_inject_get(
    State(state): State<ProxyState>,
    Query(query): Query<Value>,
) -> impl IntoResponse {
    inject_codex_app_model_catalog(state, Some(query), None).await
}

async fn api_codex_app_inject(
    State(state): State<ProxyState>,
    Query(query): Query<Value>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    inject_codex_app_model_catalog(state, Some(query), body.map(|value| value.0)).await
}

async fn api_codex_app_launch_get(
    State(state): State<ProxyState>,
    Query(query): Query<Value>,
) -> impl IntoResponse {
    launch_codex_app_with_model_catalog(state, Some(query), None).await
}

async fn api_codex_app_launch(
    State(state): State<ProxyState>,
    Query(query): Query<Value>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    launch_codex_app_with_model_catalog(state, Some(query), body.map(|value| value.0)).await
}

async fn inject_codex_app_model_catalog(
    state: ProxyState,
    query: Option<Value>,
    body: Option<Value>,
) -> axum::response::Response {
    let debug_port = debug_port_from_values(query.as_ref(), body.as_ref())
        .unwrap_or_else(crate::codex_app::default_debug_port);
    let catalog = crate::codex_app::codex_model_catalog_value(&state.active_config());
    match crate::codex_app::inject_model_catalog(debug_port, catalog).await {
        Ok(value) => {
            let _ = state
                .store
                .record_event(
                    "info",
                    "codex_app_inject_succeeded",
                    "Codex App renderer model catalog injection succeeded.",
                    Some(&value),
                )
                .await;
            (StatusCode::OK, Json(value)).into_response()
        }
        Err(error) => {
            let response = json!({
                "ok": false,
                "error": "codex_app_inject_failed",
                "debug_port": debug_port,
                "message": error.to_string()
            });
            let _ = state
                .store
                .record_event(
                    "error",
                    "codex_app_inject_failed",
                    "Codex App renderer model catalog injection failed.",
                    Some(&response),
                )
                .await;
            (StatusCode::BAD_GATEWAY, Json(response)).into_response()
        }
    }
}

async fn launch_codex_app_with_model_catalog(
    state: ProxyState,
    query: Option<Value>,
    body: Option<Value>,
) -> axum::response::Response {
    let debug_port = debug_port_from_values(query.as_ref(), body.as_ref())
        .unwrap_or_else(crate::codex_app::default_debug_port);
    let config = state.active_config();
    let inject = codex_app_launch_injection_enabled(query.as_ref(), body.as_ref(), &config);
    let result = if inject {
        let catalog = crate::codex_app::codex_model_catalog_value(&config);
        crate::codex_app::launch_with_model_catalog_injection(debug_port, catalog).await
    } else {
        crate::codex_app::launch_app(debug_port)
    };
    match result {
        Ok(value) => {
            let injection_enabled = value
                .pointer("/injection/enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let injection_ok = value
                .pointer("/injection/ok")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let (level, event_type, message) = if injection_enabled && !injection_ok {
                (
                    "warn",
                    "codex_app_launch_injection_warning",
                    "Codex App launched, but experimental renderer model-list injection did not complete.",
                )
            } else if injection_enabled {
                (
                    "info",
                    "codex_app_launch_injection_succeeded",
                    "Codex App launched and experimental renderer model-list injection succeeded.",
                )
            } else {
                (
                    "info",
                    "codex_app_launch_succeeded",
                    "Codex App launch requested.",
                )
            };
            let _ = state
                .store
                .record_event(level, event_type, message, Some(&value))
                .await;
            (StatusCode::OK, Json(value)).into_response()
        }
        Err(error) => {
            let response = json!({
                "ok": false,
                "error": "codex_app_launch_failed",
                "debug_port": debug_port,
                "injection_enabled": inject,
                "message": error.to_string()
            });
            let _ = state
                .store
                .record_event(
                    "error",
                    "codex_app_launch_failed",
                    "Codex App launch failed.",
                    Some(&response),
                )
                .await;
            (StatusCode::BAD_GATEWAY, Json(response)).into_response()
        }
    }
}

async fn verify_codex_runtime(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/codex-adapter/runtime", None, None)
            .await,
    )
}

fn codex_app_launch_injection_enabled(
    query: Option<&Value>,
    body: Option<&Value>,
    config: &AppConfig,
) -> bool {
    bool_from_values(query, body, "inject")
        .or_else(|| bool_from_values(query, body, "model_list_injection"))
        .or_else(|| bool_from_values(query, body, "CODEX_APP_MODEL_LIST_INJECTION"))
        .unwrap_or_else(|| {
            UserConfig::read_from(&config.config_path())
                .ok()
                .and_then(|user_config| user_config.ui)
                .and_then(|ui| ui.codex_app_model_list_injection)
                .unwrap_or(true)
        })
}

fn bool_from_values(query: Option<&Value>, body: Option<&Value>, key: &str) -> Option<bool> {
    body.and_then(|value| bool_from_value(value, key))
        .or_else(|| query.and_then(|value| bool_from_value(value, key)))
}

fn bool_from_value(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(|value| {
        value.as_bool().or_else(|| {
            value.as_str().and_then(|text| {
                let normalized = text.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "1" | "true" | "yes" | "on" | "enabled" => Some(true),
                    "0" | "false" | "no" | "off" | "disabled" => Some(false),
                    _ => None,
                }
            })
        })
    })
}

fn debug_port_from_values(query: Option<&Value>, body: Option<&Value>) -> Option<u16> {
    crate::codex_app::debug_port_from_values(query, body)
}

async fn api_config(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/config", None, None)
            .await,
    )
}

async fn save_config(
    State(state): State<ProxyState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("POST", "/api/config", None, Some(&payload))
            .await,
    )
}

async fn api_languages(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/languages", None, None)
            .await,
    )
}

async fn api_tools(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/tools", None, None)
            .await,
    )
}

async fn api_app_info(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/app-info", None, None)
            .await,
    )
}

async fn api_update_check(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/update-check", None, None)
            .await,
    )
}

async fn api_release_notes(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/release-notes", None, None)
            .await,
    )
}

async fn api_balance(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/deepseek/balance", None, None)
            .await,
    )
}

async fn api_events(
    State(state): State<ProxyState>,
    Query(query): Query<EventsQuery>,
) -> impl IntoResponse {
    let query = json!({
        "limit": query.limit,
        "before": query.before,
        "cursor": query.cursor,
        "after": query.after,
        "audience": query.audience,
        "category": query.category,
        "level": query.level,
        "request_id": query.request_id,
        "q": query.q
    });
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/events", Some(&query), None)
            .await,
    )
}

async fn search_sources_health(State(state): State<ProxyState>) -> impl IntoResponse {
    let proxy_mode = state.runtime_config.snapshot().config.network_proxy;
    let diagnostic = crate::tools::web::warm_search_sources(proxy_mode).await;
    Json(diagnostic)
}

async fn compatibility_action(state: ProxyState, path: &'static str) -> axum::response::Response {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("POST", path, None, None)
            .await,
    )
}

async fn api_start(State(state): State<ProxyState>) -> impl IntoResponse {
    compatibility_action(state, "/api/start").await
}

async fn api_restart(State(state): State<ProxyState>) -> impl IntoResponse {
    compatibility_action(state, "/api/restart").await
}

async fn api_stop(State(state): State<ProxyState>) -> impl IntoResponse {
    compatibility_action(state, "/api/stop").await
}

async fn generate_adapter(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/codex-adapter", None, None)
            .await,
    )
}

async fn tool_asset(
    State(state): State<ProxyState>,
    AxumPath((tool_id, file)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let config = state.active_config();
    let Some(path) = crate::community_tools::tool_asset_path(&config.data_dir, &tool_id, &file)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match file.to_ascii_lowercase().as_str() {
        "icon.svg" => "image/svg+xml; charset=utf-8",
        "icon.png" => "image/png",
        _ => "application/octet-stream",
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        bytes,
    )
        .into_response()
}

fn manager_json_response(response: ManagerJsonResponse) -> axum::response::Response {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(response.body)).into_response()
}
