use crate::models::MODEL_PRO;
use crate::pricing::PricingTable;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

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
    build_codeseex_catalog_from_document(&embedded_catalog_document())
}

pub fn build_codeseex_catalog_from_document(document: &CatalogDocument) -> Catalog {
    let mut catalog = catalog_from_seed_and_document(
        include_str!(concat!(env!("OUT_DIR"), "/model-catalog.seed.json")),
        document,
    )
    .expect("embedded CodeSeeX model catalog seed must be valid JSON");
    normalize_catalog_prompt_text(&mut catalog);
    catalog
}

pub fn app_server_model_list(params: AppServerModelListParams) -> AppServerModelListResponse {
    app_server_model_list_from_catalog(&build_codeseex_catalog(), params)
}

/// Model list for the currently active catalog document.
pub fn app_server_model_list_for_document(
    document: &CatalogDocument,
    params: AppServerModelListParams,
) -> AppServerModelListResponse {
    app_server_model_list_from_catalog(&build_codeseex_catalog_from_document(document), params)
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

/// Combines the private compile-time seed (prompt material) with the public
/// catalog document. The document is authoritative for the model set and for
/// every field it declares; the seed only fills gaps so private prompt fields
/// survive remote catalog updates.
fn catalog_from_seed_and_document(
    text: &str,
    document: &CatalogDocument,
) -> serde_json::Result<Catalog> {
    let seed: CatalogSeed = serde_json::from_str(text)?;
    if document.models.is_empty() {
        let mut fallback = Catalog { models: seed.models };
        apply_common_model_fields(&mut fallback, &seed.common_model_fields);
        return Ok(fallback);
    }
    let mut models = Vec::with_capacity(document.models.len());
    for document_model in &document.models {
        let mut model = document_model.clone();
        if let Some(seed_model) = seed
            .models
            .iter()
            .find(|candidate| candidate.slug == model.slug)
        {
            for (key, value) in &seed_model.extra {
                merge_seed_field(&mut model, key, value);
            }
        }
        models.push(model);
    }
    let mut catalog = Catalog { models };
    apply_common_model_fields(&mut catalog, &seed.common_model_fields);
    Ok(catalog)
}

fn apply_common_model_fields(catalog: &mut Catalog, fields: &BTreeMap<String, Value>) {
    if fields.is_empty() {
        return;
    }
    for model in &mut catalog.models {
        for (key, value) in fields {
            merge_seed_field(model, key, value);
        }
    }
}

/// Prompt material always comes from the private seed; every other field lets
/// the document win and only falls back to the seed.
fn merge_seed_field(model: &mut CatalogModel, key: &str, value: &Value) {
    if CATALOG_PROMPT_FIELDS.contains(&key) {
        model.extra.insert(key.to_owned(), value.clone());
        return;
    }
    model.extra.entry(key.to_owned()).or_insert_with(|| value.clone());
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
        .or_else(|| {
            app_server_display_name(model)
                .strip_prefix("DeepSeek V4 ")
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
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
    codex_toml_snippet_for_document(&embedded_catalog_document(), catalog_path, base_url)
}

pub fn codex_toml_snippet_for_document(
    document: &CatalogDocument,
    catalog_path: &Path,
    base_url: &str,
) -> String {
    [
        "model_provider = \"custom\"".to_owned(),
        format!("model = {}", toml_string(document.default_slug())),
        "disable_response_storage = true".to_owned(),
        "model_reasoning_effort = \"xhigh\"".to_owned(),
        format!(
            "model_catalog_json = {}",
            toml_path_string(catalog_path.to_string_lossy().as_ref())
        ),
        "".to_owned(),
        "[model_providers.custom]".to_owned(),
        format!("name = {}", toml_string(&document.provider_name)),
        "wire_api = \"responses\"".to_owned(),
        "requires_openai_auth = true".to_owned(),
        format!("base_url = {}", toml_string(base_url)),
    ]
    .join("\n")
}

/// The on-disk catalog only has to be structurally usable: any model set that
/// still carries a default and satisfies the Codex contract is accepted, so a
/// data-driven catalog is never fought over by the writer.
fn catalog_value_is_compatible(value: &Value) -> bool {
    let Some(models) = value.get("models").and_then(Value::as_array) else {
        return false;
    };
    !models.is_empty()
        && models
            .iter()
            .any(|model| model.get("is_default").and_then(Value::as_bool) == Some(true))
        && models.iter().all(model_is_compatible)
}

fn model_is_compatible(model: &Value) -> bool {
    prompt_fields_are_safe(model)
        && model
            .get("service_tiers")
            .and_then(Value::as_array)
            .is_some()
        && model
            .get("context_window")
            .and_then(Value::as_u64)
            .is_some_and(|value| value > 0)
        && model
            .get("max_context_window")
            .and_then(Value::as_u64)
            .is_some_and(|value| value > 0)
        && model
            .get("effective_context_window_percent")
            .and_then(Value::as_u64)
            .is_some_and(|value| (1..=100).contains(&value))
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

// ---------------------------------------------------------------------------
// Catalog document (data-driven models + pricing)
// ---------------------------------------------------------------------------

pub const SUPPORTED_CATALOG_SCHEMA_VERSION: u32 = 1;
/// Fields that may only ever come from the private, compile-time seed. A
/// catalog document - remote, cached or user-written - that declares one of
/// them would otherwise take over the shipped prompt material, so the parser
/// rejects it outright instead of merging it.
pub const CATALOG_PROMPT_FIELDS: [&str; 2] = ["base_instructions", "model_messages"];
/// Remote catalog documents larger than this are rejected outright.
pub const MAX_CATALOG_DOCUMENT_BYTES: usize = 256 * 1024;
const EMBEDDED_CATALOG_JSON: &str = include_str!("../assets/catalog.default.json");

static EMBEDDED_CATALOG: OnceLock<CatalogDocument> = OnceLock::new();

/// A complete, self-describing catalog: the model set plus the pricing table.
///
/// Layer 0 (embedded) always exists; remote and cached documents use the same
/// shape and are merged on top of it.
#[derive(Debug, Clone)]
pub struct CatalogDocument {
    pub schema_version: u32,
    pub revision: String,
    /// Publication stamp of the remote manifest. Optional: the built-in
    /// document has none, and the cache keeps whatever the remote declared.
    pub issued_at: Option<String>,
    pub provider_name: String,
    pub default_model: String,
    pub min_app_version: Option<String>,
    pub models: Vec<CatalogModel>,
    pub pricing: PricingTable,
}

impl CatalogDocument {
    pub fn from_json(text: &str) -> Result<Self, String> {
        if text.len() > MAX_CATALOG_DOCUMENT_BYTES {
            return Err(format!(
                "catalog document exceeds {MAX_CATALOG_DOCUMENT_BYTES} bytes"
            ));
        }
        let value: Value = serde_json::from_str(text)
            .map_err(|error| format!("catalog document is not valid JSON: {error}"))?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "catalog document must be an object".to_owned())?;
        let schema_version = object
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| "catalog document is missing schema_version".to_owned())?;
        if schema_version != SUPPORTED_CATALOG_SCHEMA_VERSION as u64 {
            return Err(format!(
                "unsupported catalog schema_version {schema_version}; expected {SUPPORTED_CATALOG_SCHEMA_VERSION}"
            ));
        }
        let revision = object
            .get("revision")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "catalog document needs a non-empty revision".to_owned())?
            .to_owned();
        let provider_name = object
            .get("provider_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("CodeSeeX")
            .to_owned();
        let issued_at = object
            .get("issued_at")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let min_app_version = object
            .get("min_app_version")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if let Some(required) = min_app_version.as_deref() {
            let current = env!("CARGO_PKG_VERSION");
            if !version_at_least(current, required) {
                return Err(format!(
                    "catalog document requires CodeSeeX {required} or newer (current {current})"
                ));
            }
        }
        let default_model = object
            .get("default_model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_owned();
        let model_values = object
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| "catalog document is missing a models array".to_owned())?;
        if model_values.is_empty() {
            return Err("catalog document must declare at least one model".to_owned());
        }
        let mut models = Vec::with_capacity(model_values.len());
        let mut seen = BTreeSet::new();
        for model_value in model_values {
            let model: CatalogModel = serde_json::from_value(model_value.clone())
                .map_err(|error| format!("catalog model is invalid: {error}"))?;
            validate_catalog_model(&model)?;
            if !seen.insert(model.slug.clone()) {
                return Err(format!("duplicate model slug: {}", model.slug));
            }
            models.push(model);
        }
        let has_explicit_default = models.iter().any(|model| {
            model.extra.get("is_default").and_then(Value::as_bool) == Some(true)
        });
        if !has_explicit_default
            && !models.iter().any(|model| model.slug == default_model)
        {
            return Err("catalog document must declare a default model".to_owned());
        }
        let pricing = match object.get("pricing") {
            Some(value) => PricingTable::from_value(value)?,
            None => PricingTable::default(),
        };
        Ok(Self {
            schema_version: SUPPORTED_CATALOG_SCHEMA_VERSION,
            revision,
            issued_at,
            provider_name,
            default_model,
            min_app_version,
            models,
            pricing,
        })
    }

    pub fn to_value(&self) -> Value {
        let models = self
            .models
            .iter()
            .map(|model| serde_json::to_value(model).unwrap_or(Value::Null))
            .collect::<Vec<_>>();
        json!({
            "schema_version": self.schema_version,
            "revision": self.revision,
            "issued_at": self.issued_at,
            "provider_name": self.provider_name,
            "default_model": self.default_model,
            "min_app_version": self.min_app_version,
            "models": models,
            "pricing": self.pricing.to_value(),
        })
    }

    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(&self.to_value())
            .unwrap_or_else(|_| "{}".to_owned());
        text.push('\n');
        text
    }

    pub fn model(&self, slug: &str) -> Option<&CatalogModel> {
        let slug = slug.trim();
        self.models
            .iter()
            .find(|model| model.slug == slug)
            .or_else(|| {
                self.models.iter().find(|model| {
                    model
                        .extra
                        .get("aliases")
                        .and_then(Value::as_array)
                        .is_some_and(|aliases| {
                            aliases
                                .iter()
                                .filter_map(Value::as_str)
                                .any(|alias| alias.eq_ignore_ascii_case(slug))
                        })
                })
            })
    }

    pub fn default_slug(&self) -> &str {
        if self.default_model.trim().is_empty() {
            self.models
                .iter()
                .find(|model| {
                    model.extra.get("is_default").and_then(Value::as_bool) == Some(true)
                })
                .or_else(|| self.models.first())
                .map(|model| model.slug.as_str())
                .unwrap_or(MODEL_PRO)
        } else {
            self.default_model.as_str()
        }
    }

    /// Resolves an inbound model request (slug, alias or alias pattern).
    pub fn model_for_request(&self, requested: &str) -> Option<&CatalogModel> {
        let requested = requested.trim();
        if requested.is_empty() {
            return None;
        }
        if let Some(model) = self
            .models
            .iter()
            .find(|model| model.slug.eq_ignore_ascii_case(requested))
        {
            return Some(model);
        }
        if let Some(model) = self.models.iter().find(|model| {
            model
                .aliases()
                .any(|alias| alias.eq_ignore_ascii_case(requested))
        }) {
            return Some(model);
        }
        self.models.iter().find(|model| {
            model
                .alias_patterns()
                .any(|pattern| alias_pattern_matches(pattern, requested))
        })
    }

    pub fn pricing_group_for(&self, slug: &str) -> Option<String> {
        self.model(slug).and_then(|model| {
            model
                .extra
                .get("pricing_group")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
    }

    pub fn upstream_slug_for(&self, requested: &str) -> String {
        let requested = requested.trim();
        if requested.is_empty() {
            return self.default_slug().to_owned();
        }
        match self.model_for_request(requested) {
            Some(model) => model.upstream_slug_or_slug(),
            None => requested.to_owned(),
        }
    }

    /// Merges a full catalog document on top of this one. The overlay is
    /// authoritative for the model set; missing fields fall back to the base
    /// entry with the same slug.
    pub fn merge_authoritative(&self, overlay: &CatalogDocument) -> CatalogDocument {
        let mut models = Vec::with_capacity(overlay.models.len());
        for overlay_model in &overlay.models {
            let mut merged = overlay_model.clone();
            if let Some(base_model) = self.model(&overlay_model.slug) {
                for (key, value) in &base_model.extra {
                    merged
                        .extra
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
            }
            models.push(merged);
        }
        let overlay_pricing_is_empty =
            overlay.pricing.rates.is_empty() && overlay.pricing.groups.is_empty();
        CatalogDocument {
            schema_version: overlay.schema_version,
            revision: overlay.revision.clone(),
            issued_at: overlay
                .issued_at
                .clone()
                .or_else(|| self.issued_at.clone()),
            provider_name: if overlay.provider_name.trim().is_empty() {
                self.provider_name.clone()
            } else {
                overlay.provider_name.clone()
            },
            default_model: if overlay.default_model.trim().is_empty() {
                self.default_model.clone()
            } else {
                overlay.default_model.clone()
            },
            min_app_version: overlay.min_app_version.clone(),
            models,
            pricing: if overlay_pricing_is_empty {
                self.pricing.clone()
            } else {
                overlay.pricing.clone()
            },
        }
    }
}

impl CatalogModel {
    pub fn aliases(&self) -> impl Iterator<Item = &str> {
        self.extra
            .get("aliases")
            .and_then(Value::as_array)
            .map(|aliases| aliases.iter().filter_map(Value::as_str))
            .into_iter()
            .flatten()
    }

    pub fn alias_patterns(&self) -> impl Iterator<Item = &str> {
        self.extra
            .get("alias_patterns")
            .and_then(Value::as_array)
            .map(|patterns| patterns.iter().filter_map(Value::as_str))
            .into_iter()
            .flatten()
    }

    pub fn upstream_slug_or_slug(&self) -> String {
        self.extra
            .get("upstream_slug")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.slug.as_str())
            .to_owned()
    }

    pub fn pricing_group(&self) -> Option<String> {
        self.extra
            .get("pricing_group")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }
}

/// Case-insensitive wildcard match supporting a leading or trailing `*`.
fn alias_pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    let value = value.trim().to_ascii_lowercase();
    if pattern.is_empty() {
        return false;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return value.ends_with(suffix);
    }
    pattern == value
}

/// Outbound model slug for a request, resolved against the active catalog.
///
/// A pinned model wins over what the client asked for, but both go through the
/// same catalog lookup, so a pin follows the catalog's `upstream_slug` instead
/// of hard-coding one. The historical `gpt-5*` fallback stays for names the
/// catalog does not know.
pub fn resolve_upstream_slug(config: &crate::config::AppConfig, requested: &str) -> String {
    let document = config.catalog_document();
    let requested = config
        .model_override
        .pinned_slug()
        .unwrap_or(requested)
        .trim();
    // "Unspecified" always means the catalog's own default model, the single
    // rule `CatalogDocument::upstream_slug_for` applies. Falling back to the
    // hard-coded pro slug here would disagree whenever the catalog ships a
    // different default.
    if requested.is_empty() {
        return document.default_slug().to_owned();
    }
    match document.model_for_request(requested) {
        Some(model) => model.upstream_slug_or_slug(),
        None => config.model_override.upstream_slug(requested),
    }
}

pub fn embedded_catalog_document() -> CatalogDocument {
    EMBEDDED_CATALOG
        .get_or_init(|| {
            CatalogDocument::from_json(EMBEDDED_CATALOG_JSON)
                .expect("embedded CodeSeeX catalog document must be valid")
        })
        .clone()
}

fn validate_catalog_model(model: &CatalogModel) -> Result<(), String> {
    let slug = model.slug.trim();
    if slug.is_empty() {
        return Err("catalog model slug must not be empty".to_owned());
    }
    if !slug
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(format!("catalog model slug has unsupported characters: {slug}"));
    }
    for key in CATALOG_PROMPT_FIELDS {
        if model.extra.contains_key(key) {
            return Err(format!(
                "catalog model {slug} must not declare {key}; prompt material is compiled into CodeSeeX"
            ));
        }
    }
    if model.display_name.trim().is_empty() {
        return Err(format!("catalog model {slug} needs a display_name"));
    }
    if model.context_window == 0 {
        return Err(format!("catalog model {slug} needs a positive context_window"));
    }
    if !(1..=100).contains(&model.effective_context_window_percent) {
        return Err(format!(
            "catalog model {slug} needs an effective_context_window_percent between 1 and 100"
        ));
    }
    if let Some(max) = model.extra.get("max_context_window").and_then(Value::as_u64) {
        if max == 0 {
            return Err(format!("catalog model {slug} needs a positive max_context_window"));
        }
    }
    for key in ["aliases", "alias_patterns"] {
        if let Some(values) = model.extra.get(key) {
            let values = values
                .as_array()
                .ok_or_else(|| format!("catalog model {slug} {key} must be an array"))?;
            if values.iter().any(|value| {
                value
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
            }) {
                return Err(format!("catalog model {slug} has an empty {key} entry"));
            }
        }
    }
    if let Some(aliases) = model.extra.get("aliases") {
        let aliases = aliases
            .as_array()
            .ok_or_else(|| format!("catalog model {slug} aliases must be an array"))?;
        if aliases.iter().any(|alias| {
            alias
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
        }) {
            return Err(format!("catalog model {slug} has an empty alias"));
        }
    }
    Ok(())
}

/// Numeric dotted version comparison; a missing segment counts as zero.
pub fn version_at_least(current: &str, required: &str) -> bool {
    fn segments(value: &str) -> Vec<u64> {
        value
            .trim()
            .trim_start_matches('v')
            .split(['.', '-', '+'])
            .map(|segment| segment.trim().parse::<u64>().unwrap_or(0))
            .collect()
    }
    let current = segments(current);
    let required = segments(required);
    for index in 0..current.len().max(required.len()) {
        let left = current.get(index).copied().unwrap_or(0);
        let right = required.get(index).copied().unwrap_or(0);
        if left != right {
            return left > right;
        }
    }
    true
}

pub fn read_cached_catalog_document(path: &Path) -> Option<CatalogDocument> {
    let text = fs::read_to_string(path).ok()?;
    CatalogDocument::from_json(&text).ok()
}

pub fn write_cached_catalog_document(path: &Path, document: &CatalogDocument) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create catalog cache directory {}", parent.display()))?;
    }
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, document.to_json())
        .with_context(|| format!("write temp catalog cache {}", temp.display()))?;
    fs::rename(&temp, path).with_context(|| format!("replace catalog cache {}", path.display()))?;
    Ok(())
}

/// Applies user overrides (layer 3). Overrides patch models and pricing; they
/// never remove a model.
pub fn apply_catalog_overrides(
    document: &CatalogDocument,
    overrides: &crate::config::CatalogOverrides,
) -> CatalogDocument {
    if overrides.is_empty() {
        return document.clone();
    }
    let mut next = document.clone();
    for (slug, model_override) in &overrides.models {
        match next.models.iter_mut().find(|model| &model.slug == slug) {
            Some(model) => model_override.apply_to(model),
            None => {
                if let Some(model) = model_override.to_catalog_model(slug) {
                    next.models.push(model);
                }
            }
        }
    }
    if let Some(pricing) = overrides.pricing.as_ref() {
        if let Ok(updated) = next.pricing.with_override_value(pricing) {
            next.pricing = updated;
        }
    }
    if !next.revision.ends_with("+user") {
        next.revision = format!("{}+user", next.revision);
    }
    next
}

impl crate::config::CatalogModelOverride {
    fn apply_to(&self, model: &mut CatalogModel) {
        if let Some(display_name) = self.display_name.as_deref() {
            model.display_name = display_name.to_owned();
        }
        if let Some(description) = self.description.as_deref() {
            model.description = description.to_owned();
        }
        if let Some(context_window) = self.context_window {
            model.context_window = context_window;
        }
        if let Some(percent) = self.effective_context_window_percent {
            model.effective_context_window_percent = percent;
        }
        if let Some(short) = self.short_display_name.as_deref() {
            model
                .extra
                .insert("short_display_name".to_owned(), json!(short));
        }
        if let Some(upstream_slug) = self.upstream_slug.as_deref() {
            model
                .extra
                .insert("upstream_slug".to_owned(), json!(upstream_slug));
        }
        if let Some(group) = self.pricing_group.as_deref() {
            model
                .extra
                .insert("pricing_group".to_owned(), json!(group));
        }
        if let Some(aliases) = self.aliases.as_ref() {
            model.extra.insert("aliases".to_owned(), json!(aliases));
        }
        if let Some(is_default) = self.is_default {
            model.extra.insert("is_default".to_owned(), json!(is_default));
        }
        if let Some(hidden) = self.hidden {
            model.extra.insert("hidden".to_owned(), json!(hidden));
        }
    }

    fn to_catalog_model(&self, slug: &str) -> Option<CatalogModel> {
        if self.display_name.is_none() && self.description.is_none() {
            return None;
        }
        let embedded = embedded_catalog_document();
        let mut model = embedded
            .models
            .first()
            .cloned()
            .unwrap_or_else(|| CatalogModel {
                slug: slug.to_owned(),
                display_name: slug.to_owned(),
                description: String::new(),
                context_window: 1_000_000,
                effective_context_window_percent: 95,
                priority: 2,
                extra: BTreeMap::new(),
            });
        model.slug = slug.to_owned();
        model.display_name = self
            .display_name
            .clone()
            .unwrap_or_else(|| slug.to_owned());
        model.description = self.description.clone().unwrap_or_default();
        self.apply_to(&mut model);
        Some(model)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A pin follows the catalog's `upstream_slug` instead of hard-coding the
    /// built-in DeepSeek slugs, and an unknown pin is forwarded as-is.
    #[test]
    fn an_upstream_pin_resolves_through_the_catalog() {
        let mut config = crate::config::AppConfig::default();
        config.catalog_overrides.models.insert(
            "deepseek-flash".to_owned(),
            crate::config::CatalogModelOverride {
                upstream_slug: Some("upstream-flash".to_owned()),
                ..Default::default()
            },
        );

        config.model_override =
            crate::models::UpstreamModelOverride::Custom("deepseek-flash".to_owned());
        assert_eq!(
            resolve_upstream_slug(&config, crate::models::MODEL_PRO),
            "upstream-flash"
        );

        config.model_override =
            crate::models::UpstreamModelOverride::Custom("not-in-catalog".to_owned());
        assert_eq!(
            resolve_upstream_slug(&config, crate::models::MODEL_PRO),
            "not-in-catalog"
        );

        config.model_override = crate::models::UpstreamModelOverride::Default;
        assert_eq!(
            resolve_upstream_slug(&config, crate::models::MODEL_PRO),
            crate::models::MODEL_PRO
        );
    }

    /// An empty or blank request always resolves to whatever the catalog
    /// declares as its default model. Both entry points must agree, which was
    /// not the case while one of them hard-coded the pro slug.
    #[test]
    fn an_empty_request_resolves_to_the_catalog_default_model() {
        let mut overlay = embedded_catalog_document();
        overlay.default_model = crate::models::MODEL_FLASH.to_owned();
        let config = crate::config::AppConfig {
            catalog_remote: Some(std::sync::Arc::new(overlay)),
            ..crate::config::AppConfig::default()
        };

        let document = config.catalog_document();
        assert_eq!(document.default_slug(), crate::models::MODEL_FLASH);
        assert_eq!(
            resolve_upstream_slug(&config, ""),
            document.upstream_slug_for("")
        );
        assert_eq!(
            resolve_upstream_slug(&config, "   "),
            document.upstream_slug_for("   ")
        );
        assert_eq!(
            resolve_upstream_slug(&config, ""),
            crate::models::MODEL_FLASH
        );
    }

    #[test]
    fn catalog_contains_both_models() {
        let catalog = build_codeseex_catalog();
        let slugs: Vec<_> = catalog
            .models
            .iter()
            .map(|model| model.slug.as_str())
            .collect();
        assert!(slugs.contains(&"deepseek-flash"));
        assert!(slugs.contains(&"deepseek-v4-pro"));
    }

    /// The names DeepSeek retired are still accepted by the API (billed as
    /// Flash), so a client that asks for one keeps resolving to that model.
    #[test]
    fn retired_model_names_still_resolve() {
        let document = embedded_catalog_document();
        for requested in ["deepseek-v4-flash", "deepseek-v4-flash-vision-exp"] {
            let model = document
                .model_for_request(requested)
                .unwrap_or_else(|| panic!("{requested} must resolve"));
            assert_eq!(model.slug, "deepseek-flash");
            assert_eq!(model.upstream_slug_or_slug(), "deepseek-v4-flash");
        }
        assert_eq!(
            document.upstream_slug_for("deepseek-v4-flash"),
            "deepseek-v4-flash"
        );
    }

    /// The published remote manifest (`catalog/model-catalog.json`) is the
    /// document every client fetches first; it must always parse with the same
    /// parser and validation rules as any other catalog document.
    #[test]
    fn published_remote_catalog_document_is_valid() {
        let document = CatalogDocument::from_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../catalog/model-catalog.json"
        )))
        .expect("published catalog document must parse");

        assert_eq!(document.schema_version, SUPPORTED_CATALOG_SCHEMA_VERSION);
        assert!(!document.revision.trim().is_empty());
        assert!(!document.models.is_empty());
        assert!(document.model(&document.default_model).is_some());
        assert!(document
            .pricing
            .rate_for(&document.default_model, None)
            .is_some());
    }

    /// Whatever the published manifest declares, the Codex-facing file it
    /// generates has to stay inside the Codex contract - a placeholder model
    /// that misses a capability field would otherwise break the client's model
    /// list for everyone pulling that manifest.
    #[test]
    fn published_remote_catalog_stays_codex_compatible() {
        let document = CatalogDocument::from_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../catalog/model-catalog.json"
        )))
        .expect("published catalog document must parse");

        let generated = serde_json::to_value(build_codeseex_catalog_from_document(&document))
            .expect("generated catalog serializes");
        assert!(
            catalog_value_is_compatible(&generated),
            "the published manifest generates a catalog Codex would reject"
        );
    }

    /// Every remote/cached document goes through the same gate, so the
    /// rejections below are what keeps a hostile or stale manifest from
    /// replacing a working catalog.
    #[test]
    fn remote_catalog_documents_are_validated() {
        fn document(models: &str) -> String {
            format!(
                r#"{{"schema_version":1,"revision":"r1","provider_name":"DeepSeek","default_model":"m1","models":[{models}]}}"#
            )
        }
        fn model(slug: &str) -> String {
            format!(
                r#"{{"slug":"{slug}","display_name":"{slug}","description":"d","context_window":1000,"effective_context_window_percent":95,"priority":1}}"#
            )
        }
        fn windowless_model(slug: &str) -> String {
            format!(
                r#"{{"slug":"{slug}","display_name":"{slug}","description":"d","context_window":0,"effective_context_window_percent":95,"priority":1}}"#
            )
        }

        let valid = document(&model("m1"));
        assert!(CatalogDocument::from_json(&valid).is_ok());

        let rejected = [
            valid.replace("\"schema_version\":1", "\"schema_version\":2"),
            valid.replace(
                "\"default_model\":\"m1\"",
                "\"min_app_version\":\"99.0.0\",\"default_model\":\"m1\"",
            ),
            document(""),
            document(&format!("{},{}", model("m1"), model("m1"))),
            valid.replace("\"default_model\":\"m1\"", "\"default_model\":\"missing\""),
            document(&windowless_model("m1")),
            document(&model("m1").replace("\"priority\":1", "\"base_instructions\":\"x\",\"priority\":1")),
            document(&model("m1").replace("\"priority\":1", "\"model_messages\":{},\"priority\":1")),
            "not json at all".to_owned(),
        ];
        for text in rejected {
            assert!(
                CatalogDocument::from_json(&text).is_err(),
                "expected the document to be rejected: {text}"
            );
        }
    }

    /// Prompt material is compiled in from the seed. A document carrying it -
    /// an older cache file, a hand-written overlay - must never win.
    #[test]
    fn seed_prompt_fields_beat_the_document() {
        let mut document = embedded_catalog_document();
        document.models[0]
            .extra
            .insert("base_instructions".to_owned(), Value::String("remote".to_owned()));

        let catalog = catalog_from_seed_and_document(
            r#"{"common_model_fields":{"base_instructions":"seed"},"models":[]}"#,
            &document,
        )
        .expect("compact seed");

        assert_eq!(
            catalog.models[0]
                .extra
                .get("base_instructions")
                .and_then(Value::as_str),
            Some("seed")
        );
    }

    #[test]
    fn catalog_document_keeps_issued_at() {
        let text = r#"{"schema_version":1,"revision":"r1","issued_at":"2026-09-01T00:00:00Z","provider_name":"DeepSeek","default_model":"m1","models":[{"slug":"m1","display_name":"m1","description":"d","context_window":1000,"effective_context_window_percent":95,"priority":1}]}"#;
        let document = CatalogDocument::from_json(text).expect("document");

        assert_eq!(document.issued_at.as_deref(), Some("2026-09-01T00:00:00Z"));
        assert_eq!(
            document.to_value().get("issued_at").and_then(Value::as_str),
            Some("2026-09-01T00:00:00Z")
        );
        assert!(embedded_catalog_document().issued_at.is_none());
    }

    #[test]
    fn compact_seed_expands_common_model_fields() {
        let catalog = catalog_from_seed_and_document(
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
            &embedded_catalog_document(),
        )
        .expect("compact seed");

        for model in catalog.models {
            assert_eq!(
                model.extra.get("base_instructions").and_then(Value::as_str),
                Some("shared prompt")
            );
            assert!(model.extra.contains_key("model_messages"));
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
        // The official DeepSeek V4 Pro does not accept images.
        assert_eq!(pro.input_modalities, vec!["text"]);
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
            .find(|model| model.id == "deepseek-flash")
            .expect("flash model");
        assert_eq!(flash.display_name, "DeepSeek V4.1 Flash");
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
