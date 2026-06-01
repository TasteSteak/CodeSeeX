use serde_json::Value;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const API_KEY_FIELDS: &[&str] = &["OPENAI_API_KEY", "DEEPSEEK_API_KEY", "api_key", "apiKey"];

pub fn read_codex_auth_api_key() -> Option<String> {
    let path = resolve_codex_auth_path()?;
    let text = fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<Value>(&text).ok()?;
    api_key_from_codex_auth(&parsed)
}

pub fn api_key_from_codex_auth(auth: &Value) -> Option<String> {
    let object = auth.as_object()?;
    for field in API_KEY_FIELDS {
        if let Some(value) = object.get(*field).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

pub fn resolve_codex_auth_path() -> Option<PathBuf> {
    let candidates = codex_auth_path_candidates();
    candidates
        .iter()
        .find(|path| path.exists())
        .cloned()
        .or_else(|| candidates.into_iter().next())
}

pub fn codex_auth_path_candidates() -> Vec<PathBuf> {
    if let Some(explicit) = env_value("CODEX_AUTH_JSON").or_else(|| env_value("CODEX_AUTH_FILE")) {
        return vec![resolve_auth_path(&explicit)];
    }

    let mut candidates = Vec::new();
    if let Some(codex_home) = env_value("CODEX_HOME") {
        candidates.push(resolve_auth_path(&codex_home).join("auth.json"));
    }

    if let Some(home) = env_value("USERPROFILE")
        .or_else(|| env_value("HOME"))
        .or_else(|| dirs_next::home_dir().map(|path| path.to_string_lossy().into_owned()))
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
        return home_dir();
    }
    if let Some(stripped) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return home_dir().join(stripped);
    }
    let path = PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    };
    absolute.canonicalize().unwrap_or(absolute)
}

fn home_dir() -> PathBuf {
    env_value("USERPROFILE")
        .or_else(|| env_value("HOME"))
        .map(PathBuf::from)
        .or_else(dirs_next::home_dir)
        .unwrap_or_default()
}

fn env_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn unique_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for path in paths {
        let key = normalize_path_key(&path);
        if seen.insert(key) {
            output.push(path);
        }
    }
    output
}

fn normalize_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_known_auth_fields_in_order() {
        assert_eq!(
            api_key_from_codex_auth(&json!({
                "DEEPSEEK_API_KEY": "ds-key",
                "OPENAI_API_KEY": "openai-key"
            })),
            Some("openai-key".to_owned())
        );
        assert_eq!(
            api_key_from_codex_auth(&json!({ "apiKey": " custom-key " })),
            Some("custom-key".to_owned())
        );
    }
}
