use crate::app_state::ProxyState;
use crate::manager_service::{ManagerJsonResponse, ManagerRuntime};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use codeseex_core::AppConfig;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
struct EventsQuery {
    limit: Option<u32>,
    before: Option<String>,
}

pub(crate) fn router() -> Router<ProxyState> {
    let router = Router::new()
        .route("/health", get(health))
        .route("/api/status", get(api_status))
        .route("/api/usage", get(api_usage))
        .route("/api/config", get(api_config).post(save_config))
        .route("/api/languages", get(api_languages))
        .route("/api/tools", get(api_tools))
        .route("/tool-assets/{tool_id}/{file}", get(tool_asset))
        .route("/api/app-info", get(api_app_info))
        .route("/api/update-check", get(api_update_check))
        .route("/api/deepseek/balance", get(api_balance))
        .route("/api/events", get(api_events))
        .route("/api/start", post(api_start))
        .route("/api/restart", post(api_restart))
        .route("/api/stop", post(api_stop))
        .route("/api/codex-adapter", get(generate_adapter))
        .route(
            "/api/codex-adapter/generate",
            post(generate_adapter).get(generate_adapter),
        );
    #[cfg(any(debug_assertions, test))]
    let router = router.route("/api/dev/seed-usage-template", post(seed_usage_template));
    router
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

async fn api_usage(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/usage", None, None)
            .await,
    )
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
        "before": query.before
    });
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("GET", "/api/events", Some(&query), None)
            .await,
    )
}

#[cfg(any(debug_assertions, test))]
async fn seed_usage_template(State(state): State<ProxyState>) -> impl IntoResponse {
    manager_json_response(
        ManagerRuntime::from_proxy_state(&state)
            .handle_json("POST", "/api/dev/seed-usage-template", None, None)
            .await,
    )
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
