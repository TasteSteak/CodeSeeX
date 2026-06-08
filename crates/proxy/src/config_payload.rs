use codeseex_core::models::{TemperaturePreset, UpstreamModelOverride};
use codeseex_core::{
    parse_network_proxy_mode, AppConfig, NetworkProxyMode, UserBillingConfig, UserConfig,
    UserModelConfig, UserNetworkConfig, UserProxyConfig, UserToolsConfig, UserUiConfig,
    UserUpstreamConfig,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub(crate) fn user_config_from_payload(
    payload: &Value,
    mut config: UserConfig,
    app_config: &AppConfig,
) -> UserConfig {
    if payload.get("PROXY_PORT").is_some() {
        let proxy = config.proxy.get_or_insert_with(UserProxyConfig::default);
        proxy.port = value_u16(payload, "PROXY_PORT");
    }

    if payload.get("DEEPSEEK_BASE_URL").is_some()
        || payload.get("DEEPSEEK_OFFICIAL_V1_COMPAT").is_some()
    {
        let upstream = config
            .upstream
            .get_or_insert_with(UserUpstreamConfig::default);
        if payload.get("DEEPSEEK_BASE_URL").is_some() {
            upstream.base_url = value_string(payload, "DEEPSEEK_BASE_URL");
        }
        if payload.get("DEEPSEEK_OFFICIAL_V1_COMPAT").is_some() {
            upstream.official_v1_compat = value_bool(payload, "DEEPSEEK_OFFICIAL_V1_COMPAT");
        }
    }

    if payload.get("UPSTREAM_MODEL_OVERRIDE").is_some()
        || payload.get("DEEPSEEK_TEMPERATURE_PRESET").is_some()
        || payload.get("DEEPSEEK_THINKING").is_some()
    {
        let model = config.model.get_or_insert_with(UserModelConfig::default);
        if payload.get("UPSTREAM_MODEL_OVERRIDE").is_some() {
            model.override_mode = value_model_override(payload, "UPSTREAM_MODEL_OVERRIDE");
        }
        if payload.get("DEEPSEEK_TEMPERATURE_PRESET").is_some() {
            model.temperature = value_temperature(payload, "DEEPSEEK_TEMPERATURE_PRESET");
        }
        if payload.get("DEEPSEEK_THINKING").is_some() {
            model.thinking = value_string(payload, "DEEPSEEK_THINKING");
        }
    }

    if payload.get("UI_THEME").is_some()
        || payload.get("UI_LANGUAGE").is_some()
        || payload.get("SHOW_THINKING").is_some()
        || payload.get("AUTO_START").is_some()
        || payload.get("UI_CLOSE_BEHAVIOR").is_some()
        || payload.get("LOG_RETENTION_DAYS").is_some()
    {
        let ui = config.ui.get_or_insert_with(UserUiConfig::default);
        if payload.get("UI_THEME").is_some() {
            ui.theme = value_string(payload, "UI_THEME");
        }
        if payload.get("UI_LANGUAGE").is_some() {
            ui.language = value_string(payload, "UI_LANGUAGE");
        }
        if payload.get("SHOW_THINKING").is_some() {
            ui.show_thinking = value_bool(payload, "SHOW_THINKING");
        }
        if payload.get("AUTO_START").is_some() {
            ui.auto_start = value_bool(payload, "AUTO_START");
        }
        if payload.get("UI_CLOSE_BEHAVIOR").is_some() {
            ui.close_behavior = value_string(payload, "UI_CLOSE_BEHAVIOR");
        }
        if payload.get("LOG_RETENTION_DAYS").is_some() {
            ui.log_retention_days = value_u16(payload, "LOG_RETENTION_DAYS");
        }
    }

    if payload.get("BILLING_FLASH_CACHED_INPUT_CNY").is_some()
        || payload.get("BILLING_FLASH_CACHE_MISS_INPUT_CNY").is_some()
        || payload.get("BILLING_FLASH_OUTPUT_CNY").is_some()
        || payload.get("BILLING_PRO_CACHED_INPUT_CNY").is_some()
        || payload.get("BILLING_PRO_CACHE_MISS_INPUT_CNY").is_some()
        || payload.get("BILLING_PRO_OUTPUT_CNY").is_some()
    {
        let billing = config
            .billing
            .get_or_insert_with(UserBillingConfig::default);
        if payload.get("BILLING_FLASH_CACHED_INPUT_CNY").is_some() {
            billing.flash_cached_input_cny = value_f64(payload, "BILLING_FLASH_CACHED_INPUT_CNY");
        }
        if payload.get("BILLING_FLASH_CACHE_MISS_INPUT_CNY").is_some() {
            billing.flash_cache_miss_input_cny =
                value_f64(payload, "BILLING_FLASH_CACHE_MISS_INPUT_CNY");
        }
        if payload.get("BILLING_FLASH_OUTPUT_CNY").is_some() {
            billing.flash_output_cny = value_f64(payload, "BILLING_FLASH_OUTPUT_CNY");
        }
        if payload.get("BILLING_PRO_CACHED_INPUT_CNY").is_some() {
            billing.pro_cached_input_cny = value_f64(payload, "BILLING_PRO_CACHED_INPUT_CNY");
        }
        if payload.get("BILLING_PRO_CACHE_MISS_INPUT_CNY").is_some() {
            billing.pro_cache_miss_input_cny =
                value_f64(payload, "BILLING_PRO_CACHE_MISS_INPUT_CNY");
        }
        if payload.get("BILLING_PRO_OUTPUT_CNY").is_some() {
            billing.pro_output_cny = value_f64(payload, "BILLING_PRO_OUTPUT_CNY");
        }
    }

    if payload.get("NETWORK_PROXY_MODE").is_some() || payload.get("WEB_SEARCH_PROXY_MODE").is_some()
    {
        let network = config
            .network
            .get_or_insert_with(UserNetworkConfig::default);
        network.proxy = value_network_proxy(payload, "NETWORK_PROXY_MODE")
            .or_else(|| value_network_proxy(payload, "WEB_SEARCH_PROXY_MODE"));
        clear_legacy_web_search_proxy(&mut config);
    }

    let mut tool_config_keys = crate::tools::registry::builtin_tool_config_keys();
    tool_config_keys.extend(crate::community_tools::community_tool_config_keys(
        &app_config.data_dir,
    ));
    let has_tool_settings = tool_config_keys
        .iter()
        .any(|key| payload.get(key).is_some());
    if payload.get("ENABLED_TOOLS").is_some() || has_tool_settings {
        let tools = config.tools.get_or_insert_with(UserToolsConfig::default);
        if payload.get("ENABLED_TOOLS").is_some() {
            tools.enabled = value_string_list(payload, "ENABLED_TOOLS").map(configurable_tool_ids);
        }
        if has_tool_settings {
            let settings = tools.settings.get_or_insert_with(BTreeMap::new);
            for key in tool_config_keys {
                let Some(value) = payload.get(&key) else {
                    continue;
                };
                if value.is_null() {
                    settings.remove(&key);
                } else if let Some(value) = crate::community_tools::value_to_setting_string(value) {
                    settings.insert(key, value);
                }
            }
            normalize_builtin_tool_settings(settings);
        }
    }

    config
}

fn clear_legacy_web_search_proxy(config: &mut UserConfig) {
    let Some(tools) = config.tools.as_mut() else {
        return;
    };
    if let Some(web_search) = tools.web_search.as_mut() {
        web_search.proxy = None;
    }
    if tools
        .web_search
        .as_ref()
        .is_some_and(|web_search| web_search.proxy.is_none())
    {
        tools.web_search = None;
    }
}

fn normalize_builtin_tool_settings(settings: &mut BTreeMap<String, String>) {
    migrate_tool_setting(settings, "VISUAL_SEARCH_URL", "VISION_ANALYZE_URL");
    migrate_tool_setting(settings, "VISUAL_SEARCH_MODEL", "VISION_ANALYZE_MODEL");
    migrate_tool_setting(settings, "VISUAL_SEARCH_API_KEY", "VISION_API_KEY");
}

fn migrate_tool_setting(settings: &mut BTreeMap<String, String>, old_key: &str, new_key: &str) {
    if settings.contains_key(new_key) {
        settings.remove(old_key);
        return;
    }
    if let Some(value) = settings.remove(old_key) {
        settings.insert(new_key.to_owned(), value);
    }
}

fn configurable_tool_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|id| {
            !matches!(
                id.as_str(),
                "apply_patch" | "web_search" | "mcp" | "mcp_server"
            )
        })
        .map(|id| {
            if matches!(
                id.as_str(),
                "visual_search"
                    | "vision_generate"
                    | "image_gen"
                    | "imagegen"
                    | "image_generation"
                    | "generate_image"
                    | "image_generate"
                    | "create_image"
            ) {
                "vision_analyze".to_owned()
            } else {
                id
            }
        })
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

fn value_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn value_bool(payload: &Value, key: &str) -> Option<bool> {
    let value = payload.get(key)?;
    if let Some(value) = value.as_bool() {
        return Some(value);
    }
    let value = value.as_str()?.trim().to_ascii_lowercase();
    Some(matches!(
        value.as_str(),
        "1" | "true" | "yes" | "on" | "enabled"
    ))
}

fn value_string_list(payload: &Value, key: &str) -> Option<Vec<String>> {
    let value = payload.get(key)?;
    if let Some(items) = value.as_array() {
        return Some(normalize_string_list(
            items.iter().filter_map(Value::as_str),
        ));
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    if let Ok(parsed) = serde_json::from_str::<Vec<String>>(text) {
        return Some(normalize_string_list(parsed.iter().map(String::as_str)));
    }
    Some(normalize_string_list(text.split(',')))
}

fn normalize_string_list<'a>(items: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut output = Vec::new();
    for item in items {
        let normalized = item
            .trim()
            .to_ascii_lowercase()
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let normalized = normalized.trim_matches('_').to_owned();
        if normalized.is_empty() || output.contains(&normalized) {
            continue;
        }
        output.push(normalized);
    }
    output.sort();
    output
}

fn value_u16(payload: &Value, key: &str) -> Option<u16> {
    if let Some(value) = payload.get(key).and_then(Value::as_u64) {
        return u16::try_from(value).ok();
    }
    payload
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| value.trim().parse().ok())
}

fn value_f64(payload: &Value, key: &str) -> Option<f64> {
    if let Some(value) = payload.get(key).and_then(Value::as_f64) {
        return Some(value);
    }
    payload
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| value.trim().parse().ok())
}

fn value_model_override(payload: &Value, key: &str) -> Option<UpstreamModelOverride> {
    match value_string(payload, key)?.to_ascii_lowercase().as_str() {
        "flash" | "deepseek-v4-flash" => Some(UpstreamModelOverride::Flash),
        "pro" | "deepseek-v4-pro" => Some(UpstreamModelOverride::Pro),
        "default" => Some(UpstreamModelOverride::Default),
        _ => None,
    }
}

fn value_temperature(payload: &Value, key: &str) -> Option<TemperaturePreset> {
    match value_string(payload, key)?.to_ascii_lowercase().as_str() {
        "strict" => Some(TemperaturePreset::Strict),
        "balanced" => Some(TemperaturePreset::Balanced),
        "general" => Some(TemperaturePreset::General),
        "creative" => Some(TemperaturePreset::Creative),
        "default" => Some(TemperaturePreset::Default),
        _ => None,
    }
}

fn value_network_proxy(payload: &Value, key: &str) -> Option<NetworkProxyMode> {
    parse_network_proxy_mode(&value_string(payload, key)?)
}

pub(crate) fn model_override_to_ui(value: UpstreamModelOverride) -> &'static str {
    match value {
        UpstreamModelOverride::Default => "default",
        UpstreamModelOverride::Flash => "deepseek-v4-flash",
        UpstreamModelOverride::Pro => "deepseek-v4-pro",
    }
}

pub(crate) fn temperature_to_ui(value: TemperaturePreset) -> &'static str {
    match value {
        TemperaturePreset::Default => "default",
        TemperaturePreset::Strict => "strict",
        TemperaturePreset::Balanced => "balanced",
        TemperaturePreset::General => "general",
        TemperaturePreset::Creative => "creative",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_does_not_persist_api_key_or_catalog_mode() {
        let config = user_config_from_payload(
            &json!({
                "DEEPSEEK_BASE_URL": "http://127.0.0.1:9000/v1",
                "DEEPSEEK_API_KEY": "should-not-be-saved",
                "CATALOG_MODE": "auto"
            }),
            UserConfig::default(),
            &AppConfig::default(),
        );

        assert_eq!(
            config
                .upstream
                .as_ref()
                .and_then(|upstream| upstream.base_url.as_deref()),
            Some("http://127.0.0.1:9000/v1")
        );
        assert!(config
            .upstream
            .as_ref()
            .and_then(|upstream| upstream.api_key.as_ref())
            .is_none());
        assert!(config.catalog.is_none());
    }

    #[test]
    fn payload_persists_vision_tool_settings() {
        let config = user_config_from_payload(
            &json!({
                "ENABLED_TOOLS": ["vision_analyze"],
                "VISION_ANALYZE_URL": "https://vision.example.com/v1",
                "VISION_ANALYZE_MODEL": "vision-model",
                "VISION_GENERATE_URL": "https://vision.example.com/v1/images/generations",
                "VISION_GENERATE_MODEL": "image-model",
                "VISION_API_KEY": "visual-secret"
            }),
            UserConfig::default(),
            &AppConfig::default(),
        );
        let tools = config.tools.expect("tools config");
        assert_eq!(
            tools.enabled.as_deref(),
            Some(&["vision_analyze".to_owned()][..])
        );
        let settings = tools.settings.expect("tool settings");
        assert_eq!(
            settings.get("VISION_ANALYZE_URL").map(String::as_str),
            Some("https://vision.example.com/v1")
        );
        assert_eq!(
            settings.get("VISION_ANALYZE_MODEL").map(String::as_str),
            Some("vision-model")
        );
        assert_eq!(
            settings.get("VISION_GENERATE_URL").map(String::as_str),
            Some("https://vision.example.com/v1/images/generations")
        );
        assert_eq!(
            settings.get("VISION_GENERATE_MODEL").map(String::as_str),
            Some("image-model")
        );
        assert_eq!(
            settings.get("VISION_API_KEY").map(String::as_str),
            Some("visual-secret")
        );
    }

    #[test]
    fn payload_persists_network_proxy_outside_tool_config() {
        let config = user_config_from_payload(
            &json!({ "NETWORK_PROXY_MODE": "none" }),
            UserConfig {
                tools: Some(UserToolsConfig {
                    web_search: Some(codeseex_core::UserWebSearchToolConfig {
                        proxy: Some(NetworkProxyMode::System),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &AppConfig::default(),
        );

        assert_eq!(
            config.network.as_ref().and_then(|network| network.proxy),
            Some(NetworkProxyMode::None)
        );
        assert!(config
            .tools
            .as_ref()
            .and_then(|tools| tools.web_search.as_ref())
            .is_none());
    }

    #[test]
    fn payload_accepts_legacy_web_search_proxy_key_as_network_proxy() {
        let config = user_config_from_payload(
            &json!({ "WEB_SEARCH_PROXY_MODE": "none" }),
            UserConfig::default(),
            &AppConfig::default(),
        );

        assert_eq!(
            config.network.as_ref().and_then(|network| network.proxy),
            Some(NetworkProxyMode::None)
        );
    }

    #[test]
    fn payload_canonicalizes_legacy_visual_search_tool_id() {
        let config = user_config_from_payload(
            &json!({ "ENABLED_TOOLS": ["visual_search"] }),
            UserConfig::default(),
            &AppConfig::default(),
        );
        let tools = config.tools.expect("tools config");
        assert_eq!(
            tools.enabled.as_deref(),
            Some(&["vision_analyze".to_owned()][..])
        );
    }

    #[test]
    fn payload_canonicalizes_vision_generate_to_vision_module() {
        let config = user_config_from_payload(
            &json!({ "ENABLED_TOOLS": ["vision_generate", "image_gen"] }),
            UserConfig::default(),
            &AppConfig::default(),
        );
        let tools = config.tools.expect("tools config");
        assert_eq!(
            tools.enabled.as_deref(),
            Some(&["vision_analyze".to_owned()][..])
        );
    }

    #[test]
    fn payload_migrates_legacy_visual_search_tool_settings() {
        let config = user_config_from_payload(
            &json!({
                "VISUAL_SEARCH_URL": "https://vision.example.com/v1",
                "VISUAL_SEARCH_MODEL": "vision-model",
                "VISUAL_SEARCH_API_KEY": "visual-secret"
            }),
            UserConfig::default(),
            &AppConfig::default(),
        );
        let settings = config
            .tools
            .expect("tools config")
            .settings
            .expect("tool settings");
        assert_eq!(
            settings.get("VISION_ANALYZE_URL").map(String::as_str),
            Some("https://vision.example.com/v1")
        );
        assert_eq!(
            settings.get("VISION_ANALYZE_MODEL").map(String::as_str),
            Some("vision-model")
        );
        assert_eq!(
            settings.get("VISION_API_KEY").map(String::as_str),
            Some("visual-secret")
        );
        assert!(!settings.contains_key("VISUAL_SEARCH_URL"));
        assert!(!settings.contains_key("VISUAL_SEARCH_MODEL"));
        assert!(!settings.contains_key("VISUAL_SEARCH_API_KEY"));
    }
}
