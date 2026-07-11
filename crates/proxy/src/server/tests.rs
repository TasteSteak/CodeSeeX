use super::streaming_state::unregister_streaming_response;
use super::*;
use crate::responses::context::{
    response_history_messages, response_output_tool_call_messages,
    response_output_tool_call_messages_with_config,
};
use crate::tool_passthrough::ToolContext;
use crate::tools::chat_protocol::assistant_message_from_chat_tool_subset;
use crate::tools::ownership::ChatToolCall;
use codeseex_core::config::UpstreamConfig;
use codeseex_core::models::MODEL_FLASH;
use codeseex_core::NetworkProxyMode;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct FakeUpstreamState {
    requests: Arc<Mutex<Vec<Value>>>,
    active_requests: Arc<AtomicUsize>,
    max_concurrent_requests: Arc<AtomicUsize>,
}

fn test_config(data_dir: PathBuf) -> AppConfig {
    AppConfig {
        data_dir,
        ..Default::default()
    }
}

fn test_config_with_upstream(data_dir: PathBuf, fake_addr: SocketAddr) -> AppConfig {
    AppConfig {
        data_dir,
        upstream: UpstreamConfig {
            base_url: format!("http://{fake_addr}"),
            official_v1_compat: false,
            api_key: Some("test-key".to_owned()),
            timeout_ms: 30_000,
        },
        ..Default::default()
    }
}

fn temp_workspace(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("codeseex-{label}-{}", Uuid::new_v4().simple()))
}

fn read_v1_access_token(data_dir: &std::path::Path) -> String {
    std::fs::read_to_string(data_dir.join("secrets").join("v1_access.token"))
        .expect("read generated v1 access token")
        .trim()
        .to_owned()
}

fn write_vision_analyze_config(config: &AppConfig, endpoint: &str) {
    std::fs::create_dir_all(&config.data_dir).expect("create test data dir");
    let mut settings = BTreeMap::new();
    settings.insert("VISION_ANALYZE_URL".to_owned(), endpoint.to_owned());
    settings.insert(
        "VISION_ANALYZE_MODEL".to_owned(),
        "vision-test-model".to_owned(),
    );
    settings.insert("VISION_API_KEY".to_owned(), "vision-test-key".to_owned());
    codeseex_core::UserConfig {
        tools: Some(codeseex_core::UserToolsConfig {
            enabled: Some(vec!["vision_analyze".to_owned()]),
            settings: Some(settings),
            ..Default::default()
        }),
        ..Default::default()
    }
    .write_atomic(&config.config_path())
    .expect("write vision config");
}

fn write_enabled_tools_config(config: &AppConfig, enabled: Vec<String>) {
    std::fs::create_dir_all(&config.data_dir).expect("create test data dir");
    codeseex_core::UserConfig {
        tools: Some(codeseex_core::UserToolsConfig {
            enabled: Some(enabled),
            ..Default::default()
        }),
        ..Default::default()
    }
    .write_atomic(&config.config_path())
    .expect("write tools config");
}

fn upstream_function_tool_names(request: &Value) -> Vec<&str> {
    request
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect()
}

fn assert_default_codeseex_tools_present(names: &[&str]) {
    for name in [
        "apply_patch",
        "web_search",
        "list_directory",
        "read_file_range",
        "workspace_search",
        "vision_analyze",
        "image_gen",
    ] {
        assert!(names.contains(&name), "missing {name}: {names:?}");
    }
}

#[test]
fn streaming_cancellation_registry_marks_active_response() {
    let response_id = format!("resp_cancel_registry_{}", Uuid::new_v4().simple());
    unregister_streaming_response(&response_id);
    let cancelled = register_streaming_response(&response_id);

    assert!(!streaming_response_cancelled(&cancelled));
    assert!(cancel_streaming_response(&response_id));
    assert!(streaming_response_cancelled(&cancelled));

    unregister_streaming_response(&response_id);
    assert!(!cancel_streaming_response(&response_id));
}

#[tokio::test]
async fn cancel_response_marks_active_request_interrupted() {
    let data_dir = temp_workspace("cancel-response");
    let config = test_config(data_dir.clone());
    let store = Store::open(&config.data_dir).await.unwrap();
    let response_id = format!("resp_cancel_endpoint_{}", Uuid::new_v4().simple());
    store
        .checkpoint_request(&response_id, None, Some("deepseek-v4-pro"), &json!({}))
        .await
        .unwrap();
    let cancelled = register_streaming_response(&response_id);
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses/{response_id}/cancel", post(cancel_response))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!(
            "http://{proxy_addr}/v1/responses/{response_id}/cancel"
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["status"], "cancelled");
    assert!(streaming_response_cancelled(&cancelled));
    assert_eq!(
        store.response_status(&response_id).await.unwrap(),
        Some(RequestStatus::Interrupted)
    );

    unregister_streaming_response(&response_id);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn serve_with_shutdown_exits_after_listening() {
    let data_dir = temp_workspace("serve-shutdown");
    let mut config = test_config(data_dir.clone());
    config.port = 0;
    let database_path = config.legacy_database_path();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let result = serve_with_shutdown(
        config,
        async move {
            let _ = shutdown_rx.await;
        },
        move || {
            let _ = started_tx.send(());
            let _ = shutdown_tx.send(());
        },
    )
    .await;

    assert!(started_rx.await.is_ok(), "listening callback should run");
    assert!(result.is_ok(), "server should stop cleanly: {result:?}");
    assert!(
        !database_path.exists(),
        "CodeSeeX should not create codeseex.db for runtime state"
    );
    let store = Store::open(&data_dir)
        .await
        .expect("open store after shutdown");
    let (events, _) = store
        .recent_visible_events(10, None)
        .await
        .expect("recent events");
    let event_types = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect::<Vec<_>>();
    assert!(event_types.contains(&"proxy_started"), "{event_types:?}");
    assert!(event_types.contains(&"proxy_stopped"), "{event_types:?}");
    let _ = std::fs::remove_dir_all(data_dir);
}

async fn spawn_test_app(config: AppConfig) -> (PathBuf, SocketAddr) {
    let data_dir = config.data_dir.clone();
    let store = Store::open(&config.data_dir).await.unwrap();
    let state = ProxyState::for_test(config.clone(), store);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = app_router(state, &config);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (data_dir, addr)
}

#[tokio::test]
async fn manager_api_rejects_when_configured_listener_host_is_not_loopback() {
    let mut config = test_config(temp_workspace("manager-non-loopback-bind"));
    config.host = "0.0.0.0".to_owned();
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/status"))
        .header(header::HOST, "localhost")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn v1_routes_reject_non_loopback_listener_without_token() {
    let mut config = test_config(temp_workspace("v1-non-loopback-bind"));
    config.host = "0.0.0.0".to_owned();
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .header(header::ORIGIN, "https://example-client.test")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn manager_api_rejects_non_local_origin() {
    let config = test_config(temp_workspace("manager-origin-rejects"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/status"))
        .header(header::ORIGIN, "https://evil.example")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["error"]["code"], "manager_api_forbidden");
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn manager_api_rejects_null_origin() {
    let config = test_config(temp_workspace("manager-origin-null"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/status"))
        .header(header::ORIGIN, "null")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn manager_api_rejects_non_local_host() {
    let config = test_config(temp_workspace("manager-host-rejects"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/status"))
        .header(header::HOST, "evil.example")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn manager_api_rejects_cross_site_fetch_metadata_without_origin() {
    let config = test_config(temp_workspace("manager-fetch-site-rejects"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .post(format!("http://{addr}/api/restart"))
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["error"]["code"], "manager_api_forbidden");
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn tool_assets_reject_non_local_origin() {
    let config = test_config(temp_workspace("manager-tool-assets-origin"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/tool-assets/test/icon.svg"))
        .header(header::ORIGIN, "https://evil.example")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn manager_api_allows_local_requests_without_origin() {
    let config = test_config(temp_workspace("manager-origin-none"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/status"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn v1_routes_reject_cross_site_browser_requests_without_token() {
    let config = test_config(temp_workspace("v1-cors"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .header(header::ORIGIN, "https://example-client.test")
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn v1_routes_allow_requests_with_local_access_token() {
    let mut config = test_config(temp_workspace("v1-token-allowed"));
    config.host = "0.0.0.0".to_owned();
    let (data_dir, addr) = spawn_test_app(config).await;
    let token = read_v1_access_token(&data_dir);

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .header(CODESEEX_V1_ACCESS_TOKEN_HEADER, token)
        .header(header::ORIGIN, "https://example-client.test")
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some("https://example-client.test")
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn v1_routes_allow_token_preflight_without_exposing_upstream_key() {
    let mut config = test_config(temp_workspace("v1-token-preflight"));
    config.host = "0.0.0.0".to_owned();
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .request(Method::OPTIONS, format!("http://{addr}/v1/responses"))
        .header(header::ORIGIN, "https://example-client.test")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            "content-type, x-codeseex-token",
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some("https://example-client.test")
    );
    assert!(response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("x-codeseex-token"));
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn v1_routes_reject_preflight_without_capability_header() {
    let config = test_config(temp_workspace("v1-preflight-reject"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .request(Method::OPTIONS, format!("http://{addr}/v1/responses"))
        .header(header::ORIGIN, "https://example-client.test")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn api_models_returns_app_server_model_list_shape() {
    let config = test_config(temp_workspace("api-models"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/models?limit=1"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(
        body.pointer("/data/0/id").and_then(Value::as_str),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        body.pointer("/data/0/model").and_then(Value::as_str),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        body.pointer("/data/0/defaultReasoningEffort")
            .and_then(Value::as_str),
        Some("medium")
    );
    assert_eq!(
        body.pointer("/data/0/supportedReasoningEfforts/0/reasoningEffort")
            .and_then(Value::as_str),
        Some("low")
    );
    assert_eq!(body.get("nextCursor").and_then(Value::as_str), Some("1"));
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_model_catalog_route_returns_injected_model_choices() {
    let config = test_config(temp_workspace("codex-model-catalog"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/codex-model-catalog"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body.get("status").and_then(Value::as_str), Some("ok"));
    assert_eq!(
        body.get("default_model").and_then(Value::as_str),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        body.get("models").and_then(Value::as_array).map(Vec::len),
        Some(2)
    );
    assert_eq!(
        body.pointer("/appServer/data/0/model")
            .and_then(Value::as_str),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        body.pointer("/appServer/data/0/displayName")
            .and_then(Value::as_str),
        Some("DeepSeek V4 Flash")
    );
    assert_eq!(
        body.pointer("/appServer/data/0/shortDisplayName")
            .and_then(Value::as_str),
        Some("Flash")
    );
    assert_eq!(
        body.pointer("/appServer/data/1/model")
            .and_then(Value::as_str),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        body.pointer("/appServer/data/1/displayName")
            .and_then(Value::as_str),
        Some("DeepSeek V4 Pro")
    );
    assert_eq!(
        body.pointer("/appServer/data/1/shortDisplayName")
            .and_then(Value::as_str),
        Some("Pro")
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn renderer_inject_script_route_embeds_codex_app_model_patch() {
    let config = test_config(temp_workspace("renderer-inject-script"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/codeseex/renderer-inject.js"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    let body = response.text().await.unwrap();
    assert!(body.contains("deepseek-v4-pro"), "{body}");
    assert!(body.contains("list-models-for-host"), "{body}");
    assert!(body.contains("model-queries-"), "{body}");
    assert!(body.contains("use-host-config:request-bridge"), "{body}");
    assert!(body.contains("module && module.Vt"), "{body}");
    assert!(body.contains("shortenSelectedModelButtonLabels"), "{body}");
    assert!(body.contains("message.method"), "{body}");
    assert!(body.contains("modelListResultLooksPatchable"), "{body}");
    assert!(
        body.contains("const initialDiagnostic = await refresh(\"initial\");"),
        "{body}"
    );
    assert!(body.contains("return initialDiagnostic;"), "{body}");
    assert!(body.contains("appServerPatch"), "{body}");
    assert!(body.contains("patchStatsigDynamicConfig"), "{body}");
    assert!(body.contains("available_models"), "{body}");
    assert!(body.contains("107580212"), "{body}");
    assert!(body.contains("dynamicConfigPatch"), "{body}");
    assert!(!body.contains("Response.prototype.json"), "{body}");
    assert!(!body.contains("window.dispatchEvent ="), "{body}");
    assert!(!body.contains("MODEL_DYNAMIC_CONFIG_NAMES"), "{body}");
    assert!(!body.contains("patchReactModelState"), "{body}");
    assert!(!body.contains("patchObjectGraph"), "{body}");
    assert!(!body.contains("Object.values(module)"), "{body}");
    assert!(!body.contains("app-server-manager-signals-"), "{body}");
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_app_inject_reports_cdp_connection_failure() {
    let config = test_config(temp_workspace("codex-app-inject-failure"));
    let (data_dir, addr) = spawn_test_app(config).await;
    let closed_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let closed_port = closed_listener.local_addr().unwrap().port();
    drop(closed_listener);

    let response = reqwest::Client::new()
        .post(format!("http://{addr}/api/codex-app/inject"))
        .json(&json!({ "debugPort": closed_port }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some("codex_app_inject_failed")
    );
    assert_eq!(
        body.get("debug_port").and_then(Value::as_u64),
        Some(closed_port as u64)
    );

    let events = reqwest::Client::new()
        .get(format!(
            "http://{addr}/api/events?level=error&q=codex_app_inject_failed"
        ))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        events.pointer("/events/0/type").and_then(Value::as_str),
        Some("codex_app_inject_failed")
    );
    assert_eq!(
        events
            .pointer("/events/0/safe_detail/debug_port")
            .and_then(Value::as_u64),
        Some(closed_port as u64)
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn app_server_rpc_dispatches_model_list() {
    let config = test_config(temp_workspace("api-app-server-model-list"));
    let (data_dir, addr) = spawn_test_app(config).await;

    let response = reqwest::Client::new()
        .post(format!("http://{addr}/api/app-server"))
        .json(&json!({
            "id": 6,
            "method": "model/list",
            "params": {
                "limit": 20,
                "includeHidden": false
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body.get("id").and_then(Value::as_u64), Some(6));
    assert!(body.get("error").is_none());
    assert_eq!(
        body.pointer("/result/data/1/id").and_then(Value::as_str),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        body.pointer("/result/data/1/inputModalities/1")
            .and_then(Value::as_str),
        Some("image")
    );
    assert_eq!(body.pointer("/result/nextCursor"), Some(&Value::Null));
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn web_search_source_probe_runs_after_proxy_startup_event() {
    let data_dir = temp_workspace("search-probe-startup");
    let config = test_config(data_dir.clone());
    let store = Store::open(&config.data_dir).await.unwrap();
    let runtime_config = crate::runtime_config::RuntimeConfigService::new(config.clone());
    let changes = runtime_config.subscribe();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_warm = calls.clone();
    let handle = spawn_web_search_source_probe_subscriber(
        runtime_config.clone(),
        changes,
        store.clone(),
        std::time::Duration::from_millis(20),
        Arc::new(move |_proxy_mode| {
            let calls = calls_for_warm.clone();
            Box::pin(async move {
                calls.fetch_add(1, AtomicOrdering::SeqCst);
                json!({
                    "stage": "search_source_probe",
                    "source_order": ["bing_html"],
                    "source_health": []
                })
            })
        }),
    );

    runtime_config.emit_proxy_startup();

    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    let (events, _) = store.recent_events(20, None).await.unwrap();
    let probe = events
        .iter()
        .find(|event| event.event_type == "web_search_source_probe")
        .expect("probe event should be recorded");
    assert_eq!(
        probe
            .detail
            .as_ref()
            .and_then(|detail| detail.get("trigger"))
            .and_then(Value::as_str),
        Some("proxy_startup")
    );
    handle.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn web_search_source_probe_ignores_non_network_config_changes() {
    let data_dir = temp_workspace("search-probe-ignore-model");
    let config = test_config(data_dir.clone());
    let store = Store::open(&config.data_dir).await.unwrap();
    let runtime_config = crate::runtime_config::RuntimeConfigService::new(config.clone());
    let changes = runtime_config.subscribe();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_warm = calls.clone();
    let handle = spawn_web_search_source_probe_subscriber(
        runtime_config.clone(),
        changes,
        store.clone(),
        std::time::Duration::from_millis(20),
        Arc::new(move |_proxy_mode| {
            let calls = calls_for_warm.clone();
            Box::pin(async move {
                calls.fetch_add(1, AtomicOrdering::SeqCst);
                json!({ "ok": true, "test": true })
            })
        }),
    );

    std::fs::create_dir_all(&config.data_dir).unwrap();
    codeseex_core::UserConfig {
        model: Some(codeseex_core::UserModelConfig {
            thinking: Some("disabled".to_owned()),
            ..Default::default()
        }),
        ..Default::default()
    }
    .write_atomic(&config.config_path())
    .unwrap();
    let change = runtime_config
        .refresh(crate::runtime_config::RuntimeConfigChangeSource::ManagerSave)
        .expect("model config change");
    assert!(change.has_kind(crate::runtime_config::RuntimeConfigChangeKind::Model));

    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 0);
    let (events, _) = store.recent_events(20, None).await.unwrap();
    assert!(!events
        .iter()
        .any(|event| event.event_type == "web_search_source_probe"));
    handle.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn web_search_source_probe_runs_after_network_proxy_change() {
    let data_dir = temp_workspace("search-probe-network");
    let config = test_config(data_dir.clone());
    std::fs::create_dir_all(&config.data_dir).unwrap();
    let store = Store::open(&config.data_dir).await.unwrap();
    let runtime_config = crate::runtime_config::RuntimeConfigService::new(config.clone());
    let changes = runtime_config.subscribe();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_warm = calls.clone();
    let handle = spawn_web_search_source_probe_subscriber(
        runtime_config.clone(),
        changes,
        store.clone(),
        std::time::Duration::from_millis(20),
        Arc::new(move |proxy_mode| {
            let calls = calls_for_warm.clone();
            Box::pin(async move {
                calls.fetch_add(1, AtomicOrdering::SeqCst);
                json!({
                    "ok": true,
                    "test": true,
                    "proxy_mode": format!("{proxy_mode:?}")
                })
            })
        }),
    );

    codeseex_core::UserConfig {
        network: Some(codeseex_core::UserNetworkConfig {
            proxy: Some(NetworkProxyMode::None),
        }),
        ..Default::default()
    }
    .write_atomic(&config.config_path())
    .unwrap();
    let change = runtime_config
        .refresh(crate::runtime_config::RuntimeConfigChangeSource::ManagerSave)
        .expect("network proxy change");
    assert!(change.has_kind(crate::runtime_config::RuntimeConfigChangeKind::NetworkProxy));

    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    let (events, _) = store.recent_events(20, None).await.unwrap();
    let probe = events
        .iter()
        .find(|event| event.event_type == "web_search_source_probe")
        .expect("probe event should be recorded");
    assert_eq!(
        probe
            .detail
            .as_ref()
            .and_then(|detail| detail.get("trigger"))
            .and_then(Value::as_str),
        Some("manager_save")
    );
    handle.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn web_search_source_probe_survives_lagged_config_events() {
    let data_dir = temp_workspace("search-probe-lagged-events");
    let config = test_config(data_dir.clone());
    let store = Store::open(&config.data_dir).await.unwrap();
    let runtime_config = crate::runtime_config::RuntimeConfigService::new(config.clone());
    let changes = runtime_config.subscribe();
    for _ in 0..80 {
        runtime_config.emit_proxy_startup();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_warm = calls.clone();
    let handle = spawn_web_search_source_probe_subscriber(
        runtime_config.clone(),
        changes,
        store.clone(),
        std::time::Duration::from_millis(20),
        Arc::new(move |_proxy_mode| {
            let calls = calls_for_warm.clone();
            Box::pin(async move {
                calls.fetch_add(1, AtomicOrdering::SeqCst);
                json!({
                    "stage": "search_source_probe",
                    "source_order": ["bing_html"],
                    "source_health": []
                })
            })
        }),
    );

    runtime_config.emit_proxy_startup();

    tokio::time::sleep(std::time::Duration::from_millis(160)).await;
    assert!(
        calls.load(AtomicOrdering::SeqCst) >= 1,
        "lagged receiver should continue handling later probe events"
    );
    let (events, _) = store.recent_events(20, None).await.unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == "web_search_source_probe_lagged"));
    assert!(events
        .iter()
        .any(|event| event.event_type == "web_search_source_probe"));
    handle.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

async fn fake_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    let body = if request_index == 1 {
        concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"need directory\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_ls\",\"type\":\"function\",\"function\":{\"name\":\"list_directory\",\"arguments\":\"{\\\"path\\\":\\\".\\\"}\"}}]}}]}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
    } else {
        concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"after tool\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"directory checked\"}}],\"usage\":{\"prompt_tokens\":10,\"prompt_cache_hit_tokens\":4,\"completion_tokens\":2,\"total_tokens\":12}}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_streaming_usage_chunks_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"usage checked\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":100,\"prompt_cache_hit_tokens\":70,\"completion_tokens\":3,\"total_tokens\":103}}\n\n",
        "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":100,\"prompt_cache_hit_tokens\":70,\"completion_tokens\":7,\"total_tokens\":107}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_mixed_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    let body = if request_index == 1 {
        concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"need directory first\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
                "{\"index\":0,\"id\":\"call_ls\",\"type\":\"function\",\"function\":{\"name\":\"list_directory\",\"arguments\":\"{\\\"path\\\":\\\".\\\"}\"}},",
                "{\"index\":1,\"id\":\"call_client_tool\",\"type\":\"function\",\"function\":{\"name\":\"plain_external_tool\",\"arguments\":\"{\\\"code\\\":\\\"1+1\\\"}\"}}",
                "]}}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
    } else {
        concat!(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_client_tool_2\",\"type\":\"function\",\"function\":{\"name\":\"plain_external_tool\",\"arguments\":\"{\\\"code\\\":\\\"1+1\\\"}\"}}]}}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_web_search_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tool_result = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("tool")
                    && message.get("tool_call_id").and_then(Value::as_str) == Some("call_web")
            })
        })
        .unwrap_or(false);
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = if has_tool_result {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"web-search-result-handled\"}}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"need web evidence\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_web\",\"type\":\"function\",\"function\":{\"name\":\"web_search\",\"arguments\":\"{\\\"mode\\\":\\\"open\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_web_search_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tool_result = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("tool")
                    && message.get("tool_call_id").and_then(Value::as_str) == Some("call_web")
            })
        })
        .unwrap_or(false);
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    if has_tool_result {
        return Json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "web-search-result-handled" }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13 }
        }))
        .into_response();
    }
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "reasoning_content": "need web evidence",
                "tool_calls": [{
                    "id": "call_web",
                    "type": "function",
                    "function": {
                        "name": "web_search",
                        "arguments": "{\"mode\":\"open\"}"
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10 }
    }))
    .into_response()
}

async fn fake_repeated_failing_web_search_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    if !has_tools {
        return Json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "I could not open the pages, so I am answering from the available search diagnostics."
                }
            }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 5, "total_tokens": 14 }
        }))
        .into_response();
    }
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call_web_{}", Uuid::new_v4().simple()),
                    "type": "function",
                    "function": {
                        "name": "web_search",
                        "arguments": "{\"mode\":\"open\"}"
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10 }
    }))
    .into_response()
}

async fn fake_repeated_failing_web_search_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = if has_tools {
        format!(
            concat!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_web_{}\",\"type\":\"function\",\"function\":{{\"name\":\"web_search\",\"arguments\":\"{{\\\"mode\\\":\\\"open\\\"}}\"}}}}]}}}}]}}\n\n",
                "data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}}}\n\n",
                "data: [DONE]\n\n"
            ),
            Uuid::new_v4().simple()
        )
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"streaming recovered after repeated web search failures\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":6,\"total_tokens\":17}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_budgeted_web_search_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    let body = if has_tools {
        format!(
            concat!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_web_{}\",\"type\":\"function\",\"function\":{{\"name\":\"web_search\",\"arguments\":\"{{\\\"query\\\":\\\"bounded search {}\\\"}}\"}}}}]}}}}]}}\n\n",
                "data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}}}\n\n",
                "data: [DONE]\n\n"
            ),
            Uuid::new_v4().simple(),
            request_index
        )
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"streaming recovered after the web search budget\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":13,\"completion_tokens\":7,\"total_tokens\":20}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_budgeted_web_search_then_deepseek_protocol_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    let body = if has_tools {
        format!(
            concat!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_web_{}\",\"type\":\"function\",\"function\":{{\"name\":\"web_search\",\"arguments\":\"{{\\\"query\\\":\\\"bounded dsml search {}\\\"}}\"}}}}]}}}}]}}\n\n",
                "data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}}}\n\n",
                "data: [DONE]\n\n"
            ),
            Uuid::new_v4().simple(),
            request_index
        )
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<\\uff5c\\uff5cDSML\\uff5c\\uff5ctool_calls><\\uff5c\\uff5cDSML\\uff5c\\uff5cinvoke name=\\\"web_search\\\"><\\uff5c\\uff5cDSML\\uff5c\\uff5cparameter name=\\\"query\\\" string=\\\"true\\\">retry</\\uff5c\\uff5cDSML\\uff5c\\uff5cparameter></\\uff5c\\uff5cDSML\\uff5c\\uff5cinvoke></\\uff5c\\uff5cDSML\\uff5c\\uff5ctool_calls>\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":13,\"completion_tokens\":7,\"total_tokens\":20}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_deepseek_reasoning_protocol_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tool_result = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("tool")
                    && message
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .is_some()
            })
        })
        .unwrap_or(false);
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = if has_tool_result {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"used reasoning protocol tool result\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":13,\"completion_tokens\":7,\"total_tokens\":20}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"<\\uff5c\\uff5cDSML\\uff5c\\uff5ctool_calls><\\uff5c\\uff5cDSML\\uff5c\\uff5cinvoke name=\\\"web_search\\\"><\\uff5c\\uff5cDSML\\uff5c\\uff5cparameter name=\\\"query\\\" string=\\\"true\\\">reasoning protocol search</\\uff5c\\uff5cDSML\\uff5c\\uff5cparameter></\\uff5c\\uff5cDSML\\uff5c\\uff5cinvoke></\\uff5c\\uff5cDSML\\uff5c\\uff5ctool_calls>\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_budgeted_web_search_then_deepseek_reasoning_protocol_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    let body = if has_tools {
        format!(
            concat!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_web_{}\",\"type\":\"function\",\"function\":{{\"name\":\"web_search\",\"arguments\":\"{{\\\"query\\\":\\\"bounded reasoning dsml search {}\\\"}}\"}}}}]}}}}]}}\n\n",
                "data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}}}\n\n",
                "data: [DONE]\n\n"
            ),
            Uuid::new_v4().simple(),
            request_index
        )
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"<\\uff5c\\uff5cDSML\\uff5c\\uff5ctool_calls><\\uff5c\\uff5cDSML\\uff5c\\uff5cinvoke name=\\\"web_search\\\"><\\uff5c\\uff5cDSML\\uff5c\\uff5cparameter name=\\\"query\\\" string=\\\"true\\\">retry reasoning</\\uff5c\\uff5cDSML\\uff5c\\uff5cparameter></\\uff5c\\uff5cDSML\\uff5c\\uff5cinvoke></\\uff5c\\uff5cDSML\\uff5c\\uff5ctool_calls>\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"recovered without leaking reasoning protocol\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":13,\"completion_tokens\":7,\"total_tokens\":20}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_protocol_marker_text_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"custom upstream visible <\\uff5c\\uff5cDSML\\uff5c\\uff5ctool_calls> text\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":4,\"total_tokens\":9}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_budgeted_web_search_then_tool_delta_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    let body = if has_tools {
        format!(
            concat!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_web_{}\",\"type\":\"function\",\"function\":{{\"name\":\"web_search\",\"arguments\":\"{{\\\"query\\\":\\\"bounded standard search {}\\\"}}\"}}}}]}}}}]}}\n\n",
                "data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}}}\n\n",
                "data: [DONE]\n\n"
            ),
            Uuid::new_v4().simple(),
            request_index
        )
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_recovery\",\"type\":\"function\",\"function\":{\"name\":\"web_search\",\"arguments\":\"{\\\"query\\\":\\\"retry\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":13,\"completion_tokens\":7,\"total_tokens\":20}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_repeated_failing_list_directory_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call_list_{}", Uuid::new_v4().simple()),
                    "type": "function",
                    "function": {
                        "name": "list_directory",
                        "arguments": "{\"path\":\"..\"}"
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10 }
    }))
    .into_response()
}

async fn fake_repeated_failing_list_directory_changing_args_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let call_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call_list_{}", Uuid::new_v4().simple()),
                    "type": "function",
                    "function": {
                        "name": "list_directory",
                        "arguments": format!("{{\"path\":\"../{}\"}}", call_index)
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10 }
    }))
    .into_response()
}

async fn fake_successful_list_directory_forever_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let call_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call_web_{}", Uuid::new_v4().simple()),
                    "type": "function",
                    "function": {
                        "name": "list_directory",
                        "arguments": format!("{{\"path\":\".\",\"depth\":0,\"include_files\":true,\"include_dirs\":true,\"nonce\":{call_index}}}")
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10 }
    }))
    .into_response()
}

async fn fake_apply_patch_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tool_result = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("tool")
                    && message.get("tool_call_id").and_then(Value::as_str) == Some("call_patch")
            })
        })
        .unwrap_or(false);
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = if has_tool_result {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"patch-ok\"}}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    } else {
        concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"patch the file natively\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_patch\",\"type\":\"function\",\"function\":{\"name\":\"apply_patch\",\"arguments\":\"{\\\"patch\\\":\\\"*** Begin Patch\\\\n\"}}]}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"*** Add File: target/codeseex-apply-patch-streaming-test/hello.txt\\\\n+hello\\\\n\"}}]}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"*** End Patch\\\"}\"}}]}}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6}}\n\n",
                "data: [DONE]\n\n"
            )
            .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_apply_patch_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_patch",
                    "type": "function",
                    "function": {
                        "name": "apply_patch",
                        "arguments": "{\"patch\":\"*** Begin Patch\\n*** Add File: target/codeseex-apply-patch-nonstream-test/hello.txt\\n+hello\\n*** End Patch\"}"
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 6, "completion_tokens": 4, "total_tokens": 10 }
    }))
    .into_response()
}

async fn fake_apply_patch_blank_context_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_patch",
                    "type": "function",
                    "function": {
                        "name": "apply_patch",
                        "arguments": "{\"patch\":\"*** Begin Patch\\n*** Update File: src/lib.rs\\n@@\\n fn before() {}\\n\\n pub fn after() {}\\n*** End Patch\"}"
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 6, "completion_tokens": 4, "total_tokens": 10 }
    }))
    .into_response()
}

async fn fake_apply_patch_recovery_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let tool_outputs = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|message| {
                    message.get("role").and_then(Value::as_str) == Some("tool")
                        && message.get("tool_call_id").and_then(Value::as_str) == Some("call_patch")
                })
                .map(|message| {
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = if tool_outputs
        .iter()
        .any(|output| output.contains("Success."))
    {
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "patch recovered"
                }
            }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12 }
        })
    } else if tool_outputs
        .iter()
        .any(|output| output.contains("apply_patch verification failed"))
    {
        json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_patch",
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "arguments": "{\"patch\":\"*** Begin Patch\\n*** Add File: target/codeseex-apply-patch-recovery-test/hello.txt\\n+fixed\\n*** End Patch\"}"
                        }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12 }
        })
    } else {
        json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_patch",
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "arguments": "{\"patch\":\"*** Begin Patch\\n*** Add File\\n+broken\\n*** End Patch\"}"
                        }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 6, "completion_tokens": 4, "total_tokens": 10 }
        })
    };
    Json(body).into_response()
}

async fn fake_tool_search_bridge_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"need deferred tools\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_tool_search\",\"type\":\"function\",\"function\":{\"name\":\"tool_search_tool\",\"arguments\":\"{\\\"query\\\":\\\"sub-agent\\\",\\\"limit\\\":5}\"}}]}}]}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_codex_handoff_then_final_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tool_result = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("tool")
                    && message.get("tool_call_id").and_then(Value::as_str) == Some("call_shell")
            })
        })
        .unwrap_or(false);
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    if has_tool_result {
        return Json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "shell output handled" }
            }],
            "usage": {
                "prompt_tokens": 1240,
                "prompt_cache_hit_tokens": 1000,
                "completion_tokens": 5,
                "total_tokens": 1245
            }
        }))
        .into_response();
    }
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_shell",
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "arguments": "{\"command\":\"Get-ChildItem\"}"
                    }
                }]
            }
        }],
        "usage": {
            "prompt_tokens": 420,
            "prompt_cache_hit_tokens": 300,
            "completion_tokens": 12,
            "total_tokens": 432
        }
    }))
    .into_response()
}

async fn fake_repeated_shell_handoff_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call_shell_{request_index}"),
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "arguments": "{\"command\":\"date\"}"
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 30, "completion_tokens": 2, "total_tokens": 32 }
    }))
    .into_response()
}

async fn fake_reasoning_then_content_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think before answering\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"final answer\"}}]}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_service_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"prompt_cache_hit_tokens\":4,\"completion_tokens\":3,\"total_tokens\":13}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

async fn fake_final_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    Json(json!({
        "choices": [{
            "message": { "role": "assistant", "content": "tool result acknowledged" }
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13 }
    }))
    .into_response()
}

async fn fake_chained_client_tool_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        let request_index = requests.len();
        requests.push(payload);
        request_index
    };
    if request_index < 4 {
        return Json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": format!("call_chain_{request_index}"),
                        "type": "function",
                        "function": {
                            "name": "shell_command",
                            "arguments": format!(r#"{{"command":"chain step {request_index}"}}"#)
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 100 + request_index,
                "prompt_cache_hit_tokens": 80 + request_index,
                "completion_tokens": 5,
                "total_tokens": 105 + request_index
            }
        }))
        .into_response();
    }
    Json(json!({
        "choices": [{
            "message": { "role": "assistant", "content": "chain completed" }
        }],
        "usage": {
            "prompt_tokens": 104,
            "prompt_cache_hit_tokens": 84,
            "completion_tokens": 3,
            "total_tokens": 107
        }
    }))
    .into_response()
}

fn assert_chat_tool_calls_are_paired(payload: &Value) {
    let messages = payload["messages"]
        .as_array()
        .expect("chat request messages");
    for (assistant_index, message) in messages.iter().enumerate() {
        let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for (offset, tool_call) in tool_calls.iter().enumerate() {
            let call_id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .expect("assistant tool call id");
            let tool_message = messages
                .get(assistant_index + offset + 1)
                .expect("assistant tool call should have a following tool message");
            assert_eq!(
                tool_message.get("role").and_then(Value::as_str),
                Some("tool")
            );
            assert_eq!(
                tool_message.get("tool_call_id").and_then(Value::as_str),
                Some(call_id),
                "assistant tool call {call_id} should be paired immediately"
            );
        }
    }
}

async fn fake_failed_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"error":{"message":"simulated failure"}}"#))
        .expect("fake upstream response should build")
}

fn thread_title_service_request(id: &str) -> Value {
    json!({
        "id": id,
        "model": "gpt-5.4",
        "stream": false,
        "store": false,
        "client_metadata": {
            "x-codex-installation-id": "codex-install"
        },
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "Generate a short conversation title." }]
        }],
        "reasoning": { "effort": "medium" },
        "max_output_tokens": 32,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "thread_title",
                "schema": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "minLength": 1, "maxLength": 36 }
                    },
                    "required": ["title"],
                    "additionalProperties": false
                }
            }
        }
    })
}

fn ambient_suggestions_service_request(id: &str) -> Value {
    let context = "Large workspace context. ".repeat(1_000);
    json!({
        "id": id,
        "model": "gpt-5.4",
        "stream": false,
        "store": false,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": context
            }]
        }],
        "reasoning": { "effort": "low" },
        "max_output_tokens": 512,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "ambient_suggestions",
                "schema": {
                    "type": "object",
                    "properties": {
                        "suggestions": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "title": { "type": "string" },
                                    "description": { "type": "string" },
                                    "prompt": { "type": "string" },
                                    "appId": { "type": "string" }
                                },
                                "required": ["title", "description", "prompt", "appId"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["suggestions"],
                    "additionalProperties": false
                }
            }
        }
    })
}

fn ambient_suggestion_safety_service_request(id: &str) -> Value {
    json!({
        "id": id,
        "model": "gpt-5.4-mini",
        "stream": false,
        "store": false,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "Classify ambient suggestion candidates." }]
        }],
        "reasoning": { "effort": "none" },
        "max_output_tokens": 256,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "ambient_suggestion_safety",
                "schema": {
                    "type": "object",
                    "properties": {
                        "exclude": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "reason": { "type": "string" }
                                },
                                "required": ["id", "reason"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["exclude"],
                    "additionalProperties": false
                }
            }
        }
    })
}

async fn fake_vision_instruction_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    if request_index == 1 {
        return Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_vision_exact",
                    "type": "function",
                    "function": {
                        "name": "vision_analyze",
                        "arguments": "{\"image\":\"data:image/png;base64,AAAA\",\"prompt\":\"Return exactly what you see.\"}"
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10 }
        }))
        .into_response();
    }
    Json(json!({
        "choices": [{
            "message": { "role": "assistant", "content": "VISION EXACT LINE 1\nVISION EXACT LINE 2\nMODEL CONTINUATION AFTER VISION" }
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 9, "total_tokens": 19 }
    }))
    .into_response()
}

async fn fake_parallel_vision_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let has_tool_results = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            ["call_vision_a", "call_vision_b"]
                .into_iter()
                .all(|call_id| {
                    messages.iter().any(|message| {
                        message.get("role").and_then(Value::as_str) == Some("tool")
                            && message.get("tool_call_id").and_then(Value::as_str) == Some(call_id)
                    })
                })
        })
        .unwrap_or(false);
    {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
    }
    if has_tool_results {
        return Json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "parallel vision handled" }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13 }
        }))
        .into_response();
    }
    Json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_vision_a",
                        "type": "function",
                        "function": {
                            "name": "vision_analyze",
                            "arguments": "{\"image\":\"data:image/png;base64,AAAA\",\"prompt\":\"first\"}"
                        }
                    },
                    {
                        "id": "call_vision_b",
                        "type": "function",
                        "function": {
                            "name": "vision_analyze",
                            "arguments": "{\"image\":\"data:image/png;base64,BBBB\",\"prompt\":\"second\"}"
                        }
                    }
                ]
            }
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10 }
    }))
    .into_response()
}

async fn fake_vision_instruction_streaming_chat_completions(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let request_index = {
        let mut requests = state.requests.lock().expect("fake upstream lock poisoned");
        requests.push(payload);
        requests.len()
    };
    let body = if request_index == 1 {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_vision_exact\",\"type\":\"function\",\"function\":{\"name\":\"vision_analyze\",\"arguments\":\"{\\\"image\\\":\\\"data:image/png;base64,AAAA\\\",\\\"prompt\\\":\\\"Return exactly what you see.\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"VISION EXACT LINE 1\\nVISION EXACT LINE 2\\nMODEL CONTINUATION AFTER VISION\"}}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("fake upstream response should build")
}

fn assert_vision_tool_result_without_system_instruction(payload: &Value, call_id: &str) {
    let messages = payload["messages"]
        .as_array()
        .expect("chat request messages");
    assert!(!messages.iter().any(|message| {
        message.get("role").and_then(Value::as_str) == Some("system")
            && message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("Vision result")
    }));
    let assistant_index = messages
        .iter()
        .position(|message| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .map(|calls| {
                        calls
                            .iter()
                            .any(|call| call.get("id").and_then(Value::as_str) == Some(call_id))
                    })
                    .unwrap_or(false)
        })
        .expect("assistant tool call message");
    let tool_index = messages
        .iter()
        .position(|message| {
            message.get("role").and_then(Value::as_str) == Some("tool")
                && message.get("tool_call_id").and_then(Value::as_str) == Some(call_id)
        })
        .expect("tool result message");
    let user_index = messages
        .iter()
        .position(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .expect("user message");

    assert!(assistant_index < tool_index);
    assert!(user_index < assistant_index);
    let tool_content = messages[tool_index]
        .get("content")
        .and_then(Value::as_str)
        .expect("tool content");
    assert!(tool_content.contains("VISION EXACT LINE 1"));
    assert!(!tool_content.contains("model_instruction"));
}

async fn fake_vision_responses(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    {
        let mut requests = state.requests.lock().expect("fake vision lock poisoned");
        requests.push(payload);
    }
    Json(json!({
        "output": [{
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": "VISION EXACT LINE 1\nVISION EXACT LINE 2"
            }]
        }],
        "usage": { "total_tokens": 7 }
    }))
    .into_response()
}

async fn fake_delayed_vision_responses(
    State(state): State<FakeUpstreamState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let active = state.active_requests.fetch_add(1, AtomicOrdering::SeqCst) + 1;
    state
        .max_concurrent_requests
        .fetch_max(active, AtomicOrdering::SeqCst);
    {
        let mut requests = state.requests.lock().expect("fake vision lock poisoned");
        requests.push(payload);
    }
    tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    state.active_requests.fetch_sub(1, AtomicOrdering::SeqCst);
    Json(json!({
        "output": [{
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": "delayed vision ok"
            }]
        }],
        "usage": { "total_tokens": 7 }
    }))
    .into_response()
}

#[test]
fn maps_chat_usage_to_responses_usage_shape() {
    let usage = json!({
        "prompt_tokens": 100,
        "prompt_cache_hit_tokens": 60,
        "completion_tokens": 20,
        "completion_tokens_details": { "reasoning_tokens": 7 },
        "total_tokens": 120
    });

    let mapped = response_usage_from_chat_usage(Some(&usage));

    assert_eq!(mapped["input_tokens"], 100);
    assert_eq!(mapped["cached_input_tokens"], 60);
    assert_eq!(mapped["cache_miss_input_tokens"], 40);
    assert_eq!(mapped["input_tokens_details"]["cached_tokens"], 60);
    assert_eq!(mapped["output_tokens"], 20);
    assert_eq!(mapped["reasoning_output_tokens"], 7);
    assert_eq!(mapped["output_tokens_details"]["reasoning_tokens"], 7);
    assert_eq!(mapped["total_tokens"], 120);
}

#[test]
fn mapped_response_keeps_codex_completion_metadata() {
    let config = test_config(temp_workspace("mapped-response"));
    let chat = json!({
        "choices": [{
            "message": { "role": "assistant", "content": "ok" }
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    });

    let response = chat_completion_to_response(&config, "resp_test", "deepseek-v4-pro", chat, true);

    assert_eq!(response["status"], "completed");
    assert_eq!(response["error"], Value::Null);
    assert_eq!(response["incomplete_details"], Value::Null);
    assert_eq!(response["parallel_tool_calls"], true);
    assert_eq!(response["usage"]["input_tokens"], 10);
    assert_eq!(response["usage"]["output_tokens"], 2);
}

#[test]
fn chat_payload_forwards_codex_generation_parameters() {
    let config = test_config(std::env::temp_dir().join(format!(
        "codeseex-payload-params-test-{}",
        Uuid::new_v4().simple()
    )));
    let request = json!({
        "temperature": 0.7,
        "top_p": 0.8,
        "max_output_tokens": 1234,
        "text": { "format": { "type": "json_schema" } },
        "reasoning": { "effort": "xhigh" }
    });
    let mut payload = json!({
        "model": "deepseek-v4-pro",
        "messages": [],
        "stream": true
    });

    normalize_chat_payload(&config, &request, &mut payload);

    assert_eq!(payload["temperature"], json!(0.7));
    assert_eq!(payload["top_p"], json!(0.8));
    assert_eq!(payload["max_tokens"], json!(1234));
    assert_eq!(payload["response_format"], json!({ "type": "json_object" }));
    assert_eq!(payload["thinking"], json!({ "type": "enabled" }));
    assert_eq!(payload["stream_options"], json!({ "include_usage": true }));
}

#[test]
fn configured_temperature_overrides_request_temperature() {
    let config = AppConfig {
        temperature: codeseex_core::models::TemperaturePreset::Strict,
        ..Default::default()
    };
    let mut payload = json!({
        "model": "deepseek-v4-pro",
        "messages": [],
        "stream": false
    });

    normalize_chat_payload(&config, &json!({ "temperature": 1.5 }), &mut payload);

    assert_eq!(payload["temperature"], json!(0.0));
}

#[test]
fn tool_choice_none_is_not_rewritten_to_auto() {
    let tools = vec![json!({
        "type": "function",
        "function": { "name": "read_file_range" }
    })];

    assert_eq!(
        normalized_tool_choice(Some(&json!("none")), &tools),
        Some(json!("none"))
    );
    assert_eq!(
        normalized_tool_choice(
            Some(&json!({ "type": "function", "function": { "name": "read_file_range" } })),
            &tools
        ),
        Some(json!({ "type": "function", "function": { "name": "read_file_range" } }))
    );
}

#[test]
fn compact_threshold_accepts_codex_context_management_shapes() {
    assert_eq!(resolve_compact_threshold(Some(&json!(1200))), Some(1200));
    assert_eq!(
        resolve_compact_threshold(Some(&json!({ "compaction": { "threshold": 3400 } }))),
        Some(3400)
    );
    assert_eq!(
        resolve_compact_threshold(Some(&json!([{ "token_threshold": 5600 }]))),
        Some(5600)
    );
    assert_eq!(resolve_compact_threshold(Some(&json!(false))), None);
}

#[test]
fn automatic_compaction_is_not_appended_to_client_tool_calls() {
    let mut response = json!({
        "output": [{
            "type": "custom_tool_call",
            "name": "apply_patch",
            "call_id": "call_patch",
            "input": "*** Begin Patch\n*** End Patch"
        }]
    });
    let item = json!({ "type": "compaction", "summary": [] });

    assert!(!append_auto_compaction_if_safe(&mut response, Some(&item)));
    assert_eq!(response["output"].as_array().unwrap().len(), 1);
}

#[test]
fn automatic_compaction_requires_explicit_context_management() {
    let context = crate::responses::context::BuiltResponseContext {
        messages: vec![ChatMessage::text("user", "large replay")],
        current_messages: vec![ChatMessage::text("user", "large replay")],
        current_image_refs: Vec::new(),
        tool_facts: Vec::new(),
        diagnostic: json!({
            "codex_full_context_replay": {
                "tool_history_compaction": { "triggered": true }
            },
            "budget": { "triggered": true }
        }),
        history_message_count: 0,
        upstream_context_limit: None,
    };
    let request = json!({
        "instructions": "You are Codex.",
        "tools": [],
        "prompt_cache_key": "thread",
        "input": [{ "role": "user", "content": "continue" }]
    });

    let compact = build_automatic_compaction(
        &AppConfig::default(),
        &request,
        "deepseek-v4-flash",
        &context,
    )
    .expect("build compaction");

    assert!(compact.is_none());
}

#[test]
fn apply_patch_surface_gets_synthetic_tool_search_bridge() {
    let mut context = ToolContext::from_request_tools(Some(&json!([{
        "type": "function",
        "function": {
            "name": "apply_patch",
            "description": "Apply a patch.",
            "parameters": { "type": "object", "properties": {} }
        }
    }])));

    assert!(request_has_codex_native_tool_surface(&context));
    context.ensure_codex_tool_search_bridge();

    let names = context.upstream_names();
    assert!(names.iter().any(|name| name == "apply_patch"), "{names:?}");
    assert!(
        names.iter().any(|name| name == "tool_search_tool"),
        "{names:?}"
    );
}

#[test]
fn codex_request_marker_gets_synthetic_tool_search_bridge() {
    let mut context = ToolContext::from_request_tools(Some(&json!([{
        "type": "function",
        "function": {
            "name": "plain_external_tool",
            "description": "External client tool.",
            "parameters": { "type": "object", "properties": {} }
        }
    }])));
    let request = json!({
        "client_metadata": {
            "x-codex-installation-id": "codex-install"
        },
        "tools": []
    });
    let decision = codex_tool_search_bridge_decision(&request, false, &context);

    assert!(decision.injected);
    assert_eq!(decision.reason, "codex_request_marker");
    context.ensure_codex_tool_search_bridge();

    let names = context.upstream_names();
    assert!(
        names.iter().any(|name| name == "tool_search_tool"),
        "{names:?}"
    );
}

#[test]
fn codex_request_marker_promotes_existing_tool_search_bridge() {
    let mut context = ToolContext::from_request_tools(Some(&json!([{
        "type": "function",
        "name": "tool_search_tool",
        "description": "Search deferred tools.",
        "parameters": { "type": "object", "properties": {} }
    }])));
    let request = json!({ "client_metadata": {} });
    let decision = codex_tool_search_bridge_decision(&request, false, &context);

    assert!(!decision.injected);
    assert_eq!(decision.reason, "already_present");
    assert!(decision.upstream_had_tool_search);
    context.promote_codex_tool_search_tools();

    let item = context.response_item_from_chat_call(&ChatToolCall {
        id: "call_search".to_owned(),
        name: "tool_search_tool".to_owned(),
        arguments: r#"{"query":"spawn_agent"}"#.to_owned(),
    });
    assert_eq!(
        item.get("type").and_then(Value::as_str),
        Some("tool_search_call")
    );
}

#[test]
fn plain_external_existing_tool_search_stays_function_call() {
    let context = ToolContext::from_request_tools(Some(&json!([{
        "type": "function",
        "name": "tool_search_tool",
        "description": "Plain external tool.",
        "parameters": { "type": "object", "properties": {} }
    }])));
    let request = json!({});
    let decision = codex_tool_search_bridge_decision(&request, false, &context);

    assert!(!decision.injected);
    assert_eq!(decision.reason, "already_present");
    let item = context.response_item_from_chat_call(&ChatToolCall {
        id: "call_search".to_owned(),
        name: "tool_search_tool".to_owned(),
        arguments: r#"{"query":"spawn_agent"}"#.to_owned(),
    });
    assert_eq!(
        item.get("type").and_then(Value::as_str),
        Some("function_call")
    );
}

#[test]
fn plain_external_client_tool_does_not_get_synthetic_tool_search_bridge() {
    let context = ToolContext::from_request_tools(Some(&json!([{
        "type": "function",
        "function": {
            "name": "plain_external_tool",
            "description": "External client tool.",
            "parameters": { "type": "object", "properties": {} }
        }
    }])));
    let request = json!({ "tools": [] });
    let decision = codex_tool_search_bridge_decision(&request, false, &context);

    assert!(!decision.injected);
    assert_eq!(decision.reason, "not_codex_request");
    assert!(!context
        .upstream_names()
        .iter()
        .any(|name| name == "tool_search_tool"));
}

#[test]
fn synthetic_tool_search_bridge_diagnostic_records_decision() {
    let mut context = ToolContext::from_request_tools(Some(&json!([{
        "type": "function",
        "function": {
            "name": "plain_external_tool",
            "description": "External client tool.",
            "parameters": { "type": "object", "properties": {} }
        }
    }])));
    let request = json!({ "prompt_cache_key": "thread-full-context" });
    let decision = codex_tool_search_bridge_decision(&request, false, &context);
    assert!(decision.injected);
    context.ensure_codex_tool_search_bridge();

    let upstream_tools = context.upstream_tools.clone();
    let diagnostic = tool_exposure_diagnostic(
        "resp_bridge_diagnostic",
        &context,
        &upstream_tools,
        &decision,
        &crate::tools::default_enabled_tool_ids(),
        true,
        crate::upstream::payload::CodexServiceRequestKind::None,
    );

    assert_eq!(
        diagnostic["tool_search_bridge"]["reason"],
        "codex_request_marker"
    );
    assert_eq!(diagnostic["tool_search_bridge"]["injected"], true);
    assert_eq!(
        diagnostic["tool_search_bridge"]["has_tool_search_tool"],
        true
    );
    assert_eq!(
        diagnostic["codex_request_markers"]["prompt_cache_key"],
        true
    );
}

#[test]
fn service_request_suppresses_synthetic_tool_search_bridge() {
    let context = ToolContext::from_request_tools(Some(&json!([{
        "type": "function",
        "function": {
            "name": "shell_command",
            "description": "Run a shell command.",
            "parameters": { "type": "object", "properties": {} }
        }
    }])));
    let request = thread_title_service_request("resp_service_bridge");
    let decision = codex_tool_search_bridge_decision(&request, true, &context);

    assert!(!decision.injected);
    assert_eq!(decision.reason, "suppressed_service_request");
}

#[test]
fn streaming_tool_loop_preserves_deepseek_reasoning_content() {
    let tool_calls = vec![ChatToolCall {
        id: "call_abc".to_owned(),
        name: "list_directory".to_owned(),
        arguments: r#"{"path":"."}"#.to_owned(),
    }];

    let message =
        chat_tool_calls_to_assistant_message(&tool_calls, "", "look up the directory first");

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], "");
    assert_eq!(message["reasoning_content"], "look up the directory first");
    assert_eq!(message["tool_calls"][0]["id"], "call_abc");
    assert_eq!(
        message["tool_calls"][0]["function"]["name"],
        "list_directory"
    );
    assert_eq!(
        message["tool_calls"][0]["function"]["arguments"],
        r#"{"path":"."}"#
    );
}

#[test]
fn mixed_native_and_code_tool_replay_keeps_only_executed_code_tools() {
    let chat = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "reasoning_content": "inspect before patching",
                "tool_calls": [
                    {
                        "id": "call_code",
                        "type": "function",
                        "function": {
                            "name": "list_directory",
                            "arguments": "{\"path\":\".\"}"
                        }
                    },
                    {
                        "id": "call_patch",
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "arguments": "{\"patch\":\"*** Begin Patch\\n*** End Patch\"}"
                        }
                    }
                ]
            }
        }]
    });
    let code_tool_calls = vec![ChatToolCall {
        id: "call_code".to_owned(),
        name: "list_directory".to_owned(),
        arguments: r#"{"path":"."}"#.to_owned(),
    }];

    let assistant = assistant_message_from_chat_tool_subset(&chat, &code_tool_calls);

    assert_eq!(assistant["reasoning_content"], "inspect before patching");
    let calls = assistant["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_code");
    assert_eq!(calls[0]["function"]["name"], "list_directory");
}

#[test]
fn external_collision_takes_ownership_from_ordinary_codeseex_code_tool() {
    let config = test_config(temp_workspace("tool-calls-response"));
    let chat = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {
                        "id": "call_internal",
                        "type": "function",
                        "function": {
                            "name": "list_directory",
                            "arguments": "{\"path\":\".\"}"
                        }
                    },
                    {
                        "id": "call_external",
                        "type": "function",
                        "function": {
                            "name": "js",
                            "arguments": "{\"code\":\"1+1\"}"
                        }
                    }
                ]
            }
        }]
    });
    let tool_context = crate::tool_passthrough::ToolContext::from_request_tools(Some(&json!([
        {
            "type": "function",
            "function": {
                "name": "list_directory",
                "description": "A colliding client-side tool that should take ownership from ordinary CodeSeeX tools.",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "js",
                "description": "Run JavaScript.",
                "parameters": { "type": "object", "properties": {} }
            }
        }
    ])));

    let response = chat_completion_tool_calls_to_response(
        &config,
        "resp_test",
        "deepseek-v4-pro",
        chat,
        &crate::community_tools::CommunityToolSet::default(),
        &tool_context,
        true,
    );
    let output = response["output"].as_array().unwrap();

    assert!(!output.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("proxy_tool_call")
            && item.get("name").and_then(Value::as_str) == Some("list_directory")
    }));
    assert!(output.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
            && item.get("name").and_then(Value::as_str) == Some("list_directory")
            && item.get("call_id").and_then(Value::as_str) == Some("call_internal")
    }));
    assert!(output.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
            && item.get("name").and_then(Value::as_str) == Some("js")
            && item.get("call_id").and_then(Value::as_str) == Some("call_external")
    }));
}

#[test]
fn external_collision_triggers_external_passthrough_for_ordinary_codeseex_tool() {
    let tool_calls = vec![ChatToolCall {
        id: "call_internal".to_owned(),
        name: "workspace_search".to_owned(),
        arguments: r#"{"query":"needle"}"#.to_owned(),
    }];
    let community_tools = crate::community_tools::CommunityToolSet::default();
    let external_tool_context =
        crate::tool_passthrough::ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "function": {
                    "name": "workspace_search",
                    "description": "A colliding client-side tool.",
                    "parameters": { "type": "object", "properties": {} }
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "js",
                    "description": "Run JavaScript.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }
        ])));

    let partition = partition_tool_calls(tool_calls, &community_tools, &external_tool_context);
    assert!(partition.code.is_empty());
    assert_eq!(partition.external.len(), 1);
    assert!(partition.unknown.is_empty());
}

#[test]
fn web_search_maps_to_native_response_item_not_proxy_tool_item() {
    let config = test_config(temp_workspace("web-search-response"));
    let chat = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_web",
                    "type": "function",
                    "function": {
                        "name": "web_search",
                        "arguments": "{\"query\":\"today weather\"}"
                    }
                }]
            }
        }]
    });
    let response = chat_completion_tool_calls_to_response(
        &config,
        "resp_web",
        "deepseek-v4-pro",
        chat,
        &crate::community_tools::CommunityToolSet::default(),
        &crate::tool_passthrough::ToolContext::default(),
        true,
    );
    let output = response["output"].as_array().unwrap();

    assert!(output.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("web_search_call")
            && item.get("call_id").and_then(Value::as_str) == Some("call_web")
    }));
    assert!(!output.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("proxy_tool_call")
            && item.get("name").and_then(Value::as_str) == Some("web_search")
    }));
    let web_item = output
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
        .unwrap();
    let mut sequence = 0;
    let events = String::from_utf8(
        web_search_call_sse_events("resp_web", 0, web_item, &mut sequence).to_vec(),
    )
    .unwrap();
    assert!(events.contains("response.web_search_call.searching"));
    assert!(!events.contains("proxy_tool_call"));
}

#[test]
fn web_search_open_page_sse_uses_opening_event() {
    let item = json!({
        "id": "ws_open",
        "type": "web_search_call",
        "status": "completed",
        "call_id": "call_web",
        "action": { "type": "open_page", "url": "https://example.com/" }
    });
    let mut sequence = 0;
    let events =
        String::from_utf8(web_search_call_sse_events("resp_web", 0, &item, &mut sequence).to_vec())
            .unwrap();

    assert!(events.contains("response.web_search_call.opening"));
    assert!(!events.contains("response.web_search_call.searching"));
}

#[test]
fn proxy_tool_call_sse_uses_in_progress_then_completed_lifecycle() {
    let item = json!({
        "id": "ptc_test",
        "type": "proxy_tool_call",
        "status": "completed",
        "call_id": "call_test",
        "name": "list_directory",
        "arguments": "{\"path\":\".\"}"
    });
    let mut sequence = 0;
    let events = String::from_utf8(
        proxy_tool_call_sse_events("resp_tool", 0, &item, &mut sequence).to_vec(),
    )
    .unwrap();

    let added = events
        .find("response.output_item.added")
        .expect("proxy tool call should emit added");
    let in_progress = events
        .find("\"status\":\"in_progress\"")
        .expect("added proxy tool call should be in progress");
    let done = events
        .find("response.output_item.done")
        .expect("proxy tool call should emit done");
    let completed = events
        .find("\"status\":\"completed\"")
        .expect("done proxy tool call should be completed");
    assert!(added < done, "{events}");
    assert!(in_progress < completed, "{events}");
}

#[test]
fn proxy_visible_items_preserve_tool_order_without_text_messages() {
    let calls = vec![
        ChatToolCall {
            id: "call_ls_1".to_owned(),
            name: "list_directory".to_owned(),
            arguments: r#"{"path":"."}"#.to_owned(),
        },
        ChatToolCall {
            id: "call_web".to_owned(),
            name: "web_search".to_owned(),
            arguments: r#"{"query":"CodeSeeX"}"#.to_owned(),
        },
        ChatToolCall {
            id: "call_ls_2".to_owned(),
            name: "workspace_search".to_owned(),
            arguments: r#"{"query":"needle"}"#.to_owned(),
        },
    ];

    let items = proxy_visible_response_items(&calls);
    let types = items
        .iter()
        .map(|item| item.get("type").and_then(Value::as_str).unwrap_or(""))
        .collect::<Vec<_>>();

    assert_eq!(
        types,
        vec!["proxy_tool_call", "web_search_call", "proxy_tool_call"]
    );
    assert_eq!(items[0]["call_id"], "call_ls_1");
    assert_eq!(items[1]["call_id"], "call_web");
    assert_eq!(items[2]["call_id"], "call_ls_2");
}

#[tokio::test]
async fn streaming_closes_thinking_before_final_content() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_reasoning_then_content_streaming_chat_completions),
        )
        .with_state(fake_state);
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-thinking-order-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config_with_upstream(data_dir, fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_thinking_order",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "answer once" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let reasoning_done = body
        .find("response.reasoning_summary_text.done")
        .expect("reasoning should be closed");
    let thinking_done = body
        .find("\"codeseex_display_only\":\"thinking_markdown\"")
        .expect("thinking display item should be emitted");
    let content_delta = body
        .find("\"delta\":\"final answer\"")
        .expect("final content should stream");
    assert!(reasoning_done < content_delta, "{body}");
    assert!(thinking_done < content_delta, "{body}");
}

#[tokio::test]
async fn streaming_internal_tools_execute_inside_proxy_without_client_function_call() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_streaming_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-streaming-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config_with_upstream(data_dir, fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let inspection_store = store.clone();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_internal_tool",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "list files then answer" }]
                }
            ]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body.contains("response.reasoning_summary_text.delta"),
        "{body}"
    );
    assert!(body.contains("DeepSeek Thinking"), "{body}");
    assert!(body.contains("codeseex_display_only"), "{body}");
    assert_eq!(
        body.matches("\"delta\":\"**DeepSeek Thinking**\\n\"")
            .count(),
        1,
        "{body}"
    );
    assert!(body.contains("after tool"), "{body}");
    assert!(!body.contains("已使用工具 `list_directory`"), "{body}");
    assert!(body.contains("\"type\":\"proxy_tool_call\""), "{body}");
    assert!(body.contains("directory checked"), "{body}");
    assert!(
        !body.contains("response.function_call_arguments.delta"),
        "{body}"
    );
    assert!(!body.contains("\"type\":\"function_call\""), "{body}");
    assert!(!body.contains("unsupported call"), "{body}");
    let reasoning_done = body
        .find("response.reasoning_summary_text.done")
        .expect("reasoning should close before tool display");
    let thinking_added = body
        .find("\"codeseex_display_only\":\"thinking_markdown\"")
        .expect("thinking display should be emitted before tool display");
    let proxy_tool = body
        .find("\"type\":\"proxy_tool_call\"")
        .expect("proxy tool item should be emitted");
    assert!(reasoning_done < proxy_tool, "{body}");
    assert!(thinking_added < proxy_tool, "{body}");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 2);
    let second_messages = requests[1]["messages"].as_array().unwrap();
    let assistant_tool_message = second_messages
        .iter()
        .find(|message| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && message.get("tool_calls").is_some()
        })
        .expect("second upstream request should include assistant tool call message");
    assert_eq!(
        assistant_tool_message["reasoning_content"],
        "need directory"
    );
    assert!(second_messages.iter().any(|message| {
        message.get("role").and_then(Value::as_str) == Some("tool")
            && message.get("tool_call_id").and_then(Value::as_str) == Some("call_ls")
    }));

    let (events, _) = inspection_store.recent_events(50, None).await.unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == "context_compile_diagnostic"
            && event.audience.as_deref() == Some("safe_diagnostic")));
    let usage_events = events
        .iter()
        .filter(|event| event.event_type == "upstream_call_usage_breakdown")
        .collect::<Vec<_>>();
    assert_eq!(usage_events.len(), 2);
    assert!(usage_events.iter().all(|event| {
        event.audience.as_deref() == Some("safe_diagnostic")
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.pointer("/payload/payload_hash"))
                .is_some()
    }));
    assert!(usage_events.iter().any(|event| {
        event
            .detail
            .as_ref()
            .and_then(|detail| detail.pointer("/usage/input_tokens"))
            .and_then(Value::as_u64)
            == Some(10)
    }));
}

#[tokio::test]
async fn streaming_usage_chunks_keep_last_chunk_within_iteration() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_streaming_usage_chunks_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("streaming-usage-chunks-last-wins");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_usage_chunks",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "check usage chunks" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("usage checked"), "{body}");
    assert!(body.contains("\"input_tokens\":100"), "{body}");
    assert!(body.contains("\"cached_input_tokens\":70"), "{body}");
    assert!(body.contains("\"cache_miss_input_tokens\":30"), "{body}");
    assert!(body.contains("\"output_tokens\":7"), "{body}");
    assert!(body.contains("\"total_tokens\":107"), "{body}");
    assert!(!body.contains("\"output_tokens\":10"), "{body}");

    let (events, _) = store.recent_events(50, None).await.unwrap();
    assert!(events.iter().any(|event| {
        event.event_type == "upstream_usage_chunk_diagnostic"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.get("usage_chunks"))
                .and_then(Value::as_u64)
                == Some(2)
    }));
    assert!(events.iter().any(|event| {
        event.event_type == "upstream_call_usage_breakdown"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.pointer("/usage/output_tokens"))
                .and_then(Value::as_u64)
                == Some(7)
    }));
}

#[tokio::test]
async fn responses_missing_previous_response_id_is_ignored_without_local_replay() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("missing-previous-conflict");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_missing_previous",
            "model": "deepseek-v4-pro",
            "previous_response_id": "resp_not_in_process",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "continue" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .len(),
        1,
        "missing parent should not block Codex-provided input"
    );
    let chain = store
        .response_context_chain("resp_missing_previous", 1)
        .await
        .unwrap();
    assert_eq!(chain[0].previous_response_id, None);
    let (events, _) = store.recent_events(20, None).await.unwrap();
    let request_started = events
        .iter()
        .find(|event| event.event_type == "request_started")
        .expect("request_started event should be recorded");
    assert_eq!(
        request_started
            .detail
            .as_ref()
            .unwrap()
            .pointer("/previous_response_resolution/kind")
            .and_then(Value::as_str),
        Some("missing")
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn responses_missing_previous_response_id_allows_codex_full_context() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("missing-previous-full-context");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let input_items = (0..81)
        .map(|index| {
            json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": format!("full context item {index}") }]
            })
        })
        .collect::<Vec<_>>();
    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_full_context_after_restart",
            "model": "deepseek-v4-pro",
            "previous_response_id": "resp_not_in_process",
            "prompt_cache_key": "thread-full-context",
            "instructions": "answer briefly",
            "tools": [],
            "input": input_items
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .len(),
        1
    );
    let chain = store
        .response_context_chain("resp_full_context_after_restart", 1)
        .await
        .unwrap();
    assert_eq!(chain[0].previous_response_id, None);
    assert_eq!(chain[0].input["input"].as_array().unwrap().len(), 0);
    let (events, _) = store.recent_events(20, None).await.unwrap();
    let request_started = events
        .iter()
        .find(|event| event.event_type == "request_started")
        .expect("request_started event should be recorded");
    assert_eq!(
        request_started
            .detail
            .as_ref()
            .unwrap()
            .pointer("/runtime_context_storage/current/mode")
            .and_then(Value::as_str),
        Some("codex_full_context_not_stored")
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_full_context_request_is_budgeted_before_upstream() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("full-context-budget-upstream");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .layer(DefaultBodyLimit::max(RESPONSES_BODY_LIMIT_BYTES))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let mut input_items = (0..600)
        .map(|index| {
            json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("old full context item {index} {}", "x".repeat(8_000))
                }]
            })
        })
        .collect::<Vec<_>>();
    input_items.push(json!({
        "type": "message",
        "role": "user",
        "content": [{ "type": "input_text", "text": "latest full-context task" }]
    }));

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_full_context_budgeted",
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "thread-full-context",
            "instructions": "answer briefly",
            "tools": [],
            "input": input_items
        }))
        .send()
        .await
        .unwrap();

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let error = response.json::<Value>().await.unwrap();
    assert_eq!(
        error.pointer("/error/code").and_then(Value::as_str),
        Some("context_limit_exceeded")
    );
    assert!(
        requests.is_empty(),
        "over-limit replay must not reach upstream"
    );

    let (events, _) = store.recent_events(40, None).await.unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == "context_limit_exceeded"));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_model_switch_full_context_uses_prompt_cache_session_anchor() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("codex-model-switch-anchor");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let first = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_anchor_pro",
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "codex-thread-switch",
            "instructions": "answer briefly",
            "tools": [],
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "remember switch-anchor-marker" }]
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_anchor_flash",
            "model": "deepseek-v4-flash",
            "prompt_cache_key": "codex-thread-switch",
            "instructions": "answer briefly",
            "tools": [],
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "remember switch-anchor-marker" }]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "tool result acknowledged" }]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "what did I ask you to remember?" }]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 2);
    let second_messages = serde_json::to_string(&requests[1]["messages"]).unwrap();
    assert!(second_messages.contains("remember switch-anchor-marker"));
    assert!(second_messages.contains("what did I ask you to remember?"));
    assert_eq!(
        second_messages
            .matches("remember switch-anchor-marker")
            .count(),
        1
    );
    assert_eq!(
        second_messages.matches("tool result acknowledged").count(),
        1
    );

    let chain = store
        .response_context_chain("resp_anchor_flash", 2)
        .await
        .unwrap();
    assert_eq!(
        chain
            .last()
            .and_then(|record| record.previous_response_id.as_deref()),
        Some("resp_anchor_pro")
    );
    let (events, _) = store.recent_events(40, None).await.unwrap();
    let request_started = events
        .iter()
        .find(|event| {
            event.event_type == "request_started"
                && event
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("id"))
                    .and_then(Value::as_str)
                    == Some("resp_anchor_flash")
        })
        .expect("request_started event should be recorded");
    assert_eq!(
        request_started
            .detail
            .as_ref()
            .unwrap()
            .pointer("/previous_response_resolution/kind")
            .and_then(Value::as_str),
        Some("inferred_prompt_cache_anchor")
    );
    let context_compiled = events
        .iter()
        .find(|event| {
            event.event_type == "context_compile_diagnostic"
                && event
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("id"))
                    .and_then(Value::as_str)
                    == Some("resp_anchor_flash")
        })
        .expect("context_compile_diagnostic event should be recorded");
    assert_eq!(
        context_compiled
            .detail
            .as_ref()
            .unwrap()
            .pointer("/context/codex_full_context_replay/strategy")
            .and_then(Value::as_str),
        Some("canonical_authoritative_replay")
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_full_context_handoff_chain_advances_without_previous_response_id() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_chained_client_tool_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("codex-full-context-handoff-chain");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let mut replay_items = vec![json!({
        "type": "message",
        "role": "user",
        "content": [{ "type": "input_text", "text": "complete four client tool steps" }]
    })];
    for step in 0..=4 {
        let response = client
            .post(format!("http://{proxy_addr}/v1/responses"))
            .json(&json!({
                "id": format!("resp_chain_{step}"),
                "model": "deepseek-v4-pro",
                "prompt_cache_key": "codex-thread-handoff-chain",
                "client_metadata": { "x-codex-installation-id": "codex-chain-test" },
                "instructions": "Use the requested client tool and then continue.",
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "description": "Run a client shell command.",
                        "parameters": {
                            "type": "object",
                            "properties": { "command": { "type": "string" } },
                            "required": ["command"]
                        }
                    }
                }],
                "input": replay_items.clone()
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.json::<Value>().await.unwrap();
        let expected_anchor = format!("resp_chain_{step}");
        assert_eq!(
            store
                .latest_completed_context_response_for_prompt_cache_key(
                    "codex-thread-handoff-chain",
                )
                .await
                .unwrap()
                .as_deref(),
            Some(expected_anchor.as_str()),
            "completed client handoffs should advance the prompt-cache anchor"
        );
        if step == 4 {
            assert_eq!(
                body.pointer("/output/0/content/0/text")
                    .and_then(Value::as_str),
                Some("chain completed")
            );
            continue;
        }
        let tool_call = body
            .get("output")
            .and_then(Value::as_array)
            .and_then(|output| {
                output.iter().find(|item| {
                    matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("function_call") | Some("custom_tool_call")
                    )
                })
            })
            .cloned()
            .expect("client tool call");
        let call_id = tool_call
            .get("call_id")
            .and_then(Value::as_str)
            .expect("call id")
            .to_owned();
        replay_items.push(tool_call);
        replay_items.push(json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": format!("CHAIN_OUTPUT_{step}")
        }));
    }

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 5);
    for (request_index, request) in requests.iter().enumerate() {
        assert_chat_tool_calls_are_paired(request);
        let rendered = serde_json::to_string(&request["messages"]).unwrap();
        for marker_index in 0..request_index {
            assert_eq!(
                rendered
                    .matches(&format!("CHAIN_OUTPUT_{marker_index}"))
                    .count(),
                1,
                "request {request_index} should contain marker {marker_index} exactly once"
            );
        }
        let message_count = request["messages"].as_array().map(Vec::len).unwrap_or(0);
        assert!(
            message_count <= 3 + request_index * 3,
            "request {request_index} grew unexpectedly to {message_count} messages"
        );
    }

    let chain = store
        .response_context_chain("resp_chain_4", 10)
        .await
        .unwrap();
    assert_eq!(chain.len(), 5);
    for (index, record) in chain.iter().enumerate() {
        let expected_previous = (index > 0).then(|| format!("resp_chain_{}", index - 1));
        assert_eq!(record.previous_response_id, expected_previous);
        if index < 4 {
            let expected_call_id = format!("call_chain_{index}");
            assert!(record.turn_messages.iter().any(|message| {
                message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| {
                        calls.iter().any(|call| {
                            call.get("id").and_then(Value::as_str)
                                == Some(expected_call_id.as_str())
                        })
                    })
            }));
        }
    }

    let (events, _) = store.recent_events(200, None).await.unwrap();
    assert!(events
        .iter()
        .all(|event| event.event_type != "context_compacted"));
    for step in 0..=4 {
        let id = format!("resp_chain_{step}");
        let diagnostic = events
            .iter()
            .find(|event| {
                event.event_type == "context_compile_diagnostic"
                    && event
                        .detail
                        .as_ref()
                        .and_then(|detail| detail.get("id"))
                        .and_then(Value::as_str)
                        == Some(id.as_str())
            })
            .and_then(|event| event.detail.as_ref())
            .expect("context diagnostic");
        let strategy = diagnostic
            .pointer("/context/codex_full_context_replay/strategy")
            .and_then(Value::as_str);
        assert_eq!(
            strategy,
            Some("canonical_authoritative_replay"),
            "step {step} context diagnostic: {diagnostic}"
        );
    }

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_model_switch_keeps_client_replay_when_local_root_is_not_complete() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("codex-model-switch-long-root");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let first_input = (0..24)
        .map(|index| {
            json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": format!("legacy thread item {index}") }]
            })
        })
        .collect::<Vec<_>>();

    let client = reqwest::Client::new();
    let first = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_long_root_pro",
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "codex-thread-long-root",
            "instructions": "answer briefly",
            "tools": [],
            "input": first_input
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_long_root_flash",
            "model": "deepseek-v4-flash",
            "prompt_cache_key": "codex-thread-long-root",
            "instructions": "answer briefly",
            "tools": [],
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "legacy thread item 0" }]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "legacy answer that only Codex replay still has" }]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "continue safely" }]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 2);
    let second_messages = serde_json::to_string(&requests[1]["messages"]).unwrap();
    assert!(second_messages.contains("legacy answer that only Codex replay still has"));
    assert!(second_messages.contains("legacy thread item 0"));
    assert!(second_messages.contains("continue safely"));

    let (events, _) = store.recent_events(80, None).await.unwrap();
    let context_compiled = events
        .iter()
        .find(|event| {
            event.event_type == "context_compile_diagnostic"
                && event
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("id"))
                    .and_then(Value::as_str)
                    == Some("resp_long_root_flash")
        })
        .expect("context_compile_diagnostic event should be recorded");
    assert_eq!(
        context_compiled
            .detail
            .as_ref()
            .unwrap()
            .pointer("/context/codex_full_context_replay/strategy")
            .and_then(Value::as_str),
        Some("canonical_authoritative_replay")
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn non_full_context_continuation_warns_when_parent_input_was_not_stored() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("full-context-continuation-warning");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let input_items = (0..81)
        .map(|index| {
            json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": format!("full context item {index}") }]
            })
        })
        .collect::<Vec<_>>();
    store
        .checkpoint_request(
            "resp_full_context_parent",
            None,
            Some("deepseek-v4-pro"),
            &json!({
                "model": "deepseek-v4-pro",
                "prompt_cache_key": "thread-full-context",
                "instructions": "answer briefly",
                "tools": [],
                "input": input_items
            }),
        )
        .await
        .unwrap();
    store
        .replace_request_turn_messages(
            "resp_full_context_parent",
            &[json!({ "role": "user", "content": "summary replay survives" })],
        )
        .await
        .unwrap();
    store
        .finish_request(
            "resp_full_context_parent",
            RequestStatus::Completed,
            Some(&json!({
                "id": "resp_full_context_parent",
                "status": "completed",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "parent done" }]
                }]
            })),
            None,
        )
        .await
        .unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_after_full_context_parent",
            "model": "deepseek-v4-pro",
            "previous_response_id": "resp_full_context_parent",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "continue" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let (events, _) = store.recent_events(20, None).await.unwrap();
    let request_started = events
        .iter()
        .find(|event| {
            event.event_type == "request_started"
                && event
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("id"))
                    .and_then(Value::as_str)
                    == Some("resp_after_full_context_parent")
        })
        .expect("request_started event should be recorded");
    assert_eq!(
        request_started
            .detail
            .as_ref()
            .unwrap()
            .pointer("/runtime_context_storage/previous/full_context_not_stored")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        request_started
            .detail
            .as_ref()
            .unwrap()
            .pointer("/runtime_context_storage/continuation_warning")
            .and_then(Value::as_bool),
        Some(true)
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn responses_interrupted_previous_is_ignored_without_local_replay() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("interrupted-previous-conflict");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    store
        .checkpoint_request(
            "resp_interrupted_parent",
            None,
            Some("deepseek-v4-pro"),
            &json!({ "input": "first request" }),
        )
        .await
        .unwrap();
    store
        .interrupt_request_if_in_progress("resp_interrupted_parent", "client cancelled")
        .await
        .unwrap();
    let inspection_store = store.clone();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_continue_after_interrupted",
            "model": "deepseek-v4-pro",
            "previous_response_id": "resp_interrupted_parent",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "please continue" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .len(),
        1,
        "interrupted parent should not block Codex-provided input"
    );
    let (events, _) = inspection_store.recent_events(20, None).await.unwrap();
    let request_started = events
        .iter()
        .find(|event| event.event_type == "request_started")
        .expect("request_started event should be recorded");
    assert_eq!(
        request_started
            .detail
            .as_ref()
            .unwrap()
            .pointer("/previous_response_resolution/status")
            .and_then(Value::as_str),
        Some("interrupted")
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn responses_failed_previous_is_ignored_without_local_replay() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("failed-previous-conflict");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    store
        .checkpoint_request(
            "resp_failed_parent",
            None,
            Some("deepseek-v4-pro"),
            &json!({ "input": "first request" }),
        )
        .await
        .unwrap();
    store
        .finish_request(
            "resp_failed_parent",
            RequestStatus::Failed,
            None,
            Some(&json!({ "error": "upstream failed" })),
        )
        .await
        .unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_continue_after_failed",
            "model": "deepseek-v4-pro",
            "previous_response_id": "resp_failed_parent",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "please continue" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .len(),
        1,
        "failed parent should not block Codex-provided input"
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn responses_interrupted_previous_allows_codex_full_context() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("interrupted-previous-full-context");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    store
        .checkpoint_request(
            "resp_interrupted_parent_full",
            None,
            Some("deepseek-v4-pro"),
            &json!({ "input": "first request" }),
        )
        .await
        .unwrap();
    store
        .interrupt_request_if_in_progress("resp_interrupted_parent_full", "client cancelled")
        .await
        .unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let input_items = (0..81)
        .map(|index| {
            json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": format!("full context item {index}") }]
            })
        })
        .collect::<Vec<_>>();
    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_full_context_after_interrupted",
            "model": "deepseek-v4-pro",
            "previous_response_id": "resp_interrupted_parent_full",
            "prompt_cache_key": "thread-full-context",
            "instructions": "answer briefly",
            "tools": [],
            "input": input_items
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .len(),
        1
    );
    let chain = store
        .response_context_chain("resp_full_context_after_interrupted", 1)
        .await
        .unwrap();
    assert_eq!(chain[0].previous_response_id, None);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn current_input_tool_outputs_replay_as_chat_tool_protocol() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-current-tool-replay-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config_with_upstream(data_dir, fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_current_tool_replay",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "test a shell tool" }]
                },
                {
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": "create the test file first" }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_shell",
                    "name": "shell_command",
                    "arguments": "{\"command\":\"echo ok\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_shell",
                    "output": "Exit code: 0\nOutput:\nok\n"
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "continue" }]
                }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "shell_command",
                    "description": "Run shell command.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let messages = requests[0]["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|message| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && message.get("tool_calls").is_some()
        })
        .expect("upstream should receive the prior assistant tool call");
    assert_eq!(assistant["reasoning_content"], "create the test file first");
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"],
        "shell_command"
    );
    assert!(messages.iter().any(|message| {
        message.get("role").and_then(Value::as_str) == Some("tool")
            && message.get("tool_call_id").and_then(Value::as_str) == Some("call_shell")
            && message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("ok")
    }));
}

#[tokio::test]
async fn upstream_failure_records_retry_cache_diagnostic_without_prompt_text() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_failed_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("retry-cache-diagnostic");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_retry_cache_failure",
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "thread-cache-key",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "secret prompt should not be logged" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let (events, _) = store.recent_events(50, None).await.expect("events");
    let retry = events
        .iter()
        .find(|event| event.event_type == "retry_cache_diagnostic")
        .expect("retry/cache diagnostic should be recorded");
    assert_eq!(retry.audience.as_deref(), Some("safe_diagnostic"));
    let detail = retry.detail.as_ref().expect("detail");
    assert_eq!(detail["error_kind"], "upstream_status_failed_initial");
    assert!(detail.pointer("/request/request_hash").is_some());
    assert!(detail.pointer("/request/input_hash").is_some());
    assert!(detail.pointer("/payload/payload_hash").is_some());
    let detail_text = serde_json::to_string(detail).unwrap();
    assert!(!detail_text.contains("secret prompt should not be logged"));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_service_responses_requests_route_flash_disable_thinking_and_suppress_tools() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("service-no-tools");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let inspection_store = store.clone();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let client = reqwest::Client::new();
    for request in [
        thread_title_service_request("resp_service_title"),
        ambient_suggestions_service_request("resp_service_suggestions"),
        ambient_suggestion_safety_service_request("resp_service_safety"),
    ] {
        let response = client
            .post(format!("http://{proxy_addr}/v1/responses"))
            .json(&request)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
    }

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request["model"], MODEL_FLASH);
        assert_eq!(request["thinking"], json!({ "type": "disabled" }));
        assert!(
            request.get("tools").is_none(),
            "service requests should not receive proxy tools: {request}"
        );
        assert!(
            request.get("tool_choice").is_none(),
            "service requests should not receive tool_choice: {request}"
        );
        let messages = request["messages"]
            .as_array()
            .expect("service payload messages");
        assert!(
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("system")
                    && message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .contains("json")
            }),
            "service structured payload should include a JSON instruction: {request}"
        );
    }

    let summary = inspection_store.runtime_summary(10).await.unwrap();
    assert_eq!(summary.request_count, 0);
    assert!(summary.turn_history.is_empty());
    assert!(summary.last_turn.is_none());
    assert_eq!(summary.billable_request_count, 3);
    assert_eq!(summary.billable_history.len(), 3);
    assert!(summary
        .billable_history
        .iter()
        .all(|item| item.lifecycle == "service_ephemeral" && !item.conversation_turn));

    let (events, _) = inspection_store
        .recent_visible_events(20, None)
        .await
        .expect("visible events");
    let service_diagnostics = events
        .iter()
        .filter(|event| event.event_type == "service_request_diagnostic")
        .collect::<Vec<_>>();
    assert_eq!(service_diagnostics.len(), 3, "{events:?}");
    let diagnostics_text = serde_json::to_string(&service_diagnostics).unwrap();
    assert!(
        !diagnostics_text.contains("Large workspace context"),
        "default service diagnostics should not persist prompt text: {diagnostics_text}"
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_service_responses_request_suppresses_previous_history_replay() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("service-suppresses-previous");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    store
        .checkpoint_request(
            "resp_service_parent",
            None,
            Some("deepseek-v4-pro"),
            &json!({
                "model": "deepseek-v4-pro",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "PARENT_SECRET_SHOULD_NOT_REPLAY" }]
                }]
            }),
        )
        .await
        .unwrap();
    store
        .finish_request(
            "resp_service_parent",
            RequestStatus::Completed,
            Some(&json!({
                "model": "deepseek-v4-pro",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "PARENT_ASSISTANT_SECRET" }]
                }],
                "usage": { "input_tokens": 5, "output_tokens": 2, "total_tokens": 7 }
            })),
            None,
        )
        .await
        .unwrap();

    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let mut request = thread_title_service_request("resp_service_child");
    request["previous_response_id"] = json!("resp_service_parent");
    request["feature"] = json!("thread_title");
    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&request)
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let upstream_payload = serde_json::to_string(&requests[0]).unwrap();
    assert!(!upstream_payload.contains("PARENT_SECRET_SHOULD_NOT_REPLAY"));
    assert!(!upstream_payload.contains("PARENT_ASSISTANT_SECRET"));

    let chain = store
        .response_context_chain("resp_service_child", 1)
        .await
        .unwrap();
    assert_eq!(chain[0].previous_response_id, None);
    let (events, _) = store.recent_events(80, None).await.unwrap();
    let context_event = events
        .iter()
        .find(|event| {
            event.event_type == "context_compile_diagnostic"
                && event
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("id"))
                    .and_then(Value::as_str)
                    == Some("resp_service_child")
        })
        .expect("context diagnostic");
    assert_eq!(
        context_event
            .detail
            .as_ref()
            .and_then(|detail| detail.pointer("/context/history_messages"))
            .and_then(Value::as_u64),
        Some(0)
    );
    let started_event = events
        .iter()
        .find(|event| {
            event.event_type == "request_started"
                && event
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("id"))
                    .and_then(Value::as_str)
                    == Some("resp_service_child")
        })
        .expect("request started");
    assert_eq!(
        started_event
            .detail
            .as_ref()
            .and_then(|detail| detail.pointer("/previous_response_resolution/kind"))
            .and_then(Value::as_str),
        Some("suppressed_service")
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_service_chat_completion_suppresses_incoming_tools() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("service-chat-no-tools");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/chat/completions"))
        .json(&json!({
            "model": "gpt-5.4",
            "feature": "thread_title",
            "store": false,
            "messages": [{ "role": "user", "content": "Generate title." }],
            "reasoning": { "effort": "medium" },
            "tools": [{
                "type": "function",
                "function": {
                    "name": "plain_external_tool",
                    "description": "External client tool.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }],
            "tool_choice": { "type": "function", "function": { "name": "plain_external_tool" } },
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "thread_title",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }
            }
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], MODEL_FLASH);
    assert_eq!(requests[0]["thinking"], json!({ "type": "disabled" }));
    assert!(requests[0].get("tools").is_none(), "{}", requests[0]);
    assert!(requests[0].get("tool_choice").is_none(), "{}", requests[0]);

    let summary = store.runtime_summary(10).await.unwrap();
    assert_eq!(summary.request_count, 0);
    assert_eq!(summary.billable_request_count, 1);
    assert_eq!(summary.billable_history[0].lifecycle, "service_ephemeral");

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_codex_service_chat_completion_keeps_billable_usage() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_service_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("service-chat-stream-usage");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/chat/completions"))
        .json(&json!({
            "model": "gpt-5.4",
            "feature": "thread_title",
            "stream": true,
            "store": false,
            "messages": [{ "role": "user", "content": "Generate title." }],
            "reasoning": { "effort": "medium" },
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "thread_title",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }
            }
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("data: [DONE]"), "{body}");
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], MODEL_FLASH);
    assert_eq!(requests[0]["thinking"], json!({ "type": "disabled" }));
    assert_eq!(
        requests[0]["stream_options"],
        json!({ "include_usage": true })
    );

    let summary = store.runtime_summary(10).await.unwrap();
    assert_eq!(summary.request_count, 0);
    assert_eq!(summary.billable_request_count, 1);
    assert_eq!(summary.billable_history[0].lifecycle, "service_ephemeral");
    assert_eq!(summary.billable_history[0].total_tokens, 13);
    assert_eq!(summary.total_cached_input_tokens, 4);
    assert_eq!(summary.total_cache_miss_input_tokens, 6);
    assert_eq!(summary.total_output_tokens, 3);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_marker_responses_request_injects_synthetic_tool_search_upstream() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("codex-marker-tool-search");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_codex_marker_tool_search",
            "model": "deepseek-v4-pro",
            "stream": false,
            "client_metadata": {
                "x-codex-installation-id": "codex-install"
            },
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "Find deferred agent tools if needed." }]
            }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "plain_external_tool",
                    "description": "External client tool.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let upstream_tool_names = upstream_function_tool_names(&requests[0]);
    assert_default_codeseex_tools_present(&upstream_tool_names);
    assert!(
        upstream_tool_names.contains(&"plain_external_tool"),
        "{upstream_tool_names:?}"
    );
    assert!(
        upstream_tool_names.contains(&"tool_search_tool"),
        "{upstream_tool_names:?}"
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_marker_without_request_tools_keeps_codeseex_tools_and_tool_search_bridge() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("codex-marker-no-proxy-tools");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_codex_marker_no_proxy_tools",
            "model": "deepseek-v4-pro",
            "stream": false,
            "client_metadata": {
                "x-codex-installation-id": "codex-install"
            },
            "prompt_cache_key": "thread-cache-key",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "Reply OK." }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let upstream_tool_names = upstream_function_tool_names(&requests[0]);
    assert_default_codeseex_tools_present(&upstream_tool_names);
    assert!(
        upstream_tool_names.contains(&"tool_search_tool"),
        "{upstream_tool_names:?}"
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn ordinary_mini_responses_request_keeps_tools_and_final_turn_lifecycle() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("ordinary-mini-tools");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_ordinary_mini",
            "model": "gpt-5.4-mini",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "Use the external tool if needed." }]
            }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "plain_external_tool",
                    "description": "External client tool.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], "deepseek-v4-pro");
    let upstream_tool_names = upstream_function_tool_names(&requests[0]);
    assert_default_codeseex_tools_present(&upstream_tool_names);
    assert!(
        upstream_tool_names.contains(&"plain_external_tool"),
        "{upstream_tool_names:?}"
    );

    let summary = store.runtime_summary(10).await.unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.billable_request_count, 1);
    assert_eq!(summary.billable_history[0].lifecycle, "final_turn");
    assert!(summary.billable_history[0].conversation_turn);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn plain_external_feature_label_does_not_suppress_tools_or_turn_lifecycle() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("plain-feature-label-no-service");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_plain_feature_label",
            "model": "gpt-5.4",
            "feature": "thread_title",
            "stream": false,
            "reasoning": { "effort": "medium" },
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "Update the project title using tools if needed." }]
            }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "plain_external_tool",
                    "description": "External client tool.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], "deepseek-v4-pro");
    assert_eq!(requests[0]["thinking"], json!({ "type": "enabled" }));
    let upstream_tool_names = upstream_function_tool_names(&requests[0]);
    assert_default_codeseex_tools_present(&upstream_tool_names);
    assert!(
        upstream_tool_names.contains(&"plain_external_tool"),
        "{upstream_tool_names:?}"
    );

    let summary = store.runtime_summary(10).await.unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.billable_request_count, 1);
    assert_eq!(summary.billable_history[0].lifecycle, "final_turn");
    assert!(summary.billable_history[0].conversation_turn);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn plain_external_responses_request_does_not_inject_synthetic_tool_search_upstream() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("plain-external-no-tool-search");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_plain_external_no_tool_search",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "Use the external tool if needed." }]
            }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "plain_external_tool",
                    "description": "External client tool.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let upstream_tool_names = upstream_function_tool_names(&requests[0]);
    assert_default_codeseex_tools_present(&upstream_tool_names);
    assert!(
        upstream_tool_names.contains(&"plain_external_tool"),
        "{upstream_tool_names:?}"
    );
    assert!(
        !upstream_tool_names.contains(&"tool_search_tool"),
        "{upstream_tool_names:?}"
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn explicit_empty_enabled_tools_keeps_system_tools_and_diagnoses_disabled_configurables() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("empty-enabled-tools");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    write_enabled_tools_config(&config, Vec::new());
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_empty_enabled_tools",
            "model": "deepseek-v4-pro",
            "stream": false,
            "client_metadata": {
                "x-codex-installation-id": "codex-install"
            },
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "List files if tools are available." }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let upstream_tool_names = upstream_function_tool_names(&requests[0]);
    assert!(
        upstream_tool_names.contains(&"apply_patch"),
        "{upstream_tool_names:?}"
    );
    assert!(
        upstream_tool_names.contains(&"web_search"),
        "{upstream_tool_names:?}"
    );
    assert!(
        upstream_tool_names.contains(&"tool_search_tool"),
        "{upstream_tool_names:?}"
    );
    for name in [
        "list_directory",
        "read_file_range",
        "workspace_search",
        "vision_analyze",
        "image_gen",
    ] {
        assert!(
            !upstream_tool_names.contains(&name),
            "{upstream_tool_names:?}"
        );
    }

    let (events, _) = store.recent_events(10, None).await.unwrap();
    let diagnostic = events
        .iter()
        .find(|event| event.event_type == "tool_exposure_diagnostic")
        .and_then(|event| event.detail.as_ref())
        .expect("tool exposure diagnostic");
    assert_eq!(diagnostic["configurable_tools_disabled_by_config"], true);
    assert_eq!(diagnostic["codeseex_enabled_tools"]["count"], 0);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn previous_response_history_pairs_tool_outputs_with_parent_calls() {
    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-history-tool-pair-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config(data_dir);
    let store = Store::open(&config.data_dir).await.unwrap();
    let state = ProxyState::for_test(config, store);

    state
        .store
        .checkpoint_request(
            "resp_parent",
            None,
            Some("deepseek-v4-pro"),
            &json!({
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "run a shell command" }]
                }]
            }),
        )
        .await
        .unwrap();
    state
        .store
        .finish_request(
            "resp_parent",
            RequestStatus::Completed,
            Some(&json!({
                "output": [{
                    "type": "function_call",
                    "call_id": "call_shell",
                    "name": "shell_command",
                    "arguments": "{\"command\":\"echo ok\"}"
                }]
            })),
            None,
        )
        .await
        .unwrap();
    state
        .store
        .checkpoint_request(
            "resp_child",
            Some("resp_parent"),
            Some("deepseek-v4-pro"),
            &json!({
                "input": [{
                    "type": "function_call_output",
                    "call_id": "call_shell",
                    "output": "Exit code: 0\nOutput:\nok\n"
                }]
            }),
        )
        .await
        .unwrap();
    state
        .store
        .finish_request(
            "resp_child",
            RequestStatus::Completed,
            Some(&json!({
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "done" }]
                }]
            })),
            None,
        )
        .await
        .unwrap();

    let messages = response_history_messages(&state, Some("resp_child")).await;

    assert!(messages.iter().any(|message| {
        message.role == "assistant"
            && message
                .tool_calls
                .as_ref()
                .and_then(|calls| calls.first())
                .and_then(|call| call.pointer("/function/name"))
                .and_then(Value::as_str)
                == Some("shell_command")
    }));
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_shell")
            && message.content.contains("ok")
    }));
    assert!(messages
        .last()
        .map(|message| message.role == "assistant" && message.content == "done")
        .unwrap_or(false));
}

#[tokio::test]
async fn previous_response_history_prefers_persisted_turn_messages() {
    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-turn-message-history-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config(data_dir);
    let store = Store::open(&config.data_dir).await.unwrap();
    let state = ProxyState::for_test(config, store);

    state
        .store
        .checkpoint_request(
            "resp_turn",
            None,
            Some("deepseek-v4-pro"),
            &json!({"input":"ignored once turn messages exist"}),
        )
        .await
        .unwrap();
    state
        .store
        .replace_request_turn_messages(
            "resp_turn",
            &[
                json!({"role":"user","content":"list files"}),
                json!({
                    "role":"assistant",
                    "content":"",
                    "reasoning_content":"need directory first",
                    "tool_calls":[{
                        "id":"call_ls",
                        "type":"function",
                        "function":{"name":"list_directory","arguments":"{\"path\":\".\"}"}
                    }]
                }),
                json!({"role":"tool","tool_call_id":"call_ls","content":"Cargo.toml"}),
                json!({"role":"assistant","content":"I saw Cargo.toml."}),
            ],
        )
        .await
        .unwrap();
    state
        .store
        .finish_request(
            "resp_turn",
            RequestStatus::Completed,
            Some(&json!({
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "fallback must not duplicate" }]
                }]
            })),
            None,
        )
        .await
        .unwrap();

    let messages = response_history_messages(&state, Some("resp_turn")).await;
    assert_eq!(messages.len(), 4);
    assert_eq!(
        messages[1].reasoning_content.as_deref(),
        Some("need directory first")
    );
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("call_ls"));
    assert_eq!(messages[3].content, "I saw Cargo.toml.");
}

#[tokio::test]
async fn streaming_mixed_internal_and_external_tools_runs_internal_first() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_mixed_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-mixed-streaming-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config_with_upstream(data_dir, fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let inspection_store = store.clone();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_mixed_tool",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "use local files and js" }]
                }
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "plain_external_tool",
                        "description": "External client tool.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                }
            ]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body.contains("response.function_call_arguments.delta"),
        "{body}"
    );
    assert!(body.contains("\"name\":\"plain_external_tool\""), "{body}");
    assert!(!body.contains("已使用工具 `list_directory`"), "{body}");
    assert!(body.contains("\"type\":\"proxy_tool_call\""), "{body}");
    assert!(
        !body.contains(
            "\"name\":\"list_directory\",\"status\":\"completed\",\"type\":\"function_call\""
        ),
        "{body}"
    );
    assert!(!body.contains("unsupported call"), "{body}");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);

    let (events, _) = inspection_store.recent_events(20, None).await.unwrap();
    let completed = events
        .iter()
        .find(|event| event.event_type == "request_completed")
        .expect("client tool handoff completion should be observable");
    assert_eq!(
        completed
            .detail
            .as_ref()
            .and_then(|value| value.get("lifecycle")),
        Some(&json!("client_tool_handoff"))
    );
    assert_eq!(
        completed
            .detail
            .as_ref()
            .and_then(|value| value.get("total_tokens")),
        Some(&json!(10))
    );
    let summary = inspection_store.runtime_summary(10).await.unwrap();
    assert_eq!(summary.request_count, 0);
    assert!(summary.turn_history.is_empty());
    assert_eq!(summary.billable_request_count, 1);
    assert_eq!(summary.billable_history.len(), 1);
    assert_eq!(summary.billable_history[0].total_tokens, 10);
}

#[tokio::test]
async fn non_streaming_web_search_emits_replayable_output_item() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_web_search_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-web-search-non-stream-output-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config_with_upstream(data_dir, fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_non_stream_web_search_output",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "use web search" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("\"type\":\"web_search_call\""), "{body}");
    assert!(
        body.contains("\"type\":\"web_search_call_output\""),
        "{body}"
    );
    assert!(body.contains("missing_url"), "{body}");
    assert!(body.contains("web-search-result-handled"), "{body}");
    let call_item = body
        .find("\"type\":\"web_search_call\"")
        .expect("web_search call item should be present");
    let output_item = body
        .find("\"type\":\"web_search_call_output\"")
        .expect("web_search output item should be present");
    let final_text = body
        .find("web-search-result-handled")
        .expect("final answer should be present after tool result");
    assert!(call_item < output_item, "{body}");
    assert!(output_item < final_text, "{body}");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 2);
    let second_messages = requests[1]["messages"].as_array().unwrap();
    assert!(second_messages.iter().any(|message| {
        message.get("role").and_then(Value::as_str) == Some("tool")
            && message.get("tool_call_id").and_then(Value::as_str) == Some("call_web")
            && message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("missing_url")
    }));
}

#[tokio::test]
async fn streaming_web_search_emits_replayable_output_item() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_web_search_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-web-search-output-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config_with_upstream(data_dir, fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_web_search_output",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "use web search" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("\"type\":\"web_search_call\""), "{body}");
    assert!(
        body.contains("\"type\":\"web_search_call_output\""),
        "{body}"
    );
    assert!(body.contains("missing_url"), "{body}");
    assert!(body.contains("web-search-result-handled"), "{body}");
    let call_completed = body
        .find("response.web_search_call.completed")
        .expect("web_search call should complete");
    let output_item = body
        .find("\"type\":\"web_search_call_output\"")
        .expect("web_search output should be emitted");
    let final_text = body
        .find("web-search-result-handled")
        .expect("final answer should stream after tool result");
    assert!(call_completed < output_item, "{body}");
    assert!(output_item < final_text, "{body}");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 2);
    let second_messages = requests[1]["messages"].as_array().unwrap();
    assert!(second_messages.iter().any(|message| {
        message.get("role").and_then(Value::as_str) == Some("tool")
            && message.get("tool_call_id").and_then(Value::as_str) == Some("call_web")
            && message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("missing_url")
    }));
}

#[tokio::test]
async fn streaming_web_search_repeated_failure_recovers_with_final_answer() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_repeated_failing_web_search_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("streaming-web-search-failure-recovery");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_web_search_failure_recovery",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "keep searching but then recover" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body.contains("streaming recovered after repeated web search failures"),
        "{body}"
    );
    assert!(body.contains("response.completed"), "{body}");
    assert!(!body.contains("response.failed"), "{body}");
    assert!(!body.contains("tool_loop_repeated_failure\""), "{body}");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].get("tools").is_some());
    assert!(requests[1].get("tools").is_some());
    assert!(requests[2].get("tools").is_none());
    let recovery_messages = requests[2]["messages"].as_array().unwrap();
    assert!(recovery_messages.iter().any(|message| {
        message.get("role").and_then(Value::as_str) == Some("user")
            && message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("stopped the tool loop and removed tools")
    }));

    let (events, _) = store.recent_events(80, None).await.unwrap();
    assert!(events.iter().any(|event| {
        event.event_type == "tool_loop_repeated_failure_stopped"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.get("recover_with_final_response"))
                .and_then(Value::as_bool)
                == Some(true)
    }));
    assert!(!events
        .iter()
        .any(|event| event.event_type == "request_failed"));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_web_search_budget_recovers_with_final_answer() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_budgeted_web_search_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("streaming-web-search-budget-recovery");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_web_search_budget_recovery",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "keep searching distinct topics but then recover" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body.contains("streaming recovered after the web search budget"),
        "{body}"
    );
    assert!(body.contains("response.completed"), "{body}");
    assert!(!body.contains("response.failed"), "{body}");
    assert!(!body.contains("tool_loop_web_search_budget\""), "{body}");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert!(requests.len() >= 2, "{requests:?}");
    let (recovery_request, tool_requests) = requests.split_last().expect("at least one request");
    assert!(recovery_request.get("tools").is_none());
    assert!(
        tool_requests
            .iter()
            .all(|request| request.get("tools").is_some()),
        "{requests:?}"
    );
    let recovery_messages = recovery_request["messages"].as_array().unwrap();
    assert!(
        recovery_messages.iter().any(|message| {
            message.get("role").and_then(Value::as_str) == Some("user")
                && message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains("CodeSeeX stopped the tool loop")
                && message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains("Do not request or encode additional tool calls")
        }),
        "{}",
        serde_json::to_string_pretty(recovery_messages).unwrap()
    );

    let (events, _) = store.recent_events(80, None).await.unwrap();
    assert!(events.iter().any(|event| {
        matches!(
            event.event_type.as_str(),
            "tool_loop_web_search_budget_stopped" | "tool_loop_repeated_failure_stopped"
        ) && event
            .detail
            .as_ref()
            .and_then(|detail| detail.get("recover_with_final_response"))
            .and_then(Value::as_bool)
            == Some(true)
    }));
    assert!(!events
        .iter()
        .any(|event| event.event_type == "request_failed"));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_recovery_blocks_deepseek_tool_protocol_when_tools_are_removed() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_budgeted_web_search_then_deepseek_protocol_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("streaming-recovery-blocks-deepseek-protocol");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_recovery_blocks_deepseek_protocol",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "keep searching until the recovery turn emits a provider tool protocol block" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("response.completed"), "{body}");
    assert!(!body.contains("response.failed"), "{body}");
    assert!(
        !body.contains(&format!("<{}DSML", "\u{ff5c}\u{ff5c}")),
        "{body}"
    );
    assert!(!body.contains("\\uff5c\\uff5cDSML"), "{body}");
    assert!(!body.contains("invoke name=\\\"web_search\\\""), "{body}");

    let (events, _) = store.recent_events(120, None).await.unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == "deepseek_tool_protocol_blocked"));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_recovery_blocks_deepseek_reasoning_tool_protocol_when_tools_are_removed() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_budgeted_web_search_then_deepseek_reasoning_protocol_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("streaming-recovery-blocks-deepseek-reasoning-protocol");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_recovery_blocks_deepseek_reasoning_protocol",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "keep searching until recovery reasoning emits protocol" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body.contains("recovered without leaking reasoning protocol"),
        "{body}"
    );
    assert!(body.contains("response.completed"), "{body}");
    assert!(!body.contains("response.failed"), "{body}");
    assert!(
        !body.contains(&format!("<{}DSML", "\u{ff5c}\u{ff5c}")),
        "{body}"
    );
    assert!(!body.contains("\\uff5c\\uff5cDSML"), "{body}");
    assert!(!body.contains("retry reasoning"), "{body}");

    let (events, _) = store.recent_events(140, None).await.unwrap();
    assert!(events.iter().any(|event| {
        event.event_type == "deepseek_tool_protocol_blocked"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.get("channel"))
                .and_then(Value::as_str)
                == Some("reasoning_content")
    }));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_adapts_deepseek_tool_protocol_from_reasoning_content() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_deepseek_reasoning_protocol_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("streaming-adapts-deepseek-reasoning-protocol");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_adapts_deepseek_reasoning_protocol",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "reasoning emits provider tool protocol" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body.contains("response.web_search_call.searching"),
        "{body}"
    );
    assert!(
        body.contains("used reasoning protocol tool result"),
        "{body}"
    );
    assert!(
        !body.contains(&format!("<{}DSML", "\u{ff5c}\u{ff5c}")),
        "{body}"
    );
    assert!(!body.contains("\\uff5c\\uff5cDSML"), "{body}");
    assert!(!body.contains("invoke name="), "{body}");

    let (events, _) = store.recent_events(120, None).await.unwrap();
    assert!(events.iter().any(|event| {
        event.event_type == "deepseek_tool_protocol_adapted"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.get("channel"))
                .and_then(Value::as_str)
                == Some("reasoning_content")
    }));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_custom_upstream_does_not_apply_deepseek_tool_protocol_adapter() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_protocol_marker_text_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("streaming-custom-upstream-no-deepseek-adapter");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_custom_upstream_protocol_text",
            "model": "gpt-4o",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "echo provider protocol marker text" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("response.completed"), "{body}");
    assert!(
        body.contains(&format!("<{}DSML", "\u{ff5c}\u{ff5c}")),
        "{body}"
    );

    let (events, _) = store.recent_events(120, None).await.unwrap();
    assert!(!events
        .iter()
        .any(|event| event.event_type.starts_with("deepseek_tool_protocol_")));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_recovery_blocks_standard_tool_calls_when_tools_are_removed() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_budgeted_web_search_then_tool_delta_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("streaming-recovery-blocks-standard-tools");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_recovery_blocks_standard_tool",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "keep searching until the recovery turn emits tool delta" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("response.completed"), "{body}");
    assert!(!body.contains("response.failed"), "{body}");
    assert!(!body.contains("call_recovery"), "{body}");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert!(requests.len() >= 2, "{requests:?}");
    let (recovery_request, tool_requests) = requests.split_last().expect("at least one request");
    assert!(recovery_request.get("tools").is_none());
    assert!(
        tool_requests
            .iter()
            .all(|request| request.get("tools").is_some()),
        "{requests:?}"
    );

    let (events, _) = store.recent_events(120, None).await.unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == "streaming_tool_call_blocked"));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn non_streaming_vision_result_uses_tool_schema_without_system_instruction() {
    let chat_state = FakeUpstreamState::default();
    let chat_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let chat_addr = chat_listener.local_addr().unwrap();
    let chat_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_vision_instruction_chat_completions),
        )
        .with_state(chat_state.clone());
    tokio::spawn(async move {
        axum::serve(chat_listener, chat_app).await.unwrap();
    });

    let vision_state = FakeUpstreamState::default();
    let vision_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let vision_addr = vision_listener.local_addr().unwrap();
    let vision_app = Router::new()
        .route("/v1/responses", post(fake_vision_responses))
        .with_state(vision_state.clone());
    tokio::spawn(async move {
        axum::serve(vision_listener, vision_app).await.unwrap();
    });

    let data_dir = temp_workspace("vision-instruction-non-stream");
    let config = test_config_with_upstream(data_dir.clone(), chat_addr);
    write_vision_analyze_config(&config, &format!("http://{vision_addr}/v1/responses"));
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_vision_instruction_non_stream",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "analyze the image" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();

    let serialized = serde_json::to_string(&body).unwrap();
    assert!(serialized.contains("VISION EXACT LINE 1\\nVISION EXACT LINE 2"));
    assert!(serialized.contains("MODEL CONTINUATION AFTER VISION"));
    let chat_requests = chat_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(
        chat_requests.len(),
        2,
        "vision result should continue through a second chat completion"
    );
    assert_vision_tool_result_without_system_instruction(&chat_requests[1], "call_vision_exact");
    assert_eq!(
        vision_state
            .requests
            .lock()
            .expect("fake vision lock poisoned")
            .len(),
        1
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn responses_accepts_large_client_image_output_and_stores_redacted_input() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_final_chat_completions))
        .with_state(fake_state);
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("large-client-image-output");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .layer(DefaultBodyLimit::max(RESPONSES_BODY_LIMIT_BYTES))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let large_image = format!("data:image/png;base64,{}", "A".repeat(3 * 1024 * 1024));
    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_large_client_image_output",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [{
                "type": "function_call_output",
                "call_id": "call_view_image",
                "output": [{
                    "type": "input_image",
                    "image_url": large_image
                }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let _body = response.json::<Value>().await.unwrap();
    let chain = store
        .response_context_chain("resp_large_client_image_output", 1)
        .await
        .expect("chain");
    let stored = serde_json::to_string(&chain[0].input).expect("stored input json");
    assert!(!stored.contains("data:image/png;base64"));
    assert!(!stored.contains(&"A".repeat(128)));
    assert!(stored.contains("redacted inline data url"));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn non_streaming_proxy_tools_execute_parallel_batch_and_preserve_order() {
    let chat_state = FakeUpstreamState::default();
    let chat_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let chat_addr = chat_listener.local_addr().unwrap();
    let chat_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_parallel_vision_chat_completions),
        )
        .with_state(chat_state.clone());
    tokio::spawn(async move {
        axum::serve(chat_listener, chat_app).await.unwrap();
    });

    let vision_state = FakeUpstreamState::default();
    let vision_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let vision_addr = vision_listener.local_addr().unwrap();
    let vision_app = Router::new()
        .route("/v1/responses", post(fake_delayed_vision_responses))
        .with_state(vision_state.clone());
    tokio::spawn(async move {
        axum::serve(vision_listener, vision_app).await.unwrap();
    });

    let data_dir = temp_workspace("parallel-vision-tools");
    let config = test_config_with_upstream(data_dir.clone(), chat_addr);
    write_vision_analyze_config(&config, &format!("http://{vision_addr}/v1/responses"));
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let started = std::time::Instant::now();
    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_parallel_vision_tools",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "inspect two images" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "tool calls should not hang, elapsed={elapsed:?}"
    );
    assert!(
        vision_state
            .max_concurrent_requests
            .load(AtomicOrdering::SeqCst)
            >= 2,
        "tool calls should run concurrently"
    );
    assert!(serde_json::to_string(&body)
        .unwrap()
        .contains("parallel vision handled"));
    assert_eq!(
        vision_state
            .requests
            .lock()
            .expect("fake vision lock poisoned")
            .len(),
        2
    );
    let chat_requests = chat_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(chat_requests.len(), 2);
    let messages = chat_requests[1]["messages"].as_array().unwrap();
    let assistant_index = messages
        .iter()
        .position(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("assistant tool call message");
    let ordered_tool_ids = messages[assistant_index + 1..assistant_index + 3]
        .iter()
        .map(|message| message.get("tool_call_id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        ordered_tool_ids,
        vec![Some("call_vision_a"), Some("call_vision_b")]
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn repeated_failing_proxy_tool_calls_stop_before_unbounded_retry() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_repeated_failing_list_directory_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("repeated-failing-tool-stop");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_repeated_failing_tool",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "list parent repeatedly" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response.text().await.unwrap();
    assert!(body.contains("repeated failing tool retries"), "{body}");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(
        requests.len(),
        3,
        "proxy should stop after the third repeated failing tool result"
    );

    let (events, _) = store.recent_events(80, None).await.unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == "tool_loop_repeated_failure_stopped"));
    assert!(events.iter().any(|event| {
        event.event_type == "request_failed"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.get("id"))
                .and_then(Value::as_str)
                == Some("resp_repeated_failing_tool")
    }));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn repeated_failing_web_search_stops_after_two_failures() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_repeated_failing_web_search_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("repeated-failing-web-search-stop");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_repeated_failing_web_search",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "keep searching without url" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("available search diagnostics"), "{body}");
    assert_eq!(
        fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .len(),
        3
    );
    let (events, _) = store.recent_events(80, None).await.unwrap();
    assert!(events.iter().any(|event| {
        event.event_type == "tool_loop_repeated_failure_stopped"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.get("recover_with_final_response"))
                .and_then(Value::as_bool)
                == Some(true)
    }));
    assert!(!events.iter().any(|event| {
        event.event_type == "request_failed"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.get("id"))
                .and_then(Value::as_str)
                == Some("resp_repeated_failing_web_search")
    }));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn consecutive_failing_proxy_tool_calls_stop_even_when_arguments_change() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_repeated_failing_list_directory_changing_args_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("consecutive-failing-tool-stop");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_consecutive_failing_tool",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "keep trying bad paths" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response.text().await.unwrap();
    assert!(body.contains("failed 3 consecutive times"), "{body}");
    assert_eq!(
        fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .len(),
        3
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn successful_tool_loop_stops_at_hard_iteration_limit_and_remains_billable() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_successful_list_directory_forever_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("successful-tool-loop-limit");
    let workspace_root = data_dir.join("workspace");
    std::fs::create_dir_all(workspace_root.join("src")).expect("create workspace");
    std::fs::write(workspace_root.join("README.md"), "loop test").expect("write workspace file");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_successful_tool_loop_limit",
            "model": "deepseek-v4-pro",
            "stream": false,
            "input": [{
                "type": "environment_context",
                "content": format!(
                    "<environment_context>\n  <cwd>{}</cwd>\n  <filesystem><workspace_roots><root>{}</root></workspace_roots></filesystem>\n</environment_context>",
                    workspace_root.display(),
                    workspace_root.display()
                )
            }, {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "keep listing the workspace forever" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response.text().await.unwrap();
    assert!(body.contains("Tool loop exceeded"), "{body}");
    assert_eq!(
        fake_state
            .requests
            .lock()
            .expect("fake upstream lock poisoned")
            .len(),
        usize::try_from(MAX_TOOL_LOOP_ITERATIONS).unwrap()
    );

    let (events, _) = store.recent_events(120, None).await.unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == "tool_loop_iteration_limit_stopped"));
    let summary = store.runtime_summary(10).await.unwrap();
    assert_eq!(summary.request_count, 0);
    assert_eq!(summary.failed_request_count, 1);
    assert_eq!(summary.billable_request_count, 1);
    assert_eq!(
        summary.total_output_tokens,
        u64::from(MAX_TOOL_LOOP_ITERATIONS) * 2
    );
    assert!(summary.last_turn.is_none());
    assert_eq!(summary.billable_history[0].lifecycle, "failed_billable");

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_vision_result_uses_tool_schema_without_system_instruction() {
    let chat_state = FakeUpstreamState::default();
    let chat_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let chat_addr = chat_listener.local_addr().unwrap();
    let chat_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_vision_instruction_streaming_chat_completions),
        )
        .with_state(chat_state.clone());
    tokio::spawn(async move {
        axum::serve(chat_listener, chat_app).await.unwrap();
    });

    let vision_state = FakeUpstreamState::default();
    let vision_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let vision_addr = vision_listener.local_addr().unwrap();
    let vision_app = Router::new()
        .route("/v1/responses", post(fake_vision_responses))
        .with_state(vision_state.clone());
    tokio::spawn(async move {
        axum::serve(vision_listener, vision_app).await.unwrap();
    });

    let data_dir = temp_workspace("vision-instruction-stream");
    let config = test_config_with_upstream(data_dir.clone(), chat_addr);
    write_vision_analyze_config(&config, &format!("http://{vision_addr}/v1/responses"));
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_vision_instruction_stream",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "analyze the image" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("VISION EXACT LINE 1"), "{body}");
    assert!(body.contains("VISION EXACT LINE 2"), "{body}");
    assert!(body.contains("MODEL CONTINUATION AFTER VISION"), "{body}");
    let chat_requests = chat_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(
        chat_requests.len(),
        2,
        "vision result should continue through a second streaming chat completion"
    );
    assert_vision_tool_result_without_system_instruction(&chat_requests[1], "call_vision_exact");
    assert_eq!(
        vision_state
            .requests
            .lock()
            .expect("fake vision lock poisoned")
            .len(),
        1
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn responses_do_not_recover_global_web_search_facts_when_client_returns_call_without_output()
{
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_reasoning_then_content_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-web-search-recovery-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    store
        .checkpoint_request(
            "resp_prior_web",
            None,
            Some("deepseek-v4-pro"),
            &json!({
                "input": [{"role":"user","content":[{"type":"input_text","text":"weather"}]}]
            }),
        )
        .await
        .unwrap();
    store
        .append_request_tool_fact(
            "resp_prior_web",
            "tool=web_search call_id=call_web arguments={\"mode\":\"search\",\"query\":\"Shanghai weather\"} ok=true result={\"summary\":\"light rain\"}",
        )
        .await
        .unwrap();
    store
        .finish_request(
            "resp_prior_web",
            RequestStatus::Completed,
            Some(&json!({
                "output": [{
                    "type":"web_search_call",
                    "status":"completed",
                    "action":{"type":"search","query":"Shanghai weather"}
                }]
            })),
            None,
        )
        .await
        .unwrap();

    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_client_returned_web_call_only",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [
                {
                    "type":"web_search_call",
                    "status":"completed",
                    "action":{"type":"search","query":"Shanghai weather"}
                },
                {
                    "type":"message",
                    "role":"user",
                    "content":[{"type":"input_text","text":"what happened with web_search?"}]
                }
            ]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("final answer"), "{body}");
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let upstream_messages = serde_json::to_string(&requests[0]["messages"]).unwrap();
    assert!(
        upstream_messages.contains("Verified prior tool/request facts from the client context"),
        "{upstream_messages}"
    );
    assert!(
        upstream_messages.contains("Shanghai weather"),
        "{upstream_messages}"
    );
    assert!(
        !upstream_messages.contains("light rain"),
        "{upstream_messages}"
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn responses_do_not_recover_global_empty_web_search_fact_when_prior_final_text_matches() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_reasoning_then_content_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-web-search-empty-recovery-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    store
        .checkpoint_request(
            "resp_prior_web",
            None,
            Some("deepseek-v4-pro"),
            &json!({
                "input": [{"role":"user","content":[{"type":"input_text","text":"weather"}]}]
            }),
        )
        .await
        .unwrap();
    store
        .append_request_tool_fact(
            "resp_prior_web",
            "tool=web_search call_id=call_web arguments={\"mode\":\"open\"} ok=false result={\"error\":\"missing_url\"}",
        )
        .await
        .unwrap();
    store
        .finish_request(
            "resp_prior_web",
            RequestStatus::Completed,
            Some(&json!({
                "output": [
                    {
                        "type":"web_search_call",
                        "status":"completed",
                        "action":{"type":"open_page"}
                    },
                    {
                        "type":"message",
                        "role":"assistant",
                        "content":[{"type":"output_text","text":"prior final answer"}]
                    }
                ]
            })),
            None,
        )
        .await
        .unwrap();

    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_client_returned_empty_web_call",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [
                {
                    "type":"web_search_call",
                    "status":"completed",
                    "action":{"type":"open_page"}
                },
                {
                    "type":"message",
                    "role":"assistant",
                    "content":[{"type":"output_text","text":"prior final answer"}]
                },
                {
                    "type":"message",
                    "role":"user",
                    "content":[{"type":"input_text","text":"what happened with web_search?"}]
                }
            ]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("final answer"), "{body}");
    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let upstream_messages = serde_json::to_string(&requests[0]["messages"]).unwrap();
    assert!(
        upstream_messages.contains("Verified prior tool/request facts from the client context"),
        "{upstream_messages}"
    );
    assert!(
        !upstream_messages.contains("missing_url"),
        "{upstream_messages}"
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_apply_patch_returns_native_custom_tool_call() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_apply_patch_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-apply-patch-streaming-test-{}",
        Uuid::new_v4().simple()
    ));
    let patch_dir = PathBuf::from("target").join("codeseex-apply-patch-streaming-test");
    std::fs::create_dir_all(&patch_dir).expect("create ignored apply_patch test directory");
    let patch_file = patch_dir.join("hello.txt");
    let _ = std::fs::remove_file(&patch_file);
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_apply_patch",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "patch a file" }]
                }
            ]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("\"type\":\"custom_tool_call\""), "{body}");
    assert!(body.contains("\"name\":\"apply_patch\""), "{body}");
    assert!(body.contains("*** Begin Patch"), "{body}");
    assert!(
        body.matches("response.custom_tool_call_input.delta")
            .count()
            >= 2,
        "{body}"
    );
    assert!(!body.contains("patch-ok"), "{body}");
    assert!(body.contains("encrypted_content"), "{body}");
    let thinking_done = body
        .find("response.output_text.done")
        .expect("thinking display should close before native tool completion");
    let completed = body
        .find("response.completed")
        .expect("stream should complete");
    assert!(thinking_done < completed, "{body}");
    assert!(
        !patch_file.exists(),
        "proxy must not execute native apply_patch"
    );
    let _ = std::fs::remove_file(&patch_file);

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);

    let store = Store::open(&data_dir).await.unwrap();
    let summary = store.runtime_summary(10).await.expect("runtime summary");
    assert_eq!(summary.request_count, 0);
    assert_eq!(summary.billable_request_count, 1);
    assert_eq!(summary.billable_history.len(), 1);
    assert_eq!(summary.billable_history[0].lifecycle, "client_tool_handoff");
    assert_eq!(summary.billable_history[0].total_tokens, 6);

    let (events, _) = store
        .recent_visible_events(20, None)
        .await
        .expect("visible events");
    let completed = events
        .iter()
        .find(|event| event.event_type == "request_completed")
        .expect("streaming client tool handoff completion should be observable");
    assert_eq!(
        completed
            .detail
            .as_ref()
            .and_then(|value| value.get("lifecycle")),
        Some(&json!("client_tool_handoff"))
    );
}

#[tokio::test]
async fn nonstreaming_client_tool_handoff_is_not_user_completed_turn() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route("/chat/completions", post(fake_apply_patch_chat_completions))
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("apply-patch-nonstream-handoff");
    let patch_dir = PathBuf::from("target").join("codeseex-apply-patch-nonstream-test");
    std::fs::create_dir_all(&patch_dir).expect("create ignored apply_patch test directory");
    let patch_file = patch_dir.join("hello.txt");
    let _ = std::fs::remove_file(&patch_file);
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_apply_patch_handoff",
            "model": "deepseek-v4-pro",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "patch a file" }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    let body = response.json::<Value>().await.unwrap();
    let output = body["output"].as_array().unwrap();
    assert!(output.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("custom_tool_call")
            && item.get("name").and_then(Value::as_str) == Some("apply_patch")
    }));
    assert!(
        !patch_file.exists(),
        "proxy must not execute native apply_patch"
    );

    let summary = store.runtime_summary(10).await.expect("runtime summary");
    assert_eq!(summary.request_count, 0);
    assert_eq!(summary.billable_request_count, 1);
    assert_eq!(summary.billable_history.len(), 1);
    assert_eq!(summary.total_output_tokens, 4);
    let (events, _) = store
        .recent_visible_events(20, None)
        .await
        .expect("visible events");
    let completed = events
        .iter()
        .find(|event| event.event_type == "request_completed")
        .expect("client tool handoff completion should be observable");
    assert_eq!(
        completed
            .detail
            .as_ref()
            .and_then(|value| value.get("lifecycle")),
        Some(&json!("client_tool_handoff"))
    );
    let (all_events, _) = store.recent_events(50, None).await.expect("all events");
    let handoff_diagnostic = all_events
        .iter()
        .find(|event| event.event_type == "client_tool_handoff_diagnostic")
        .expect("client handoff diagnostic should be recorded");
    assert_eq!(
        handoff_diagnostic.audience.as_deref(),
        Some("safe_diagnostic")
    );
    assert_eq!(
        handoff_diagnostic
            .detail
            .as_ref()
            .and_then(|detail| detail.pointer("/tools/counts/native"))
            .and_then(Value::as_u64),
        Some(1)
    );
    assert!(handoff_diagnostic
        .detail
        .as_ref()
        .and_then(|detail| detail.pointer("/usage/total_tokens"))
        .and_then(Value::as_u64)
        .is_some());
    assert!(all_events
        .iter()
        .any(|event| event.event_type == "upstream_call_usage_breakdown"
            && event.audience.as_deref() == Some("safe_diagnostic")));

    let _ = std::fs::remove_file(&patch_file);
    let _ = std::fs::remove_dir_all(patch_dir);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn nonstreaming_apply_patch_handoff_repairs_blank_context_line() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_apply_patch_blank_context_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("apply-patch-blank-context-handoff");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_apply_patch_blank_context_handoff",
            "model": "deepseek-v4-pro",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "patch a file" }]
            }]
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();

    let patch_input = body["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| {
            item.get("type").and_then(Value::as_str) == Some("custom_tool_call")
                && item.get("name").and_then(Value::as_str) == Some("apply_patch")
        })
        .and_then(|item| item.get("input"))
        .and_then(Value::as_str)
        .expect("apply_patch handoff should include native input");

    assert!(
        patch_input.contains(" fn before() {}\n \n pub fn after() {}"),
        "{patch_input}"
    );
    assert!(
        !patch_input.contains(" fn before() {}\n\n pub fn after() {}"),
        "{patch_input}"
    );

    let (events, _) = store.recent_events(50, None).await.expect("events");
    let diagnostic = events
        .iter()
        .find(|event| event.event_type == "apply_patch_input_micro_repair_diagnostic")
        .expect("micro repair diagnostic should be recorded");
    assert_eq!(diagnostic.audience.as_deref(), Some("safe_diagnostic"));
    let detail = diagnostic.detail.as_ref().expect("diagnostic detail");
    assert_eq!(detail["id"], "resp_apply_patch_blank_context_handoff");
    assert_eq!(detail["call_id"], "call_patch");
    assert_eq!(detail["tool_name"], "apply_patch");
    assert_eq!(detail["repair_kind"], "blank_update_hunk_context_line");
    assert_eq!(detail["blank_context_lines_repaired"], 1);
    assert!(detail["input_chars"].as_u64().is_some());
    let detail_json = serde_json::to_string(detail).expect("diagnostic json");
    assert!(!detail_json.contains("fn before"));
    assert!(!detail_json.contains("pub fn after"));
    assert!(!detail_json.contains("*** Begin Patch"));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn apply_patch_client_handoff_failure_can_be_replayed_and_corrected() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_apply_patch_recovery_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("apply-patch-client-recovery");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let first = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_apply_patch_recovery_first",
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "apply-patch-recovery-thread",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "patch a file" }]
            }]
        }))
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());
    let first_body = first.json::<Value>().await.unwrap();
    let first_call = first_body["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("custom_tool_call"))
        .expect("first apply_patch handoff")
        .clone();
    assert_eq!(first_call["name"], "apply_patch");

    let second = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_apply_patch_recovery_second",
            "previous_response_id": "resp_apply_patch_recovery_first",
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "apply-patch-recovery-thread",
            "input": [
                first_call,
                {
                    "type": "custom_tool_call_output",
                    "call_id": "call_patch",
                    "output": "apply_patch verification failed: invalid patch"
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(second.status().is_success());
    let second_body = second.json::<Value>().await.unwrap();
    let second_call = second_body["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("custom_tool_call"))
        .expect("corrected apply_patch handoff")
        .clone();
    assert_eq!(second_call["name"], "apply_patch");
    assert!(second_call["input"]
        .as_str()
        .unwrap_or_default()
        .contains("+fixed"));
    assert_eq!(
        second_body.pointer("/incomplete_details/reason"),
        None,
        "guard must not stop a corrected second attempt"
    );

    let third = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_apply_patch_recovery_third",
            "previous_response_id": "resp_apply_patch_recovery_second",
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "apply-patch-recovery-thread",
            "input": [
                second_call,
                {
                    "type": "custom_tool_call_output",
                    "call_id": "call_patch",
                    "output": "Success. Updated the following files: A target/codeseex-apply-patch-recovery-test/hello.txt"
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(third.status().is_success());
    let third_body = third.json::<Value>().await.unwrap();
    assert_eq!(third_body["status"], "completed");
    assert!(
        serde_json::to_string(&third_body)
            .unwrap()
            .contains("patch recovered"),
        "{third_body}"
    );

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 3);
    let second_messages = serde_json::to_string(&requests[1]["messages"]).unwrap();
    assert!(second_messages.contains("\"tool_call_id\":\"call_patch\""));
    assert!(second_messages.contains("apply_patch verification failed"));
    assert!(second_messages.contains("\\\"patch\\\""));
    let third_messages = serde_json::to_string(&requests[2]["messages"]).unwrap();
    assert!(third_messages.contains("Success. Updated the following files"));
    assert!(third_messages.contains("+fixed"));

    let summary = store.runtime_summary(10).await.expect("runtime summary");
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.billable_request_count, 3);
    assert_eq!(summary.turn_history.len(), 1);
    assert_eq!(summary.billable_history[0].lifecycle, "client_tool_handoff");
    assert_eq!(summary.billable_history[1].lifecycle, "client_tool_handoff");
    assert_eq!(summary.billable_history[2].lifecycle, "final_turn");
    assert!(summary.billable_history[2].conversation_turn);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn client_tool_handoff_guard_diagnoses_repeated_failures_without_sticky_stop() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_repeated_shell_handoff_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("client-handoff-guard-failures");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let client = reqwest::Client::new();
    for index in 0..3 {
        let response = client
            .post(format!("http://{proxy_addr}/v1/responses"))
            .json(&json!({
                "id": format!("resp_guard_handoff_{index}"),
                "model": "deepseek-v4-pro",
                "prompt_cache_key": "guard-thread",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "run a client command" }]
                }],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "description": "Run a shell command.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                }]
            }))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let body = response.json::<Value>().await.unwrap();
        let call = body["output"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .expect("function call");
        let call_id = call
            .get("call_id")
            .and_then(Value::as_str)
            .expect("call id")
            .to_owned();

        let followup = client
            .post(format!("http://{proxy_addr}/v1/responses"))
            .json(&json!({
                "id": format!("resp_guard_followup_{index}"),
                "model": "deepseek-v4-pro",
                "prompt_cache_key": "guard-thread",
                "input": [{
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": "Error: shell execution failed"
                }],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "description": "Run a shell command.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                }]
            }))
            .send()
            .await
            .unwrap();
        assert!(followup.status().is_success());
        let body = followup.json::<Value>().await.unwrap();
        assert_eq!(
            body["status"], "completed",
            "repeated client tool failures should stay agent-visible instead of becoming a terminal proxy error"
        );
    }

    let continuation = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_guard_after_stop",
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "guard-thread",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "run a client command again" }]
            }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "shell_command",
                    "description": "Run a shell command.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]
        }))
        .send()
        .await
        .unwrap();
    assert!(continuation.status().is_success());
    let body = continuation.json::<Value>().await.unwrap();
    assert_eq!(
        body["status"], "completed",
        "later continue-style requests must not inherit a sticky guard stop"
    );
    assert!(body
        .get("output")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false));

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(
        requests.len(),
        7,
        "diagnostic-only guard should not short-circuit upstream retries or later continue requests"
    );
    let (events, _) = store.recent_events(80, None).await.expect("events");
    assert!(!events
        .iter()
        .any(|event| { event.event_type == "client_tool_handoff_guard_diagnostic" }));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn codex_shaped_client_handoff_avoids_proxy_tool_and_context_pollution() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_codex_handoff_then_final_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = temp_workspace("codex-shaped-handoff-no-pollution");
    let config = test_config_with_upstream(data_dir.clone(), fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store.clone());
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let full_context_items = (0..95)
        .map(|index| {
            json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": format!("prior compact context item {index}") }]
            })
        })
        .collect::<Vec<_>>();

    let client = reqwest::Client::new();
    let first_response = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_codex_handoff_no_pollution",
            "model": "deepseek-v4-pro",
            "instructions": "Use client tools when necessary.",
            "client_metadata": {
                "x-codex-installation-id": "codex-install"
            },
            "prompt_cache_key": "thread-cache-key",
            "input": full_context_items,
            "tools": [{
                "type": "function",
                "function": {
                    "name": "shell_command",
                    "description": "Run a shell command.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": { "type": "string" }
                        },
                        "required": ["command"]
                    }
                }
            }]
        }))
        .send()
        .await
        .unwrap();

    assert!(first_response.status().is_success());
    let first_body = first_response.json::<Value>().await.unwrap();
    assert_eq!(first_body["status"], "completed");
    assert!(first_body["output"].as_array().unwrap().iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
            && item.get("name").and_then(Value::as_str) == Some("shell_command")
    }));
    let first_summary = store.runtime_summary(10).await.expect("first summary");
    assert_eq!(first_summary.request_count, 0);
    assert_eq!(first_summary.billable_request_count, 1);
    assert_eq!(
        first_summary.billable_history[0].lifecycle,
        "client_tool_handoff"
    );

    let first_requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(first_requests.len(), 1);
    let first_tool_names = first_requests[0]
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_default_codeseex_tools_present(&first_tool_names);
    assert!(
        first_tool_names.contains(&"shell_command"),
        "{first_tool_names:?}"
    );
    assert!(
        first_tool_names.contains(&"tool_search_tool"),
        "{first_tool_names:?}"
    );
    assert_eq!(
        first_requests[0]
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(96),
        "{}",
        first_requests[0]
    );
    assert!(first_requests[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|message| message.get("role").and_then(Value::as_str) != Some("tool")));
    let parent_chain = store
        .response_context_chain("resp_codex_handoff_no_pollution", 1)
        .await
        .expect("parent context chain");
    let parent_turn_messages = &parent_chain[0].turn_messages;
    assert_eq!(
        parent_turn_messages.len(),
        full_context_items.len() + 1,
        "full-context parent should retain every replay message and one assistant tool call"
    );
    let serialized_parent_turn_messages = serde_json::to_string(parent_turn_messages).unwrap();
    assert!(serialized_parent_turn_messages.contains("prior compact context item 0"));
    assert!(serialized_parent_turn_messages.contains("prior compact context item 94"));
    assert!(parent_turn_messages
        .iter()
        .any(|message| message.get("role").and_then(Value::as_str) == Some("user")));
    assert_eq!(
        parent_turn_messages
            .iter()
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
            .count(),
        1,
        "non-streaming handoff must persist the assistant tool call exactly once"
    );
    assert!(parent_turn_messages
        .iter()
        .all(|message| message.get("role").and_then(Value::as_str) != Some("tool")));

    let tool_call = first_body["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .expect("handoff tool call")
        .clone();
    let second_response = client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_codex_handoff_no_pollution_followup",
            "previous_response_id": "resp_codex_handoff_no_pollution",
            "model": "deepseek-v4-pro",
            "instructions": "Use client tools when necessary.",
            "client_metadata": {
                "x-codex-installation-id": "codex-install"
            },
            "prompt_cache_key": "thread-cache-key",
            "input": [
                tool_call,
                {
                    "type": "function_call_output",
                    "call_id": "call_shell",
                    "output": "TOOL_OUTPUT_BODY_".repeat(4_000)
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "finish after shell output" }]
                }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "shell_command",
                    "description": "Run a shell command.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]
        }))
        .send()
        .await
        .unwrap();
    assert!(second_response.status().is_success());
    let second_body = second_response.json::<Value>().await.unwrap();
    assert_eq!(second_body["status"], "completed");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 2);
    let second_tool_names = requests[1]
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_default_codeseex_tools_present(&second_tool_names);
    assert!(
        second_tool_names.contains(&"shell_command"),
        "{second_tool_names:?}"
    );
    assert!(
        second_tool_names.contains(&"tool_search_tool"),
        "{second_tool_names:?}"
    );
    let serialized_second_request = serde_json::to_string(&requests[1]).unwrap();
    assert!(
        serialized_second_request
            .matches("TOOL_OUTPUT_BODY_")
            .count()
            < 800,
        "tool output should be truncated before replay, occurrences={}",
        serialized_second_request
            .matches("TOOL_OUTPUT_BODY_")
            .count()
    );
    let second_messages = requests[1]["messages"].as_array().expect("messages");
    assert_eq!(
        second_messages.len(),
        full_context_items.len() + 4,
        "{}",
        requests[1]
    );
    assert_eq!(
        second_messages
            .iter()
            .filter(|message| {
                message.get("role").and_then(Value::as_str) == Some("user")
                    && message
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|content| content.starts_with("prior compact context item "))
            })
            .count(),
        full_context_items.len(),
        "every client replay message must remain exactly once"
    );
    assert!(serde_json::to_string(&requests[1])
        .unwrap()
        .contains("prior compact context item 0"));
    assert!(serde_json::to_string(&requests[1])
        .unwrap()
        .contains("prior compact context item 94"));
    assert_eq!(
        second_messages
            .iter()
            .filter(|message| {
                message.pointer("/tool_calls/0/id").and_then(Value::as_str) == Some("call_shell")
            })
            .count(),
        1,
        "the parent tool call must be represented once for its output"
    );
    let tool_message = second_messages
        .iter()
        .find(|message| {
            message.get("role").and_then(Value::as_str) == Some("tool")
                && message.get("tool_call_id").and_then(Value::as_str) == Some("call_shell")
        })
        .expect("tool message should be replayed compactly");
    let tool_content = tool_message["content"].as_str().expect("tool content");
    assert!(tool_content.contains("[truncated"), "{tool_content}");
    assert!(tool_content.len() < 13_000, "{}", tool_content.len());

    let summary = store.runtime_summary(10).await.expect("runtime summary");
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.billable_request_count, 2);
    assert_eq!(summary.billable_history[0].lifecycle, "client_tool_handoff");
    assert_eq!(summary.billable_history[1].lifecycle, "final_turn");
    assert_eq!(summary.total_cached_input_tokens, 1_300);
    assert_eq!(summary.total_cache_miss_input_tokens, 360);

    let (all_events, _) = store.recent_events(80, None).await.expect("events");
    assert!(all_events.iter().any(|event| {
        event.event_type == "context_compile_diagnostic"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.pointer("/runtime_context_storage/current/mode"))
                .and_then(Value::as_str)
                == Some("codex_full_context_not_stored")
    }));
    assert!(all_events.iter().any(|event| {
        event.event_type == "context_compile_diagnostic"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.pointer("/request/has_previous_response_id"))
                .and_then(Value::as_bool)
                == Some(true)
    }));
    assert!(all_events.iter().any(|event| {
        event.event_type == "context_compile_diagnostic"
            && event
                .detail
                .as_ref()
                .and_then(|detail| {
                    detail.pointer("/context/current_input/truncated_tool_output_items")
                })
                .and_then(Value::as_u64)
                .unwrap_or(0)
                >= 1
    }));
    assert!(all_events.iter().any(|event| {
        event.event_type == "upstream_call_usage_breakdown"
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.pointer("/payload/tools_count"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                >= 9
            && event
                .detail
                .as_ref()
                .and_then(|detail| detail.pointer("/usage/cache_miss_input_tokens"))
                .and_then(Value::as_u64)
                == Some(240)
    }));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn streaming_synthetic_tool_search_bridge_returns_codex_tool_search_call() {
    let fake_state = FakeUpstreamState::default();
    let fake_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let fake_addr = fake_listener.local_addr().unwrap();
    let fake_app = Router::new()
        .route(
            "/chat/completions",
            post(fake_tool_search_bridge_streaming_chat_completions),
        )
        .with_state(fake_state.clone());
    tokio::spawn(async move {
        axum::serve(fake_listener, fake_app).await.unwrap();
    });

    let data_dir = std::env::temp_dir().join(format!(
        "codeseex-tool-search-bridge-streaming-test-{}",
        Uuid::new_v4().simple()
    ));
    let config = test_config_with_upstream(data_dir, fake_addr);
    let store = Store::open(&config.data_dir).await.unwrap();
    let proxy_state = ProxyState::for_test(config, store);
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_app = Router::new()
        .route("/v1/responses", post(responses))
        .with_state(proxy_state);
    tokio::spawn(async move {
        axum::serve(proxy_listener, proxy_app).await.unwrap();
    });

    let body = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/responses"))
        .json(&json!({
            "id": "resp_stream_tool_search_bridge",
            "model": "deepseek-v4-pro",
            "stream": true,
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "use a sub-agent if useful" }]
                }
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "description": "Run a shell command.",
                        "parameters": { "type": "object", "properties": {} }
                    }
                }
            ]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("\"type\":\"tool_search_call\""), "{body}");
    assert!(body.contains("\"execution\":\"client\""), "{body}");
    assert!(body.contains("sub-agent"), "{body}");
    assert!(
        !body.contains("response.function_call_arguments.delta"),
        "{body}"
    );
    assert!(!body.contains("\"type\":\"proxy_tool_call\""), "{body}");
    assert!(!body.contains("tool_loop_failed"), "{body}");

    let requests = fake_state
        .requests
        .lock()
        .expect("fake upstream lock poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let upstream_tool_names = requests[0]["tools"]
        .as_array()
        .expect("upstream request should include tools")
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        upstream_tool_names.contains(&"tool_search_tool"),
        "{upstream_tool_names:?}"
    );
}

#[test]
fn reconstructed_tool_call_history_keeps_reasoning_content_field() {
    let config = test_config(temp_workspace("reasoning-replay"));
    let reasoning = "read the file before answering";
    let response = json!({
        "output": [
            reasoning_response_item(&config, reasoning, false),
            {
                "type": "function_call",
                "call_id": "call_prev",
                "name": "read_file_range",
                "arguments": "{\"path\":\"README.md\"}"
            }
        ]
    });
    assert!(response["output"][0]["encrypted_content"]
        .as_str()
        .unwrap()
        .starts_with("codeseex-reasoning-v1:"));

    let messages = response_output_tool_call_messages_with_config(&response, &config);
    let serialized = serde_json::to_value(&messages[0]).unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(serialized["role"], "assistant");
    assert_eq!(serialized["content"], "");
    assert_eq!(serialized["reasoning_content"], reasoning);
    assert_eq!(serialized["tool_calls"][0]["id"], "call_prev");
    assert_eq!(
        serialized["tool_calls"][0]["function"]["name"],
        "read_file_range"
    );
}

#[test]
fn reasoning_item_empty_content_is_safe_only_when_replay_payload_exists() {
    let config = test_config(temp_workspace("reasoning-empty-content-compare"));
    let reasoning = "read the file before answering";
    let call = json!({
        "type": "function_call",
        "call_id": "call_prev",
        "name": "read_file_range",
        "arguments": "{\"path\":\"README.md\"}"
    });

    let encrypted_response = json!({
        "output": [reasoning_response_item(&config, reasoning, false), call.clone()]
    });
    let summary_response = json!({
        "output": [
            {
                "type": "reasoning",
                "status": "completed",
                "summary": [{ "type": "summary_text", "text": reasoning }],
                "content": []
            },
            call.clone()
        ]
    });
    let empty_response = json!({
        "output": [
            {
                "type": "reasoning",
                "status": "completed",
                "summary": [],
                "content": []
            },
            call
        ]
    });

    for response in [encrypted_response, summary_response] {
        let messages = response_output_tool_call_messages_with_config(&response, &config);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "");
        assert_eq!(messages[0].reasoning_content.as_deref(), Some(reasoning));
    }

    let messages = response_output_tool_call_messages_with_config(&empty_response, &config);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "");
    assert_ne!(messages[0].reasoning_content.as_deref(), Some(reasoning));
}

#[test]
fn reconstructed_custom_apply_patch_history_uses_patch_argument() {
    let response = json!({
        "output": [
            {
                "type": "custom_tool_call",
                "call_id": "call_patch",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** Add File: hi.txt\n+hi\n*** End Patch"
            }
        ]
    });

    let messages = response_output_tool_call_messages(&response);
    let serialized = serde_json::to_value(&messages[0]).unwrap();
    let arguments = serialized["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap();
    let parsed = serde_json::from_str::<Value>(arguments).unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(serialized["tool_calls"][0]["id"], "call_patch");
    assert_eq!(
        serialized["tool_calls"][0]["function"]["name"],
        "apply_patch"
    );
    assert!(parsed["patch"]
        .as_str()
        .unwrap()
        .contains("*** Begin Patch"));
}
