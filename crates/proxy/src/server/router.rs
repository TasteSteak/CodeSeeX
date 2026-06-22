use crate::app_state::ProxyState;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use codeseex_core::AppConfig;
use tower_http::trace::TraceLayer;

use super::access::{
    authority_host_is_local, manager_local_request_guard, v1_local_request_guard, v1_options,
    ManagerAccessPolicy,
};
use super::{
    cancel_response, chat_completions, models, responses, responses_compact,
    RESPONSES_BODY_LIMIT_BYTES,
};

pub(super) fn app_router(state: ProxyState, config: &AppConfig) -> Router {
    let manager_access = ManagerAccessPolicy {
        listener_host_is_local: authority_host_is_local(&config.host),
    };
    let manager_router = crate::manager_api::router().route_layer(middleware::from_fn_with_state(
        manager_access,
        manager_local_request_guard,
    ));
    let v1_router = Router::new()
        .route("/v1/models", get(models).options(v1_options))
        .route(
            "/v1/chat/completions",
            post(chat_completions).options(v1_options),
        )
        .route(
            "/v1/responses/compact",
            post(responses_compact).options(v1_options),
        )
        .route(
            "/v1/responses/{response_id}/cancel",
            post(cancel_response).options(v1_options),
        )
        .route("/v1/responses", post(responses).options(v1_options))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            v1_local_request_guard,
        ));
    Router::new()
        .merge(manager_router)
        .merge(v1_router)
        .layer(DefaultBodyLimit::max(RESPONSES_BODY_LIMIT_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
