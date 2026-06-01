use crate::models::{available_models, ModelInfo};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

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
    pub max_output_tokens: u64,
    pub priority: u32,
    pub available_plans: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

pub fn build_codeseex_catalog() -> Catalog {
    Catalog {
        models: available_models()
            .into_iter()
            .enumerate()
            .map(|(index, model)| catalog_model(model, index))
            .collect(),
    }
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

pub fn codex_toml_snippet(catalog_path: &Path, base_url: &str) -> String {
    [
        "model_provider = \"custom\"".to_owned(),
        "model = \"deepseek-v4-pro\"".to_owned(),
        "disable_response_storage = true".to_owned(),
        "model_reasoning_effort = \"xhigh\"".to_owned(),
        format!(
            "model_catalog_json = {}",
            toml_string(catalog_path.to_string_lossy().as_ref())
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

fn catalog_model(model: ModelInfo, index: usize) -> CatalogModel {
    let mut extra = BTreeMap::new();
    extra.insert("base_model".to_owned(), json!("gpt-5.5"));
    extra.insert("supports_reasoning".to_owned(), json!(true));
    extra.insert("supports_streaming".to_owned(), json!(true));
    extra.insert("apply_patch_tool_type".to_owned(), json!("freeform"));
    extra.insert("codeseex_next".to_owned(), json!(true));

    CatalogModel {
        slug: model.slug,
        display_name: model.display_name,
        description: model.description,
        context_window: model.context_window,
        effective_context_window_percent: model.effective_context_window_percent,
        max_output_tokens: 64_000,
        priority: 10 + index as u32,
        available_plans: default_plans(),
        extra,
    }
}

fn default_plans() -> Vec<String> {
    [
        "free",
        "plus",
        "pro",
        "team",
        "business",
        "enterprise",
        "edu",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
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
    fn toml_snippet_contains_catalog_and_proxy() {
        let snippet = codex_toml_snippet(
            Path::new("C:/Users/test/.codeseex-next/model-catalog.json"),
            "http://127.0.0.1:8787/v1",
        );
        assert!(snippet.contains("model_catalog_json"));
        assert!(snippet.contains("http://127.0.0.1:8787/v1"));
    }
}
