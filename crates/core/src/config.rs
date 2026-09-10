use crate::catalog::CatalogDocument;
use crate::models::{TemperaturePreset, UpstreamModelOverride};
use crate::pricing::PricingTable;
use crate::urls::normalize_base_url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

pub const IMAGE_CAPABILITY_SCHEMA_VERSION: u8 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub host: String,
    pub port: u16,
    pub upstream: UpstreamConfig,
    pub model_override: UpstreamModelOverride,
    pub temperature: TemperaturePreset,
    pub network_proxy: NetworkProxyMode,
    pub web_search_backend: WebSearchBackend,
    /// Remote catalog manifest URL. `None` uses the built-in release URL.
    pub catalog_source_url: Option<String>,
    pub catalog_remote_enabled: bool,
    /// Layer 3 catalog overrides parsed from the user TOML.
    #[serde(skip)]
    pub catalog_overrides: CatalogOverrides,
    /// Highest layer reached so far: cache file at load time, remote document
    /// after a successful background refresh.
    #[serde(skip)]
    pub catalog_remote: Option<Arc<CatalogDocument>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    pub base_url: String,
    pub official_v1_compat: bool,
    pub transport: UpstreamTransport,
    /// Which credential reaches the upstream. `Auto` keeps the historical
    /// resolution order; explicit values pin a single source so changing the
    /// upstream URL cannot silently reuse another provider's key.
    pub credential: UpstreamCredentialSource,
    // Process environment fallback only. Manager/user TOML is not credential storage.
    pub api_key: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamCredentialSource {
    /// Official endpoints isolate client credentials; custom endpoints forward
    /// the request Authorization when present.
    #[default]
    Auto,
    /// Always forward the inbound request Authorization.
    Request,
    /// Always use the process `DEEPSEEK_API_KEY` environment variable.
    Env,
    /// Always use the Codex auth source (`auth.json` or cached request header).
    CodexAuth,
    /// Always use the OS credential store entry managed by CodeSeeX.
    Secret,
}

impl UpstreamCredentialSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Request => "request",
            Self::Env => "env",
            Self::CodexAuth => "codex_auth",
            Self::Secret => "secret",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "default" => Some(Self::Auto),
            "request" | "client" | "passthrough" | "inbound" => Some(Self::Request),
            "env" | "environment" | "deepseek_api_key" => Some(Self::Env),
            "codex_auth" | "codex-auth" | "auth" | "auth_json" => Some(Self::CodexAuth),
            "secret" | "secret_store" | "credential_store" | "keyring" => Some(Self::Secret),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamTransport {
    /// Native Responses API transport. This is the default for every
    /// upstream; Chat API compatibility is an explicit user opt-in.
    #[default]
    #[serde(alias = "auto", alias = "native", alias = "responses")]
    NativeResponses,
    #[serde(alias = "chat", alias = "compat")]
    ChatCompat,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchBackend {
    #[default]
    Local,
    Official,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    pub proxy: Option<UserProxyConfig>,
    pub upstream: Option<UserUpstreamConfig>,
    pub model: Option<UserModelConfig>,
    /// Per-model catalog overrides, keyed by slug: `[models."<slug>"]`.
    pub models: Option<BTreeMap<String, UserCatalogModelConfig>>,
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
    pub transport: Option<UpstreamTransport>,
    pub credential: Option<UpstreamCredentialSource>,
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
    /// Remote catalog manifest URL. Empty disables remote refresh.
    pub source_url: Option<String>,
    pub remote_enabled: Option<bool>,
}

/// `[models."<slug>"]` user override for one model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserCatalogModelConfig {
    pub display_name: Option<String>,
    pub short_display_name: Option<String>,
    pub description: Option<String>,
    pub context_window: Option<u64>,
    pub effective_context_window_percent: Option<u8>,
    pub upstream_slug: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub pricing_group: Option<String>,
    pub is_default: Option<bool>,
    pub hidden: Option<bool>,
}

/// Resolved layer 3 overrides applied on top of the catalog document.
#[derive(Debug, Clone, Default)]
pub struct CatalogOverrides {
    pub models: BTreeMap<String, CatalogModelOverride>,
    pub pricing: Option<Value>,
}

impl CatalogOverrides {
    pub fn is_empty(&self) -> bool {
        self.models.is_empty() && self.pricing.is_none()
    }
}

#[derive(Debug, Clone, Default)]
pub struct CatalogModelOverride {
    pub display_name: Option<String>,
    pub short_display_name: Option<String>,
    pub description: Option<String>,
    pub context_window: Option<u64>,
    pub effective_context_window_percent: Option<u8>,
    pub upstream_slug: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub pricing_group: Option<String>,
    pub is_default: Option<bool>,
    pub hidden: Option<bool>,
}

impl From<&UserCatalogModelConfig> for CatalogModelOverride {
    fn from(value: &UserCatalogModelConfig) -> Self {
        Self {
            display_name: value.display_name.clone(),
            short_display_name: value.short_display_name.clone(),
            description: value.description.clone(),
            context_window: value.context_window,
            effective_context_window_percent: value.effective_context_window_percent,
            upstream_slug: value.upstream_slug.clone(),
            aliases: value.aliases.clone(),
            pricing_group: value.pricing_group.clone(),
            is_default: value.is_default,
            hidden: value.hidden,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserModelRatesConfig {
    pub cached_input: Option<f64>,
    pub cache_miss_input: Option<f64>,
    pub output: Option<f64>,
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
    pub codex_app_model_list_injection: Option<bool>,
    pub close_behavior: Option<String>,
    pub log_retention_days: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserBillingConfig {
    pub peak_valley_enabled: Option<bool>,
    pub peak_multiplier: Option<f64>,
    /// Peak windows in `HH:MM-HH:MM` form.
    pub peak_windows: Option<Vec<String>>,
    pub timezone: Option<String>,
    pub currency: Option<String>,
    pub unit: Option<String>,
    /// Per-model rates, keyed by slug: `[billing.rates."<slug>"]`.
    pub rates: Option<BTreeMap<String, UserModelRatesConfig>>,
    pub flash_cached_input_cny: Option<f64>,
    pub flash_cache_miss_input_cny: Option<f64>,
    pub flash_output_cny: Option<f64>,
    pub pro_cached_input_cny: Option<f64>,
    pub pro_cache_miss_input_cny: Option<f64>,
    pub pro_output_cny: Option<f64>,
    pub vision_cached_input_cny: Option<f64>,
    pub vision_cache_miss_input_cny: Option<f64>,
    pub vision_output_cny: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserToolsConfig {
    pub capability_schema_version: Option<u8>,
    pub enabled: Option<Vec<String>>,
    pub web_search: Option<UserWebSearchToolConfig>,
    pub vision_analyze: Option<UserVisionToolConfig>,
    pub vision_generate: Option<UserVisionGenerateToolConfig>,
    pub settings: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserWebSearchToolConfig {
    pub proxy: Option<NetworkProxyMode>,
    pub backend: Option<WebSearchBackend>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserVisionToolConfig {
    pub backend: Option<VisionAnalyzeBackend>,
    pub image_detail: Option<VisionImageDetail>,
    pub analyze_url: Option<String>,
    pub analyze_model: Option<String>,
    // Legacy fields are retained for TOML migration only.
    pub generate_url: Option<String>,
    pub generate_model: Option<String>,
    pub api_key: Option<String>,
    pub analyze_api_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserVisionGenerateToolConfig {
    pub generate_url: Option<String>,
    pub generate_model: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisionAnalyzeBackend {
    Deepseek,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisionImageDetail {
    Auto,
    Low,
    Original,
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
            web_search_backend: env_web_search_backend(),
            catalog_source_url: env::var("CODESEEX_CATALOG_URL")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            catalog_remote_enabled: env_bool("CODESEEX_CATALOG_REMOTE", true),
            catalog_overrides: CatalogOverrides::default(),
            catalog_remote: None,
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
            transport: env_upstream_transport(),
            credential: env::var("DEEPSEEK_CREDENTIAL_SOURCE")
                .ok()
                .and_then(|value| UpstreamCredentialSource::parse(&value))
                .unwrap_or_default(),
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
    pub fn load_base() -> Self {
        load_dotenv_once();
        Self::default()
    }

    pub fn load() -> Self {
        let mut config = Self::load_base();
        config.load_cached_catalog();
        let path = config.config_path();
        let Ok(user_config) = UserConfig::read_from(&path) else {
            config.refresh_catalog_document();
            return config;
        };
        config.apply_user_config(user_config);
        config
    }

    /// Reads the cached remote catalog document (layer 2). Never touches the
    /// network; a missing or invalid cache file simply keeps the built-in
    /// document.
    pub fn load_cached_catalog(&mut self) {
        if !self.catalog_remote_enabled {
            return;
        }
        if let Some(document) = crate::catalog::read_cached_catalog_document(&self.catalog_cache_path())
        {
            self.catalog_remote = Some(Arc::new(document));
        }
    }

    pub fn catalog_cache_path(&self) -> PathBuf {
        self.data_dir.join("cache").join("model-catalog.json")
    }

    /// Currently effective catalog: built-in, then the highest available layer
    /// (cache or remote), then user overrides.
    pub fn catalog_document(&self) -> CatalogDocument {
        let base = crate::catalog::embedded_catalog_document();
        let layered = match self.catalog_remote.as_deref() {
            Some(remote) => base.merge_authoritative(remote),
            None => base,
        };
        crate::catalog::apply_catalog_overrides(&layered, &self.catalog_overrides)
    }

    pub fn pricing_table(&self) -> PricingTable {
        self.catalog_document().pricing
    }

    pub fn catalog_revision(&self) -> String {
        self.catalog_document().revision
    }

    /// `builtin`, `cache` or `remote` depending on the highest layer in use.
    pub fn catalog_source_label(&self) -> &'static str {
        if self.catalog_remote.is_some() {
            "remote"
        } else {
            "builtin"
        }
    }

    pub fn refresh_catalog_document(&mut self) {
        let _ = self.catalog_document();
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
            if env::var("DEEPSEEK_TRANSPORT").is_err() {
                if let Some(transport) = upstream.transport {
                    self.upstream.transport = transport;
                }
            }
            if env::var("DEEPSEEK_CREDENTIAL_SOURCE").is_err() {
                if let Some(credential) = upstream.credential {
                    self.upstream.credential = credential;
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

        if let Some(catalog) = user_config.catalog.as_ref() {
            if env::var("CODESEEX_CATALOG_URL").is_err() {
                if let Some(source_url) = catalog
                    .source_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    self.catalog_source_url = Some(source_url.to_owned());
                }
            }
            if env::var("CODESEEX_CATALOG_REMOTE").is_err() {
                if let Some(enabled) = catalog.remote_enabled {
                    self.catalog_remote_enabled = enabled;
                }
            }
        }

        let mut overrides = CatalogOverrides::default();
        if let Some(models) = user_config.models.as_ref() {
            for (slug, model) in models {
                let slug = slug.trim();
                if slug.is_empty() {
                    continue;
                }
                overrides
                    .models
                    .insert(slug.to_owned(), CatalogModelOverride::from(model));
            }
        }
        if let Some(billing) = user_config.billing.as_ref() {
            overrides.pricing = pricing_override_from_user_billing(billing);
        }
        self.catalog_overrides = overrides;
        if !self.catalog_remote_enabled {
            self.catalog_remote = None;
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
        if env::var("WEB_SEARCH_BACKEND").is_err() {
            if let Some(backend) = user_config
                .tools
                .as_ref()
                .and_then(|tools| tools.web_search.as_ref())
                .and_then(|web_search| web_search.backend)
            {
                self.web_search_backend = backend;
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

/// Translates user billing settings into a sparse pricing override document.
///
/// The legacy `BILLING_*` fields are still read so a 0.7.0 configuration keeps
/// its rates, but they are written back as `[billing.rates."<slug>"]`.
pub fn pricing_override_from_user_billing(billing: &UserBillingConfig) -> Option<Value> {
    let mut map = serde_json::Map::new();
    if let Some(enabled) = billing.peak_valley_enabled {
        map.insert("peak_valley_enabled".to_owned(), json!(enabled));
    }
    if let Some(multiplier) = billing.peak_multiplier {
        map.insert("peak_multiplier".to_owned(), json!(multiplier));
    }
    if let Some(timezone) = billing
        .timezone
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        map.insert("timezone".to_owned(), json!(timezone));
    }
    if let Some(currency) = billing
        .currency
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        map.insert("currency".to_owned(), json!(currency));
    }
    if let Some(unit) = billing
        .unit
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        map.insert("unit".to_owned(), json!(unit));
    }
    if let Some(windows) = billing.peak_windows.as_ref() {
        let parsed = windows
            .iter()
            .filter_map(|window| parse_peak_window_spec(window))
            .collect::<Vec<_>>();
        if !parsed.is_empty() {
            map.insert("peak_windows".to_owned(), Value::Array(parsed));
        }
    }

    let mut rates = serde_json::Map::new();
    if let Some(configured) = billing.rates.as_ref() {
        for (slug, rate) in configured {
            let slug = slug.trim();
            if slug.is_empty() {
                continue;
            }
            let values = [
                rate.cached_input.unwrap_or(0.0),
                rate.cache_miss_input.unwrap_or(0.0),
                rate.output.unwrap_or(0.0),
            ];
            if values.iter().any(|value| !value.is_finite() || *value < 0.0) {
                continue;
            }
            rates.insert(
                slug.to_owned(),
                json!({
                    "cached_input": values[0],
                    "cache_miss_input": values[1],
                    "output": values[2],
                }),
            );
        }
    }
    for (slug, legacy) in legacy_billing_rates(billing) {
        rates.entry(slug.to_owned()).or_insert(legacy);
    }
    if !rates.is_empty() {
        map.insert("rates".to_owned(), Value::Object(rates));
    }

    (!map.is_empty()).then(|| Value::Object(map))
}

/// Legacy 0.7.0 rate fields mapped onto the built-in slugs.
fn legacy_billing_rates(billing: &UserBillingConfig) -> Vec<(&'static str, Value)> {
    let mut output = Vec::new();
    let mut push = |slug: &'static str,
                    cached: Option<f64>,
                    cache_miss: Option<f64>,
                    out: Option<f64>| {
        if cached.is_none() && cache_miss.is_none() && out.is_none() {
            return;
        }
        let values = [
            cached.unwrap_or(0.0),
            cache_miss.unwrap_or(0.0),
            out.unwrap_or(0.0),
        ];
        if values.iter().any(|value| !value.is_finite() || *value < 0.0) {
            return;
        }
        output.push((
            slug,
            json!({
                "cached_input": values[0],
                "cache_miss_input": values[1],
                "output": values[2],
            }),
        ));
    };
    push(
        crate::models::MODEL_FLASH,
        billing.flash_cached_input_cny,
        billing.flash_cache_miss_input_cny,
        billing.flash_output_cny,
    );
    push(
        crate::models::MODEL_PRO,
        billing.pro_cached_input_cny,
        billing.pro_cache_miss_input_cny,
        billing.pro_output_cny,
    );
    push(
        "deepseek-v4-flash-vision-exp",
        billing.vision_cached_input_cny,
        billing.vision_cache_miss_input_cny,
        billing.vision_output_cny,
    );
    output
}

fn parse_peak_window_spec(value: &str) -> Option<Value> {
    let raw = value.trim();
    let (from, to) = raw
        .split_once('-')
        .or_else(|| raw.split_once(".."))
        .or_else(|| raw.split_once('~'))?;
    let from = crate::pricing::parse_hhmm(from)?;
    let to = crate::pricing::parse_hhmm(to)?;
    if from >= to {
        return None;
    }
    Some(json!({
        "from": crate::pricing::minute_to_hhmm(from),
        "to": crate::pricing::minute_to_hhmm(to),
    }))
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

fn env_upstream_transport() -> UpstreamTransport {
    env::var("DEEPSEEK_TRANSPORT")
        .ok()
        .and_then(|value| parse_upstream_transport(&value))
        .unwrap_or_default()
}

pub fn parse_upstream_transport(value: &str) -> Option<UpstreamTransport> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" | "native" | "native_responses" | "responses" => {
            Some(UpstreamTransport::NativeResponses)
        }
        "chat" | "chat_compat" | "compat" => Some(UpstreamTransport::ChatCompat),
        _ => None,
    }
}

fn env_web_search_backend() -> WebSearchBackend {
    env::var("WEB_SEARCH_BACKEND")
        .ok()
        .and_then(|value| parse_web_search_backend(&value))
        .unwrap_or_default()
}

pub fn parse_web_search_backend(value: &str) -> Option<WebSearchBackend> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "local" | "codeseex" => Some(WebSearchBackend::Local),
        "official" | "deepseek" => Some(WebSearchBackend::Official),
        _ => None,
    }
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
        let mut config = AppConfig {
            network_proxy: NetworkProxyMode::System,
            ..Default::default()
        };
        config.apply_user_config(UserConfig {
            network: Some(UserNetworkConfig {
                proxy: Some(NetworkProxyMode::None),
            }),
            tools: Some(UserToolsConfig {
                web_search: Some(UserWebSearchToolConfig {
                    proxy: Some(NetworkProxyMode::System),
                    backend: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(config.network_proxy, NetworkProxyMode::None);
    }

    #[test]
    fn network_proxy_accepts_legacy_web_search_user_config() {
        let mut config = AppConfig {
            network_proxy: NetworkProxyMode::System,
            ..Default::default()
        };
        config.apply_user_config(UserConfig {
            tools: Some(UserToolsConfig {
                web_search: Some(UserWebSearchToolConfig {
                    proxy: Some(NetworkProxyMode::None),
                    backend: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(config.network_proxy, NetworkProxyMode::None);
    }

    #[test]
    fn web_search_backend_is_local_by_default_and_official_is_explicit() {
        assert_eq!(WebSearchBackend::default(), WebSearchBackend::Local);
        assert_eq!(
            parse_web_search_backend("local"),
            Some(WebSearchBackend::Local)
        );
        assert_eq!(
            parse_web_search_backend("codeseex"),
            Some(WebSearchBackend::Local)
        );
        assert_eq!(
            parse_web_search_backend("official"),
            Some(WebSearchBackend::Official)
        );
        assert_eq!(
            parse_web_search_backend("deepseek"),
            Some(WebSearchBackend::Official)
        );
        assert_eq!(parse_web_search_backend("unknown"), None);
    }

    #[test]
    fn legacy_user_config_without_transport_keeps_native_default() {
        let mut config = AppConfig {
            upstream: UpstreamConfig {
                transport: UpstreamTransport::NativeResponses,
                ..UpstreamConfig::default()
            },
            ..AppConfig::default()
        };
        config.apply_user_config(UserConfig {
            upstream: Some(UserUpstreamConfig {
                base_url: Some("https://api.deepseek.com".to_owned()),
                official_v1_compat: Some(true),
                transport: None,
                credential: None,
                api_key: None,
                timeout_ms: None,
            }),
            ..UserConfig::default()
        });

        assert_eq!(config.upstream.transport, UpstreamTransport::NativeResponses);
    }

    #[test]
    fn legacy_transport_aliases_resolve_to_native_or_chat() {
        assert_eq!(
            parse_upstream_transport("auto"),
            Some(UpstreamTransport::NativeResponses)
        );
        assert_eq!(
            parse_upstream_transport("native"),
            Some(UpstreamTransport::NativeResponses)
        );
        assert_eq!(
            parse_upstream_transport("responses"),
            Some(UpstreamTransport::NativeResponses)
        );
        assert_eq!(
            parse_upstream_transport("chat"),
            Some(UpstreamTransport::ChatCompat)
        );
        assert_eq!(
            parse_upstream_transport("compat"),
            Some(UpstreamTransport::ChatCompat)
        );
        assert_eq!(parse_upstream_transport("unknown"), None);
    }
}
