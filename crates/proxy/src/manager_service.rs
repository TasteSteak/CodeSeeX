use crate::app_state::ProxyState;
use crate::config_payload::{model_override_to_ui, temperature_to_ui, user_config_from_payload};
use crate::http_utils::{config_version, is_newer_version, normalize_version_label, now_seconds};
use crate::tools::registry::{enabled_tool_ids, tool_registry, tool_settings};
use codeseex_core::catalog::{
    build_codeseex_catalog, catalog_file_is_compatible, codex_toml_snippet, write_catalog_atomic,
};
use codeseex_core::models::available_models;
use codeseex_core::urls::balance_url;
use codeseex_core::{AppConfig, UserConfig};
use codeseex_store::Store;
use serde::Serialize;
use serde_json::{json, Value};
use std::env;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ManagerRuntime {
    config: Arc<AppConfig>,
    client: reqwest::Client,
    store: Store,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagerJsonResponse {
    pub status: u16,
    pub body: Value,
}

impl ManagerRuntime {
    pub async fn open(config: AppConfig) -> anyhow::Result<Self> {
        let timeout = std::time::Duration::from_millis(config.upstream.timeout_ms);
        Ok(Self {
            store: Store::open(&config.data_dir).await?,
            client: crate::network::client(config.network_proxy, timeout)?,
            config: Arc::new(config),
        })
    }

    pub(crate) fn from_proxy_state(state: &ProxyState) -> Self {
        Self {
            config: state.config.clone(),
            client: state.client.clone(),
            store: state.store.clone(),
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
            ("GET", "/api/usage") => ok(self.usage().await),
            ("GET", "/api/config") => ok(self.config_payload()),
            ("POST", "/api/config") => {
                self.save_config(body.cloned().unwrap_or_else(|| json!({})))
                    .await
            }
            ("GET", "/api/languages") => ok(languages()),
            ("GET", "/api/tools") => ok(self.tools()),
            ("GET", "/api/app-info") => ok(app_info()),
            ("GET", "/api/update-check") => ok(self.update_check().await),
            ("GET", "/api/deepseek/balance") => self.balance().await,
            ("GET", "/api/events") => self.events(query).await,
            ("GET", "/api/codex-adapter")
            | ("POST", "/api/codex-adapter/generate")
            | ("GET", "/api/codex-adapter/generate") => self.generate_adapter(),
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
        let mut config = self.config.as_ref().clone();
        if let Ok(user_config) = UserConfig::read_from(&config.config_path()) {
            config.apply_user_config(user_config);
        }
        config
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
                "active_requests": runtime.as_ref().map(|value| value.active_requests).unwrap_or(0),
                "request_count": runtime.as_ref().map(|value| value.request_count).unwrap_or(0),
                "failed_request_count": runtime.as_ref().map(|value| value.failed_request_count).unwrap_or(0),
                "last_request_at": runtime.as_ref().and_then(|value| value.last_request_at.clone()),
                "last_turn": runtime.as_ref().and_then(|value| value.last_turn.clone()),
                "turn_history": [],
                "total_cached_input_tokens": 0,
                "total_cache_miss_input_tokens": 0,
                "total_output_tokens": 0,
                "average_ms": 0
            },
            "upstream": {
                "base_url": config.upstream.base_url,
                "official_v1_compat": config.upstream.official_v1_compat
            }
        })
    }

    pub async fn usage(&self) -> Value {
        let runtime = self.store.runtime_summary(120).await.ok();
        json!({
            "ok": true,
            "runtime": {
                "active_requests": runtime.as_ref().map(|value| value.active_requests).unwrap_or(0),
                "request_count": runtime.as_ref().map(|value| value.request_count).unwrap_or(0),
                "failed_request_count": runtime.as_ref().map(|value| value.failed_request_count).unwrap_or(0),
                "last_request_at": runtime.as_ref().and_then(|value| value.last_request_at.clone()),
                "last_turn": runtime.as_ref().and_then(|value| value.last_turn.clone()),
                "turn_history": runtime.as_ref().map(|value| value.turn_history.clone()).unwrap_or_default(),
                "total_cached_input_tokens": runtime.as_ref().map(|value| value.total_cached_input_tokens).unwrap_or(0),
                "total_cache_miss_input_tokens": runtime.as_ref().map(|value| value.total_cache_miss_input_tokens).unwrap_or(0),
                "total_output_tokens": runtime.as_ref().map(|value| value.total_output_tokens).unwrap_or(0),
                "average_ms": runtime.as_ref().map(|value| value.average_ms).unwrap_or(0)
            }
        })
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
            "UPSTREAM_MODEL_OVERRIDE": model_override_to_ui(model_override),
            "DEEPSEEK_TEMPERATURE_PRESET": temperature_to_ui(temperature),
            "DEEPSEEK_THINKING": model.and_then(|value| value.thinking.as_deref()).unwrap_or("auto"),
            "SHOW_THINKING": ui.and_then(|value| value.show_thinking).unwrap_or(true).to_string(),
            "NETWORK_PROXY_MODE": network_proxy_to_ui(config.network_proxy),
            "AUTO_START": ui.and_then(|value| value.auto_start).unwrap_or(false).to_string(),
            "UI_THEME": ui.and_then(|value| value.theme.as_deref()).unwrap_or("system"),
            "UI_LANGUAGE": ui.and_then(|value| value.language.as_deref()).unwrap_or("system"),
            "UI_CLOSE_BEHAVIOR": ui.and_then(|value| value.close_behavior.as_deref()).unwrap_or("exit"),
            "LOG_RETENTION_DAYS": ui.and_then(|value| value.log_retention_days).unwrap_or(7).to_string(),
            "BILLING_FLASH_CACHED_INPUT_CNY": billing.and_then(|value| value.flash_cached_input_cny).unwrap_or(0.02).to_string(),
            "BILLING_FLASH_CACHE_MISS_INPUT_CNY": billing.and_then(|value| value.flash_cache_miss_input_cny).unwrap_or(1.0).to_string(),
            "BILLING_FLASH_OUTPUT_CNY": billing.and_then(|value| value.flash_output_cny).unwrap_or(2.0).to_string(),
            "BILLING_PRO_CACHED_INPUT_CNY": billing.and_then(|value| value.pro_cached_input_cny).unwrap_or(0.025).to_string(),
            "BILLING_PRO_CACHE_MISS_INPUT_CNY": billing.and_then(|value| value.pro_cache_miss_input_cny).unwrap_or(3.0).to_string(),
            "BILLING_PRO_OUTPUT_CNY": billing.and_then(|value| value.pro_output_cny).unwrap_or(6.0).to_string(),
            "ENABLED_TOOLS": tools.and_then(|value| value.enabled.as_deref()).map(canonical_enabled_tool_ids).map(Value::from).unwrap_or(Value::Null)
        });
        if let Some(settings) = user_config
            .tools
            .as_ref()
            .and_then(|tools| tools.settings.as_ref())
        {
            if let Some(object) = payload.as_object_mut() {
                let mut tool_config_keys = crate::tools::registry::builtin_tool_config_keys();
                tool_config_keys.extend(crate::community_tools::community_tool_config_keys(
                    &config.data_dir,
                ));
                for key in tool_config_keys {
                    if let Some(value) = settings.get(&key) {
                        object.insert(key, Value::String(value.clone()));
                    }
                }
                insert_tool_setting_alias(
                    object,
                    settings,
                    "VISION_ANALYZE_URL",
                    "VISUAL_SEARCH_URL",
                );
                insert_tool_setting_alias(
                    object,
                    settings,
                    "VISION_ANALYZE_MODEL",
                    "VISUAL_SEARCH_MODEL",
                );
                insert_tool_setting_alias(
                    object,
                    settings,
                    "VISION_API_KEY",
                    "VISUAL_SEARCH_API_KEY",
                );
            }
        }
        payload
    }

    pub async fn save_config(&self, payload: Value) -> ManagerJsonResponse {
        let config = self.active_config();
        let existing_config = UserConfig::read_from(&config.config_path()).unwrap_or_default();
        let existing_retention_days = existing_config.log_retention_days();
        let user_config = user_config_from_payload(&payload, existing_config, &config);
        match user_config.write_atomic(&config.config_path()) {
            Ok(()) => {
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
                    "config_version": now_seconds().to_string(),
                    "path": config.config_path().to_string_lossy(),
                    "maintenance": maintenance
                }))
            }
            Err(error) => status(500, json!({ "ok": false, "error": error.to_string() })),
        }
    }

    pub fn tools(&self) -> Value {
        let config = self.active_config();
        let enabled_tools = enabled_tool_ids(&config);
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
        let result = self
            .client
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

        match self
            .client
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
        let before = query
            .and_then(|value| value.get("before"))
            .and_then(Value::as_str);
        match self.store.recent_visible_events(limit, before).await {
            Ok((events, has_more)) => ok(json!({
                "ok": true,
                "events": events,
                "has_more": has_more
            })),
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
        match ensure_catalog(&config) {
            Ok(()) => ok(json!({
                "ok": true,
                "ready": true,
                "catalog_mode": "builtin",
                "catalog_path": config.catalog_path().to_string_lossy(),
                "models": available_models().into_iter().map(|m| m.slug).collect::<Vec<_>>(),
                "context_window": 1_000_000,
                "effective_context_window_percent": 95,
                "toml_snippet": codex_toml_snippet(&config.catalog_path(), &config.proxy_base_url())
            })),
            Err(error) => status(500, json!({ "ok": false, "error": error.to_string() })),
        }
    }
}

pub fn ensure_catalog(config: &AppConfig) -> anyhow::Result<()> {
    if catalog_file_is_compatible(&config.catalog_path()) {
        return Ok(());
    }
    let catalog = build_codeseex_catalog();
    write_catalog_atomic(&config.catalog_path(), &catalog)
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

fn insert_tool_setting_alias(
    object: &mut serde_json::Map<String, Value>,
    settings: &std::collections::BTreeMap<String, String>,
    key: &str,
    legacy_key: &str,
) {
    if object.contains_key(key) {
        return;
    }
    if let Some(value) = settings.get(legacy_key) {
        object.insert(key.to_owned(), Value::String(value.clone()));
    }
}

fn canonical_enabled_tool_ids(ids: &[String]) -> Vec<String> {
    let mut output = Vec::new();
    for id in ids {
        let canonical = match id.as_str() {
            "visual_search" | "vision_generate" | "image_gen" | "imagegen" | "image_generation"
            | "generate_image" | "image_generate" | "create_image" => "vision_analyze",
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
            { "id": "en_us", "name": "English", "url": "/lang/en_us.json" },
            { "id": "zh_cn", "name": "Chinese (Simplified)", "url": "/lang/zh_cn.json" },
            { "id": "zh_tw", "name": "Chinese (Traditional)", "url": "/lang/zh_tw.json" },
            { "id": "zh_hk", "name": "Chinese (Hong Kong)", "url": "/lang/zh_hk.json" },
            { "id": "ja_jp", "name": "Japanese", "url": "/lang/ja_jp.json" },
            { "id": "ko_kr", "name": "Korean", "url": "/lang/ko_kr.json" },
            { "id": "fr_fr", "name": "French", "url": "/lang/fr_fr.json" },
            { "id": "de_de", "name": "Deutsch", "url": "/lang/de_de.json" },
            { "id": "ru_ru", "name": "Russian", "url": "/lang/ru_ru.json" }
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
        "repository": "https://github.com/TasteSteak/CodeSeeX",
        "urls": {
            "source": "https://github.com/TasteSteak/CodeSeeX",
            "feedback": "https://github.com/TasteSteak/CodeSeeX/issues",
            "license": "https://github.com/TasteSteak/CodeSeeX/blob/main/LICENSE",
            "releases": "https://github.com/TasteSteak/CodeSeeX/releases"
        }
    })
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
    fn ensure_catalog_generates_embedded_catalog_without_native_codex() {
        let config = temp_config("embedded-catalog");

        ensure_catalog(&config).expect("ensure embedded catalog");

        assert!(catalog_file_is_compatible(&config.catalog_path()));
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
    async fn config_payload_maps_legacy_visual_search_settings_to_vision_fields() {
        let config = temp_config("legacy-vision-config");
        let mut settings = std::collections::BTreeMap::new();
        settings.insert(
            "VISUAL_SEARCH_URL".to_owned(),
            "https://vision.example.com/v1".to_owned(),
        );
        settings.insert("VISUAL_SEARCH_MODEL".to_owned(), "vision-model".to_owned());
        settings.insert("VISUAL_SEARCH_API_KEY".to_owned(), "secret-key".to_owned());
        let user_config = UserConfig {
            tools: Some(codeseex_core::UserToolsConfig {
                enabled: Some(vec!["visual_search".to_owned()]),
                settings: Some(settings),
                ..Default::default()
            }),
            ..UserConfig::default()
        };
        user_config
            .write_atomic(&config.config_path())
            .expect("write legacy vision config");
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
        assert_eq!(
            payload.get("VISION_API_KEY").and_then(Value::as_str),
            Some("secret-key")
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn app_info_uses_final_product_name() {
        let info = app_info();
        assert_eq!(info.get("name").and_then(Value::as_str), Some("CodeSeeX"));
        assert_eq!(
            info.get("product_name").and_then(Value::as_str),
            Some("CodeSeeX")
        );
    }
}
