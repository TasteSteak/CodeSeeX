use crate::app_state::ProxyState;
use crate::config_payload::{
    model_override_to_ui, temperature_to_ui, upstream_transport_to_ui, user_config_from_payload,
    web_search_backend_to_ui,
};
use crate::http_utils::{config_version, is_newer_version, normalize_version_label, now_seconds};
use crate::runtime_config::{RuntimeConfigChangeSource, RuntimeConfigService};
use crate::tools::registry::{selected_tool_ids, tool_registry, tool_settings};
use codeseex_core::catalog::{
    app_server_model_list, build_codeseex_catalog, catalog_file_is_compatible, codex_toml_snippet,
    write_catalog_atomic,
};
use codeseex_core::models::available_models;
use codeseex_core::urls::balance_url;
use codeseex_core::AppServerModelListParams;
use codeseex_core::{AppConfig, UserConfig};
use codeseex_store::{EventViewQuery, Store};
use serde::Serialize;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CODESEEX_WEBSITE_URL: &str = "https://tastesteak.github.io/CodeSeeX/";
const CODESEEX_REPOSITORY_URL: &str = "https://github.com/TasteSteak/CodeSeeX";
const CODESEEX_RELEASE_NOTES_RAW_BASE_URL: &str =
    "https://raw.githubusercontent.com/TasteSteak/CodeSeeX";
const RELEASE_NOTES_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const RELEASE_NOTES_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const EMBEDDED_RELEASE_NOTES: &str = include_str!("../../../docs/release-notes.json");

#[derive(Clone, Default)]
pub(crate) struct ReleaseNotesCache {
    inner: Arc<Mutex<ReleaseNotesCacheState>>,
}

#[derive(Default)]
struct ReleaseNotesCacheState {
    document: Option<Value>,
    etag: Option<String>,
    fetched_at: Option<Instant>,
    failed_at: Option<Instant>,
    failure: Option<String>,
}

#[derive(Clone)]
struct CachedReleaseNotes {
    document: Value,
    etag: Option<String>,
}

impl ReleaseNotesCache {
    fn fresh_for(&self, version: &str) -> Option<Value> {
        let state = self.inner.lock().ok()?;
        let fetched_at = state.fetched_at?;
        (fetched_at.elapsed() < RELEASE_NOTES_CACHE_TTL)
            .then(|| state.document.clone())
            .flatten()
            .filter(|document| release_notes_document_is_valid(document, version))
    }

    fn cached_for(&self, version: &str) -> Option<CachedReleaseNotes> {
        let state = self.inner.lock().ok()?;
        let document = state.document.clone()?;
        release_notes_document_is_valid(&document, version).then(|| CachedReleaseNotes {
            document,
            etag: state.etag.clone(),
        })
    }

    fn store(&self, document: Value, etag: Option<String>) {
        if let Ok(mut state) = self.inner.lock() {
            state.document = Some(document);
            state.etag = etag;
            state.fetched_at = Some(Instant::now());
            state.failed_at = None;
            state.failure = None;
        }
    }

    fn mark_revalidated(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.fetched_at = Some(Instant::now());
            state.failed_at = None;
            state.failure = None;
        }
    }

    fn recent_failure(&self) -> Option<String> {
        let state = self.inner.lock().ok()?;
        let failed_at = state.failed_at?;
        (failed_at.elapsed() < RELEASE_NOTES_CACHE_TTL)
            .then(|| state.failure.clone())
            .flatten()
    }

    fn record_failure(&self, error: &str) {
        if let Ok(mut state) = self.inner.lock() {
            state.failed_at = Some(Instant::now());
            state.failure = Some(error.to_owned());
        }
    }
}

#[derive(Clone)]
pub struct ManagerRuntime {
    runtime_config: RuntimeConfigService,
    store: Store,
    release_notes: ReleaseNotesCache,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagerJsonResponse {
    pub status: u16,
    pub body: Value,
}

impl ManagerRuntime {
    pub async fn open(config: AppConfig) -> anyhow::Result<Self> {
        if let Err(error) = migrate_image_capability_config(&config) {
            tracing::warn!(
                error = %error,
                path = %config.config_path().display(),
                "image capability migration was not completed; keeping the existing configuration"
            );
        }
        Ok(Self {
            store: Store::open(&config.data_dir).await?,
            runtime_config: RuntimeConfigService::new(config),
            release_notes: ReleaseNotesCache::default(),
        })
    }

    pub(crate) fn from_proxy_state(state: &ProxyState) -> Self {
        Self {
            runtime_config: state.runtime_config.clone(),
            store: state.store.clone(),
            release_notes: state.release_notes.clone(),
        }
    }

    pub async fn handle_json(
        &self,
        method: &str,
        path: &str,
        query: Option<&Value>,
        body: Option<&Value>,
    ) -> ManagerJsonResponse {
        let method = method.trim().to_ascii_uppercase();
        match (method.as_str(), path) {
            ("GET", "/health") => ok(json!({ "ok": true, "service": "codeseex" })),
            ("GET", "/api/status") => ok(self.status().await),
            ("GET", "/api/usage") => ok(self.usage_with_query(query).await),
            ("GET", "/api/usage/session") => self.usage_session(query).await,
            ("GET", "/api/models") => ok(self.model_list_value(query)),
            ("POST", "/api/app-server") => ok(self.app_server_rpc(body.unwrap_or(&Value::Null))),
            ("GET", "/codex-model-catalog") | ("POST", "/codex-model-catalog") => {
                ok(self.codex_model_catalog())
            }
            ("POST", "/api/codex-app/inject") => self.inject_codex_app(query, body).await,
            ("POST", "/api/codex-app/launch") => self.launch_codex_app(query, body).await,
            ("GET", "/api/config") => ok(self.config_payload()),
            ("POST", "/api/config") => {
                self.save_config(body.cloned().unwrap_or_else(|| json!({})))
                    .await
            }
            ("GET", "/api/languages") => ok(languages()),
            ("GET", "/api/tools") => ok(self.tools()),
            ("GET", "/api/app-info") => ok(app_info()),
            ("GET", "/api/update-check") => ok(self.update_check().await),
            ("GET", "/api/release-notes") => ok(self.release_notes().await),
            ("GET", "/api/deepseek/balance") => self.balance().await,
            ("GET", "/api/events") => self.events(query).await,
            ("GET", "/api/codex-adapter")
            | ("POST", "/api/codex-adapter/generate")
            | ("GET", "/api/codex-adapter/generate") => self.generate_adapter(),
            ("GET", "/api/codex-adapter/runtime") | ("POST", "/api/codex-adapter/runtime") => {
                self.verify_codex_runtime().await
            }
            ("POST", "/api/start") | ("POST", "/api/restart") | ("POST", "/api/stop") => {
                self.compatibility_action(path).await
            }
            _ => status(
                404,
                json!({
                    "ok": false,
                    "error": "manager_route_not_found",
                    "method": method,
                    "path": path
                }),
            ),
        }
    }

    pub(crate) fn active_config(&self) -> AppConfig {
        self.runtime_config.active_config()
    }

    pub fn model_list(&self, params: AppServerModelListParams) -> Value {
        serde_json::to_value(app_server_model_list(params)).unwrap_or_else(|_| {
            json!({
                "data": [],
                "nextCursor": null
            })
        })
    }

    fn model_list_value(&self, query: Option<&Value>) -> Value {
        self.model_list(model_list_params_from_query(query))
    }

    fn app_server_rpc(&self, body: &Value) -> Value {
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("model/list") => json!({
                "id": id,
                "result": self.model_list(model_list_params_from_query(body.get("params"))),
            }),
            Some(method) => json!({
                "id": id,
                "error": {
                    "code": "method_not_found",
                    "message": format!("Unsupported app server method '{method}'.")
                }
            }),
            None => json!({
                "id": id,
                "error": {
                    "code": "invalid_request",
                    "message": "Missing app server method."
                }
            }),
        }
    }

    pub fn codex_model_catalog(&self) -> Value {
        crate::codex_app::codex_model_catalog_value(&self.active_config())
    }

    async fn inject_codex_app(
        &self,
        query: Option<&Value>,
        body: Option<&Value>,
    ) -> ManagerJsonResponse {
        let debug_port = crate::codex_app::debug_port_from_values(query, body)
            .unwrap_or_else(crate::codex_app::default_debug_port);
        let catalog = self.codex_model_catalog();
        match crate::codex_app::inject_model_catalog(debug_port, catalog).await {
            Ok(value) if codex_app_injection_effective(&value) => {
                let _ = self
                    .store
                    .record_event(
                        "info",
                        "codex_app_inject_succeeded",
                        "Codex App renderer model catalog injection succeeded.",
                        Some(&value),
                    )
                    .await;
                ok(value)
            }
            Ok(mut value) => {
                if let Some(object) = value.as_object_mut() {
                    object.insert("error".to_owned(), json!("codex_app_inject_incomplete"));
                }
                let _ = self
                    .store
                    .record_event(
                        "warn",
                        "codex_app_inject_incomplete",
                        "Codex App renderer script ran but did not patch the app-server model list path.",
                        Some(&value),
                    )
                    .await;
                status(502, value)
            }
            Err(error) => {
                let body = json!({
                    "ok": false,
                    "error": "codex_app_inject_failed",
                    "debug_port": debug_port,
                    "message": error.to_string()
                });
                let _ = self
                    .store
                    .record_event(
                        "error",
                        "codex_app_inject_failed",
                        "Codex App renderer model catalog injection failed.",
                        Some(&body),
                    )
                    .await;
                status(502, body)
            }
        }
    }

    async fn launch_codex_app(
        &self,
        query: Option<&Value>,
        body: Option<&Value>,
    ) -> ManagerJsonResponse {
        let debug_port = crate::codex_app::debug_port_from_values(query, body)
            .unwrap_or_else(crate::codex_app::default_debug_port);
        let config = self.active_config();
        let inject = codex_app_launch_injection_enabled(query, body, &config);
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
                let _ = self
                    .store
                    .record_event(level, event_type, message, Some(&value))
                    .await;
                ok(value)
            }
            Err(error) => {
                let body = json!({
                    "ok": false,
                    "error": "codex_app_launch_failed",
                    "debug_port": debug_port,
                    "injection_enabled": inject,
                    "message": error.to_string()
                });
                let _ = self
                    .store
                    .record_event(
                        "error",
                        "codex_app_launch_failed",
                        "Codex App launch failed.",
                        Some(&body),
                    )
                    .await;
                status(502, body)
            }
        }
    }

    fn client(&self) -> reqwest::Client {
        let config = self.active_config();
        let timeout = std::time::Duration::from_millis(config.upstream.timeout_ms);
        crate::network::client(config.network_proxy, timeout)
            .unwrap_or_else(|_| reqwest::Client::new())
    }

    pub async fn status(&self) -> Value {
        let config = self.active_config();
        let runtime = self.store.runtime_overview().await.ok();
        json!({
            "ok": true,
            "running": true,
            "runtime_status": "running",
            "process_mode": "inline",
            "process_label": "CodeSeeX proxy",
            "pid": std::process::id(),
            "config_version": config_version(&config),
            "data_dir": config.data_dir.to_string_lossy(),
            "base_url": config.proxy_base_url(),
            "catalog_path": config.catalog_path().to_string_lossy(),
            "models": available_models().into_iter().map(|m| m.slug).collect::<Vec<_>>(),
            "runtime": {
                "status": "running",
                "port": config.port,
                "usage_revision": runtime.as_ref().map(|value| value.usage_revision).unwrap_or(0),
                "event_revision": runtime.as_ref().map(|value| value.event_revision).unwrap_or(0),
                "active_requests": runtime.as_ref().map(|value| value.active_requests).unwrap_or(0),
                "request_count": runtime.as_ref().map(|value| value.request_count).unwrap_or(0),
                "billable_request_count": runtime.as_ref().map(|value| value.billable_request_count).unwrap_or(0),
                "failed_request_count": runtime.as_ref().map(|value| value.failed_request_count).unwrap_or(0),
                "last_request_at": runtime.as_ref().and_then(|value| value.last_request_at.clone()),
                "last_activity_at": runtime.as_ref().and_then(|value| value.last_activity_at.clone()),
                "total_cached_input_tokens": runtime.as_ref().map(|value| value.total_cached_input_tokens).unwrap_or(0),
                "total_cache_miss_input_tokens": runtime.as_ref().map(|value| value.total_cache_miss_input_tokens).unwrap_or(0),
                "total_output_tokens": runtime.as_ref().map(|value| value.total_output_tokens).unwrap_or(0),
                "average_ms": runtime.as_ref().map(|value| value.average_ms).unwrap_or(0)
            },
            "upstream": {
                "base_url": config.upstream.base_url,
                "official_v1_compat": config.upstream.official_v1_compat
            }
        })
    }

    pub async fn usage(&self) -> Value {
        self.usage_with_query(None).await
    }

    pub async fn usage_with_query(&self, query: Option<&Value>) -> Value {
        let limit = query
            .and_then(|value| value.get("limit"))
            .and_then(value_to_u32)
            .unwrap_or(60);
        let cursor = query
            .and_then(|value| value.get("cursor"))
            .and_then(Value::as_str);
        let since_revision = query
            .and_then(|value| value.get("since_revision"))
            .and_then(value_to_u64);
        let page = self
            .store
            .usage_page(limit, cursor, since_revision)
            .await
            .ok();
        json!({
            "ok": true,
            "runtime": {
                "usage_revision": page.as_ref().map(|value| value.usage_revision).unwrap_or(0),
                "event_revision": page.as_ref().map(|value| value.event_revision).unwrap_or(0),
                "unchanged": page.as_ref().map(|value| value.unchanged).unwrap_or(false),
                "active_requests": page.as_ref().map(|value| value.active_requests).unwrap_or(0),
                "request_count": page.as_ref().map(|value| value.request_count).unwrap_or(0),
                "billable_request_count": page.as_ref().map(|value| value.billable_request_count).unwrap_or(0),
                "failed_request_count": page.as_ref().map(|value| value.failed_request_count).unwrap_or(0),
                "last_request_at": page.as_ref().and_then(|value| value.last_request_at.clone()),
                "last_activity_at": page.as_ref().and_then(|value| value.last_activity_at.clone()),
                "usage_sessions": page.as_ref().map(|value| value.usage_sessions.clone()).unwrap_or_default(),
                "has_more": page.as_ref().map(|value| value.has_more).unwrap_or(false),
                "next_cursor": page.as_ref().and_then(|value| value.next_cursor.clone()),
                "billing_buckets": page.as_ref().map(|value| value.billing_buckets.clone()).unwrap_or_default(),
                "total_cached_input_tokens": page.as_ref().map(|value| value.total_cached_input_tokens).unwrap_or(0),
                "total_cache_miss_input_tokens": page.as_ref().map(|value| value.total_cache_miss_input_tokens).unwrap_or(0),
                "total_output_tokens": page.as_ref().map(|value| value.total_output_tokens).unwrap_or(0),
                "average_ms": page.as_ref().map(|value| value.average_ms).unwrap_or(0)
            }
        })
    }

    pub async fn usage_session(&self, query: Option<&Value>) -> ManagerJsonResponse {
        let Some(id) = query
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return status(
                400,
                json!({
                    "ok": false,
                    "error": "missing_usage_session_id"
                }),
            );
        };
        match self.store.usage_session_detail(id).await {
            Ok(detail) => ok(json!({
                "ok": true,
                "usage_revision": detail.usage_revision,
                "session": detail.session
            })),
            Err(error) => status(
                500,
                json!({
                    "ok": false,
                    "error": "usage_session_failed",
                    "message": error.to_string()
                }),
            ),
        }
    }

    pub fn config_payload(&self) -> Value {
        let config = self.active_config();
        let user_config = UserConfig::read_from(&config.config_path()).unwrap_or_default();
        let proxy = user_config.proxy.as_ref();
        let upstream = user_config.upstream.as_ref();
        let model = user_config.model.as_ref();
        let ui = user_config.ui.as_ref();
        let billing = user_config.billing.as_ref();
        let tools = user_config.tools.as_ref();
        let upstream_base_url = upstream
            .and_then(|value| value.base_url.as_deref())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("");
        let model_override = model
            .and_then(|value| value.override_mode)
            .unwrap_or(config.model_override);
        let temperature = model
            .and_then(|value| value.temperature)
            .unwrap_or(config.temperature);

        let mut payload = json!({
            "config_version": config_version(&config),
            "PROXY_PORT": proxy.and_then(|value| value.port).unwrap_or(config.port).to_string(),
            "PROXY_PORT_EFFECTIVE": config.port.to_string(),
            "PROXY_PORT_SOURCE": proxy_port_source(proxy.and_then(|value| value.port)),
            "DEEPSEEK_BASE_URL": upstream_base_url,
            "DEEPSEEK_OFFICIAL_V1_COMPAT": upstream.and_then(|value| value.official_v1_compat).unwrap_or(config.upstream.official_v1_compat).to_string(),
            "DEEPSEEK_TRANSPORT": upstream_transport_to_ui(upstream.and_then(|value| value.transport).unwrap_or(config.upstream.transport)),
            "UPSTREAM_MODEL_OVERRIDE": model_override_to_ui(model_override),
            "DEEPSEEK_TEMPERATURE_PRESET": temperature_to_ui(temperature),
            "DEEPSEEK_THINKING": model.and_then(|value| value.thinking.as_deref()).unwrap_or("auto"),
            "SHOW_THINKING": ui.and_then(|value| value.show_thinking).unwrap_or(true).to_string(),
            "NETWORK_PROXY_MODE": network_proxy_to_ui(config.network_proxy),
            "WEB_SEARCH_BACKEND": web_search_backend_to_ui(tools.and_then(|value| value.web_search.as_ref()).and_then(|value| value.backend).unwrap_or(config.web_search_backend)),
            "AUTO_START": ui.and_then(|value| value.auto_start).unwrap_or(false).to_string(),
            "CODEX_APP_MODEL_LIST_INJECTION": ui.and_then(|value| value.codex_app_model_list_injection).unwrap_or(true).to_string(),
            "UI_THEME": ui.and_then(|value| value.theme.as_deref()).unwrap_or("system"),
            "UI_LANGUAGE": ui.and_then(|value| value.language.as_deref()).unwrap_or("system"),
            "UI_CLOSE_BEHAVIOR": ui.and_then(|value| value.close_behavior.as_deref()).unwrap_or("exit"),
            "LOG_RETENTION_DAYS": ui.and_then(|value| value.log_retention_days).unwrap_or(7).to_string(),
            "BILLING_PEAK_VALLEY_ENABLED": billing.and_then(|value| value.peak_valley_enabled).unwrap_or(true).to_string(),
            "BILLING_FLASH_CACHED_INPUT_CNY": billing.and_then(|value| value.flash_cached_input_cny).unwrap_or(0.02).to_string(),
            "BILLING_FLASH_CACHE_MISS_INPUT_CNY": billing.and_then(|value| value.flash_cache_miss_input_cny).unwrap_or(1.0).to_string(),
            "BILLING_FLASH_OUTPUT_CNY": billing.and_then(|value| value.flash_output_cny).unwrap_or(2.0).to_string(),
            "BILLING_PRO_CACHED_INPUT_CNY": billing.and_then(|value| value.pro_cached_input_cny).unwrap_or(0.025).to_string(),
            "BILLING_PRO_CACHE_MISS_INPUT_CNY": billing.and_then(|value| value.pro_cache_miss_input_cny).unwrap_or(3.0).to_string(),
            "BILLING_PRO_OUTPUT_CNY": billing.and_then(|value| value.pro_output_cny).unwrap_or(6.0).to_string(),
            "BILLING_VISION_CACHED_INPUT_CNY": billing.and_then(|value| value.vision_cached_input_cny).unwrap_or(0.05).to_string(),
            "BILLING_VISION_CACHE_MISS_INPUT_CNY": billing.and_then(|value| value.vision_cache_miss_input_cny).unwrap_or(1.5).to_string(),
            "BILLING_VISION_OUTPUT_CNY": billing.and_then(|value| value.vision_output_cny).unwrap_or(4.5).to_string(),
            "ENABLED_TOOLS": tools.and_then(|value| value.enabled.as_deref()).map(canonical_enabled_tool_ids).map(Value::from).unwrap_or(Value::Null)
        });
        let settings = crate::config_payload::tool_settings_from_user_config(&user_config);
        if let Some(object) = payload.as_object_mut() {
            if !settings.is_empty() {
                let mut tool_config_keys = crate::tools::registry::builtin_tool_config_keys();
                tool_config_keys.extend(crate::community_tools::community_tool_config_keys(
                    &config.data_dir,
                ));
                for key in tool_config_keys {
                    if matches!(
                        key.as_str(),
                        crate::tools::vision::API_KEY_KEY
                            | crate::tools::vision::ANALYZE_API_KEY_KEY
                            | crate::tools::vision::GENERATE_API_KEY_KEY
                    ) {
                        continue;
                    }
                    if let Some(value) = settings.get(&key) {
                        object.insert(key, Value::String(value.clone()));
                    }
                }
            }
            object.insert(
                "VISION_ANALYZE_API_KEY_CONFIGURED".to_owned(),
                Value::Bool(
                    crate::secrets::vision_analyze_api_key_configured(&config)
                        || settings
                            .get(crate::tools::vision::ANALYZE_API_KEY_KEY)
                            .is_some_and(|value| !value.trim().is_empty())
                        || settings
                            .get(crate::tools::vision::API_KEY_KEY)
                            .is_some_and(|value| !value.trim().is_empty()),
                ),
            );
            object.insert(
                "VISION_GENERATE_API_KEY_CONFIGURED".to_owned(),
                Value::Bool(
                    crate::secrets::vision_generate_api_key_configured(&config)
                        || settings
                            .get(crate::tools::vision::GENERATE_API_KEY_KEY)
                            .is_some_and(|value| !value.trim().is_empty()),
                ),
            );
        }
        payload
    }

    pub async fn save_config(&self, payload: Value) -> ManagerJsonResponse {
        let config = self.active_config();
        if let Some(client_version) = payload
            .get("CONFIG_VERSION")
            .or_else(|| payload.get("config_version"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let current_version = config_version(&config);
            if client_version != current_version {
                return status(
                    409,
                    json!({
                        "ok": false,
                        "code": "config_version_conflict",
                        "message": "Configuration changed outside this editor. Refresh before saving.",
                        "config_version": current_version
                    }),
                );
            }
        }
        let mut existing_config = UserConfig::read_from(&config.config_path()).unwrap_or_default();
        if let Err(error) = migrate_legacy_vision_secret(&config, &mut existing_config) {
            return status(500, json!({ "ok": false, "error": error.to_string() }));
        }
        if let Err(error) = apply_secret_payload(&config, &payload, &mut existing_config) {
            return status(500, json!({ "ok": false, "error": error.to_string() }));
        }
        let existing_retention_days = existing_config.log_retention_days();
        let mut user_config = user_config_from_payload(&payload, existing_config, &config);
        if let Err(error) = migrate_legacy_vision_secret(&config, &mut user_config) {
            return status(500, json!({ "ok": false, "error": error.to_string() }));
        }
        match user_config.write_atomic(&config.config_path()) {
            Ok(()) => {
                if let Some(change) = self
                    .runtime_config
                    .refresh(RuntimeConfigChangeSource::ManagerSave)
                {
                    let _ = self
                        .store
                        .record_event(
                            "info",
                            "runtime_config_changed",
                            "CodeSeeX runtime configuration changed.",
                            Some(&change.diagnostic()),
                        )
                        .await;
                }
                let new_retention_days = user_config.log_retention_days();
                let maintenance = if new_retention_days == existing_retention_days {
                    json!({
                        "ok": true,
                        "skipped": true,
                        "reason": "log retention days unchanged"
                    })
                } else {
                    match self.store.run_maintenance(new_retention_days).await {
                        Ok(report) => {
                            if report.deleted_events > 0 {
                                let _ = self
                                    .store
                                    .record_event(
                                        "info",
                                        "log_maintenance_completed",
                                        "CodeSeeX log maintenance completed.",
                                        Some(&json!({
                                            "log_retention_days": report.log_retention_days,
                                            "deleted_log_files": report.deleted_events
                                        })),
                                    )
                                    .await;
                            }
                            json!({
                                "ok": true,
                                "log_retention_days": report.log_retention_days,
                                "deleted_events": report.deleted_events,
                                "sanitized_requests": report.sanitized_requests,
                                "request_sanitize_batches": report.request_sanitize_batches,
                                "request_sanitize_limit_reached": report.request_sanitize_limit_reached,
                                "vacuumed_storage": report.vacuumed_storage
                            })
                        }
                        Err(error) => {
                            let _ = self
                                .store
                                .record_event(
                                    "error",
                                    "log_maintenance_failed",
                                    "CodeSeeX failed to prune expired logs.",
                                    Some(&json!({ "error": error.to_string() })),
                                )
                                .await;
                            json!({ "ok": false, "error": error.to_string() })
                        }
                    }
                };
                let _ = self
                    .store
                    .record_event(
                        "info",
                        "manager_config_saved",
                        "Configuration saved.",
                        Some(&json!({ "path": config.config_path().to_string_lossy() })),
                    )
                    .await;
                ok(json!({
                    "ok": true,
                    "saved": true,
                    "config_version": config_version(&config),
                    "path": config.config_path().to_string_lossy(),
                    "maintenance": maintenance
                }))
            }
            Err(error) => status(500, json!({ "ok": false, "error": error.to_string() })),
        }
    }

    pub fn tools(&self) -> Value {
        let config = self.active_config();
        let enabled_tools = selected_tool_ids(&config);
        let settings = tool_settings(&config);
        json!({
            "ok": true,
            "tools": tool_registry(&config, &enabled_tools, &settings)
        })
    }

    pub async fn update_check(&self) -> Value {
        let current_version = env!("CARGO_PKG_VERSION");
        let checked_at = now_seconds().to_string();
        let fallback_url = "https://github.com/TasteSteak/CodeSeeX/releases";
        let client = self.client();
        let result = client
            .get("https://api.github.com/repos/TasteSteak/CodeSeeX/releases/latest")
            .header(reqwest::header::USER_AGENT, "CodeSeeX")
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await;

        let Ok(response) = result else {
            return json!({
                "ok": false,
                "has_update": false,
                "latest_version": current_version,
                "current_version": current_version,
                "url": fallback_url,
                "checked_at": checked_at,
                "error": "update_check_unreachable"
            });
        };

        if !response.status().is_success() {
            return json!({
                "ok": false,
                "has_update": false,
                "latest_version": current_version,
                "current_version": current_version,
                "url": fallback_url,
                "checked_at": checked_at,
                "error": format!("github_status_{}", response.status().as_u16())
            });
        }

        let payload = response.json::<Value>().await.unwrap_or_else(|_| json!({}));
        let latest_version = payload
            .get("tag_name")
            .and_then(Value::as_str)
            .or_else(|| payload.get("name").and_then(Value::as_str))
            .unwrap_or(current_version);
        let url = payload
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or(fallback_url);
        json!({
            "ok": true,
            "has_update": is_newer_version(latest_version, current_version),
            "latest_version": normalize_version_label(latest_version),
            "current_version": current_version,
            "url": url,
            "checked_at": checked_at,
            "error": null
        })
    }

    pub async fn release_notes(&self) -> Value {
        let current_version = env!("CARGO_PKG_VERSION");
        let fallback = embedded_release_notes(current_version);
        if let Some(document) = self.release_notes.fresh_for(current_version) {
            return release_notes_response(document, current_version, "cache", false, None);
        }

        let cached = self.release_notes.cached_for(current_version);
        if let Some(error) = self.release_notes.recent_failure() {
            return release_notes_fallback_response(cached, fallback, current_version, &error);
        }
        let mut request = self
            .client()
            .get(release_notes_url(current_version))
            .header(reqwest::header::USER_AGENT, "CodeSeeX")
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(etag) = cached.as_ref().and_then(|entry| entry.etag.as_deref()) {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }

        let response = tokio::time::timeout(RELEASE_NOTES_REQUEST_TIMEOUT, request.send()).await;
        let Ok(Ok(response)) = response else {
            self.release_notes
                .record_failure("release_notes_unreachable");
            return release_notes_fallback_response(
                cached,
                fallback,
                current_version,
                "release_notes_unreachable",
            );
        };
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            if let Some(cached) = cached {
                self.release_notes.mark_revalidated();
                return release_notes_response(
                    cached.document,
                    current_version,
                    "cache",
                    false,
                    None,
                );
            }
            self.release_notes
                .record_failure("release_notes_cache_miss");
            return release_notes_fallback_response(
                None,
                fallback,
                current_version,
                "release_notes_cache_miss",
            );
        }
        if !response.status().is_success() {
            self.release_notes
                .record_failure("release_notes_http_error");
            return release_notes_fallback_response(
                cached,
                fallback,
                current_version,
                "release_notes_http_error",
            );
        }

        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let Ok(document) = response.json::<Value>().await else {
            self.release_notes
                .record_failure("release_notes_invalid_json");
            return release_notes_fallback_response(
                cached,
                fallback,
                current_version,
                "release_notes_invalid_json",
            );
        };
        if !release_notes_document_is_valid(&document, current_version) {
            self.release_notes
                .record_failure("release_notes_invalid_document");
            return release_notes_fallback_response(
                cached,
                fallback,
                current_version,
                "release_notes_invalid_document",
            );
        }

        self.release_notes.store(document.clone(), etag);
        release_notes_response(document, current_version, "github", false, None)
    }

    pub async fn balance(&self) -> ManagerJsonResponse {
        let config = self.active_config();
        let Some(api_key) = codeseex_core::codex_auth::read_codex_auth_api_key(true) else {
            return ok(json!({
                "ok": false,
                "code": "missing_api_key",
                "message": "API key is not configured."
            }));
        };
        let balance_url = match balance_url(&config.upstream.base_url) {
            Ok(value) => value,
            Err(_) => {
                return ok(json!({
                    "ok": false,
                    "code": "invalid_deepseek_base_url",
                    "message": "Invalid DeepSeek base URL."
                }));
            }
        };

        let client = self.client();
        match client
            .get(balance_url)
            .bearer_auth(api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            Ok(response) => {
                let status_code = response.status();
                match response.bytes().await {
                    Ok(bytes) if status_code.is_success() => {
                        let body = serde_json::from_slice::<Value>(&bytes)
                            .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }));
                        ok(json!({
                            "ok": true,
                            "is_available": body.get("is_available").and_then(Value::as_bool).unwrap_or(false),
                            "balance_infos": body.get("balance_infos")
                                .and_then(Value::as_array)
                                .map(|items| items.iter().map(normalize_balance_info).collect::<Vec<_>>())
                                .unwrap_or_default(),
                            "checked_at": now_seconds().to_string()
                        }))
                    }
                    Ok(bytes) => ok(json!({
                        "ok": false,
                        "code": "deepseek_balance_error",
                        "status": status_code.as_u16(),
                        "message": balance_error_message(&bytes)
                    })),
                    Err(error) => ok(json!({
                        "ok": false,
                        "code": "deepseek_balance_failed",
                        "message": error.to_string()
                    })),
                }
            }
            Err(error) => {
                let code = if error.is_timeout() {
                    "deepseek_balance_timeout"
                } else {
                    "deepseek_balance_failed"
                };
                ok(json!({
                    "ok": false,
                    "code": code,
                    "message": if error.is_timeout() {
                        "DeepSeek balance request timed out.".to_owned()
                    } else {
                        error.to_string()
                    }
                }))
            }
        }
    }

    pub async fn events(&self, query: Option<&Value>) -> ManagerJsonResponse {
        let limit = query
            .and_then(|value| value.get("limit"))
            .and_then(value_to_u32)
            .unwrap_or(30);
        let event_query = EventViewQuery {
            limit,
            before: query
                .and_then(|value| value.get("before"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            cursor: query
                .and_then(|value| value.get("cursor"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            after: query
                .and_then(|value| value.get("after"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            audience: query
                .and_then(|value| value.get("audience"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            category: query
                .and_then(|value| value.get("category"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            level: query
                .and_then(|value| value.get("level"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            request_id: query
                .and_then(|value| value.get("request_id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            q: query
                .and_then(|value| value.get("q"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        match self.store.recent_event_views(event_query).await {
            Ok((events, has_more, next_cursor)) => {
                let (_, event_revision) = self.store.revisions().await.unwrap_or((0, 0));
                let latest_cursor = events.last().and_then(|event| event.cursor.clone());
                ok(json!({
                    "ok": true,
                    "events": events,
                    "has_more": has_more,
                    "next_cursor": next_cursor,
                    "latest_cursor": latest_cursor,
                    "event_revision": event_revision
                }))
            }
            Err(error) => status(
                500,
                json!({ "ok": false, "error": error.to_string(), "events": [] }),
            ),
        }
    }

    pub async fn compatibility_action(&self, path: &str) -> ManagerJsonResponse {
        let action = path
            .rsplit('/')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        let _ = self
            .store
            .record_event(
                "info",
                "manager_action",
                "Manager action acknowledged by HTTP compatibility adapter.",
                Some(&json!({
                    "action": action,
                    "path": path,
                    "effect": "desktop_lifecycle_is_managed_by_tauri_command"
                })),
            )
            .await;
        ok(json!({
            "ok": true,
            "mode": "http_compat",
            "action": action,
            "effect": "not_applicable_without_desktop_runtime"
        }))
    }

    pub fn generate_adapter(&self) -> ManagerJsonResponse {
        let config = self.active_config();
        let before = catalog_file_state(&config);
        match ensure_catalog(&config) {
            Ok(()) => {
                let toml_snippet =
                    codex_toml_snippet(&config.catalog_path(), &config.proxy_base_url());
                let after = catalog_file_state(&config);
                ok(json!({
                    "ok": true,
                    "ready": true,
                    "catalog_mode": "builtin",
                    "catalog_path": config.catalog_path().to_string_lossy(),
                    "catalog_diagnostic": catalog_diagnostic(&config, &toml_snippet, &before, &after),
                    "models": available_models().into_iter().map(|m| m.slug).collect::<Vec<_>>(),
                    "context_window": 1_000_000,
                    "effective_context_window_percent": 95,
                    "toml_snippet": toml_snippet
                }))
            }
            Err(error) => status(500, json!({ "ok": false, "error": error.to_string() })),
        }
    }

    pub async fn verify_codex_runtime(&self) -> ManagerJsonResponse {
        let config = self.active_config();
        if let Err(error) = ensure_catalog(&config) {
            return status(
                500,
                json!({
                    "ok": false,
                    "error": "catalog_prepare_failed",
                    "message": error.to_string()
                }),
            );
        }
        let diagnostic = crate::codex_app::verify_runtime_catalog(&config).await;
        let status_text = diagnostic
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let issue = diagnostic
            .get("issue")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let path_note = diagnostic
            .get("path_note")
            .and_then(Value::as_str)
            .unwrap_or("none");
        let (level, event_type, message) = match status_text {
            "ok" => (
                "info",
                "codex_runtime_catalog_verified",
                "Codex runtime model catalog verified.",
            ),
            "unavailable" => (
                "warn",
                "codex_runtime_catalog_unavailable",
                "Codex runtime model catalog verification is unavailable.",
            ),
            "error" => (
                "error",
                "codex_runtime_catalog_error",
                "Codex runtime did not load the expected model catalog.",
            ),
            _ => (
                "warn",
                "codex_runtime_catalog_warning",
                "Codex runtime model catalog verification needs attention.",
            ),
        };
        let _ = self
            .store
            .record_event(
                level,
                event_type,
                message,
                Some(&json!({
                    "status": status_text,
                    "issue": issue,
                    "path_note": path_note,
                    "runtime": diagnostic
                })),
            )
            .await;
        ok(json!({
            "ok": true,
            "runtime_diagnostic": diagnostic
        }))
    }
}

/// Upgrade the pre-0.7 combined Vision capability once. The public capability
/// id stays `vision_analyze`; only explicit legacy generation evidence enables
/// the new independent `image_gen` capability.
pub(crate) fn migrate_image_capability_config(config: &AppConfig) -> anyhow::Result<bool> {
    let path = config.config_path();
    if !path.exists() {
        return Ok(false);
    }
    let mut user_config = match UserConfig::read_from(&path) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(
                error = %error,
                path = %path.display(),
                "could not parse configuration for image capability migration"
            );
            return Ok(false);
        }
    };
    if user_config
        .tools
        .as_ref()
        .and_then(|tools| tools.capability_schema_version)
        == Some(codeseex_core::IMAGE_CAPABILITY_SCHEMA_VERSION)
    {
        return Ok(false);
    }

    let old_enabled = user_config
        .tools
        .as_ref()
        .and_then(|tools| tools.enabled.clone());
    let old_canonical_enabled = old_enabled
        .as_ref()
        .map(|ids| canonical_enabled_tool_ids(ids));
    let old_vision_enabled = old_canonical_enabled
        .as_ref()
        .map(|ids| ids.iter().any(|id| id == "vision_analyze"))
        .unwrap_or(true);
    let has_legacy_generation = user_config
        .tools
        .as_ref()
        .is_some_and(legacy_generation_evidence);

    // Move legacy sections and secrets before writing the new capability
    // marker. A failed secure-store operation must leave the config retryable.
    migrate_legacy_vision_secret(config, &mut user_config).map_err(anyhow::Error::msg)?;

    let tools = user_config
        .tools
        .get_or_insert_with(codeseex_core::UserToolsConfig::default);
    let mut enabled = old_enabled.unwrap_or_else(crate::tools::default_enabled_tool_ids);
    enabled = canonical_enabled_tool_ids(&enabled);
    if !enabled.iter().any(|id| id == "vision_analyze") {
        enabled.push("vision_analyze".to_owned());
    }
    if old_vision_enabled && has_legacy_generation && !enabled.iter().any(|id| id == "image_gen") {
        enabled.push("image_gen".to_owned());
    }
    tools.enabled = Some(enabled);
    tools.capability_schema_version = Some(codeseex_core::IMAGE_CAPABILITY_SCHEMA_VERSION);
    user_config.write_atomic(&path)?;
    Ok(true)
}

fn legacy_generation_evidence(tools: &codeseex_core::UserToolsConfig) -> bool {
    let config_fields = tools
        .settings
        .as_ref()
        .map(|settings| {
            settings.iter().any(|(key, value)| {
                !value.trim().is_empty()
                    && matches!(
                        key.as_str(),
                        crate::tools::vision::GENERATE_URL_KEY
                            | crate::tools::vision::GENERATE_MODEL_KEY
                            | crate::tools::vision::GENERATE_API_KEY_KEY
                    )
            })
        })
        .unwrap_or(false);
    let analyze_legacy_fields = tools.vision_analyze.as_ref().is_some_and(|vision| {
        non_empty_option(&vision.generate_url) || non_empty_option(&vision.generate_model)
    });
    let generation_fields = tools.vision_generate.as_ref().is_some_and(|generation| {
        non_empty_option(&generation.generate_url)
            || non_empty_option(&generation.generate_model)
            || non_empty_option(&generation.api_key)
    });
    config_fields || analyze_legacy_fields || generation_fields
}

fn non_empty_option(value: &Option<String>) -> bool {
    value.as_ref().is_some_and(|value| !value.trim().is_empty())
}

#[derive(Debug, Clone)]
struct CatalogFileState {
    exists: bool,
    readable: bool,
    compatible: bool,
    exact_current: bool,
    model_count: usize,
    models: Vec<String>,
    error: Option<String>,
}

fn catalog_file_state(config: &AppConfig) -> CatalogFileState {
    let path = config.catalog_path();
    if !path.exists() {
        return CatalogFileState {
            exists: false,
            readable: false,
            compatible: false,
            exact_current: false,
            model_count: 0,
            models: Vec::new(),
            error: Some("missing".to_owned()),
        };
    }

    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            return CatalogFileState {
                exists: true,
                readable: false,
                compatible: false,
                exact_current: false,
                model_count: 0,
                models: Vec::new(),
                error: Some(error.to_string()),
            };
        }
    };

    let value = serde_json::from_str::<Value>(&text).ok();
    let models = value
        .as_ref()
        .and_then(|value| value.get("models"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("slug")
                        .or_else(|| item.get("model"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let expected = serde_json::to_string_pretty(&build_codeseex_catalog())
        .map(|text| text + "\n")
        .unwrap_or_default();

    CatalogFileState {
        exists: true,
        readable: true,
        compatible: catalog_file_is_compatible(&path),
        exact_current: !expected.is_empty() && text == expected,
        model_count: models.len(),
        models,
        error: value.is_none().then(|| "invalid_json".to_owned()),
    }
}

fn catalog_diagnostic(
    config: &AppConfig,
    toml_snippet: &str,
    before: &CatalogFileState,
    after: &CatalogFileState,
) -> Value {
    let toml_catalog_path = toml_model_catalog_path(toml_snippet);
    let toml_has_catalog = toml_catalog_path.is_some();
    let catalog_path = config.catalog_path();
    let toml_catalog_matches = toml_catalog_path
        .as_deref()
        .map(|path| catalog_path.as_path() == std::path::Path::new(path))
        .unwrap_or(false);
    let repaired = !before.exact_current && after.exact_current;
    let mut status = if after.exists && after.readable && after.compatible && after.exact_current {
        "ok"
    } else if after.exists && after.readable && after.compatible {
        "warning"
    } else {
        "error"
    };
    let mut issue = if !after.exists {
        "missing"
    } else if !after.readable {
        "unreadable"
    } else if !after.compatible {
        "incompatible"
    } else if !after.exact_current {
        "outdated"
    } else if !toml_has_catalog {
        status = "warning";
        "toml_missing_catalog"
    } else if !toml_catalog_matches {
        status = "warning";
        "toml_catalog_mismatch"
    } else if repaired {
        status = "repaired";
        "regenerated"
    } else {
        "none"
    };
    if repaired && issue == "none" {
        status = "repaired";
        issue = "regenerated";
    }

    json!({
        "status": status,
        "issue": issue,
        "path": catalog_path.to_string_lossy(),
        "exists": after.exists,
        "readable": after.readable,
        "compatible": after.compatible,
        "exact_current": after.exact_current,
        "repaired": repaired,
        "previous_issue": before.error.as_deref().unwrap_or(if before.exact_current { "none" } else { "outdated_or_missing" }),
        "model_count": after.model_count,
        "models": after.models,
        "default_model": "deepseek-v4-pro",
        "toml_has_model_catalog_json": toml_has_catalog,
        "toml_catalog_path": toml_catalog_path,
        "toml_catalog_path_matches": toml_catalog_matches,
        "ccs_import_model_catalog_risk": true
    })
}

fn toml_model_catalog_path(toml_snippet: &str) -> Option<String> {
    toml_snippet.lines().find_map(|line| {
        let trimmed = line.trim();
        let value = trimmed.strip_prefix("model_catalog_json")?.trim();
        let value = value.strip_prefix('=')?.trim();
        let unquoted = value
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
            .or_else(|| {
                value
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
            })
            .unwrap_or(value)
            .trim();
        (!unquoted.is_empty()).then(|| unquoted.to_owned())
    })
}

pub fn ensure_catalog(config: &AppConfig) -> anyhow::Result<()> {
    let catalog = build_codeseex_catalog();
    if catalog_file_matches_current(&config.catalog_path(), &catalog) {
        return Ok(());
    }
    write_catalog_atomic(&config.catalog_path(), &catalog)
}

fn catalog_file_matches_current(
    path: &std::path::Path,
    catalog: &codeseex_core::catalog::Catalog,
) -> bool {
    let Ok(existing) = fs::read_to_string(path) else {
        return false;
    };
    if !catalog_file_is_compatible(path) {
        return false;
    }
    let Ok(expected) = serde_json::to_string_pretty(catalog).map(|text| text + "\n") else {
        return false;
    };
    existing == expected
}

fn apply_secret_payload(
    config: &AppConfig,
    payload: &Value,
    existing_config: &mut UserConfig,
) -> Result<(), String> {
    let legacy_key = secret_payload_value(payload, crate::tools::vision::API_KEY_KEY);
    let analyze_key = secret_payload_value(payload, crate::tools::vision::ANALYZE_API_KEY_KEY)
        .or_else(|| legacy_key.clone());
    let generate_key = secret_payload_value(payload, crate::tools::vision::GENERATE_API_KEY_KEY);
    if payload_bool(payload, "VISION_API_KEY_CLEAR")
        || payload_bool(payload, "VISION_ANALYZE_API_KEY_CLEAR")
    {
        crate::secrets::clear_vision_analyze_api_key(config).map_err(|error| error.to_string())?;
    }
    if payload_bool(payload, "VISION_API_KEY_CLEAR")
        || payload_bool(payload, "VISION_GENERATE_API_KEY_CLEAR")
    {
        crate::secrets::clear_vision_generate_api_key(config).map_err(|error| error.to_string())?;
    }
    if let Some(key) = analyze_key {
        crate::secrets::write_vision_analyze_api_key(config, &key)
            .map_err(|error| error.to_string())?;
    }
    if let Some(key) = generate_key {
        crate::secrets::write_vision_generate_api_key(config, &key)
            .map_err(|error| error.to_string())?;
    }
    if legacy_key.is_some() || payload_bool(payload, "VISION_API_KEY_CLEAR") {
        clear_legacy_vision_key(existing_config);
        let _ = crate::secrets::clear_legacy_vision_api_key(config);
    }
    Ok(())
}

fn secret_payload_value(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn migrate_legacy_vision_secret(
    config: &AppConfig,
    user_config: &mut UserConfig,
) -> Result<(), String> {
    migrate_legacy_vision_config(user_config);
    let flat_legacy_key = vision_setting_value(user_config, crate::tools::vision::API_KEY_KEY);
    let flat_analyze_key =
        vision_setting_value(user_config, crate::tools::vision::ANALYZE_API_KEY_KEY);
    let flat_generate_key =
        vision_setting_value(user_config, crate::tools::vision::GENERATE_API_KEY_KEY);
    remove_vision_secret_settings(user_config);
    let legacy_shared_key = take_legacy_shared_vision_key(user_config)
        .or(flat_legacy_key)
        .or_else(|| crate::secrets::legacy_vision_api_key(config));
    let analyze_key = take_vision_analyze_key(user_config)
        .or(flat_analyze_key)
        .or_else(|| legacy_shared_key.clone());
    let generation_key = take_vision_generate_key(user_config).or(flat_generate_key);
    let has_generation = user_config
        .tools
        .as_ref()
        .and_then(|tools| tools.vision_generate.as_ref())
        .is_some();
    if let Some(legacy_key) = analyze_key.as_deref() {
        if !crate::secrets::current_vision_analyze_api_key_configured(config) {
            crate::secrets::write_vision_analyze_api_key(config, legacy_key)
                .map_err(|error| error.to_string())?;
        }
    }
    let generation_key = generation_key.or_else(|| legacy_shared_key.filter(|_| has_generation));
    if let Some(generation_key) = generation_key.as_deref() {
        if has_generation && !crate::secrets::current_vision_generate_api_key_configured(config) {
            crate::secrets::write_vision_generate_api_key(config, generation_key)
                .map_err(|error| error.to_string())?;
        }
    }
    if analyze_key.is_none() && generation_key.is_none() {
        return Ok(());
    }
    crate::secrets::clear_legacy_vision_api_key(config).map_err(|error| error.to_string())?;
    Ok(())
}

fn vision_setting_value(user_config: &UserConfig, key: &str) -> Option<String> {
    user_config
        .tools
        .as_ref()
        .and_then(|tools| tools.settings.as_ref())
        .and_then(|settings| settings.get(key))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn remove_vision_secret_settings(user_config: &mut UserConfig) {
    let Some(tools) = user_config.tools.as_mut() else {
        return;
    };
    let Some(settings) = tools.settings.as_mut() else {
        return;
    };
    for key in [
        crate::tools::vision::API_KEY_KEY,
        crate::tools::vision::ANALYZE_API_KEY_KEY,
        crate::tools::vision::GENERATE_API_KEY_KEY,
    ] {
        settings.remove(key);
    }
    if settings.is_empty() {
        tools.settings = None;
    }
}

fn migrate_legacy_vision_config(user_config: &mut UserConfig) {
    let Some(tools) = user_config.tools.as_mut() else {
        return;
    };
    let mut settings = tools.settings.take().unwrap_or_default();
    let setting = |settings: &mut std::collections::BTreeMap<String, String>, key: &str| {
        settings
            .remove(key)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let analyze_backend = setting(&mut settings, crate::tools::vision::ANALYZE_BACKEND_KEY);
    let image_detail = setting(&mut settings, crate::tools::vision::IMAGE_DETAIL_KEY);
    let analyze_url = setting(&mut settings, crate::tools::vision::ANALYZE_URL_KEY);
    let analyze_model = setting(&mut settings, crate::tools::vision::ANALYZE_MODEL_KEY);
    let generate_url = setting(&mut settings, crate::tools::vision::GENERATE_URL_KEY);
    let generate_model = setting(&mut settings, crate::tools::vision::GENERATE_MODEL_KEY);
    let analyze_api_key = setting(&mut settings, crate::tools::vision::ANALYZE_API_KEY_KEY);
    let generate_api_key = setting(&mut settings, crate::tools::vision::GENERATE_API_KEY_KEY);

    let has_analyze_config = analyze_backend.is_some()
        || image_detail.is_some()
        || analyze_url.is_some()
        || analyze_model.is_some()
        || analyze_api_key.is_some();
    if has_analyze_config && tools.vision_analyze.is_none() {
        tools.vision_analyze = Some(codeseex_core::UserVisionToolConfig::default());
    }
    let Some(vision) = tools.vision_analyze.as_mut() else {
        if generate_url.is_some() || generate_model.is_some() || generate_api_key.is_some() {
            let generation = tools
                .vision_generate
                .get_or_insert_with(codeseex_core::UserVisionGenerateToolConfig::default);
            if generation.generate_url.is_none() {
                generation.generate_url = generate_url;
            }
            if generation.generate_model.is_none() {
                generation.generate_model = generate_model;
            }
            if generation.api_key.is_none() {
                generation.api_key = generate_api_key;
            }
        }
        if !settings.is_empty() {
            tools.settings = Some(settings);
        }
        return;
    };
    if let Some(value) = analyze_backend {
        vision.backend = parse_legacy_vision_backend(&value).or(vision.backend);
    }
    if let Some(value) = image_detail {
        vision.image_detail = parse_legacy_vision_detail(&value).or(vision.image_detail);
    }
    if vision.analyze_url.is_none() {
        vision.analyze_url = analyze_url;
    }
    if vision.analyze_model.is_none() {
        vision.analyze_model = analyze_model;
    }
    if vision.analyze_api_key.is_none() {
        vision.analyze_api_key = analyze_api_key;
    }
    if vision.backend.is_none() {
        vision.backend = Some(
            if vision
                .analyze_url
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty())
                || vision
                    .analyze_model
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty())
            {
                codeseex_core::VisionAnalyzeBackend::External
            } else {
                codeseex_core::VisionAnalyzeBackend::Deepseek
            },
        );
    }
    if vision.image_detail.is_none() {
        vision.image_detail = Some(codeseex_core::VisionImageDetail::Auto);
    }
    let legacy_generate_url = vision.generate_url.take().or(generate_url);
    let legacy_generate_model = vision.generate_model.take().or(generate_model);
    if legacy_generate_url.is_some()
        || legacy_generate_model.is_some()
        || generate_api_key.is_some()
    {
        let generation = tools
            .vision_generate
            .get_or_insert_with(codeseex_core::UserVisionGenerateToolConfig::default);
        if generation.generate_url.is_none() {
            generation.generate_url = legacy_generate_url;
        }
        if generation.generate_model.is_none() {
            generation.generate_model = legacy_generate_model;
        }
        if generation.api_key.is_none() {
            generation.api_key = generate_api_key;
        }
    }
    if !settings.is_empty() {
        tools.settings = Some(settings);
    }
}

fn parse_legacy_vision_backend(value: &str) -> Option<codeseex_core::VisionAnalyzeBackend> {
    match value.trim().to_ascii_lowercase().as_str() {
        "deepseek" => Some(codeseex_core::VisionAnalyzeBackend::Deepseek),
        "external" | "custom" => Some(codeseex_core::VisionAnalyzeBackend::External),
        _ => None,
    }
}

fn parse_legacy_vision_detail(value: &str) -> Option<codeseex_core::VisionImageDetail> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(codeseex_core::VisionImageDetail::Auto),
        "low" => Some(codeseex_core::VisionImageDetail::Low),
        "original" => Some(codeseex_core::VisionImageDetail::Original),
        _ => None,
    }
}

fn payload_bool(payload: &Value, key: &str) -> bool {
    match payload.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "enabled"
        ),
        _ => false,
    }
}

fn clear_legacy_vision_key(config: &mut UserConfig) {
    let _ = take_vision_analyze_key(config);
    let _ = take_legacy_shared_vision_key(config);
    let _ = take_vision_generate_key(config);
}

fn take_vision_analyze_key(config: &mut UserConfig) -> Option<String> {
    let tools = config.tools.as_mut()?;
    let vision = tools.vision_analyze.as_mut()?;
    let value = vision
        .analyze_api_key
        .take()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if vision.analyze_url.is_none()
        && vision.analyze_model.is_none()
        && vision.generate_url.is_none()
        && vision.generate_model.is_none()
        && vision.api_key.is_none()
    {
        tools.vision_analyze = None;
    }
    value
}

fn take_legacy_shared_vision_key(config: &mut UserConfig) -> Option<String> {
    let tools = config.tools.as_mut()?;
    let vision = tools.vision_analyze.as_mut()?;
    let value = vision
        .api_key
        .take()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if vision.analyze_url.is_none()
        && vision.analyze_model.is_none()
        && vision.generate_url.is_none()
        && vision.generate_model.is_none()
        && vision.analyze_api_key.is_none()
        && vision.api_key.is_none()
    {
        tools.vision_analyze = None;
    }
    value
}

fn take_vision_generate_key(config: &mut UserConfig) -> Option<String> {
    let tools = config.tools.as_mut()?;
    let vision = tools.vision_generate.as_mut()?;
    let value = vision
        .api_key
        .take()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if vision.generate_url.is_none() && vision.generate_model.is_none() && vision.api_key.is_none()
    {
        tools.vision_generate = None;
    }
    value
}

fn codex_app_injection_effective(value: &Value) -> bool {
    value.get("ok").and_then(Value::as_bool).unwrap_or(false)
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

fn ok(body: Value) -> ManagerJsonResponse {
    status(200, body)
}

fn status(status: u16, body: Value) -> ManagerJsonResponse {
    ManagerJsonResponse { status, body }
}

fn proxy_port_source(configured_port: Option<u16>) -> &'static str {
    if env::var("CODESEEX_PORT").is_ok() {
        "env"
    } else if configured_port.is_some() {
        "config"
    } else {
        "default"
    }
}

fn network_proxy_to_ui(value: codeseex_core::NetworkProxyMode) -> &'static str {
    match value {
        codeseex_core::NetworkProxyMode::System => "system",
        codeseex_core::NetworkProxyMode::None => "none",
    }
}

fn canonical_enabled_tool_ids(ids: &[String]) -> Vec<String> {
    let mut output = Vec::new();
    for id in ids {
        let canonical = match id.as_str() {
            "vision_generate" | "imagegen" | "image_generation" | "generate_image"
            | "image_generate" | "create_image" => "image_gen",
            _ => id.as_str(),
        };
        if !output.iter().any(|value| value == canonical) {
            output.push(canonical.to_owned());
        }
    }
    output
}

fn languages() -> Value {
    json!({
        "ok": true,
        "default": "en_us",
        "system": "system",
        "system_locale": std::env::var("LANG").ok(),
        "languages": [
            { "id": "en_us", "name": "English - US", "url": "/lang/en_us.json" },
            { "id": "zh_cn", "name": "简体中文 - 中国大陆", "url": "/lang/zh_cn.json" },
            { "id": "zh_tw", "name": "繁體中文 - 台灣", "url": "/lang/zh_tw.json" },
            { "id": "zh_hk", "name": "繁體中文 - 香港", "url": "/lang/zh_hk.json" },
            { "id": "ja_jp", "name": "日本語", "url": "/lang/ja_jp.json" },
            { "id": "ko_kr", "name": "한국어", "url": "/lang/ko_kr.json" },
            { "id": "fr_fr", "name": "Français", "url": "/lang/fr_fr.json" },
            { "id": "de_de", "name": "Deutsch", "url": "/lang/de_de.json" },
            { "id": "ru_ru", "name": "Русский", "url": "/lang/ru_ru.json" }
        ]
    })
}

fn app_info() -> Value {
    json!({
        "ok": true,
        "name": "CodeSeeX",
        "product_name": "CodeSeeX",
        "version": env!("CARGO_PKG_VERSION"),
        "license": "AGPL-3.0-only",
        "description": "Local Codex and DeepSeek bridge with a lightweight Tauri manager.",
        "repository": CODESEEX_REPOSITORY_URL,
        "urls": {
            "website": CODESEEX_WEBSITE_URL,
            "source": CODESEEX_REPOSITORY_URL,
            "feedback": format!("{CODESEEX_REPOSITORY_URL}/issues"),
            "license": format!("{CODESEEX_REPOSITORY_URL}/blob/main/LICENSE"),
            "releases": format!("{CODESEEX_REPOSITORY_URL}/releases")
        }
    })
}

fn release_notes_url(version: &str) -> String {
    format!("{CODESEEX_RELEASE_NOTES_RAW_BASE_URL}/v{version}/docs/release-notes.json")
}

fn embedded_release_notes(version: &str) -> Value {
    let document = serde_json::from_str::<Value>(EMBEDDED_RELEASE_NOTES).unwrap_or_else(|_| {
        json!({
            "schema_version": 1,
            "releases": []
        })
    });
    debug_assert!(release_notes_document_is_valid(&document, version));
    document
}

fn release_notes_fallback_response(
    cached: Option<CachedReleaseNotes>,
    fallback: Value,
    current_version: &str,
    error: &str,
) -> Value {
    if let Some(cached) = cached {
        return release_notes_response(
            cached.document,
            current_version,
            "cache",
            true,
            Some(error),
        );
    }
    release_notes_response(fallback, current_version, "bundled", true, Some(error))
}

fn release_notes_response(
    document: Value,
    current_version: &str,
    source: &str,
    stale: bool,
    error: Option<&str>,
) -> Value {
    json!({
        "ok": true,
        "current_version": current_version,
        "source": source,
        "stale": stale,
        "error": error,
        "release_notes": document
    })
}

fn release_notes_document_is_valid(document: &Value, current_version: &str) -> bool {
    if document.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return false;
    }
    document
        .get("releases")
        .and_then(Value::as_array)
        .is_some_and(|releases| {
            releases.iter().any(|release| {
                release
                    .get("version")
                    .and_then(Value::as_str)
                    .is_some_and(|version| release_notes_version_matches(version, current_version))
                    && release
                        .pointer("/notes/en_us")
                        .is_some_and(Value::is_object)
                    && release
                        .pointer("/notes/zh_cn")
                        .is_some_and(Value::is_object)
            })
        })
}

fn release_notes_version_matches(value: &str, current_version: &str) -> bool {
    value.trim().trim_start_matches('v') == current_version.trim().trim_start_matches('v')
}

fn normalize_balance_info(item: &Value) -> Value {
    json!({
        "currency": item.get("currency").and_then(Value::as_str).unwrap_or("").to_owned(),
        "total_balance": balance_value_to_string(item.get("total_balance")),
        "granted_balance": balance_value_to_string(item.get("granted_balance")),
        "topped_up_balance": balance_value_to_string(item.get("topped_up_balance"))
    })
}

fn balance_value_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => "0".to_owned(),
    }
}

fn balance_error_message(bytes: &[u8]) -> String {
    let body = serde_json::from_slice::<Value>(bytes).unwrap_or_else(|_| json!({}));
    body.get("error")
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .or_else(|| body.get("message").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let text = String::from_utf8_lossy(bytes).trim().to_owned();
            if text.is_empty() {
                "DeepSeek balance request failed.".to_owned()
            } else {
                text
            }
        })
}

fn value_to_u32(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| value.as_str().and_then(|text| text.parse::<u32>().ok()))
}

fn value_to_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
}

fn model_list_params_from_query(query: Option<&Value>) -> AppServerModelListParams {
    AppServerModelListParams {
        cursor: query
            .and_then(|value| value.get("cursor"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
        limit: query
            .and_then(|value| value.get("limit"))
            .and_then(value_to_u32),
        include_hidden: query
            .and_then(|value| value.get("includeHidden"))
            .and_then(value_to_bool),
    }
}

fn value_to_bool(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| {
        let text = value.as_str()?.trim().to_ascii_lowercase();
        match text.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_config(label: &str) -> AppConfig {
        AppConfig {
            data_dir: std::env::temp_dir()
                .join(format!("codeseex-manager-{label}-{}", Uuid::new_v4())),
            ..Default::default()
        }
    }

    #[test]
    fn image_capability_migration_adds_analyze_without_mapping_the_old_id_to_generation() {
        let config = temp_config("image-capability-migration");
        UserConfig {
            tools: Some(codeseex_core::UserToolsConfig {
                enabled: Some(vec![
                    "list_directory".to_owned(),
                    "read_file_range".to_owned(),
                    "workspace_search".to_owned(),
                ]),
                vision_analyze: Some(codeseex_core::UserVisionToolConfig {
                    generate_url: Some(String::new()),
                    generate_model: Some(String::new()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
        .write_atomic(&config.config_path())
        .expect("write legacy config");

        assert!(migrate_image_capability_config(&config).expect("migrate config"));
        let migrated = UserConfig::read_from(&config.config_path()).expect("read migrated config");
        let tools = migrated.tools.as_ref().expect("migrated tools");
        let enabled = tools.enabled.as_ref().expect("migrated enabled tools");
        assert_eq!(
            tools.capability_schema_version,
            Some(codeseex_core::IMAGE_CAPABILITY_SCHEMA_VERSION)
        );
        assert!(enabled.iter().any(|id| id == "vision_analyze"));
        assert!(!enabled.iter().any(|id| id == "image_gen"));

        let mut user_closed = migrated;
        user_closed
            .tools
            .as_mut()
            .expect("tools")
            .enabled
            .as_mut()
            .expect("enabled")
            .retain(|id| id != "vision_analyze");
        user_closed
            .write_atomic(&config.config_path())
            .expect("write user choice");
        assert!(!migrate_image_capability_config(&config).expect("repeat migration"));
        let after_user_choice =
            UserConfig::read_from(&config.config_path()).expect("read after user choice");
        assert!(!after_user_choice
            .tools
            .and_then(|tools| tools.enabled)
            .unwrap_or_default()
            .iter()
            .any(|id| id == "vision_analyze"));
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn image_capability_migration_enables_generation_only_for_legacy_generation_evidence() {
        let config = temp_config("image-capability-generation-migration");
        UserConfig {
            tools: Some(codeseex_core::UserToolsConfig {
                enabled: Some(vec!["vision_analyze".to_owned()]),
                vision_analyze: Some(codeseex_core::UserVisionToolConfig {
                    generate_url: Some("https://images.example/v1/images/generations".to_owned()),
                    generate_model: Some("legacy-image-model".to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
        .write_atomic(&config.config_path())
        .expect("write legacy generation config");

        migrate_image_capability_config(&config).expect("migrate generation config");
        let migrated = UserConfig::read_from(&config.config_path()).expect("read migrated config");
        let tools = migrated.tools.as_ref().expect("migrated tools");
        let enabled = tools.enabled.as_ref().expect("enabled tools");
        assert!(enabled.iter().any(|id| id == "vision_analyze"));
        assert!(enabled.iter().any(|id| id == "image_gen"));
        let generation = tools
            .vision_generate
            .as_ref()
            .expect("migrated generation config");
        assert_eq!(
            generation.generate_url.as_deref(),
            Some("https://images.example/v1/images/generations")
        );
        assert_eq!(
            generation.generate_model.as_deref(),
            Some("legacy-image-model")
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn manager_open_survives_malformed_config_without_rewriting_it() {
        let config = temp_config("malformed-image-capability-config");
        std::fs::create_dir_all(&config.data_dir).expect("create data dir");
        let original = "[tools\nenabled = [\"vision_analyze\"\n";
        std::fs::write(config.config_path(), original).expect("write malformed config");

        ManagerRuntime::open(config.clone())
            .await
            .expect("manager should start with malformed config");
        assert_eq!(
            std::fs::read_to_string(config.config_path()).expect("read malformed config"),
            original
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn ensure_catalog_generates_embedded_catalog_without_native_codex() {
        let config = temp_config("embedded-catalog");

        ensure_catalog(&config).expect("ensure embedded catalog");

        assert!(catalog_file_is_compatible(&config.catalog_path()));
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn ensure_catalog_rewrites_compatible_but_stale_catalog() {
        let config = temp_config("stale-embedded-catalog");

        ensure_catalog(&config).expect("ensure embedded catalog");
        let mut stale =
            serde_json::from_str::<Value>(&std::fs::read_to_string(config.catalog_path()).unwrap())
                .expect("catalog json");
        stale["_codeseex_stale_marker"] = json!(true);
        std::fs::write(
            config.catalog_path(),
            serde_json::to_string_pretty(&stale).unwrap() + "\n",
        )
        .expect("write stale catalog");
        assert!(catalog_file_is_compatible(&config.catalog_path()));

        ensure_catalog(&config).expect("refresh stale catalog");

        let refreshed = std::fs::read_to_string(config.catalog_path()).unwrap();
        assert!(!refreshed.contains("_codeseex_stale_marker"));
        assert!(catalog_file_matches_current(
            &config.catalog_path(),
            &build_codeseex_catalog()
        ));
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn adapter_reports_builtin_catalog_for_legacy_modes() {
        let config = temp_config("builtin-catalog-mode");
        let user_config = UserConfig {
            catalog: Some(codeseex_core::UserCatalogConfig {
                mode: Some("auto".to_owned()),
            }),
            ..UserConfig::default()
        };
        user_config
            .write_atomic(&config.config_path())
            .expect("write legacy catalog mode");
        let runtime = ManagerRuntime::open(config.clone())
            .await
            .expect("open manager runtime");

        let response = runtime.generate_adapter();

        assert_eq!(response.status, 200);
        assert_eq!(
            response.body.get("catalog_mode").and_then(Value::as_str),
            Some("builtin")
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn adapter_reports_catalog_diagnostic_after_regeneration() {
        let config = temp_config("catalog-diagnostic");
        let runtime = ManagerRuntime::open(config.clone())
            .await
            .expect("open manager runtime");

        let response = runtime.generate_adapter();

        assert_eq!(response.status, 200);
        let diagnostic = response
            .body
            .get("catalog_diagnostic")
            .expect("catalog diagnostic");
        assert_eq!(
            diagnostic.get("status").and_then(Value::as_str),
            Some("repaired")
        );
        assert_eq!(
            diagnostic.get("issue").and_then(Value::as_str),
            Some("regenerated")
        );
        assert_eq!(
            diagnostic
                .get("toml_has_model_catalog_json")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            diagnostic
                .get("toml_catalog_path_matches")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            diagnostic.get("model_count").and_then(Value::as_u64),
            Some(2)
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn lifecycle_routes_are_explicit_http_compat_actions() {
        let config = temp_config("compat-action");
        let runtime = ManagerRuntime::open(config.clone())
            .await
            .expect("open manager runtime");

        let response = runtime
            .handle_json("POST", "/api/restart", None, None)
            .await;

        assert_eq!(response.status, 200);
        assert_eq!(response.body.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            response.body.get("mode").and_then(Value::as_str),
            Some("http_compat")
        );
        assert_eq!(
            response.body.get("action").and_then(Value::as_str),
            Some("restart")
        );
        assert_eq!(
            response.body.get("effect").and_then(Value::as_str),
            Some("not_applicable_without_desktop_runtime")
        );

        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn status_does_not_embed_log_events() {
        let config = temp_config("status-no-events");
        let runtime = ManagerRuntime::open(config.clone())
            .await
            .expect("open manager runtime");
        runtime
            .store
            .record_event("info", "test_event", "event should stay in logs", None)
            .await
            .expect("record event");

        let status = runtime.status().await;

        assert!(status.get("events").is_none());
        assert_eq!(
            status.pointer("/runtime/status").and_then(Value::as_str),
            Some("running")
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn events_api_returns_safe_diagnostic_view() {
        let config = temp_config("events-safe-diagnostic-view");
        let runtime = ManagerRuntime::open(config.clone())
            .await
            .expect("open manager runtime");
        runtime
            .store
            .record_event(
                "info",
                "context_compile_diagnostic",
                "Context compile diagnostic.",
                Some(&json!({
                    "id": "resp_events_api",
                    "context": {
                        "input_items": 5,
                        "message_items": 2,
                        "tool_result_items": 1,
                        "unsafe_prompt": "do not expose"
                    }
                })),
            )
            .await
            .expect("record diagnostic");

        let response = runtime
            .handle_json(
                "GET",
                "/api/events",
                Some(&json!({
                    "limit": 10,
                    "audience": "safe",
                    "category": "context"
                })),
                None,
            )
            .await;

        assert_eq!(response.status, 200);
        assert_eq!(response.body.get("ok").and_then(Value::as_bool), Some(true));
        let events = response
            .body
            .get("events")
            .and_then(Value::as_array)
            .expect("events array");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("category").and_then(Value::as_str),
            Some("context")
        );
        assert_eq!(
            events[0].get("request_id").and_then(Value::as_str),
            Some("resp_events_api")
        );
        let body = serde_json::to_string(&response.body).expect("body json");
        assert!(!body.contains("unsafe_prompt"));

        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn status_and_usage_share_runtime_totals() {
        let config = temp_config("status-usage-runtime");
        let runtime = ManagerRuntime::open(config.clone())
            .await
            .expect("open manager runtime");
        runtime
            .store
            .checkpoint_request(
                "resp_usage",
                None,
                Some("deepseek-v4-flash"),
                &json!({ "model": "deepseek-v4-flash", "input": "hello" }),
            )
            .await
            .expect("checkpoint request");
        runtime
            .store
            .finish_request(
                "resp_usage",
                codeseex_store::RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "usage": {
                        "input_tokens": 13,
                        "cached_input_tokens": 5,
                        "output_tokens": 2,
                        "total_tokens": 15
                    }
                })),
                None,
            )
            .await
            .expect("finish request");

        let status = runtime.status().await;
        let usage = runtime.usage().await;

        for key in [
            "request_count",
            "billable_request_count",
            "total_cached_input_tokens",
            "total_cache_miss_input_tokens",
            "total_output_tokens",
            "usage_revision",
            "event_revision",
        ] {
            assert_eq!(
                status.pointer(&format!("/runtime/{key}")),
                usage.pointer(&format!("/runtime/{key}")),
                "{key} should match between status and usage"
            );
        }
        assert!(status.pointer("/runtime/billable_history").is_none());
        assert!(status.pointer("/runtime/turn_history").is_none());
        assert!(status.pointer("/runtime/usage_sessions").is_none());
        assert_eq!(
            usage
                .pointer("/runtime/usage_sessions/0/title")
                .and_then(Value::as_str),
            Some("hello")
        );

        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn config_payload_maps_vision_tool_section_to_ui_fields() {
        let config = temp_config("vision-config");
        let user_config = UserConfig {
            tools: Some(codeseex_core::UserToolsConfig {
                enabled: Some(vec!["vision_analyze".to_owned()]),
                vision_analyze: Some(codeseex_core::UserVisionToolConfig {
                    analyze_url: Some("https://vision.example.com/v1".to_owned()),
                    analyze_model: Some("vision-model".to_owned()),
                    api_key: Some("secret-key".to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..UserConfig::default()
        };
        user_config
            .write_atomic(&config.config_path())
            .expect("write vision config");
        let runtime = ManagerRuntime::open(config.clone())
            .await
            .expect("open manager runtime");

        let payload = runtime.config_payload();

        assert_eq!(
            payload
                .get("ENABLED_TOOLS")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(Value::as_str),
            Some("vision_analyze")
        );
        assert_eq!(
            payload.get("VISION_ANALYZE_URL").and_then(Value::as_str),
            Some("https://vision.example.com/v1")
        );
        assert_eq!(
            payload.get("VISION_ANALYZE_MODEL").and_then(Value::as_str),
            Some("vision-model")
        );
        assert!(payload.get("VISION_API_KEY").is_none());
        assert_eq!(
            payload
                .get("VISION_ANALYZE_API_KEY_CONFIGURED")
                .and_then(Value::as_bool),
            Some(true)
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn legacy_flat_vision_secret_is_read_before_cleanup() {
        let mut config = UserConfig {
            tools: Some(codeseex_core::UserToolsConfig {
                settings: Some(std::collections::BTreeMap::from([(
                    "VISION_API_KEY".to_owned(),
                    "legacy-flat-key".to_owned(),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            vision_setting_value(&config, "VISION_API_KEY").as_deref(),
            Some("legacy-flat-key")
        );
        remove_vision_secret_settings(&mut config);
        assert!(vision_setting_value(&config, "VISION_API_KEY").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn legacy_vision_payload_does_not_write_generation_secret() {
        let config = temp_config("legacy-vision-secret-isolation");
        crate::secrets::clear_vision_analyze_api_key(&config).expect("clear analyze secret");
        crate::secrets::clear_vision_generate_api_key(&config).expect("clear generate secret");
        crate::secrets::clear_legacy_vision_api_key(&config).expect("clear legacy secret");
        let mut existing = UserConfig::default();

        apply_secret_payload(
            &config,
            &json!({ crate::tools::vision::API_KEY_KEY: "legacy-payload-secret" }),
            &mut existing,
        )
        .expect("apply legacy vision payload");

        assert_eq!(
            crate::secrets::vision_analyze_api_key(&config).as_deref(),
            Some("legacy-payload-secret")
        );
        assert!(crate::secrets::vision_generate_api_key(&config).is_none());

        crate::secrets::clear_vision_analyze_api_key(&config).expect("clear analyze secret");
        crate::secrets::clear_vision_generate_api_key(&config).expect("clear generate secret");
        crate::secrets::clear_legacy_vision_api_key(&config).expect("clear legacy secret");
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn legacy_flat_vision_settings_move_to_separate_capabilities() {
        let mut config = UserConfig {
            tools: Some(codeseex_core::UserToolsConfig {
                settings: Some(std::collections::BTreeMap::from([
                    (
                        crate::tools::vision::ANALYZE_URL_KEY.to_owned(),
                        "https://vision.example/v1/responses".to_owned(),
                    ),
                    (
                        crate::tools::vision::ANALYZE_MODEL_KEY.to_owned(),
                        "custom-vision".to_owned(),
                    ),
                    (
                        crate::tools::vision::GENERATE_URL_KEY.to_owned(),
                        "https://image.example/v1/images/generations".to_owned(),
                    ),
                    (
                        crate::tools::vision::GENERATE_MODEL_KEY.to_owned(),
                        "custom-image".to_owned(),
                    ),
                ])),
                ..Default::default()
            }),
            ..Default::default()
        };
        migrate_legacy_vision_config(&mut config);
        let tools = config.tools.expect("tools config");
        let analyze = tools.vision_analyze.expect("analyze config");
        assert_eq!(
            analyze.backend,
            Some(codeseex_core::VisionAnalyzeBackend::External)
        );
        assert_eq!(analyze.analyze_model.as_deref(), Some("custom-vision"));
        let generate = tools.vision_generate.expect("generate config");
        assert_eq!(generate.generate_model.as_deref(), Some("custom-image"));
        assert!(tools.settings.is_none());
    }

    #[test]
    fn app_info_uses_final_product_name() {
        let info = app_info();
        assert_eq!(info.get("name").and_then(Value::as_str), Some("CodeSeeX"));
        assert_eq!(
            info.get("product_name").and_then(Value::as_str),
            Some("CodeSeeX")
        );
        assert_eq!(
            info.pointer("/urls/website").and_then(Value::as_str),
            Some(CODESEEX_WEBSITE_URL)
        );
    }

    #[test]
    fn embedded_release_notes_include_the_current_version_in_both_languages() {
        let version = env!("CARGO_PKG_VERSION");
        let document = embedded_release_notes(version);

        assert!(release_notes_document_is_valid(&document, version));
        let release = document["releases"]
            .as_array()
            .and_then(|releases| {
                releases.iter().find(|release| {
                    release
                        .get("version")
                        .and_then(Value::as_str)
                        .is_some_and(|value| release_notes_version_matches(value, version))
                })
            })
            .expect("current release notes");
        assert!(release.pointer("/notes/en_us/sections").is_some());
        assert!(release.pointer("/notes/zh_cn/sections").is_some());
    }

    #[test]
    fn release_notes_cache_preserves_etag_and_only_accepts_current_documents() {
        let version = env!("CARGO_PKG_VERSION");
        let cache = ReleaseNotesCache::default();
        let document = embedded_release_notes(version);
        cache.store(document.clone(), Some("W/\"release-notes\"".to_owned()));

        assert_eq!(cache.fresh_for(version), Some(document.clone()));
        assert_eq!(
            cache.cached_for(version).and_then(|entry| entry.etag),
            Some("W/\"release-notes\"".to_owned())
        );
        assert!(cache.fresh_for("999.0.0").is_none());

        cache.record_failure("release_notes_unreachable");
        assert_eq!(
            cache.recent_failure().as_deref(),
            Some("release_notes_unreachable")
        );
        cache.store(document, None);
        assert!(cache.recent_failure().is_none());
    }
}
