use crate::runtime_config::RuntimeConfigService;
use crate::telemetry::TelemetryHub;
use codeseex_core::AppConfig;
use codeseex_store::Store;
use std::fs;
use std::path::Path;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct ProxyState {
    pub(crate) runtime_config: RuntimeConfigService,
    pub(crate) store: Store,
    pub(crate) telemetry: TelemetryHub,
    pub(crate) v1_access_token: String,
}

impl ProxyState {
    pub(crate) fn new(config: AppConfig, store: Store) -> Self {
        let v1_access_token = load_or_create_v1_access_token(&config.data_dir);
        Self {
            runtime_config: RuntimeConfigService::new(config),
            store,
            telemetry: TelemetryHub::new(),
            v1_access_token,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(config: AppConfig, store: Store) -> Self {
        Self::new(config, store)
    }

    pub(crate) fn active_config(&self) -> AppConfig {
        self.runtime_config.active_config()
    }

    pub(crate) fn client(&self) -> reqwest::Client {
        let config = self.active_config();
        let timeout = std::time::Duration::from_millis(config.upstream.timeout_ms);
        crate::network::client(config.network_proxy, timeout)
            .unwrap_or_else(|_| reqwest::Client::new())
    }
}

fn load_or_create_v1_access_token(data_dir: &Path) -> String {
    let path = data_dir.join("secrets").join("v1_access.token");
    if let Ok(value) = fs::read_to_string(&path) {
        let token = value.trim();
        if is_valid_local_access_token(token) {
            return token.to_owned();
        }
    }

    let token = format!("csx_{}", Uuid::new_v4().simple());
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, format!("{token}\n"));
    token
}

fn is_valid_local_access_token(value: &str) -> bool {
    value.len() >= 24
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}
