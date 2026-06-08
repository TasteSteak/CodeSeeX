use serde_json::Value;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const API_KEY_FIELDS: &[&str] = &["OPENAI_API_KEY", "DEEPSEEK_API_KEY", "api_key", "apiKey"];

static CACHED_AUTHORIZATION: OnceLock<Mutex<String>> = OnceLock::new();

pub fn remember_authorization_header(value: &str) -> Option<String> {
    let normalized = normalize_authorization_header(value)?;
    if let Ok(mut cached) = cached_authorization().lock() {
        *cached = normalized.clone();
    }
    Some(normalized)
}

pub fn read_codex_auth_api_key(include_cached_authorization: bool) -> Option<String> {
    if include_cached_authorization {
        if let Some(value) = cached_authorization_api_key() {
            return Some(value);
        }
    }

    let path = resolve_codex_auth_path()?;
    let text = fs::read_to_string(path).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let parsed = serde_json::from_str::<Value>(text).ok()?;
    api_key_from_codex_auth(&parsed)
}

pub fn resolve_codex_auth_path() -> Option<PathBuf> {
    let candidates = codex_auth_path_candidates();
    candidates
        .iter()
        .find(|path| path.try_exists().unwrap_or(false))
        .cloned()
        .or_else(|| candidates.into_iter().next())
}

fn cached_authorization() -> &'static Mutex<String> {
    CACHED_AUTHORIZATION.get_or_init(|| Mutex::new(String::new()))
}

fn cached_authorization_api_key() -> Option<String> {
    let cached = cached_authorization().lock().ok()?;
    api_key_from_authorization(&cached)
}

fn api_key_from_authorization(value: &str) -> Option<String> {
    normalize_authorization_header(value).map(|value| {
        value
            .trim_start_matches(|ch: char| ch.is_ascii_whitespace())
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .unwrap_or(&value)
            .trim()
            .to_owned()
    })
}

fn normalize_authorization_header(value: &str) -> Option<String> {
    let text = value.trim();
    let (scheme, rest) = text.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") || rest.trim().is_empty() {
        return None;
    }
    Some(format!("Bearer {}", rest.trim()))
}

fn api_key_from_codex_auth(auth: &Value) -> Option<String> {
    let object = auth.as_object()?;
    API_KEY_FIELDS.iter().find_map(|field| {
        object
            .get(*field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn codex_auth_path_candidates() -> Vec<PathBuf> {
    if let Some(explicit) = env_value("CODEX_AUTH_JSON").or_else(|| env_value("CODEX_AUTH_FILE")) {
        return vec![resolve_auth_path(&explicit)];
    }

    let mut candidates = Vec::new();
    if let Some(codex_home) = env_value("CODEX_HOME") {
        candidates.push(resolve_auth_path(&codex_home).join("auth.json"));
    }

    if let Some(home) = env_value("USERPROFILE")
        .or_else(|| env_value("HOME"))
        .or_else(|| dirs_next::home_dir().map(|path| path.to_string_lossy().to_string()))
    {
        candidates.push(resolve_auth_path(&home).join(".codex").join("auth.json"));
    }

    if let Some(app_data) = env_value("APPDATA") {
        candidates.push(resolve_auth_path(&app_data).join("codex").join("auth.json"));
    }

    unique_paths(candidates)
}

fn resolve_auth_path(value: &str) -> PathBuf {
    let raw = value.trim();
    if raw == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn home_dir() -> Option<PathBuf> {
    env_value("USERPROFILE")
        .or_else(|| env_value("HOME"))
        .map(PathBuf::from)
        .or_else(dirs_next::home_dir)
}

fn env_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn unique_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    for path in paths {
        let key = display_path_key(&path);
        if seen.insert(key) {
            output.push(path);
        }
    }
    output
}

fn display_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bearer_authorization() {
        assert_eq!(
            api_key_from_authorization("Bearer test-key").as_deref(),
            Some("test-key")
        );
        assert_eq!(
            api_key_from_authorization("bearer  spaced-key  ").as_deref(),
            Some("spaced-key")
        );
        assert_eq!(api_key_from_authorization("Basic test"), None);
    }

    #[test]
    fn reads_supported_codex_auth_fields() {
        assert_eq!(
            api_key_from_codex_auth(&serde_json::json!({ "api_key": "deepseek-key" })).as_deref(),
            Some("deepseek-key")
        );
        assert_eq!(
            api_key_from_codex_auth(&serde_json::json!({ "apiKey": "camel-key" })).as_deref(),
            Some("camel-key")
        );
    }

    #[test]
    fn remembers_authorization_for_later_balance_checks() {
        remember_authorization_header("Bearer cached-key");
        assert_eq!(
            cached_authorization_api_key().as_deref(),
            Some("cached-key")
        );
    }
}
