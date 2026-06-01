use crate::models::{TemperaturePreset, UpstreamModelOverride};
use crate::urls::normalize_base_url;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub host: String,
    pub port: u16,
    pub upstream: UpstreamConfig,
    pub model_override: UpstreamModelOverride,
    pub temperature: TemperaturePreset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    pub base_url: String,
    pub official_v1_compat: bool,
    pub api_key: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    pub proxy: Option<UserProxyConfig>,
    pub upstream: Option<UserUpstreamConfig>,
    pub model: Option<UserModelConfig>,
    pub catalog: Option<UserCatalogConfig>,
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
    pub settings: Option<BTreeMap<String, String>>,
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

    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.toml")
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.data_dir.join("model-catalog.json")
    }

    pub fn database_path(&self) -> PathBuf {
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
            if env::var("DEEPSEEK_API_KEY").is_err() {
                if let Some(api_key) = upstream.api_key.filter(|value| !value.trim().is_empty()) {
                    self.upstream.api_key = Some(api_key);
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
    }
}

impl UserConfig {
    pub fn read_from(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)?;
        toml::from_str(&text).map_err(io::Error::other)
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
        .join(".codeseex-next")
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
