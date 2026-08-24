use codeseex_core::{AppConfig, UserConfig};
use notify::{RecursiveMode, Watcher};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeConfigChangeSource {
    ProxyStartup,
    ManagerSave,
    ConfigFile,
    SystemProxy,
}

impl RuntimeConfigChangeSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ProxyStartup => "proxy_startup",
            Self::ManagerSave => "manager_save",
            Self::ConfigFile => "config_file",
            Self::SystemProxy => "system_proxy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeConfigChangeKind {
    NetworkProxy,
    Upstream,
    Model,
    Tools,
    Ui,
    Billing,
    ProxyEndpoint,
}

impl RuntimeConfigChangeKind {
    fn label(self) -> &'static str {
        match self {
            Self::NetworkProxy => "network_proxy",
            Self::Upstream => "upstream",
            Self::Model => "model",
            Self::Tools => "tools",
            Self::Ui => "ui",
            Self::Billing => "billing",
            Self::ProxyEndpoint => "proxy_endpoint",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeConfigSnapshot {
    pub(crate) config: AppConfig,
    pub(crate) config_signature: String,
    pub(crate) network_proxy_signature: String,
    pub(crate) upstream_signature: String,
    pub(crate) model_signature: String,
    pub(crate) tools_signature: String,
    pub(crate) ui_signature: String,
    pub(crate) billing_signature: String,
    pub(crate) proxy_endpoint_signature: String,
}

impl RuntimeConfigSnapshot {
    fn from_base(base: &AppConfig) -> Self {
        let mut config = base.clone();
        let path = config.config_path();
        if let Ok(user_config) = UserConfig::read_from(&path) {
            config.apply_user_config(user_config);
        }
        Self::from_config(config)
    }

    fn from_config(config: AppConfig) -> Self {
        let config_signature = config_signature(&config);
        let network_proxy_signature = crate::network::proxy_cache_key(config.network_proxy);
        let upstream_signature = stable_json_signature(&json!({
            "base_url": config.upstream.base_url,
            "official_v1_compat": config.upstream.official_v1_compat,
            "transport": config.upstream.transport,
            "timeout_ms": config.upstream.timeout_ms
        }));
        let model_signature = stable_json_signature(&json!({
            "override": config.model_override,
            "temperature": config.temperature,
            "user_model": user_config_section_signature(&config.config_path(), "model")
        }));
        let tools_signature = user_config_section_signature(&config.config_path(), "tools");
        let ui_signature = user_config_section_signature(&config.config_path(), "ui");
        let billing_signature = user_config_section_signature(&config.config_path(), "billing");
        let proxy_endpoint_signature = stable_json_signature(&json!({
            "host": config.host,
            "port": config.port
        }));
        Self {
            config,
            config_signature,
            network_proxy_signature,
            upstream_signature,
            model_signature,
            tools_signature,
            ui_signature,
            billing_signature,
            proxy_endpoint_signature,
        }
    }

    fn changed_kinds(&self, next: &Self) -> Vec<RuntimeConfigChangeKind> {
        let mut kinds = Vec::new();
        if self.network_proxy_signature != next.network_proxy_signature {
            kinds.push(RuntimeConfigChangeKind::NetworkProxy);
        }
        if self.upstream_signature != next.upstream_signature {
            kinds.push(RuntimeConfigChangeKind::Upstream);
        }
        if self.model_signature != next.model_signature {
            kinds.push(RuntimeConfigChangeKind::Model);
        }
        if self.tools_signature != next.tools_signature {
            kinds.push(RuntimeConfigChangeKind::Tools);
        }
        if self.ui_signature != next.ui_signature {
            kinds.push(RuntimeConfigChangeKind::Ui);
        }
        if self.billing_signature != next.billing_signature {
            kinds.push(RuntimeConfigChangeKind::Billing);
        }
        if self.proxy_endpoint_signature != next.proxy_endpoint_signature {
            kinds.push(RuntimeConfigChangeKind::ProxyEndpoint);
        }
        kinds
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeConfigChange {
    pub(crate) source: RuntimeConfigChangeSource,
    pub(crate) kinds: Vec<RuntimeConfigChangeKind>,
    pub(crate) snapshot: RuntimeConfigSnapshot,
    pub(crate) previous: Option<RuntimeConfigSnapshot>,
}

impl RuntimeConfigChange {
    pub(crate) fn has_kind(&self, kind: RuntimeConfigChangeKind) -> bool {
        self.kinds.contains(&kind)
    }

    pub(crate) fn diagnostic(&self) -> Value {
        json!({
            "source": self.source.label(),
            "kinds": self.kinds.iter().map(|kind| kind.label()).collect::<Vec<_>>(),
            "config_signature": self.snapshot.config_signature,
            "network_proxy_signature": self.snapshot.network_proxy_signature,
            "previous_network_proxy_signature": self
                .previous
                .as_ref()
                .map(|snapshot| snapshot.network_proxy_signature.as_str())
        })
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeConfigService {
    base: Arc<AppConfig>,
    snapshot: Arc<RwLock<RuntimeConfigSnapshot>>,
    changes: broadcast::Sender<RuntimeConfigChange>,
}

impl RuntimeConfigService {
    pub(crate) fn new(base: AppConfig) -> Self {
        let snapshot = RuntimeConfigSnapshot::from_base(&base);
        let (changes, _) = broadcast::channel(64);
        Self {
            base: Arc::new(base),
            snapshot: Arc::new(RwLock::new(snapshot)),
            changes,
        }
    }

    pub(crate) fn active_config(&self) -> AppConfig {
        self.snapshot
            .read()
            .map(|snapshot| snapshot.config.clone())
            .unwrap_or_else(|_| RuntimeConfigSnapshot::from_base(&self.base).config)
    }

    pub(crate) fn snapshot(&self) -> RuntimeConfigSnapshot {
        self.snapshot
            .read()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|_| RuntimeConfigSnapshot::from_base(&self.base))
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RuntimeConfigChange> {
        self.changes.subscribe()
    }

    pub(crate) fn refresh(&self, source: RuntimeConfigChangeSource) -> Option<RuntimeConfigChange> {
        let next = RuntimeConfigSnapshot::from_base(&self.base);
        let previous = {
            let mut guard = self.snapshot.write().ok()?;
            if guard.config_signature == next.config_signature
                && guard.network_proxy_signature == next.network_proxy_signature
            {
                return None;
            }
            let previous = guard.clone();
            *guard = next.clone();
            previous
        };
        let mut kinds = previous.changed_kinds(&next);
        if source == RuntimeConfigChangeSource::SystemProxy
            && !kinds.contains(&RuntimeConfigChangeKind::NetworkProxy)
            && previous.network_proxy_signature != next.network_proxy_signature
        {
            kinds.push(RuntimeConfigChangeKind::NetworkProxy);
        }
        if kinds.is_empty() {
            return None;
        }
        let change = RuntimeConfigChange {
            source,
            kinds,
            snapshot: next,
            previous: Some(previous),
        };
        let _ = self.changes.send(change.clone());
        Some(change)
    }

    pub(crate) fn emit_proxy_startup(&self) {
        let snapshot = self.snapshot();
        let change = RuntimeConfigChange {
            source: RuntimeConfigChangeSource::ProxyStartup,
            kinds: vec![RuntimeConfigChangeKind::NetworkProxy],
            snapshot,
            previous: None,
        };
        let _ = self.changes.send(change);
    }

    pub(crate) fn spawn_config_file_watcher(
        &self,
        store: codeseex_store::Store,
    ) -> tokio::task::JoinHandle<()> {
        let config_path = self.base.config_path();
        let service = self.clone();
        tokio::spawn(async move {
            watch_config_file(config_path, service, store).await;
        })
    }

    pub(crate) fn spawn_system_proxy_watcher(
        &self,
        store: codeseex_store::Store,
    ) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            crate::runtime_config::platform::watch_system_proxy(service, store).await;
        })
    }
}

async fn watch_config_file(
    config_path: PathBuf,
    service: RuntimeConfigService,
    store: codeseex_store::Store,
) {
    let Some(parent) = config_path.parent().map(PathBuf::from) else {
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<notify::Event>();
    let mut watcher =
        match notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else {
                return;
            };
            let _ = tx.send(event);
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                let _ = store
                    .record_event(
                        "warn",
                        "runtime_config_watcher_failed",
                        "CodeSeeX config file watcher failed.",
                        Some(&json!({ "error": error.to_string() })),
                    )
                    .await;
                return;
            }
        };
    if let Err(error) = watcher.watch(&parent, RecursiveMode::NonRecursive) {
        let _ = store
            .record_event(
                "warn",
                "runtime_config_watcher_failed",
                "CodeSeeX config file watcher failed.",
                Some(&json!({ "error": error.to_string() })),
            )
            .await;
        return;
    }
    while let Some(event) = rx.recv().await {
        if !event.paths.iter().any(|path| path == &config_path) {
            continue;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if let Some(change) = service.refresh(RuntimeConfigChangeSource::ConfigFile) {
            let _ = store
                .record_event(
                    "info",
                    "runtime_config_changed",
                    "CodeSeeX runtime configuration changed.",
                    Some(&change.diagnostic()),
                )
                .await;
        }
    }
    drop(watcher);
}

fn stable_json_signature(value: &Value) -> String {
    crate::diagnostics::stable_payload_hash(value)
}

fn user_config_section_signature(path: &std::path::Path, section: &str) -> String {
    let value = UserConfig::read_from(path)
        .ok()
        .and_then(|config| serde_json::to_value(config).ok())
        .and_then(|value| value.get(section).cloned())
        .unwrap_or(Value::Null);
    stable_json_signature(&value)
}

fn config_signature(config: &AppConfig) -> String {
    stable_json_signature(&json!({
        "data_dir": config.data_dir,
        "host": config.host,
        "port": config.port,
        "upstream": {
            "base_url": config.upstream.base_url,
            "official_v1_compat": config.upstream.official_v1_compat,
            "transport": config.upstream.transport,
            "timeout_ms": config.upstream.timeout_ms
        },
        "model_override": config.model_override,
        "temperature": config.temperature,
        "model": user_config_section_signature(&config.config_path(), "model"),
        "network_proxy": config.network_proxy,
        "network_proxy_signature": crate::network::proxy_cache_key(config.network_proxy),
        "tools": {
            "config": user_config_section_signature(&config.config_path(), "tools"),
            "web_search_backend": config.web_search_backend
        },
        "ui": user_config_section_signature(&config.config_path(), "ui"),
        "billing": user_config_section_signature(&config.config_path(), "billing")
    }))
}

#[cfg(windows)]
mod platform {
    use super::{RuntimeConfigChangeSource, RuntimeConfigService};
    use serde_json::json;
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, ERROR_SUCCESS, WAIT_FAILED,
        WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY,
        REG_NOTIFY_CHANGE_LAST_SET,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, SetEvent, WaitForMultipleObjects, INFINITE,
    };

    const INTERNET_SETTINGS: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";

    pub(super) async fn watch_system_proxy(
        service: RuntimeConfigService,
        store: codeseex_store::Store,
    ) {
        let runtime = tokio::runtime::Handle::current();
        let (join, stop_signal) = {
            let mut key: HKEY = std::ptr::null_mut();
            let key_path = wide_null(INTERNET_SETTINGS);
            let open_result = unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    key_path.as_ptr(),
                    0,
                    KEY_NOTIFY,
                    &mut key,
                )
            };
            if open_result != ERROR_SUCCESS {
                runtime.spawn(async move {
                    let _ = store
                        .record_event(
                            "warn",
                            "runtime_config_watcher_failed",
                            "CodeSeeX system proxy watcher failed.",
                            Some(
                                &json!({ "error": format!("RegOpenKeyExW failed: {open_result}") }),
                            ),
                        )
                        .await;
                });
                return;
            }
            let change_event = unsafe { CreateEventW(null(), 0, 0, null()) };
            let stop_event = unsafe { CreateEventW(null(), 1, 0, null()) };
            if change_event.is_null() || stop_event.is_null() {
                unsafe {
                    RegCloseKey(key);
                    if !change_event.is_null() {
                        CloseHandle(change_event);
                    }
                    if !stop_event.is_null() {
                        CloseHandle(stop_event);
                    }
                }
                return;
            }
            let mut stop_signal_event = std::ptr::null_mut();
            let duplicated = unsafe {
                let current = GetCurrentProcess();
                DuplicateHandle(
                    current,
                    stop_event,
                    current,
                    &mut stop_signal_event,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if duplicated == 0 || stop_signal_event.is_null() {
                unsafe {
                    RegCloseKey(key);
                    CloseHandle(change_event);
                    CloseHandle(stop_event);
                }
                return;
            }
            let stop_signal = SystemProxyStopSignal {
                stop_event: stop_signal_event,
            };
            let guard = SystemProxyWatcherGuard {
                key,
                change_event,
                stop_event,
            };
            let join = tokio::task::spawn_blocking(move || {
                watch_system_proxy_blocking(guard, runtime, service, store);
            });
            (join, stop_signal)
        };
        let _ = join.await;
        drop(stop_signal);
    }

    struct SystemProxyWatcherGuard {
        key: HKEY,
        change_event: windows_sys::Win32::Foundation::HANDLE,
        stop_event: windows_sys::Win32::Foundation::HANDLE,
    }

    // SAFETY: The handles are OS-owned synchronization/registry handles. They are moved to one
    // blocking watcher thread, used there, and closed by that same owner when the watcher exits.
    unsafe impl Send for SystemProxyWatcherGuard {}

    impl Drop for SystemProxyWatcherGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.change_event);
                CloseHandle(self.stop_event);
                RegCloseKey(self.key);
            }
        }
    }

    struct SystemProxyStopSignal {
        stop_event: windows_sys::Win32::Foundation::HANDLE,
    }

    // SAFETY: This value never closes the handle. It only signals the event so the blocking owner
    // can wake up and perform orderly cleanup.
    unsafe impl Send for SystemProxyStopSignal {}

    impl Drop for SystemProxyStopSignal {
        fn drop(&mut self) {
            unsafe {
                SetEvent(self.stop_event);
                CloseHandle(self.stop_event);
            }
        }
    }

    fn watch_system_proxy_blocking(
        guard: SystemProxyWatcherGuard,
        runtime: tokio::runtime::Handle,
        service: RuntimeConfigService,
        store: codeseex_store::Store,
    ) {
        loop {
            let notify_result = unsafe {
                RegNotifyChangeKeyValue(
                    guard.key,
                    0,
                    REG_NOTIFY_CHANGE_LAST_SET,
                    guard.change_event,
                    1,
                )
            };
            if notify_result != ERROR_SUCCESS {
                let store = store.clone();
                runtime.spawn(async move {
                    let _ = store
                        .record_event(
                            "warn",
                            "runtime_config_watcher_failed",
                            "CodeSeeX system proxy watcher failed.",
                            Some(&json!({
                                "error": format!("RegNotifyChangeKeyValue failed: {notify_result}")
                            })),
                        )
                        .await;
                });
                return;
            }
            let handles = [guard.change_event, guard.stop_event];
            let wait = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
            if wait == WAIT_OBJECT_0 + 1 {
                return;
            }
            if wait == WAIT_FAILED {
                let store = store.clone();
                runtime.spawn(async move {
                    let _ = store
                        .record_event(
                            "warn",
                            "runtime_config_watcher_failed",
                            "CodeSeeX system proxy watcher failed.",
                            Some(&json!({ "error": "WaitForMultipleObjects failed" })),
                        )
                        .await;
                });
                return;
            }
            let service = service.clone();
            let store = store.clone();
            runtime.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if let Some(change) = service.refresh(RuntimeConfigChangeSource::SystemProxy) {
                    let _ = store
                        .record_event(
                            "info",
                            "runtime_config_changed",
                            "CodeSeeX runtime configuration changed.",
                            Some(&change.diagnostic()),
                        )
                        .await;
                }
            });
        }
    }

    fn wide_null(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(not(windows))]
mod platform {
    use super::RuntimeConfigService;

    pub(super) async fn watch_system_proxy(
        _service: RuntimeConfigService,
        _store: codeseex_store::Store,
    ) {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeseex_core::{
        NetworkProxyMode, UpstreamTransport, UserModelConfig, UserNetworkConfig, UserToolsConfig,
        UserUpstreamConfig, UserWebSearchToolConfig, WebSearchBackend,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_config(label: &str) -> AppConfig {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        AppConfig {
            data_dir: std::env::temp_dir().join(format!("codeseex-runtime-config-{label}-{nanos}")),
            ..Default::default()
        }
    }

    #[test]
    fn runtime_config_refresh_reports_network_proxy_change() {
        let config = temp_config("network");
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let service = RuntimeConfigService::new(config.clone());
        UserConfig {
            network: Some(UserNetworkConfig {
                proxy: Some(NetworkProxyMode::None),
            }),
            ..UserConfig::default()
        }
        .write_atomic(&config.config_path())
        .unwrap();

        let change = service
            .refresh(RuntimeConfigChangeSource::ManagerSave)
            .expect("network proxy change");

        assert!(change.has_kind(RuntimeConfigChangeKind::NetworkProxy));
        assert_eq!(
            service.active_config().network_proxy,
            NetworkProxyMode::None
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn runtime_config_proxy_startup_emits_only_network_proxy_probe_signal() {
        let config = temp_config("proxy-startup");
        let service = RuntimeConfigService::new(config.clone());
        let mut changes = service.subscribe();

        service.emit_proxy_startup();

        let change = changes.try_recv().expect("proxy startup change");
        assert_eq!(change.source, RuntimeConfigChangeSource::ProxyStartup);
        assert_eq!(change.kinds, vec![RuntimeConfigChangeKind::NetworkProxy]);
        assert!(change.previous.is_none());
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn runtime_config_refresh_ignores_unchanged_config() {
        let config = temp_config("unchanged");
        let service = RuntimeConfigService::new(config.clone());

        assert!(service
            .refresh(RuntimeConfigChangeSource::ManagerSave)
            .is_none());
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn runtime_config_refresh_reports_model_section_change() {
        let config = temp_config("model");
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let service = RuntimeConfigService::new(config.clone());
        UserConfig {
            model: Some(UserModelConfig {
                thinking: Some("disabled".to_owned()),
                ..UserModelConfig::default()
            }),
            ..UserConfig::default()
        }
        .write_atomic(&config.config_path())
        .unwrap();

        let change = service
            .refresh(RuntimeConfigChangeSource::ManagerSave)
            .expect("model config change");

        assert!(change.has_kind(RuntimeConfigChangeKind::Model));
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn runtime_config_refresh_reports_transport_change_as_upstream_change() {
        let config = temp_config("transport");
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let service = RuntimeConfigService::new(config.clone());
        UserConfig {
            upstream: Some(UserUpstreamConfig {
                transport: Some(UpstreamTransport::ChatCompat),
                ..UserUpstreamConfig::default()
            }),
            ..UserConfig::default()
        }
        .write_atomic(&config.config_path())
        .unwrap();

        let change = service
            .refresh(RuntimeConfigChangeSource::ManagerSave)
            .expect("transport change");

        assert!(change.has_kind(RuntimeConfigChangeKind::Upstream));
        assert_eq!(
            service.active_config().upstream.transport,
            UpstreamTransport::ChatCompat
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn runtime_config_refresh_reports_web_search_backend_as_tools_change() {
        let config = temp_config("web-search-backend");
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let service = RuntimeConfigService::new(config.clone());
        UserConfig {
            tools: Some(UserToolsConfig {
                web_search: Some(UserWebSearchToolConfig {
                    backend: Some(WebSearchBackend::Official),
                    ..UserWebSearchToolConfig::default()
                }),
                ..UserToolsConfig::default()
            }),
            ..UserConfig::default()
        }
        .write_atomic(&config.config_path())
        .unwrap();

        let change = service
            .refresh(RuntimeConfigChangeSource::ManagerSave)
            .expect("web search backend change");

        assert!(change.has_kind(RuntimeConfigChangeKind::Tools));
        assert_eq!(
            service.active_config().web_search_backend,
            WebSearchBackend::Official
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }
}
