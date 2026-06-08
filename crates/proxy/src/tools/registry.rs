use codeseex_core::{AppConfig, UserConfig};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

pub(crate) fn tool_registry(
    config: &AppConfig,
    enabled_tools: &[String],
    settings: &BTreeMap<String, String>,
) -> Value {
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
            "name": "Vision",
            "nameKey": "toolVisionName",
            "description": "Analyze images and generate new images with your configured OpenAI-compatible vision endpoints.",
            "descriptionKey": "toolVisionDescription",
            "source": "builtin",
            "system": false,
            "configurable": true,
            "enabled": builtin_tool_enabled(enabled_tools, "vision_analyze"),
            "iconPath": "/assets/icons/vision.svg",
            "config": crate::tools::vision::registry_config_fields(settings),
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

pub(crate) fn enabled_tool_ids(config: &AppConfig) -> Vec<String> {
    UserConfig::read_from(&config.config_path())
        .ok()
        .and_then(|user_config| user_config.tools.and_then(|tools| tools.enabled))
        .unwrap_or_else(crate::tools::default_enabled_tool_ids)
}

pub(crate) fn tool_settings(config: &AppConfig) -> BTreeMap<String, String> {
    UserConfig::read_from(&config.config_path())
        .ok()
        .and_then(|user_config| user_config.tools.and_then(|tools| tools.settings))
        .unwrap_or_default()
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
        "visual_search" | "vision_generate" | "image_gen" | "imagegen" | "image_generation"
        | "generate_image" | "image_generate" | "create_image" => "vision_analyze",
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
        settings.insert("VISION_API_KEY".to_owned(), "secret-key".to_owned());
        let tools = tool_registry(&config, &["vision_analyze".to_owned()], &settings);
        let vision = tools
            .as_array()
            .expect("tools array")
            .iter()
            .find(|tool| tool.get("id").and_then(Value::as_str) == Some("vision_analyze"))
            .expect("vision tool");

        assert_eq!(
            vision.get("nameKey").and_then(Value::as_str),
            Some("toolVisionName")
        );
        assert_eq!(
            vision.get("descriptionKey").and_then(Value::as_str),
            Some("toolVisionDescription")
        );
        assert_eq!(vision.get("enabled").and_then(Value::as_bool), Some(true));
        assert_eq!(
            vision.pointer("/config/0/key").and_then(Value::as_str),
            Some("VISION_ANALYZE_URL")
        );
        assert_eq!(
            vision.pointer("/config/0/value").and_then(Value::as_str),
            Some("https://vision.example.com/v1")
        );
        assert_eq!(
            vision.pointer("/config/2/key").and_then(Value::as_str),
            Some("VISION_GENERATE_URL")
        );
        assert_eq!(
            vision.pointer("/config/3/key").and_then(Value::as_str),
            Some("VISION_GENERATE_MODEL")
        );
        assert_eq!(
            vision.pointer("/config/4/type").and_then(Value::as_str),
            Some("password")
        );
    }
}
