use codeseex_core::{AppConfig, UserConfig};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

pub(crate) fn tool_registry(
    config: &AppConfig,
    enabled_tools: &[String],
    settings: &BTreeMap<String, String>,
) -> Value {
    let selected = enabled_tools;
    let mut tools = match json!([
        {
            "id": "apply_patch",
            "name": "Apply Patch",
            "nameKey": "toolApplyPatchName",
            "description": "Native Codex patch editor for precise file changes. This system tool follows Codex settings.",
            "descriptionKey": "toolApplyPatchDescription",
            "source": "builtin",
            "system": true,
            "configurable": false,
            "enabled": true,
            "iconPath": "/assets/icons/apply-patch.svg",
            "labels": [
                { "id": "system", "labelKey": "toolLabelSystem", "label": "System" },
                { "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }
            ]
        },
        {
            "id": "web_search",
            "name": "Web Search",
            "nameKey": "toolWebSearchName",
            "description": "Search the web and open public pages with the configured network proxy policy.",
            "descriptionKey": "toolWebSearchDescription",
            "source": "builtin",
            "system": true,
            "configurable": false,
            "enabled": true,
            "iconPath": "/assets/icons/web-search.svg",
            "labels": [
                { "id": "system", "labelKey": "toolLabelSystem", "label": "System" },
                { "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }
            ]
        },
        {
            "id": "mcp_server",
            "name": "MCP Server",
            "nameKey": "toolMcpServerName",
            "description": "Use MCP tools discovered by Codex. Server configuration stays in Codex.",
            "descriptionKey": "toolMcpServerDescription",
            "source": "builtin",
            "system": true,
            "configurable": false,
            "enabled": true,
            "iconPath": "/assets/icons/tools.svg",
            "labels": [
                { "id": "system", "labelKey": "toolLabelSystem", "label": "System" },
                { "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }
            ]
        },
        {
            "id": "list_directory",
            "name": "List Directory",
            "nameKey": "toolListDirectoryName",
            "description": "Browse workspace folders with depth limits, filters, and compact metadata.",
            "descriptionKey": "toolListDirectoryDescription",
            "source": "builtin",
            "system": false,
            "configurable": true,
            "enabled": builtin_tool_enabled(enabled_tools, "list_directory"),
            "iconPath": "/assets/icons/list-directory.svg",
            "labels": [{ "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }]
        },
        {
            "id": "read_file_range",
            "name": "Read File Range",
            "nameKey": "toolReadFileRangeName",
            "description": "Read selected lines from workspace text files without loading the whole file.",
            "descriptionKey": "toolReadFileRangeDescription",
            "source": "builtin",
            "system": false,
            "configurable": true,
            "enabled": builtin_tool_enabled(enabled_tools, "read_file_range"),
            "iconPath": "/assets/icons/read-file-range.svg",
            "labels": [{ "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }]
        },
        {
            "id": "workspace_search",
            "name": "Workspace Search",
            "nameKey": "toolWorkspaceSearchName",
            "description": "Find text across workspace files with include/exclude filters and line matches.",
            "descriptionKey": "toolWorkspaceSearchDescription",
            "source": "builtin",
            "system": false,
            "configurable": true,
            "enabled": builtin_tool_enabled(enabled_tools, "workspace_search"),
            "iconPath": "/assets/icons/workspace-search.svg",
            "labels": [{ "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }]
        },
        {
            "id": "vision_analyze",
            "name": "Image understanding",
            "nameKey": "toolVisionAnalyzeName",
            "description": "Inspect images with DeepSeek Vision or a custom image understanding endpoint.",
            "descriptionKey": "toolVisionAnalyzeDescription",
            "source": "builtin",
            "system": false,
            "configurable": true,
            "enabled": selected.iter().any(|id| canonical_builtin_tool_id(id) == "vision_analyze"),
            "iconPath": "/assets/icons/vision.svg",
            "config": crate::tools::vision::analyze_registry_config_fields(config, settings),
            "labels": [{ "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }]
        },
        {
            "id": "image_gen",
            "name": "Image generation",
            "nameKey": "toolVisionGenerateName",
            "description": "Generate images through a separately configured image generation endpoint.",
            "descriptionKey": "toolVisionGenerateDescription",
            "source": "builtin",
            "system": false,
            "configurable": true,
            "enabled": selected.iter().any(|id| canonical_builtin_tool_id(id) == "image_gen"),
            "iconPath": "/assets/icons/vision.svg",
            "config": crate::tools::vision::generate_registry_config_fields(settings),
            "labels": [{ "id": "built_in", "labelKey": "toolLabelBuiltIn", "label": "Built-in" }]
        }
    ]) {
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    tools.extend(crate::community_tools::list_community_tools(
        &config.data_dir,
        enabled_tools,
        settings,
    ));
    Value::Array(tools)
}

pub(crate) fn selected_tool_ids(config: &AppConfig) -> Vec<String> {
    let user_config = UserConfig::read_from(&config.config_path()).ok();
    let explicit = user_config
        .as_ref()
        .and_then(|user_config| user_config.tools.as_ref())
        .and_then(|tools| tools.enabled.clone());
    let enabled = explicit
        .clone()
        .unwrap_or_else(crate::tools::default_enabled_tool_ids);
    enabled
}

pub(crate) fn enabled_tool_ids(config: &AppConfig) -> Vec<String> {
    selected_tool_ids(config)
}

pub(crate) fn tool_settings(config: &AppConfig) -> BTreeMap<String, String> {
    let mut settings = UserConfig::read_from(&config.config_path())
        .ok()
        .map(|user_config| crate::config_payload::tool_settings_from_user_config(&user_config))
        .unwrap_or_default();
    if let Some(api_key) = crate::secrets::vision_analyze_api_key(config) {
        settings.insert(
            crate::tools::vision::ANALYZE_API_KEY_KEY.to_owned(),
            api_key,
        );
    }
    if let Some(api_key) = crate::secrets::vision_generate_api_key(config) {
        settings.insert(
            crate::tools::vision::GENERATE_API_KEY_KEY.to_owned(),
            api_key,
        );
    }
    settings
}

pub(crate) fn builtin_tool_config_keys() -> HashSet<String> {
    crate::tools::vision::config_keys()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

pub(crate) fn dedupe_tool_definitions(tools: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    tools
        .into_iter()
        .filter(|tool| {
            let Some(name) = tool.pointer("/function/name").and_then(Value::as_str) else {
                return true;
            };
            seen.insert(name.to_owned())
        })
        .collect()
}

pub(crate) fn normalized_tool_choice(choice: Option<&Value>, tools: &[Value]) -> Option<Value> {
    let choice = choice?;
    if let Some(value) = choice.as_str() {
        return matches!(value, "auto" | "none" | "required")
            .then(|| Value::String(value.to_owned()));
    }
    let name = choice
        .get("name")
        .or_else(|| choice.pointer("/function/name"))
        .or_else(|| choice.get("type"))
        .and_then(Value::as_str)
        .map(|value| {
            if value == "web_search_preview" {
                "web_search"
            } else {
                value
            }
        })?;
    if !tools.iter().any(|tool| {
        tool.pointer("/function/name")
            .and_then(Value::as_str)
            .map(|tool_name| tool_name == name)
            .unwrap_or(false)
    }) {
        return None;
    }
    Some(json!({ "type": "function", "function": { "name": name } }))
}

fn builtin_tool_enabled(enabled_tools: &[String], id: &str) -> bool {
    enabled_tools
        .iter()
        .any(|enabled_id| canonical_builtin_tool_id(enabled_id.as_str()) == id)
}

fn canonical_builtin_tool_id(id: &str) -> &str {
    match id {
        "vision_generate" | "imagegen" | "image_generation" | "generate_image"
        | "image_generate" | "create_image" => "image_gen",
        _ => id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_choice_none_is_not_rewritten_to_auto() {
        let tools = vec![json!({
            "type": "function",
            "function": { "name": "web_search" }
        })];

        assert_eq!(
            normalized_tool_choice(Some(&json!("none")), &tools),
            Some(json!("none"))
        );
    }

    #[test]
    fn tool_choice_preview_maps_to_web_search_when_available() {
        let tools = vec![json!({
            "type": "function",
            "function": { "name": "web_search" }
        })];

        assert_eq!(
            normalized_tool_choice(Some(&json!({ "type": "web_search_preview" })), &tools),
            Some(json!({ "type": "function", "function": { "name": "web_search" } }))
        );
    }

    #[test]
    fn web_search_registry_keeps_network_proxy_out_of_tool_config() {
        let config = AppConfig::default();
        let tools = tool_registry(&config, &[], &BTreeMap::new());
        let web_search = tools
            .as_array()
            .expect("tools array")
            .iter()
            .find(|tool| tool.get("id").and_then(Value::as_str) == Some("web_search"))
            .expect("web_search tool");

        assert!(web_search.get("config").is_none());
    }

    #[test]
    fn vision_registry_exposes_config_fields_and_i18n_keys() {
        let config = AppConfig::default();
        let mut settings = BTreeMap::new();
        settings.insert("VISION_ANALYZE_BACKEND".to_owned(), "external".to_owned());
        settings.insert(
            "VISION_ANALYZE_URL".to_owned(),
            "https://vision.example.com/v1".to_owned(),
        );
        settings.insert("VISION_ANALYZE_MODEL".to_owned(), "vision-model".to_owned());
        settings.insert(
            "VISION_GENERATE_URL".to_owned(),
            "https://vision.example.com/v1/images/generations".to_owned(),
        );
        settings.insert("VISION_GENERATE_MODEL".to_owned(), "image-model".to_owned());
        settings.insert(
            "VISION_ANALYZE_API_KEY".to_owned(),
            "analyze-key".to_owned(),
        );
        settings.insert(
            "VISION_GENERATE_API_KEY".to_owned(),
            "generate-key".to_owned(),
        );
        let tools = tool_registry(
            &config,
            &["vision_analyze".to_owned(), "image_gen".to_owned()],
            &settings,
        );
        let vision = tools
            .as_array()
            .expect("tools array")
            .iter()
            .find(|tool| tool.get("id").and_then(Value::as_str) == Some("vision_analyze"))
            .expect("vision tool");

        assert_eq!(
            vision.get("nameKey").and_then(Value::as_str),
            Some("toolVisionAnalyzeName")
        );
        assert_eq!(
            vision.get("descriptionKey").and_then(Value::as_str),
            Some("toolVisionAnalyzeDescription")
        );
        assert_eq!(vision.get("enabled").and_then(Value::as_bool), Some(true));
        assert_eq!(
            vision.pointer("/config/0/key").and_then(Value::as_str),
            Some("VISION_ANALYZE_BACKEND")
        );
        assert_eq!(
            vision.pointer("/config/0/value").and_then(Value::as_str),
            Some("external")
        );
        assert_eq!(
            vision.pointer("/config/5/key").and_then(Value::as_str),
            Some("VISION_ANALYZE_URL")
        );
        assert_eq!(
            vision.pointer("/config/6/key").and_then(Value::as_str),
            Some("VISION_ANALYZE_MODEL")
        );
        assert_eq!(
            vision.pointer("/config/7/type").and_then(Value::as_str),
            Some("password")
        );
        let generation = tools
            .as_array()
            .expect("tools array")
            .iter()
            .find(|tool| tool.get("id").and_then(Value::as_str) == Some("image_gen"))
            .expect("image generation tool");
        assert_eq!(
            generation.get("nameKey").and_then(Value::as_str),
            Some("toolVisionGenerateName")
        );
        assert_eq!(
            generation.pointer("/config/0/key").and_then(Value::as_str),
            Some("VISION_GENERATE_URL")
        );
        assert_eq!(
            generation.pointer("/config/2/key").and_then(Value::as_str),
            Some("VISION_GENERATE_API_KEY")
        );
    }

    #[test]
    fn legacy_generation_fields_do_not_implicitly_enable_image_generation() {
        let config = AppConfig {
            data_dir: std::env::temp_dir().join(format!(
                "codeseex-vision-registry-{}",
                uuid::Uuid::new_v4().simple()
            )),
            ..Default::default()
        };
        UserConfig {
            tools: Some(codeseex_core::UserToolsConfig {
                enabled: Some(vec!["vision_analyze".to_owned()]),
                vision_analyze: Some(codeseex_core::UserVisionToolConfig {
                    generate_url: Some("https://example.com/v1/images/generations".to_owned()),
                    generate_model: Some("image-model".to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
        .write_atomic(&config.config_path())
        .expect("write legacy vision config");
        let selected = selected_tool_ids(&config);
        assert_eq!(selected, vec!["vision_analyze"]);
        let _ = std::fs::remove_dir_all(config.data_dir);
    }
}
