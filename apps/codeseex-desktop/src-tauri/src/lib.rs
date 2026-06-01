use codeseex_core::{
    AppConfig, TemperaturePreset, UpstreamModelOverride, UserConfig, UserModelConfig, UserUiConfig,
};
use serde::Serialize;
use serde_json::Value;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, Theme, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;

const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_ID: &str = "codeseex-next";

#[derive(Default)]
struct DesktopRuntime {
    quitting: AtomicBool,
}

#[derive(Debug, Serialize)]
struct DesktopStatus {
    data_dir: String,
    proxy_base_url: String,
    close_behavior: String,
    auto_start: bool,
    auto_start_enabled: Option<bool>,
    auto_start_error: Option<String>,
}

#[tauri::command]
fn desktop_status(app: AppHandle) -> DesktopStatus {
    let config = AppConfig::load();
    let user_config = UserConfig::read_from(&config.config_path()).unwrap_or_default();
    let ui = user_config.ui.unwrap_or_default();
    let (auto_start_enabled, auto_start_error) = read_os_autostart_status(&app);
    DesktopStatus {
        data_dir: config.data_dir.to_string_lossy().to_string(),
        proxy_base_url: config.proxy_base_url(),
        close_behavior: ui.close_behavior.unwrap_or_else(|| "exit".to_owned()),
        auto_start: ui.auto_start.unwrap_or(false),
        auto_start_enabled,
        auto_start_error,
    }
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
    let theme = match theme.as_str() {
        "dark" => Some(Theme::Dark),
        "light" => Some(Theme::Light),
        _ => None,
    };
    main_window(&app)?.set_theme(theme).map_err(string_error)
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

pub fn run() {
    tauri::Builder::default()
        .manage(DesktopRuntime::default())
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args.iter().any(|arg| arg == "--autostart") {
                let _ = show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .setup(|app| {
            start_embedded_proxy();
            create_tray(app.handle())?;
            sync_configured_autostart(app.handle());
            let windows = app.webview_windows();
            eprintln!(
                "[codeseex-next] desktop setup complete; windows={}",
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
                if !state.quitting.load(Ordering::SeqCst) && should_hide_to_tray() {
                    api.prevent_close();
                    let _ = window.hide();
                    return;
                }
            }
            if matches!(
                event,
                WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
            ) {
                eprintln!(
                    "[codeseex-next] window event: label={} event={event:?}",
                    window.label()
                );
            }
        })
        .invoke_handler(tauri::generate_handler![
            desktop_status,
            desktop_window_action,
            desktop_apply_theme,
            desktop_apply_autostart,
            desktop_refresh_tray
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CodeSeeX Next desktop");
}

fn start_embedded_proxy() {
    tauri::async_runtime::spawn(async {
        let config = AppConfig::load();
        if let Err(error) = codeseex_proxy::serve(config).await {
            eprintln!("[codeseex-next] embedded proxy stopped: {error:#}");
        }
    });
}

fn create_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("CodeSeeX Next")
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
            i18n.text("trayShow", &[("name", "CodeSeeX Next")]),
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
        "de_de" => include_str!("../../../codeseex-ui/public/lang/de_de.json"),
        "en_us" => include_str!("../../../codeseex-ui/public/lang/en_us.json"),
        "fr_fr" => include_str!("../../../codeseex-ui/public/lang/fr_fr.json"),
        "ja_jp" => include_str!("../../../codeseex-ui/public/lang/ja_jp.json"),
        "ko_kr" => include_str!("../../../codeseex-ui/public/lang/ko_kr.json"),
        "ru_ru" => include_str!("../../../codeseex-ui/public/lang/ru_ru.json"),
        "zh_cn" => include_str!("../../../codeseex-ui/public/lang/zh_cn.json"),
        "zh_hk" => include_str!("../../../codeseex-ui/public/lang/zh_hk.json"),
        "zh_tw" => include_str!("../../../codeseex-ui/public/lang/zh_tw.json"),
        _ => return None,
    };
    serde_json::from_str(text).ok()
}

fn handle_tray_menu<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let result = match id {
        "tray:show" => show_main_window(app),
        "tray:quit" => {
            app.state::<DesktopRuntime>()
                .quitting
                .store(true, Ordering::SeqCst);
            app.exit(0);
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
        eprintln!("[codeseex-next] tray action failed: {error}");
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

fn sync_configured_autostart<R: Runtime>(app: &AppHandle<R>) {
    if let Some(enabled) = configured_autostart() {
        if let Err(error) = apply_autostart(app, enabled) {
            eprintln!("[codeseex-next] autostart sync failed: {error}");
        }
    }
}

fn apply_autostart<R: Runtime>(app: &AppHandle<R>, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(string_error)?;
    } else {
        manager.disable().map_err(string_error)?;
    }
    manager.is_enabled().map_err(string_error)
}

fn read_os_autostart_status<R: Runtime>(app: &AppHandle<R>) -> (Option<bool>, Option<String>) {
    match app.autolaunch().is_enabled() {
        Ok(enabled) => (Some(enabled), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Result<tauri::WebviewWindow<R>, String> {
    app.get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "main window not found".to_owned())
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let window = main_window(app)?;
    window.show().map_err(string_error)?;
    window.set_focus().map_err(string_error)
}

fn toggle_maximize(app: &AppHandle) -> Result<(), String> {
    let window = main_window(app)?;
    if window.is_maximized().map_err(string_error)? {
        window.unmaximize().map_err(string_error)
    } else {
        window.maximize().map_err(string_error)
    }
}

fn close_or_hide(app: &AppHandle, state: &DesktopRuntime) -> Result<(), String> {
    if should_hide_to_tray() {
        main_window(app)?.hide().map_err(string_error)
    } else {
        state.quitting.store(true, Ordering::SeqCst);
        app.exit(0);
        Ok(())
    }
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
}
