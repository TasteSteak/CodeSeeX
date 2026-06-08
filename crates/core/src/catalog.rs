use crate::models::{MODEL_FLASH, MODEL_PRO};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const CODEX_BRIDGED_IDENTITY: &str = "You are Codex, a coding agent based on DeepSeek-V4 and running through the local CodeSeeX proxy inside the Codex environment.";
const LEGACY_APPLY_PATCH_LINE: &str = "- For local text edits, call apply_patch with a single raw Codex patch string. The patch must start with *** Begin Patch and end with *** End Patch.";
const STRICT_APPLY_PATCH_LINE: &str = "- When creating, editing, deleting, or renaming local text files, call apply_patch with a single raw Codex patch string. Do not answer with file contents as prose instead of calling the tool. The patch must start with *** Begin Patch and end with *** End Patch.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub models: Vec<CatalogModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModel {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub context_window: u64,
    pub effective_context_window_percent: u8,
    pub priority: u32,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogSeed {
    #[serde(default)]
    common_model_fields: BTreeMap<String, Value>,
    models: Vec<CatalogModel>,
}

pub fn build_codeseex_catalog() -> Catalog {
    let mut catalog = catalog_from_seed(include_str!(concat!(
        env!("OUT_DIR"),
        "/model-catalog.seed.json"
    )))
    .expect("embedded CodeSeeX model catalog seed must be valid JSON");
    normalize_catalog_prompt_text(&mut catalog);
    catalog
}

fn catalog_from_seed(text: &str) -> serde_json::Result<Catalog> {
    let mut seed: CatalogSeed = serde_json::from_str(text)?;
    if !seed.common_model_fields.is_empty() {
        for model in &mut seed.models {
            for (key, value) in &seed.common_model_fields {
                model
                    .extra
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    Ok(Catalog {
        models: seed.models,
    })
}

pub fn write_catalog_atomic(path: &Path, catalog: &Catalog) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create catalog directory {}", parent.display()))?;
    }
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(catalog)? + "\n";
    fs::write(&temp, text).with_context(|| format!("write temp catalog {}", temp.display()))?;
    fs::rename(&temp, path).with_context(|| format!("replace catalog {}", path.display()))?;
    Ok(())
}

pub fn catalog_file_is_compatible(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    catalog_value_is_compatible(&value)
}

pub fn codex_toml_snippet(catalog_path: &Path, base_url: &str) -> String {
    [
        "model_provider = \"custom\"".to_owned(),
        "model = \"deepseek-v4-pro\"".to_owned(),
        "disable_response_storage = true".to_owned(),
        "model_reasoning_effort = \"xhigh\"".to_owned(),
        format!(
            "model_catalog_json = {}",
            toml_path_string(catalog_path.to_string_lossy().as_ref())
        ),
        "".to_owned(),
        "[model_providers.custom]".to_owned(),
        "name = \"DeepSeek\"".to_owned(),
        "wire_api = \"responses\"".to_owned(),
        "requires_openai_auth = true".to_owned(),
        format!("base_url = {}", toml_string(base_url)),
    ]
    .join("\n")
}

fn catalog_value_is_compatible(value: &Value) -> bool {
    let Some(models) = value.get("models").and_then(Value::as_array) else {
        return false;
    };
    models.len() == 2
        && [MODEL_FLASH, MODEL_PRO].into_iter().all(|slug| {
            models
                .iter()
                .find(|model| model.get("slug").and_then(Value::as_str) == Some(slug))
                .is_some_and(model_is_compatible)
        })
}

fn model_is_compatible(model: &Value) -> bool {
    prompt_fields_are_safe(model)
        && model
            .get("service_tiers")
            .and_then(Value::as_array)
            .is_some()
        && model.get("context_window").and_then(Value::as_u64) == Some(1_000_000)
        && model.get("max_context_window").and_then(Value::as_u64) == Some(1_000_000)
        && model
            .get("effective_context_window_percent")
            .and_then(Value::as_u64)
            == Some(95)
        && model.get("auto_compact_token_limit").is_none()
        && model.get("apply_patch_tool_type").and_then(Value::as_str) == Some("freeform")
        && model.get("web_search_tool_type").and_then(Value::as_str) == Some("text_and_image")
}

fn prompt_fields_are_safe(model: &Value) -> bool {
    let base_instructions = model.get("base_instructions").and_then(Value::as_str);
    let model_messages = model.get("model_messages");
    match (base_instructions, model_messages) {
        (Some(instructions), Some(messages)) => {
            let messages_text = messages.to_string();
            instructions.len() > 10_000
                && messages_text.len() > 10_000
                && instructions.contains(CODEX_BRIDGED_IDENTITY)
                && !instructions.contains(legacy_codeseex_identity().as_str())
                && messages_text.contains(CODEX_BRIDGED_IDENTITY)
                && !messages_text.contains(legacy_codeseex_identity().as_str())
                && instructions.contains("CodeSeeX Proxy Compatibility")
                && instructions.contains("*** Add File: path")
                && instructions.contains("Bare headers")
                && instructions.contains("Do not answer with file contents as prose")
                && messages_text.contains("CodeSeeX Proxy Compatibility")
                && messages_text.contains("*** Add File: path")
                && messages_text.contains("Bare headers")
                && messages_text.contains("Do not answer with file contents as prose")
        }
        _ => false,
    }
}

fn normalize_catalog_prompt_text(catalog: &mut Catalog) {
    for model in &mut catalog.models {
        for value in model.extra.values_mut() {
            normalize_prompt_value(value);
        }
    }
}

fn normalize_prompt_value(value: &mut Value) {
    match value {
        Value::String(text) => {
            *text = normalize_prompt_text(text);
        }
        Value::Array(items) => {
            for item in items {
                normalize_prompt_value(item);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                normalize_prompt_value(value);
            }
        }
        _ => {}
    }
}

fn normalize_prompt_text(text: &str) -> String {
    text.replace(legacy_codeseex_identity().as_str(), CODEX_BRIDGED_IDENTITY)
        .replace(LEGACY_APPLY_PATCH_LINE, STRICT_APPLY_PATCH_LINE)
}

fn legacy_codeseex_identity() -> String {
    [
        "You are",
        "CodeSeeX, a coding agent powered by DeepSeek-V4 through the local CodeSeeX proxy inside Codex.",
    ]
    .join(" ")
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

fn toml_path_string(value: &str) -> String {
    if value.contains(['\'', '\r', '\n']) {
        toml_string(value)
    } else {
        format!("'{value}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_both_models() {
        let catalog = build_codeseex_catalog();
        let slugs: Vec<_> = catalog
            .models
            .iter()
            .map(|model| model.slug.as_str())
            .collect();
        assert!(slugs.contains(&"deepseek-v4-flash"));
        assert!(slugs.contains(&"deepseek-v4-pro"));
    }

    #[test]
    fn compact_seed_expands_common_model_fields() {
        let catalog = catalog_from_seed(
            r#"{
              "common_model_fields": {
                "base_instructions": "shared prompt",
                "model_messages": [{"role": "system", "content": "shared"}]
              },
              "models": [
                {
                  "slug": "deepseek-v4-flash",
                  "display_name": "Flash",
                  "description": "Flash model",
                  "context_window": 1000000,
                  "effective_context_window_percent": 95,
                  "priority": 1
                },
                {
                  "slug": "deepseek-v4-pro",
                  "display_name": "Pro",
                  "description": "Pro model",
                  "context_window": 1000000,
                  "effective_context_window_percent": 95,
                  "priority": 2
                }
              ]
            }"#,
        )
        .expect("compact seed");

        for model in catalog.models {
            assert_eq!(
                model.extra.get("base_instructions").and_then(Value::as_str),
                Some("shared prompt")
            );
            assert!(model.extra.get("model_messages").is_some());
        }
    }

    #[test]
    fn catalog_preserves_codex_desktop_capability_fields() {
        let catalog = build_codeseex_catalog();
        for model in catalog.models {
            assert!(!model.extra.contains_key("base_model"));
            assert!(model
                .extra
                .get("base_instructions")
                .and_then(Value::as_str)
                .is_some_and(|value| value.len() > 10_000
                    && value.contains("CodeSeeX Proxy Compatibility")
                    && value.contains(CODEX_BRIDGED_IDENTITY)
                    && !value.contains(legacy_codeseex_identity().as_str())));
            assert!(model
                .extra
                .get("model_messages")
                .is_some_and(|value| value.to_string().len() > 10_000
                    && value.to_string().contains("CodeSeeX Proxy Compatibility")
                    && value.to_string().contains(CODEX_BRIDGED_IDENTITY)
                    && !value
                        .to_string()
                        .contains(legacy_codeseex_identity().as_str())));
            assert!(!model.extra.contains_key("id"));
            assert!(!model.extra.contains_key("model"));
            assert!(!model.extra.contains_key("displayName"));
            assert_eq!(
                model
                    .extra
                    .get("apply_patch_tool_type")
                    .and_then(Value::as_str),
                Some("freeform")
            );
            assert_eq!(
                model
                    .extra
                    .get("web_search_tool_type")
                    .and_then(Value::as_str),
                Some("text_and_image")
            );
            assert_eq!(
                model
                    .extra
                    .get("supports_search_tool")
                    .and_then(Value::as_bool),
                Some(true)
            );
            assert_eq!(
                model
                    .extra
                    .get("supports_parallel_tool_calls")
                    .and_then(Value::as_bool),
                Some(true)
            );
            assert!(model
                .extra
                .get("service_tiers")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty));
            assert_eq!(model.extra.get("auto_compact_token_limit"), None);
        }
    }

    #[test]
    fn catalog_prompts_keep_codex_identity_and_strict_apply_patch_guidance() {
        let catalog = build_codeseex_catalog();
        for model in catalog.models {
            let base = model
                .extra
                .get("base_instructions")
                .and_then(Value::as_str)
                .expect("base instructions");
            let messages = model
                .extra
                .get("model_messages")
                .expect("model messages")
                .to_string();

            assert!(base.starts_with(CODEX_BRIDGED_IDENTITY), "{base}");
            assert!(!base.contains(legacy_codeseex_identity().as_str()));
            assert!(!messages.contains(legacy_codeseex_identity().as_str()));
            assert!(base.contains(
                "When creating, editing, deleting, or renaming local text files, call apply_patch"
            ));
            assert!(messages.contains("Do not answer with file contents as prose"));
        }
    }

    #[test]
    fn generated_catalog_is_self_compatible() {
        let catalog = build_codeseex_catalog();
        let value = serde_json::to_value(catalog).expect("catalog to json");
        assert!(catalog_value_is_compatible(&value));
    }

    #[test]
    fn toml_snippet_contains_catalog_and_proxy() {
        let snippet = codex_toml_snippet(
            Path::new(r"C:\Users\test\.codeseex\model-catalog.json"),
            "http://127.0.0.1:8787/v1",
        );
        assert!(
            snippet.contains(r"model_catalog_json = 'C:\Users\test\.codeseex\model-catalog.json'")
        );
        assert!(!snippet.contains("model_context_window"));
        assert!(!snippet.contains("model_auto_compact_token_limit"));
        assert!(snippet.contains(r#"base_url = "http://127.0.0.1:8787/v1""#));
    }

    #[test]
    fn toml_snippet_accepts_release_data_dir() {
        let snippet = codex_toml_snippet(
            Path::new("C:/Users/test/.codeseex/model-catalog.json"),
            "http://127.0.0.1:8787/v1",
        );
        assert!(snippet.contains("model_catalog_json"));
        assert!(snippet.contains("http://127.0.0.1:8787/v1"));
    }
}
