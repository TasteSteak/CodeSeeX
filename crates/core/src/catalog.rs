use crate::models::{MODEL_FLASH, MODEL_PRO};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const CODEX_BRIDGED_IDENTITY: &str = "You are Codex, a coding agent based on DeepSeek-V4 and running through the local CodeSeeX proxy inside the Codex environment.";
const LEGACY_APPLY_PATCH_LINE: &str = "- For local text edits, call apply_patch with a single raw Codex patch string. The patch must start with *** Begin Patch and end with *** End Patch.";
const PREVIOUS_STRICT_APPLY_PATCH_LINE: &str = "- When creating, editing, deleting, or renaming local text files, call apply_patch with a single raw Codex patch string. Do not answer with file contents as prose instead of calling the tool. The patch must start with *** Begin Patch and end with *** End Patch.";
pub const APPLY_PATCH_SYSTEM_PROMPT_RULES: &str = concat!(
    "- When creating, editing, deleting, or renaming local text files, call apply_patch with a single raw Codex patch string. Do not answer with file contents as prose instead of calling the tool. ",
    "Use Codex native apply_patch grammar: the first line must be *** Begin Patch and the final line must be *** End Patch. ",
    "Use standalone grammar lines for structure and hunk prefixes for file data lines. Operation headers are exactly *** Add File: path, *** Update File: path, and *** Delete File: path. Bare headers such as --- a/file or +++ b/file are invalid. ",
    "For update hunks, every file data line must start with a hunk prefix: space for unchanged context, + for added lines, or - for removed lines. An empty context line is not a blank line; encode it as a single space character line. ",
    "For add-file hunks, each file content line is written as + followed by content.\n",
    "Apply patch examples:\n",
    "Update one file:\n",
    "*** Begin Patch\n",
    "*** Update File: src/lib.rs\n",
    "@@\n",
    " pub fn old_name() {}\n",
    "-pub fn broken() {}\n",
    "+pub fn fixed() {}\n",
    "*** End Patch\n\n",
    "Update with an empty unchanged line:\n",
    "*** Begin Patch\n",
    "*** Update File: src/lib.rs\n",
    "@@\n",
    " fn before() {}\n",
    " \n",
    " fn after() {}\n",
    "*** End Patch\n",
    "The blank-looking line above is a context line containing exactly one space.\n\n",
    "Edit multiple files in one patch:\n",
    "*** Begin Patch\n",
    "*** Update File: src/lib.rs\n",
    "@@\n",
    "-pub mod old;\n",
    "+pub mod new;\n",
    "*** Update File: tests/lib_test.rs\n",
    "@@\n",
    "-assert_eq!(name(), \"old\");\n",
    "+assert_eq!(name(), \"new\");\n",
    "*** End Patch\n\n",
    "Add a file:\n",
    "*** Begin Patch\n",
    "*** Add File: src/new_module.rs\n",
    "+pub fn name() -> &'static str {\n",
    "+    \"new\"\n",
    "+}\n",
    "*** End Patch\n\n",
    "Delete a file:\n",
    "*** Begin Patch\n",
    "*** Delete File: src/old_module.rs\n",
    "*** End Patch\n\n",
    "Move or rename a file:\n",
    "*** Begin Patch\n",
    "*** Update File: src/old_name.rs\n",
    "*** Move to: src/new_name.rs\n",
    "*** End Patch"
);
pub const APPLY_PATCH_TOOL_PARAMETER_DESCRIPTION: &str = concat!(
    "One complete raw apply_patch document. The first line must be *** Begin Patch and the final line must be *** End Patch. ",
    "Use standalone grammar lines for patch structure and hunk-prefixed data lines for file content. ",
    "Operation headers are *** Add File: path, *** Update File: path, and *** Delete File: path; Bare headers such as --- a/file or +++ b/file are invalid, and do not use bare headers. ",
    "For *** Update File: path, use @@ hunks. Every hunk file data line must start with exactly one hunk prefix: space for unchanged context, + for an added line, or - for a removed line. Encode an empty context line as a line containing a single space, never as a truly blank line. ",
    "For *** Add File: path, each file content line is encoded as + followed by content. Omit content hunks for deletes. Standard unified hunk headers are accepted and normalized to native Codex @@ headers."
);
const STRICT_APPLY_PATCH_LINE: &str = APPLY_PATCH_SYSTEM_PROMPT_RULES;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppServerModelListParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub include_hidden: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppServerModelListResponse {
    pub data: Vec<AppServerModel>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppServerModel {
    pub id: String,
    pub model: String,
    pub upgrade: Option<String>,
    pub upgrade_info: Option<AppServerModelUpgradeInfo>,
    pub availability_nux: Option<AppServerModelAvailabilityNux>,
    pub display_name: String,
    pub short_display_name: Option<String>,
    pub description: String,
    pub hidden: bool,
    pub supported_reasoning_efforts: Vec<AppServerReasoningEffortOption>,
    pub default_reasoning_effort: String,
    pub input_modalities: Vec<String>,
    pub supports_personality: bool,
    pub additional_speed_tiers: Vec<String>,
    pub service_tiers: Vec<AppServerModelServiceTier>,
    pub default_service_tier: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppServerReasoningEffortOption {
    pub reasoning_effort: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppServerModelUpgradeInfo {
    pub model: String,
    pub upgrade_copy: Option<String>,
    pub model_link: Option<String>,
    pub migration_markdown: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppServerModelAvailabilityNux {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppServerModelServiceTier {
    pub id: String,
    pub name: String,
    pub description: String,
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

pub fn app_server_model_list(params: AppServerModelListParams) -> AppServerModelListResponse {
    app_server_model_list_from_catalog(&build_codeseex_catalog(), params)
}

pub fn app_server_model_list_from_catalog(
    catalog: &Catalog,
    params: AppServerModelListParams,
) -> AppServerModelListResponse {
    let include_hidden = params.include_hidden.unwrap_or(false);
    let start = params
        .cursor
        .as_deref()
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = params
        .limit
        .filter(|value| *value > 0)
        .map(|value| value as usize)
        .unwrap_or(50);
    let any_explicit_default = catalog.models.iter().any(catalog_model_is_default);
    let models = catalog
        .models
        .iter()
        .filter(|model| include_hidden || !catalog_model_is_hidden(model))
        .map(|model| app_server_model_from_catalog_model(model, any_explicit_default))
        .collect::<Vec<_>>();
    let end = if limit == 0 {
        start.min(models.len())
    } else {
        start.saturating_add(limit).min(models.len())
    };
    let data = models
        .get(start.min(models.len())..end)
        .unwrap_or_default()
        .to_vec();
    let next_cursor = (end < models.len()).then(|| end.to_string());

    AppServerModelListResponse { data, next_cursor }
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

fn catalog_model_is_hidden(model: &CatalogModel) -> bool {
    match model.extra.get("hidden").and_then(Value::as_bool) {
        Some(hidden) => hidden,
        None => matches!(
            model.extra.get("visibility").and_then(Value::as_str),
            Some("hidden")
        ),
    }
}

fn catalog_model_is_default(model: &CatalogModel) -> bool {
    model
        .extra
        .get("is_default")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn string_extra(extra: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    extra
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn bool_extra(extra: &BTreeMap<String, Value>, key: &str) -> Option<bool> {
    extra.get(key).and_then(Value::as_bool)
}

fn string_array_extra(extra: &BTreeMap<String, Value>, key: &str) -> Option<Vec<String>> {
    Some(
        extra
            .get(key)?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

fn supported_reasoning_efforts(model: &CatalogModel) -> Vec<AppServerReasoningEffortOption> {
    let efforts = model
        .extra
        .get("supported_reasoning_efforts")
        .or_else(|| model.extra.get("supported_reasoning_levels"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(reasoning_effort_option)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if efforts.is_empty() {
        vec![AppServerReasoningEffortOption {
            reasoning_effort: string_extra(&model.extra, "default_reasoning_level")
                .unwrap_or_else(|| "medium".to_owned()),
            description: "Default reasoning effort".to_owned(),
        }]
    } else {
        efforts
    }
}

fn reasoning_effort_option(value: &Value) -> Option<AppServerReasoningEffortOption> {
    let object = value.as_object()?;
    let effort = object
        .get("reasoningEffort")
        .or_else(|| object.get("reasoning_effort"))
        .or_else(|| object.get("effort"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(AppServerReasoningEffortOption {
        reasoning_effort: effort.to_owned(),
        description: description.to_owned(),
    })
}

fn model_upgrade_info(value: Option<&Value>) -> Option<AppServerModelUpgradeInfo> {
    let value = value?;
    if let Some(model) = value.as_str().filter(|value| !value.trim().is_empty()) {
        return Some(AppServerModelUpgradeInfo {
            model: model.to_owned(),
            upgrade_copy: None,
            model_link: None,
            migration_markdown: None,
        });
    }
    let object = value.as_object()?;
    let model = object
        .get("model")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    Some(AppServerModelUpgradeInfo {
        model: model.to_owned(),
        upgrade_copy: optional_string(
            object
                .get("upgradeCopy")
                .or_else(|| object.get("upgrade_copy")),
        ),
        model_link: optional_string(object.get("modelLink").or_else(|| object.get("model_link"))),
        migration_markdown: optional_string(
            object
                .get("migrationMarkdown")
                .or_else(|| object.get("migration_markdown")),
        ),
    })
}

fn availability_nux(value: Option<&Value>) -> Option<AppServerModelAvailabilityNux> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    let message = value
        .as_object()
        .and_then(|object| object.get("message"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    Some(AppServerModelAvailabilityNux {
        message: message.to_owned(),
    })
}

fn service_tiers(value: Option<&Value>) -> Vec<AppServerModelServiceTier> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let object = item.as_object()?;
                    Some(AppServerModelServiceTier {
                        id: required_string(object.get("id"))?,
                        name: required_string(object.get("name"))?,
                        description: required_string(object.get("description"))?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn required_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    required_string(value)
}

fn app_server_display_name(model: &CatalogModel) -> String {
    model.display_name.replace("DeepSeek-V4", "DeepSeek V4")
}

fn app_server_short_display_name(model: &CatalogModel) -> Option<String> {
    string_extra(&model.extra, "short_display_name")
        .or_else(|| string_extra(&model.extra, "shortDisplayName"))
        .or_else(|| match model.slug.as_str() {
            MODEL_FLASH => Some("Flash".to_owned()),
            MODEL_PRO => Some("Pro".to_owned()),
            _ => app_server_display_name(model)
                .strip_prefix("DeepSeek V4 ")
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned),
        })
}

fn app_server_model_from_catalog_model(
    model: &CatalogModel,
    any_explicit_default: bool,
) -> AppServerModel {
    let upgrade_info = model_upgrade_info(model.extra.get("upgrade"));
    let upgrade = upgrade_info.as_ref().map(|value| value.model.clone());
    AppServerModel {
        id: model.slug.clone(),
        model: model.slug.clone(),
        upgrade,
        upgrade_info,
        availability_nux: availability_nux(model.extra.get("availability_nux")),
        display_name: app_server_display_name(model),
        short_display_name: app_server_short_display_name(model),
        description: model.description.clone(),
        hidden: catalog_model_is_hidden(model),
        supported_reasoning_efforts: supported_reasoning_efforts(model),
        default_reasoning_effort: string_extra(&model.extra, "default_reasoning_level")
            .unwrap_or_else(|| "medium".to_owned()),
        input_modalities: string_array_extra(&model.extra, "input_modalities")
            .filter(|values| !values.is_empty())
            .unwrap_or_else(|| vec!["text".to_owned(), "image".to_owned()]),
        supports_personality: bool_extra(&model.extra, "supports_personality").unwrap_or(false),
        additional_speed_tiers: string_array_extra(&model.extra, "additional_speed_tiers")
            .unwrap_or_default(),
        service_tiers: service_tiers(model.extra.get("service_tiers")),
        default_service_tier: string_extra(&model.extra, "default_service_tier"),
        is_default: if any_explicit_default {
            catalog_model_is_default(model)
        } else {
            model.slug == MODEL_PRO
        },
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
                && instructions.contains("standalone grammar lines")
                && instructions.contains("hunk prefixes for file data lines")
                && messages_text.contains("CodeSeeX Proxy Compatibility")
                && messages_text.contains("*** Add File: path")
                && messages_text.contains("Bare headers")
                && messages_text.contains("Do not answer with file contents as prose")
                && messages_text.contains("standalone grammar lines")
                && messages_text.contains("hunk prefixes for file data lines")
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
        .replace(PREVIOUS_STRICT_APPLY_PATCH_LINE, STRICT_APPLY_PATCH_LINE)
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
            assert!(base.contains("first line must be *** Begin Patch"));
            assert!(base.contains("final line must be *** End Patch"));
            assert!(base.contains("empty context line is not a blank line"));
            assert!(base.contains("single space character line"));
            assert!(base.contains("Apply patch examples:"));
            assert!(base.contains("Edit multiple files in one patch:"));
            assert!(base.contains("*** Move to: src/new_name.rs"));
            assert!(messages.contains("Do not answer with file contents as prose"));
            assert!(messages.contains("empty context line is not a blank line"));
            assert!(messages.contains("single space character line"));
            assert!(messages.contains("Edit multiple files in one patch:"));
            assert!(messages.contains("*** Move to: src/new_name.rs"));
        }
    }

    #[test]
    fn generated_catalog_is_self_compatible() {
        let catalog = build_codeseex_catalog();
        let value = serde_json::to_value(catalog).expect("catalog to json");
        assert!(catalog_value_is_compatible(&value));
    }

    #[test]
    fn app_server_model_list_uses_catalog_metadata() {
        let response = app_server_model_list(AppServerModelListParams::default());

        assert_eq!(response.next_cursor, None);
        assert_eq!(response.data.len(), 2);
        let pro = response
            .data
            .iter()
            .find(|model| model.id == MODEL_PRO)
            .expect("pro model");
        assert_eq!(pro.model, MODEL_PRO);
        assert_eq!(pro.display_name, "DeepSeek V4 Pro");
        assert_eq!(pro.short_display_name.as_deref(), Some("Pro"));
        assert!(!pro.hidden);
        assert!(pro.is_default);
        assert_eq!(pro.default_reasoning_effort, "medium");
        assert_eq!(pro.input_modalities, vec!["text", "image"]);
        assert_eq!(
            pro.supported_reasoning_efforts
                .iter()
                .map(|effort| effort.reasoning_effort.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high", "xhigh"]
        );
        assert_eq!(pro.additional_speed_tiers, vec!["fast"]);
        assert!(pro.service_tiers.is_empty());
        assert_eq!(pro.upgrade, None);
        assert_eq!(pro.availability_nux, None);

        let flash = response
            .data
            .iter()
            .find(|model| model.id == MODEL_FLASH)
            .expect("flash model");
        assert_eq!(flash.display_name, "DeepSeek V4 Flash");
        assert_eq!(flash.short_display_name.as_deref(), Some("Flash"));
    }

    #[test]
    fn app_server_model_list_filters_hidden_and_paginates() {
        let mut catalog = build_codeseex_catalog();
        catalog.models[0]
            .extra
            .insert("visibility".to_owned(), Value::String("hidden".to_owned()));

        let visible = app_server_model_list_from_catalog(
            &catalog,
            AppServerModelListParams {
                include_hidden: Some(false),
                ..Default::default()
            },
        );
        assert_eq!(visible.data.len(), 1);
        assert_eq!(visible.data[0].id, MODEL_PRO);

        let first_page = app_server_model_list_from_catalog(
            &catalog,
            AppServerModelListParams {
                include_hidden: Some(true),
                limit: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(first_page.data.len(), 1);
        assert_eq!(first_page.next_cursor, Some("1".to_owned()));

        let second_page = app_server_model_list_from_catalog(
            &catalog,
            AppServerModelListParams {
                include_hidden: Some(true),
                cursor: first_page.next_cursor,
                limit: Some(1),
            },
        );
        assert_eq!(second_page.data.len(), 1);
        assert_eq!(second_page.next_cursor, None);
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
