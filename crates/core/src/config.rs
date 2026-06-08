use crate::models::{TemperaturePreset, UpstreamModelOverride};
use crate::urls::normalize_base_url;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub host: String,
    pub port: u16,
    pub upstream: UpstreamConfig,
    pub model_override: UpstreamModelOverride,
    pub temperature: TemperaturePreset,
    pub network_proxy: NetworkProxyMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    pub base_url: String,
    pub official_v1_compat: bool,
    // Process environment fallback only. Manager/user TOML is not credential storage.
    pub api_key: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    pub proxy: Option<UserProxyConfig>,
    pub upstream: Option<UserUpstreamConfig>,
    pub model: Option<UserModelConfig>,
    pub catalog: Option<UserCatalogConfig>,
    pub network: Option<UserNetworkConfig>,
    pub ui: Option<UserUiConfig>,
    pub billing: Option<UserBillingConfig>,
    pub tools: Option<UserToolsConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserProxyConfig {
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserUpstreamConfig {
    pub base_url: Option<String>,
    pub official_v1_compat: Option<bool>,
    // Kept to deserialize legacy TOML, but ignored when applying user config.
    pub api_key: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserModelConfig {
    #[serde(rename = "override")]
    pub override_mode: Option<UpstreamModelOverride>,
    pub temperature: Option<TemperaturePreset>,
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserCatalogConfig {
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserNetworkConfig {
    pub proxy: Option<NetworkProxyMode>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserUiConfig {
    pub theme: Option<String>,
    pub language: Option<String>,
    pub show_thinking: Option<bool>,
    pub auto_start: Option<bool>,
    pub close_behavior: Option<String>,
    pub log_retention_days: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserBillingConfig {
    pub flash_cached_input_cny: Option<f64>,
    pub flash_cache_miss_input_cny: Option<f64>,
    pub flash_output_cny: Option<f64>,
    pub pro_cached_input_cny: Option<f64>,
    pub pro_cache_miss_input_cny: Option<f64>,
    pub pro_output_cny: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserToolsConfig {
    pub enabled: Option<Vec<String>>,
    pub web_search: Option<UserWebSearchToolConfig>,
    pub settings: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserWebSearchToolConfig {
    pub proxy: Option<NetworkProxyMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProxyMode {
    System,
    None,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            host: env::var("CODESEEX_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
            port: env::var("CODESEEX_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8787),
            upstream: UpstreamConfig::default(),
            model_override: env_model_override("UPSTREAM_MODEL_OVERRIDE"),
            temperature: env_temperature("DEEPSEEK_TEMPERATURE_PRESET"),
            network_proxy: env_network_proxy(),
        }
    }
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        let raw_base = env::var("DEEPSEEK_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/".to_owned());
        Self {
            base_url: normalize_base_url(&raw_base),
            official_v1_compat: env_bool("DEEPSEEK_OFFICIAL_V1_COMPAT", true),
            api_key: env::var("DEEPSEEK_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            timeout_ms: env::var("UPSTREAM_REQUEST_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120_000),
        }
    }
}

impl AppConfig {
    pub fn load() -> Self {
        load_dotenv_once();
        let mut config = Self::default();
        let path = config.config_path();
        let Ok(user_config) = UserConfig::read_from(&path) else {
            return config;
        };
        config.apply_user_config(user_config);
        config
    }

    pub fn proxy_base_url(&self) -> String {
        format!("http://{}:{}/v1", self.host, self.port)
    }

    pub fn manager_base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.toml")
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.data_dir.join("model-catalog.json")
    }

    pub fn legacy_database_path(&self) -> PathBuf {
        self.data_dir.join("codeseex.db")
    }

    pub fn apply_user_config(&mut self, user_config: UserConfig) {
        if let Some(proxy) = user_config.proxy {
            if env::var("CODESEEX_HOST").is_err() {
                if let Some(host) = proxy.host.filter(|value| !value.trim().is_empty()) {
                    self.host = host;
                }
            }
            if env::var("CODESEEX_PORT").is_err() {
                if let Some(port) = proxy.port {
                    self.port = port;
                }
            }
        }

        if let Some(upstream) = user_config.upstream {
            if env::var("DEEPSEEK_BASE_URL").is_err() {
                if let Some(base_url) = upstream.base_url.filter(|value| !value.trim().is_empty()) {
                    self.upstream.base_url = normalize_base_url(&base_url);
                }
            }
            if env::var("DEEPSEEK_OFFICIAL_V1_COMPAT").is_err() {
                if let Some(official_v1_compat) = upstream.official_v1_compat {
                    self.upstream.official_v1_compat = official_v1_compat;
                }
            }
            if env::var("UPSTREAM_REQUEST_TIMEOUT_MS").is_err() {
                if let Some(timeout_ms) = upstream.timeout_ms {
                    self.upstream.timeout_ms = timeout_ms;
                }
            }
        }

        if let Some(model) = user_config.model {
            if env::var("UPSTREAM_MODEL_OVERRIDE").is_err() {
                if let Some(override_mode) = model.override_mode {
                    self.model_override = override_mode;
                }
            }
            if env::var("DEEPSEEK_TEMPERATURE_PRESET").is_err() {
                if let Some(temperature) = model.temperature {
                    self.temperature = temperature;
                }
            }
        }

        let user_network_proxy = user_config
            .network
            .as_ref()
            .and_then(|network| network.proxy)
            .or_else(|| {
                user_config
                    .tools
                    .as_ref()
                    .and_then(|tools| tools.web_search.as_ref())
                    .and_then(|web_search| web_search.proxy)
            });
        if env::var("NETWORK_PROXY_MODE").is_err() && env::var("WEB_SEARCH_PROXY_MODE").is_err() {
            if let Some(proxy) = user_network_proxy {
                self.network_proxy = proxy;
            }
        }
    }
}

impl UserConfig {
    pub fn log_retention_days(&self) -> u16 {
        self.ui
            .as_ref()
            .and_then(|ui| ui.log_retention_days)
            .unwrap_or(7)
            .clamp(1, 365)
    }

    pub fn read_from(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        toml::from_str(text).map_err(io::Error::other)
    }

    pub fn write_atomic(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(tmp, path)?;
        Ok(())
    }
}

pub fn default_data_dir() -> PathBuf {
    if let Ok(value) = env::var("CODESEEX_DATA_DIR") {
        return PathBuf::from(value);
    }
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codeseex")
}

fn load_dotenv_once() {
    static LOADED: OnceLock<()> = OnceLock::new();
    LOADED.get_or_init(load_dotenv_candidates);
}

fn load_dotenv_candidates() {
    let mut candidates = Vec::new();
    if let Ok(current_dir) = env::current_dir() {
        candidates.push(current_dir.join(".env"));
    }
    if let Ok(current_exe) = env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            candidates.push(exe_dir.join(".env"));
        }
    }
    if let Some(home_dir) = dirs_next::home_dir() {
        candidates.push(home_dir.join(".codeseex").join(".env"));
        candidates.push(home_dir.join(".codeseex").join("secrets").join(".env"));
    }

    let mut seen = Vec::<PathBuf>::new();
    for path in candidates {
        if seen.iter().any(|existing| existing == &path) {
            continue;
        }
        seen.push(path.clone());
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            apply_dotenv_line(line);
        }
    }
}

fn apply_dotenv_line(line: &str) {
    let line = line.trim().strip_prefix('\u{feff}').unwrap_or(line.trim());
    if line.is_empty() || line.starts_with('#') {
        return;
    }
    let Some((name, value)) = line.split_once('=') else {
        return;
    };
    let name = name.trim();
    if name.is_empty() || env::var_os(name).is_some() {
        return;
    }
    let mut value = value.trim().to_owned();
    if ((value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\'')))
        && value.len() >= 2
    {
        value = value[1..value.len() - 1].to_owned();
    }
    env::set_var(name, value);
}

fn env_bool(key: &str, fallback: bool) -> bool {
    match env::var(key) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => fallback,
    }
}

fn env_model_override(key: &str) -> UpstreamModelOverride {
    match env::var(key)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "flash" | "deepseek-v4-flash" => UpstreamModelOverride::Flash,
        "pro" | "deepseek-v4-pro" => UpstreamModelOverride::Pro,
        _ => UpstreamModelOverride::Default,
    }
}

fn env_temperature(key: &str) -> TemperaturePreset {
    match env::var(key)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "strict" => TemperaturePreset::Strict,
        "balanced" => TemperaturePreset::Balanced,
        "general" => TemperaturePreset::General,
        "creative" => TemperaturePreset::Creative,
        _ => TemperaturePreset::Default,
    }
}

fn env_network_proxy() -> NetworkProxyMode {
    env::var("NETWORK_PROXY_MODE")
        .ok()
        .or_else(|| env::var("WEB_SEARCH_PROXY_MODE").ok())
        .and_then(|value| parse_network_proxy_mode(&value))
        .unwrap_or(NetworkProxyMode::System)
}

pub fn parse_network_proxy_mode(value: &str) -> Option<NetworkProxyMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" | "no_proxy" | "direct" => Some(NetworkProxyMode::None),
        "system" | "follow_system" | "default" | "" => Some(NetworkProxyMode::System),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn user_config_accepts_utf8_bom() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        let path = env::temp_dir().join(format!("codeseex-bom-config-{nanos}.toml"));
        fs::write(
            &path,
            "\u{feff}[ui]\nclose_behavior = \"tray\"\nlanguage = \"system\"\n",
        )
        .expect("write bom config");

        let config = UserConfig::read_from(&path).expect("read bom config");
        let ui = config.ui.expect("ui config");
        assert_eq!(ui.close_behavior.as_deref(), Some("tray"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn dotenv_line_accepts_utf8_bom() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        let key = format!("CODESEEX_DOTENV_BOM_TEST_{nanos}");
        apply_dotenv_line(&format!("\u{feff}{key}=ok"));

        assert_eq!(env::var(&key).as_deref(), Ok("ok"));
        env::remove_var(key);
    }

    #[test]
    fn legacy_user_config_api_key_is_not_applied() {
        let mut config = AppConfig::default();
        config.upstream.api_key = None;
        config.apply_user_config(UserConfig {
            upstream: Some(UserUpstreamConfig {
                api_key: Some("legacy-manager-key".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert!(config.upstream.api_key.is_none());
    }

    #[test]
    fn network_proxy_prefers_new_user_config_over_legacy_web_search_config() {
        let mut config = AppConfig::default();
        config.network_proxy = NetworkProxyMode::System;
        config.apply_user_config(UserConfig {
            network: Some(UserNetworkConfig {
                proxy: Some(NetworkProxyMode::None),
            }),
            tools: Some(UserToolsConfig {
                web_search: Some(UserWebSearchToolConfig {
                    proxy: Some(NetworkProxyMode::System),
                }),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(config.network_proxy, NetworkProxyMode::None);
    }

    #[test]
    fn network_proxy_accepts_legacy_web_search_user_config() {
        let mut config = AppConfig::default();
        config.network_proxy = NetworkProxyMode::System;
        config.apply_user_config(UserConfig {
            tools: Some(UserToolsConfig {
                web_search: Some(UserWebSearchToolConfig {
                    proxy: Some(NetworkProxyMode::None),
                }),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(config.network_proxy, NetworkProxyMode::None);
    }
}
