use codeseex_core::{
    AppConfig, NetworkProxyMode, TemperaturePreset, UpstreamModelOverride, UserConfig,
    UserModelConfig, UserUiConfig,
};
use serde_json::{json, Value};
use std::env;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, Runtime, State, Theme, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_ID: &str = "codeseex";
const PRODUCT_NAME: &str = "CodeSeeX";
const QUIT_FOR_UPDATE_ARG: &str = "--quit-for-update";
const UPDATE_PROGRESS_EVENT: &str = "codeseex-update-progress";
const EXIT_PROXY_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Default)]
struct DesktopRuntime {
    quitting: AtomicBool,
    creating_window: AtomicBool,
    proxy: Mutex<ProxyRuntime>,
    manager: Mutex<Option<codeseex_proxy::ManagerRuntime>>,
}

#[derive(Default)]
struct PendingDesktopUpdate(Mutex<Option<Update>>);

#[derive(Default)]
struct DesktopUpdateTask(Mutex<Option<JoinHandle<()>>>);

#[derive(Debug)]
struct ProxyRuntime {
    status: String,
    error: Option<String>,
    shutdown: Option<oneshot::Sender<()>>,
    generation: u64,
    host: Option<String>,
    port: Option<u16>,
    base_url: Option<String>,
}

impl Default for ProxyRuntime {
    fn default() -> Self {
        Self {
            status: "stopped".to_owned(),
            error: None,
            shutdown: None,
            generation: 0,
            host: None,
            port: None,
            base_url: None,
        }
    }
}

#[derive(Debug, Clone)]
struct ProxyRuntimeStatus {
    status: String,
    error: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    base_url: Option<String>,
}

#[tauri::command]
fn desktop_window_action(
    app: AppHandle,
    state: State<'_, DesktopRuntime>,
    action: String,
) -> Result<(), String> {
    match action.as_str() {
        "minimize" => main_window(&app)?.minimize().map_err(string_error),
        "maximize" => toggle_maximize(&app),
        "close" => close_or_hide(&app, &state),
        _ => Err(format!("unsupported window action: {action}")),
    }
}

#[tauri::command]
fn desktop_apply_theme(app: AppHandle, theme: String) -> Result<(), String> {
    let theme = parse_theme(&theme);
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        window.set_theme(theme).map_err(string_error)?;
    }
    Ok(())
}

#[tauri::command]
fn desktop_apply_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    mutate_user_config(|config| {
        config
            .ui
            .get_or_insert_with(UserUiConfig::default)
            .auto_start = Some(enabled);
    })?;
    apply_autostart(&app, enabled)
}

#[tauri::command]
fn desktop_refresh_tray(app: AppHandle) -> Result<(), String> {
    refresh_tray_menu(&app)
}

#[tauri::command]
fn desktop_open_external(url: String) -> Result<(), String> {
    let url = url.trim();
    if !allowed_external_url(url) {
        return Err("only http, https, and ccswitch links can be opened externally".to_owned());
    }
    open_external_url(url)
}

#[tauri::command]
async fn desktop_check_update(
    app: AppHandle,
    pending_update: State<'_, PendingDesktopUpdate>,
) -> Result<Value, String> {
    let current_version = env!("CARGO_PKG_VERSION").to_owned();
    let update = desktop_updater(&app)?
        .check()
        .await
        .map_err(|error| format!("tauri_updater_check_failed: {error}"))?;

    let Some(update) = update else {
        *pending_update
            .0
            .lock()
            .map_err(|_| "failed to lock pending update state".to_owned())? = None;
        return Ok(json!({
            "ok": true,
            "has_update": false,
            "installable": false,
            "latest_version": current_version,
            "current_version": current_version,
            "source": "tauri_updater",
            "target": tauri_plugin_updater::target()
        }));
    };

    let latest_version = update.version.to_string();
    let update_current_version = update.current_version.to_string();
    *pending_update
        .0
        .lock()
        .map_err(|_| "failed to lock pending update state".to_owned())? = Some(update);

    Ok(json!({
        "ok": true,
        "has_update": true,
        "installable": true,
        "latest_version": latest_version,
        "current_version": if update_current_version.is_empty() {
            current_version
        } else {
            update_current_version
        },
        "source": "tauri_updater",
        "target": tauri_plugin_updater::target()
    }))
}

#[tauri::command]
async fn desktop_install_update(
    app: AppHandle,
    pending_update: State<'_, PendingDesktopUpdate>,
    update_task: State<'_, DesktopUpdateTask>,
) -> Result<Value, String> {
    {
        let mut task = update_task
            .0
            .lock()
            .map_err(|_| "failed to lock update task state".to_owned())?;
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return Ok(json!({
                "ok": true,
                "started": false,
                "running": true
            }));
        }
        if task.as_ref().is_some_and(|handle| handle.is_finished()) {
            *task = None;
        }
    }

    let pending = pending_update
        .0
        .lock()
        .map_err(|_| "failed to lock pending update state".to_owned())?
        .take();
    let update = match pending {
        Some(update) => update,
        None => desktop_updater(&app)?
            .check()
            .await
            .map_err(|error| format!("tauri_updater_check_failed: {error}"))?
            .ok_or_else(|| "there is no pending update".to_owned())?,
    };

    let version = update.version.to_string();
    let app_for_task = app.clone();
    emit_update_progress(&app, "starting", &version, 0, None, None);
    let handle = tokio::spawn(async move {
        run_desktop_update(app_for_task, update, version).await;
    });
    *update_task
        .0
        .lock()
        .map_err(|_| "failed to lock update task state".to_owned())? = Some(handle);

    Ok(json!({
        "ok": true,
        "started": true,
        "running": true
    }))
}

#[tauri::command]
fn desktop_cancel_update(
    app: AppHandle,
    update_task: State<'_, DesktopUpdateTask>,
) -> Result<Value, String> {
    let handle = update_task
        .0
        .lock()
        .map_err(|_| "failed to lock update task state".to_owned())?
        .take();
    if let Some(handle) = handle {
        if !handle.is_finished() {
            handle.abort();
        }
    }
    emit_update_progress(&app, "canceled", "", 0, None, None);
    Ok(json!({
        "ok": true,
        "canceled": true
    }))
}

async fn run_desktop_update(app: AppHandle, update: Update, version: String) {
    let downloaded = Arc::new(AtomicU64::new(0));
    let download_progress = Arc::clone(&downloaded);
    let finish_progress = Arc::clone(&downloaded);
    let app_progress = app.clone();
    let progress_version = version.clone();
    let app_finish = app.clone();
    let finish_version = version.clone();

    let bytes = update
        .download(
            move |chunk_length, content_length| {
                let total = download_progress.fetch_add(chunk_length as u64, Ordering::SeqCst)
                    + chunk_length as u64;
                emit_update_progress(
                    &app_progress,
                    "downloading",
                    &progress_version,
                    total,
                    content_length,
                    None,
                );
            },
            move || {
                emit_update_progress(
                    &app_finish,
                    "verifying",
                    &finish_version,
                    finish_progress.load(Ordering::SeqCst),
                    None,
                    None,
                );
            },
        )
        .await;

    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            app.state::<DesktopRuntime>()
                .quitting
                .store(false, Ordering::SeqCst);
            emit_update_progress(
                &app,
                "failed",
                &version,
                downloaded.load(Ordering::SeqCst),
                None,
                Some(&error.to_string()),
            );
            return;
        }
    };

    app.state::<DesktopRuntime>()
        .quitting
        .store(true, Ordering::SeqCst);
    emit_update_progress(
        &app,
        "installing",
        &version,
        downloaded.load(Ordering::SeqCst),
        Some(downloaded.load(Ordering::SeqCst)),
        None,
    );
    if let Err(error) = update.install(bytes) {
        app.state::<DesktopRuntime>()
            .quitting
            .store(false, Ordering::SeqCst);
        emit_update_progress(
            &app,
            "failed",
            &version,
            downloaded.load(Ordering::SeqCst),
            None,
            Some(&error.to_string()),
        );
        return;
    }
    emit_update_progress(
        &app,
        "restarting",
        &version,
        downloaded.load(Ordering::SeqCst),
        Some(downloaded.load(Ordering::SeqCst)),
        None,
    );
    app.restart();
}

fn emit_update_progress(
    app: &AppHandle,
    stage: &str,
    version: &str,
    downloaded: u64,
    content_length: Option<u64>,
    error: Option<&str>,
) {
    let percent = content_length
        .filter(|value| *value > 0)
        .map(|value| ((downloaded as f64 / value as f64) * 100.0).clamp(0.0, 100.0));
    let _ = app.emit(
        UPDATE_PROGRESS_EVENT,
        json!({
            "stage": stage,
            "version": version,
            "downloaded": downloaded,
            "content_length": content_length,
            "percent": percent,
            "error": error,
        }),
    );
}

fn desktop_updater<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<tauri_plugin_updater::Updater, String> {
    let config = AppConfig::load();
    let mut builder = app.updater_builder().timeout(Duration::from_secs(30));
    match config.network_proxy {
        NetworkProxyMode::None => {
            builder = builder.no_proxy();
        }
        NetworkProxyMode::System => {
            if let Some(proxy_url) = desktop_system_proxy_url() {
                let proxy = proxy_url
                    .parse()
                    .map_err(|error| format!("invalid system proxy for updater: {error}"))?;
                builder = builder.proxy(proxy);
            }
        }
    }
    builder.build().map_err(string_error)
}

fn desktop_system_proxy_url() -> Option<String> {
    env_proxy_url().or_else(windows_internet_settings_proxy_url)
}

fn env_proxy_url() -> Option<String> {
    ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find_map(|value| normalize_proxy_server(&value))
}

#[cfg(windows)]
fn windows_internet_settings_proxy_url() -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let settings = hkcu
        .open_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings",
            KEY_READ,
        )
        .ok()?;
    let enabled = settings
        .get_value::<u32, _>("ProxyEnable")
        .ok()
        .or_else(|| {
            settings
                .get_value::<u64, _>("ProxyEnable")
                .ok()
                .and_then(|value| u32::try_from(value).ok())
        })?;
    if enabled == 0 {
        return None;
    }
    let server = settings.get_value::<String, _>("ProxyServer").ok()?;
    normalize_proxy_server(&server)
}

#[cfg(not(windows))]
fn windows_internet_settings_proxy_url() -> Option<String> {
    None
}

fn normalize_proxy_server(value: &str) -> Option<String> {
    let selected = select_proxy_server(value)?.trim();
    if selected.is_empty() {
        return None;
    }
    if selected.contains("://") {
        return Some(selected.to_owned());
    }
    Some(format!("http://{selected}"))
}

fn select_proxy_server(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains('=') {
        return Some(trimmed);
    }
    let entries = trimmed
        .split(';')
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim()))
        .collect::<Vec<_>>();
    for wanted in ["https", "http"] {
        if let Some((_, value)) = entries.iter().find(|(key, _)| key == wanted) {
            return Some(*value);
        }
    }
    entries.first().map(|(_, value)| *value)
}

fn allowed_external_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("ccswitch://")
}

#[tauri::command]
async fn desktop_manager_request(
    app: AppHandle,
    state: State<'_, DesktopRuntime>,
    method: String,
    path: String,
    query: Option<Value>,
    body: Option<Value>,
) -> Result<codeseex_proxy::ManagerJsonResponse, String> {
    if method.eq_ignore_ascii_case("POST") {
        if let Some(response) = handle_proxy_lifecycle_action(&app, &path).await? {
            return Ok(response);
        }
    }
    let manager = desktop_manager_runtime(&state).await?;
    let mut response = manager
        .handle_json(&method, &path, query.as_ref(), body.as_ref())
        .await;
    if path == "/api/status" {
        let proxy = proxy_runtime_status(&state);
        if let Some(object) = response.body.as_object_mut() {
            object.insert("running".to_owned(), Value::Bool(proxy.status == "running"));
            object.insert(
                "runtime_status".to_owned(),
                Value::String(proxy.status.clone()),
            );
            object.insert(
                "embedded_proxy_status".to_owned(),
                Value::String(proxy.status),
            );
            object.insert(
                "embedded_proxy_error".to_owned(),
                proxy.error.map(Value::String).unwrap_or(Value::Null),
            );
            if let Some(base_url) = proxy.base_url.clone() {
                object.insert("base_url".to_owned(), Value::String(base_url.clone()));
                if let Some(runtime) = object.get_mut("runtime").and_then(Value::as_object_mut) {
                    runtime.insert("base_url".to_owned(), Value::String(base_url));
                }
            }
            if let Some(port) = proxy.port {
                if let Some(runtime) = object.get_mut("runtime").and_then(Value::as_object_mut) {
                    runtime.insert("port".to_owned(), Value::from(port));
                }
            }
            if let Some(host) = proxy.host.clone() {
                if let Some(runtime) = object.get_mut("runtime").and_then(Value::as_object_mut) {
                    runtime.insert("host".to_owned(), Value::String(host));
                }
            }
        }
    }
    Ok(response)
}

pub fn run() {
    tauri::Builder::default()
        .manage(DesktopRuntime::default())
        .manage(PendingDesktopUpdate::default())
        .manage(DesktopUpdateTask::default())
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|arg| arg == QUIT_FOR_UPDATE_ARG) {
                request_app_exit(app);
                return;
            }
            if !args.iter().any(|arg| arg == "--autostart") {
                let _ = show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            if launched_for_update_quit() {
                app.handle().exit(0);
                return Ok(());
            }
            if let Err(error) = start_embedded_proxy(app.handle().clone()) {
                eprintln!("[codeseex] failed to start embedded proxy: {error}");
            }
            create_tray(app.handle())?;
            sync_configured_autostart(app.handle());
            let windows = app.webview_windows();
            eprintln!(
                "[codeseex] desktop setup complete; windows={}",
                windows.keys().cloned().collect::<Vec<_>>().join(",")
            );
            if !launched_from_autostart() {
                show_main_window(app.handle())?;
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<DesktopRuntime>();
                if state.quitting.load(Ordering::SeqCst) {
                    return;
                }
                api.prevent_close();
                if should_hide_to_tray() {
                    let _ = window.destroy();
                } else {
                    request_app_exit(window.app_handle());
                }
                return;
            }
            if matches!(
                event,
                WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
            ) {
                eprintln!(
                    "[codeseex] window event: label={} event={event:?}",
                    window.label()
                );
            }
        })
        .invoke_handler(tauri::generate_handler![
            desktop_window_action,
            desktop_apply_theme,
            desktop_apply_autostart,
            desktop_refresh_tray,
            desktop_open_external,
            desktop_check_update,
            desktop_install_update,
            desktop_cancel_update,
            desktop_manager_request
        ])
        .build(tauri::generate_context!())
        .expect("failed to build CodeSeeX desktop")
        .run(|app, event| match event {
            RunEvent::ExitRequested { api, code, .. } => {
                let quitting = app
                    .state::<DesktopRuntime>()
                    .quitting
                    .load(Ordering::SeqCst);
                if !quitting && code.is_none() && should_hide_to_tray() {
                    api.prevent_exit();
                } else {
                    remove_tray_icon(app);
                }
            }
            RunEvent::Exit => remove_tray_icon(app),
            _ => {}
        });
}

fn start_embedded_proxy(app: AppHandle) -> Result<(), String> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let generation = {
        let state = app.state::<DesktopRuntime>();
        let mut proxy = state
            .proxy
            .lock()
            .map_err(|_| "desktop proxy runtime lock was poisoned".to_owned())?;
        if matches!(proxy.status.as_str(), "starting" | "running" | "stopping") {
            return Ok(());
        }
        proxy.generation = proxy.generation.wrapping_add(1);
        proxy.status = "starting".to_owned();
        proxy.error = None;
        proxy.shutdown = Some(shutdown_tx);
        proxy.generation
    };

    tauri::async_runtime::spawn(async move {
        let config = AppConfig::load_base();
        let effective_config = AppConfig::load();
        let endpoint = ProxyRuntimeEndpoint {
            host: effective_config.host.clone(),
            port: effective_config.port,
            base_url: effective_config.proxy_base_url(),
        };
        set_proxy_runtime_endpoint_if_generation(&app, generation, endpoint);
        let running_app = app.clone();
        let result = codeseex_proxy::serve_with_shutdown(
            config,
            async move {
                let _ = shutdown_rx.await;
            },
            move || {
                set_proxy_runtime_status_if_generation(
                    &running_app,
                    generation,
                    "running",
                    None,
                    false,
                );
            },
        )
        .await;
        match result {
            Ok(()) => {
                set_proxy_runtime_status_if_generation(&app, generation, "stopped", None, true);
            }
            Err(error) => {
                let message = format!("{error:#}");
                set_proxy_runtime_status_if_generation(
                    &app,
                    generation,
                    "failed",
                    Some(message.clone()),
                    true,
                );
                eprintln!("[codeseex] embedded proxy stopped: {message}");
            }
        }
    });
    Ok(())
}

fn stop_embedded_proxy<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let state = app.state::<DesktopRuntime>();
    let shutdown = {
        let mut proxy = state
            .proxy
            .lock()
            .map_err(|_| "desktop proxy runtime lock was poisoned".to_owned())?;
        match proxy.status.as_str() {
            "stopped" | "failed" => {
                proxy.status = "stopped".to_owned();
                proxy.error = None;
                proxy.shutdown = None;
                return Ok(());
            }
            _ => {
                proxy.status = "stopping".to_owned();
                proxy.error = None;
                proxy.shutdown.take()
            }
        }
    };
    if let Some(shutdown) = shutdown {
        let _ = shutdown.send(());
    } else {
        set_proxy_runtime_status(app, "stopped", None, true)?;
    }
    Ok(())
}

async fn wait_for_embedded_proxy_stop<R: Runtime>(
    app: &AppHandle<R>,
    timeout: std::time::Duration,
) -> bool {
    let started_at = std::time::Instant::now();
    loop {
        let status = app
            .state::<DesktopRuntime>()
            .proxy
            .lock()
            .map(|proxy| proxy.status.clone())
            .unwrap_or_else(|_| "failed".to_owned());
        if matches!(status.as_str(), "stopped" | "failed") {
            return true;
        }
        if started_at.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn restart_embedded_proxy(app: AppHandle) -> Result<(), String> {
    stop_embedded_proxy(&app)?;
    if !wait_for_embedded_proxy_stop(&app, std::time::Duration::from_secs(3)).await {
        return Err("embedded proxy did not stop before restart timeout".to_owned());
    }
    start_embedded_proxy(app)
}

async fn handle_proxy_lifecycle_action(
    app: &AppHandle,
    path: &str,
) -> Result<Option<codeseex_proxy::ManagerJsonResponse>, String> {
    let action = match path {
        "/api/start" => "start",
        "/api/restart" => "restart",
        "/api/stop" => "stop",
        _ => return Ok(None),
    };
    match action {
        "start" => start_embedded_proxy(app.clone())?,
        "restart" => restart_embedded_proxy(app.clone()).await?,
        "stop" => stop_embedded_proxy(app)?,
        _ => {}
    }
    let status = proxy_runtime_status(&app.state::<DesktopRuntime>());
    Ok(Some(codeseex_proxy::ManagerJsonResponse {
        status: 200,
        body: json!({
            "ok": true,
            "mode": "desktop",
            "action": action,
            "running": status.status == "running",
            "runtime_status": status.status,
            "embedded_proxy_error": status.error
        }),
    }))
}

fn set_proxy_runtime_status<R: Runtime>(
    app: &AppHandle<R>,
    status: &str,
    error: Option<String>,
    clear_shutdown: bool,
) -> Result<(), String> {
    let state = app.state::<DesktopRuntime>();
    let mut proxy = state
        .proxy
        .lock()
        .map_err(|_| "desktop proxy runtime lock was poisoned".to_owned())?;
    proxy.status = status.to_owned();
    proxy.error = error;
    if clear_shutdown {
        proxy.shutdown = None;
        proxy.host = None;
        proxy.port = None;
        proxy.base_url = None;
    }
    Ok(())
}

fn set_proxy_runtime_status_if_generation<R: Runtime>(
    app: &AppHandle<R>,
    generation: u64,
    status: &str,
    error: Option<String>,
    clear_shutdown: bool,
) {
    let state = app.state::<DesktopRuntime>();
    if let Ok(mut proxy) = state.proxy.lock() {
        if proxy.generation != generation {
            return;
        }
        proxy.status = status.to_owned();
        proxy.error = error;
        if clear_shutdown {
            proxy.shutdown = None;
            proxy.host = None;
            proxy.port = None;
            proxy.base_url = None;
        }
    };
}

#[derive(Debug, Clone)]
struct ProxyRuntimeEndpoint {
    host: String,
    port: u16,
    base_url: String,
}

fn set_proxy_runtime_endpoint_if_generation(
    app: &AppHandle,
    generation: u64,
    endpoint: ProxyRuntimeEndpoint,
) {
    let state = app.state::<DesktopRuntime>();
    if let Ok(mut proxy) = state.proxy.lock() {
        if proxy.generation != generation {
            return;
        }
        proxy.host = Some(endpoint.host);
        proxy.port = Some(endpoint.port);
        proxy.base_url = Some(endpoint.base_url);
    };
}

fn proxy_runtime_status(state: &DesktopRuntime) -> ProxyRuntimeStatus {
    state
        .proxy
        .lock()
        .map(|proxy| ProxyRuntimeStatus {
            status: proxy.status.clone(),
            error: proxy.error.clone(),
            host: proxy.host.clone(),
            port: proxy.port,
            base_url: proxy.base_url.clone(),
        })
        .unwrap_or_else(|_| ProxyRuntimeStatus {
            status: "failed".to_owned(),
            error: Some("desktop proxy status lock was poisoned".to_owned()),
            host: None,
            port: None,
            base_url: None,
        })
}

async fn desktop_manager_runtime(
    state: &DesktopRuntime,
) -> Result<codeseex_proxy::ManagerRuntime, String> {
    if let Ok(manager) = state.manager.lock() {
        if let Some(manager) = manager.clone() {
            return Ok(manager);
        }
    } else {
        return Err("desktop manager runtime lock was poisoned".to_owned());
    }

    let manager = codeseex_proxy::ManagerRuntime::open(AppConfig::load_base())
        .await
        .map_err(|error| format!("{error:#}"))?;
    let mut guard = state
        .manager
        .lock()
        .map_err(|_| "desktop manager runtime lock was poisoned".to_owned())?;
    *guard = Some(manager.clone());
    Ok(manager)
}

fn create_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(PRODUCT_NAME)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_tray_menu(app, event.id().as_ref()));
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                let _ = show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn build_tray_menu<R: Runtime, M: Manager<R>>(manager: &M) -> tauri::Result<Menu<R>> {
    let user_config = UserConfig::read_from(&AppConfig::load().config_path()).unwrap_or_default();
    let i18n = TrayI18n::from_user_config(&user_config);
    let model = user_config
        .model
        .as_ref()
        .and_then(|value| value.override_mode)
        .unwrap_or_default();
    let temperature = user_config
        .model
        .as_ref()
        .and_then(|value| value.temperature)
        .unwrap_or_default();
    let thinking = user_config
        .model
        .as_ref()
        .and_then(|value| value.thinking.as_deref())
        .unwrap_or("auto");

    let model_default =
        CheckMenuItemBuilder::with_id("tray:model:default", i18n.text("modelDefault", &[]))
            .checked(model == UpstreamModelOverride::Default)
            .build(manager)?;
    let model_flash =
        CheckMenuItemBuilder::with_id("tray:model:flash", i18n.text("modelFlash", &[]))
            .checked(model == UpstreamModelOverride::Flash)
            .build(manager)?;
    let model_pro = CheckMenuItemBuilder::with_id("tray:model:pro", i18n.text("modelPro", &[]))
        .checked(model == UpstreamModelOverride::Pro)
        .build(manager)?;
    let model_menu = SubmenuBuilder::new(manager, i18n.text("trayModel", &[]))
        .items(&[&model_default, &model_flash, &model_pro])
        .build()?;

    let thinking_auto =
        CheckMenuItemBuilder::with_id("tray:thinking:auto", i18n.text("thinkingAuto", &[]))
            .checked(thinking == "auto")
            .build(manager)?;
    let thinking_enabled =
        CheckMenuItemBuilder::with_id("tray:thinking:enabled", i18n.text("thinkingEnabled", &[]))
            .checked(thinking == "enabled")
            .build(manager)?;
    let thinking_disabled =
        CheckMenuItemBuilder::with_id("tray:thinking:disabled", i18n.text("thinkingDisabled", &[]))
            .checked(thinking == "disabled")
            .build(manager)?;
    let thinking_menu = SubmenuBuilder::new(manager, i18n.text("trayThinking", &[]))
        .items(&[&thinking_auto, &thinking_enabled, &thinking_disabled])
        .build()?;

    let temp_default = CheckMenuItemBuilder::with_id(
        "tray:temperature:default",
        i18n.text("temperatureDefault", &[]),
    )
    .checked(temperature == TemperaturePreset::Default)
    .build(manager)?;
    let temp_strict = CheckMenuItemBuilder::with_id(
        "tray:temperature:strict",
        i18n.text("temperatureStrict", &[]),
    )
    .checked(temperature == TemperaturePreset::Strict)
    .build(manager)?;
    let temp_balanced = CheckMenuItemBuilder::with_id(
        "tray:temperature:balanced",
        i18n.text("temperatureBalanced", &[]),
    )
    .checked(temperature == TemperaturePreset::Balanced)
    .build(manager)?;
    let temp_general = CheckMenuItemBuilder::with_id(
        "tray:temperature:general",
        i18n.text("temperatureGeneral", &[]),
    )
    .checked(temperature == TemperaturePreset::General)
    .build(manager)?;
    let temp_creative = CheckMenuItemBuilder::with_id(
        "tray:temperature:creative",
        i18n.text("temperatureCreative", &[]),
    )
    .checked(temperature == TemperaturePreset::Creative)
    .build(manager)?;
    let temperature_menu = SubmenuBuilder::new(manager, i18n.text("trayTemperature", &[]))
        .items(&[
            &temp_default,
            &temp_strict,
            &temp_balanced,
            &temp_general,
            &temp_creative,
        ])
        .build()?;

    MenuBuilder::new(manager)
        .text(
            "tray:show",
            i18n.text("trayShow", &[("name", PRODUCT_NAME)]),
        )
        .separator()
        .item(&model_menu)
        .item(&thinking_menu)
        .item(&temperature_menu)
        .separator()
        .text("tray:quit", i18n.text("trayQuit", &[]))
        .build()
}

fn refresh_tray_menu<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    let menu = build_tray_menu(app).map_err(string_error)?;
    tray.set_menu(Some(menu)).map_err(string_error)
}

struct TrayI18n {
    pack: Value,
}

impl TrayI18n {
    fn from_user_config(user_config: &UserConfig) -> Self {
        let requested = user_config
            .ui
            .as_ref()
            .and_then(|ui| ui.language.as_deref())
            .unwrap_or("system");
        let language = resolve_tray_language_id(requested);
        let pack = builtin_language_pack(&language)
            .or_else(|| builtin_language_pack("en_us"))
            .unwrap_or_else(|| Value::Object(Default::default()));
        Self { pack }
    }

    fn text(&self, key: &str, vars: &[(&str, &str)]) -> String {
        let fallback = tray_fallback_text(key);
        let mut text = self
            .pack
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_owned();
        for (name, value) in vars {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }
}

fn tray_fallback_text(key: &str) -> &str {
    match key {
        "modelDefault" => "Default",
        "modelFlash" => "Flash",
        "modelPro" => "Pro",
        "temperatureBalanced" => "Balanced",
        "temperatureCreative" => "Creative",
        "temperatureDefault" => "Default",
        "temperatureGeneral" => "General",
        "temperatureStrict" => "Strict",
        "thinkingAuto" => "Auto",
        "thinkingDisabled" => "Force off",
        "thinkingEnabled" => "Force on",
        "trayModel" => "Model",
        "trayQuit" => "Quit",
        "trayShow" => "Show {name}",
        "trayTemperature" => "Sampling temperature",
        "trayThinking" => "Thinking",
        _ => key,
    }
}

fn resolve_tray_language_id(value: &str) -> String {
    let requested = normalize_language_id(value);
    if requested != "system" {
        return requested;
    }
    for locale in system_locale_candidates() {
        let normalized = normalize_language_id(&locale);
        if builtin_language_pack(&normalized).is_some() {
            return normalized;
        }
        let prefix = normalized.split('_').next().unwrap_or("");
        if let Some(prefix_match) = builtin_language_ids()
            .iter()
            .find(|id| id.starts_with(&format!("{prefix}_")))
        {
            return (*prefix_match).to_owned();
        }
    }
    "en_us".to_owned()
}

fn system_locale_candidates() -> Vec<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|key| env::var(key).ok())
        .collect()
}

fn normalize_language_id(value: &str) -> String {
    value
        .trim()
        .split('.')
        .next()
        .unwrap_or(value)
        .replace('-', "_")
        .to_ascii_lowercase()
}

fn builtin_language_ids() -> &'static [&'static str] {
    &[
        "de_de", "en_us", "fr_fr", "ja_jp", "ko_kr", "ru_ru", "zh_cn", "zh_hk", "zh_tw",
    ]
}

fn builtin_language_pack(id: &str) -> Option<Value> {
    let text = match normalize_language_id(id).as_str() {
        "de_de" => include_str!("../../../ui/public/lang/de_de.json"),
        "en_us" => include_str!("../../../ui/public/lang/en_us.json"),
        "fr_fr" => include_str!("../../../ui/public/lang/fr_fr.json"),
        "ja_jp" => include_str!("../../../ui/public/lang/ja_jp.json"),
        "ko_kr" => include_str!("../../../ui/public/lang/ko_kr.json"),
        "ru_ru" => include_str!("../../../ui/public/lang/ru_ru.json"),
        "zh_cn" => include_str!("../../../ui/public/lang/zh_cn.json"),
        "zh_hk" => include_str!("../../../ui/public/lang/zh_hk.json"),
        "zh_tw" => include_str!("../../../ui/public/lang/zh_tw.json"),
        _ => return None,
    };
    serde_json::from_str(text).ok()
}

fn handle_tray_menu<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let result = match id {
        "tray:show" => show_main_window(app),
        "tray:quit" => {
            request_app_exit(app);
            Ok(())
        }
        "tray:model:default" => update_model_override(UpstreamModelOverride::Default),
        "tray:model:flash" => update_model_override(UpstreamModelOverride::Flash),
        "tray:model:pro" => update_model_override(UpstreamModelOverride::Pro),
        "tray:thinking:auto" => update_thinking("auto"),
        "tray:thinking:enabled" => update_thinking("enabled"),
        "tray:thinking:disabled" => update_thinking("disabled"),
        "tray:temperature:default" => update_temperature(TemperaturePreset::Default),
        "tray:temperature:strict" => update_temperature(TemperaturePreset::Strict),
        "tray:temperature:balanced" => update_temperature(TemperaturePreset::Balanced),
        "tray:temperature:general" => update_temperature(TemperaturePreset::General),
        "tray:temperature:creative" => update_temperature(TemperaturePreset::Creative),
        _ => Ok(()),
    };

    if let Err(error) = result {
        eprintln!("[codeseex] tray action failed: {error}");
        return;
    }

    if id.starts_with("tray:model:")
        || id.starts_with("tray:thinking:")
        || id.starts_with("tray:temperature:")
    {
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            if let Ok(menu) = build_tray_menu(app) {
                let _ = tray.set_menu(Some(menu));
            }
        }
        let _ = app.emit("codeseex-config-changed", ());
    }
}

fn update_model_override(value: UpstreamModelOverride) -> Result<(), String> {
    mutate_user_config(|config| {
        config
            .model
            .get_or_insert_with(UserModelConfig::default)
            .override_mode = Some(value);
    })
}

fn update_temperature(value: TemperaturePreset) -> Result<(), String> {
    mutate_user_config(|config| {
        config
            .model
            .get_or_insert_with(UserModelConfig::default)
            .temperature = Some(value);
    })
}

fn update_thinking(value: &str) -> Result<(), String> {
    mutate_user_config(|config| {
        config
            .model
            .get_or_insert_with(UserModelConfig::default)
            .thinking = Some(value.to_owned());
    })
}

fn mutate_user_config(mutator: impl FnOnce(&mut UserConfig)) -> Result<(), String> {
    let app_config = AppConfig::load();
    let path = app_config.config_path();
    let mut user_config = UserConfig::read_from(&path).unwrap_or_default();
    mutator(&mut user_config);
    user_config.write_atomic(&path).map_err(string_error)
}

fn should_hide_to_tray() -> bool {
    UserConfig::read_from(&AppConfig::load().config_path())
        .ok()
        .and_then(|config| config.ui)
        .and_then(|ui| ui.close_behavior)
        .as_deref()
        == Some("tray")
}

fn configured_autostart() -> Option<bool> {
    UserConfig::read_from(&AppConfig::load().config_path())
        .ok()
        .and_then(|config| config.ui)
        .and_then(|ui| ui.auto_start)
}

fn launched_from_autostart() -> bool {
    env::args().any(|arg| arg == "--autostart")
}

fn launched_for_update_quit() -> bool {
    env::args().any(|arg| arg == QUIT_FOR_UPDATE_ARG)
}

fn sync_configured_autostart<R: Runtime>(app: &AppHandle<R>) {
    if let Some(enabled) = configured_autostart() {
        if let Err(error) = apply_autostart(app, enabled) {
            eprintln!("[codeseex] autostart sync failed: {error}");
        }
    }
}

fn apply_autostart<R: Runtime>(app: &AppHandle<R>, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(string_error)?;
    } else {
        match manager.disable() {
            Ok(()) => {}
            Err(error) if autostart_not_found_error(&error) => return Ok(false),
            Err(error) => return Err(string_error(error)),
        }
    }
    match manager.is_enabled() {
        Ok(value) => Ok(value),
        Err(error) if !enabled && autostart_not_found_error(&error) => Ok(false),
        Err(error) => Err(string_error(error)),
    }
}

fn autostart_not_found_error(error: &impl std::fmt::Display) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("os error 2")
        || text.contains("not found")
        || text.contains("cannot find the file")
        || text.contains("找不到指定的文件")
}

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Result<tauri::WebviewWindow<R>, String> {
    app.get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "main window not found".to_owned())
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        window.show().map_err(string_error)?;
        return window.set_focus().map_err(string_error);
    }
    spawn_main_window(app);
    Ok(())
}

fn spawn_main_window<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<DesktopRuntime>();
    if state
        .creating_window
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        if let Err(error) = create_and_focus_main_window(&app) {
            eprintln!("[codeseex] create main window failed: {error}");
        }
        app.state::<DesktopRuntime>()
            .creating_window
            .store(false, Ordering::SeqCst);
    });
}

fn create_and_focus_main_window<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<tauri::WebviewWindow<R>, String> {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        window.show().map_err(string_error)?;
        window.set_focus().map_err(string_error)?;
        return Ok(window);
    }
    let window =
        WebviewWindowBuilder::new(app, MAIN_WINDOW_LABEL, WebviewUrl::App("index.html".into()))
            .title(PRODUCT_NAME)
            .inner_size(1280.0, 720.0)
            .min_inner_size(1280.0, 720.0)
            .resizable(true)
            .decorations(true)
            .center()
            .visible(true)
            .theme(configured_theme())
            .build()
            .map_err(string_error)?;
    window.set_focus().map_err(string_error)?;
    Ok(window)
}

fn configured_theme() -> Option<Theme> {
    UserConfig::read_from(&AppConfig::load().config_path())
        .ok()
        .and_then(|config| config.ui)
        .and_then(|ui| ui.theme)
        .as_deref()
        .and_then(parse_theme)
}

fn parse_theme(value: &str) -> Option<Theme> {
    match value {
        "dark" => Some(Theme::Dark),
        "light" => Some(Theme::Light),
        _ => None,
    }
}

fn open_external_url(url: &str) -> Result<(), String> {
    let mut command = external_open_command(url);
    command.spawn().map_err(string_error)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn external_open_command(url: &str) -> Command {
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let mut command = Command::new("rundll32.exe");
    command
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(target_os = "macos")]
fn external_open_command(url: &str) -> Command {
    let mut command = Command::new("open");
    command.arg(url);
    command
}

#[cfg(all(unix, not(target_os = "macos")))]
fn external_open_command(url: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(url);
    command
}

fn toggle_maximize(app: &AppHandle) -> Result<(), String> {
    let window = main_window(app)?;
    if window.is_maximized().map_err(string_error)? {
        window.unmaximize().map_err(string_error)
    } else {
        window.maximize().map_err(string_error)
    }
}

fn close_or_hide(app: &AppHandle, _state: &DesktopRuntime) -> Result<(), String> {
    if should_hide_to_tray() {
        main_window(app)?.destroy().map_err(string_error)
    } else {
        request_app_exit(app);
        Ok(())
    }
}

fn request_app_exit<R: Runtime + 'static>(app: &AppHandle<R>) {
    let already_quitting = app
        .state::<DesktopRuntime>()
        .quitting
        .swap(true, Ordering::SeqCst);
    remove_tray_icon(app);
    if already_quitting {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = stop_embedded_proxy(&app) {
            eprintln!("[codeseex] failed to stop embedded proxy before exit: {error}");
        }
        if !wait_for_embedded_proxy_stop(&app, EXIT_PROXY_STOP_TIMEOUT).await {
            eprintln!("[codeseex] embedded proxy did not stop before app exit timeout");
        }
        remove_tray_icon(&app);
        app.exit(0);
    });
}

fn remove_tray_icon<R: Runtime>(app: &AppHandle<R>) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_visible(false);
    }
    let _ = app.remove_tray_by_id(TRAY_ID);
}

fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_i18n_uses_configured_language_pack() {
        let user_config = UserConfig {
            ui: Some(UserUiConfig {
                language: Some("zh_cn".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let i18n = TrayI18n::from_user_config(&user_config);
        assert_eq!(i18n.text("trayModel", &[]), "\u{6a21}\u{578b}");
        assert_eq!(
            i18n.text("trayShow", &[("name", "CodeSeeX")]),
            "\u{663e}\u{793a} CodeSeeX"
        );
    }

    #[test]
    fn tray_i18n_falls_back_to_english_for_unknown_language() {
        let user_config = UserConfig {
            ui: Some(UserUiConfig {
                language: Some("missing_lang".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let i18n = TrayI18n::from_user_config(&user_config);
        assert_eq!(i18n.text("trayModel", &[]), "Model");
    }

    #[test]
    fn autostart_not_found_is_treated_as_disabled() {
        assert!(autostart_not_found_error(
            &"系统找不到指定的文件。 (os error 2)"
        ));
        assert!(autostart_not_found_error(
            &"The system cannot find the file specified. (os error 2)"
        ));
        assert!(!autostart_not_found_error(&"access denied"));
    }
}
