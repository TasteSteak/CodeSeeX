use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::tools::ownership::ChatToolCall;

#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    entries: BTreeMap<String, ToolEntry>,
    pub upstream_tools: Vec<Value>,
    request_tool_items: usize,
    source_names: Vec<String>,
}

#[derive(Debug, Clone)]
struct ToolEntry {
    response_name: String,
    namespace: Option<String>,
}

impl ToolContext {
    pub fn from_request_tools(tools: Option<&Value>) -> Self {
        let mut context = Self::default();
        let Some(Value::Array(items)) = tools else {
            return context;
        };
        context.request_tool_items = items.len();

        for (index, tool) in items.iter().enumerate() {
            for declaration in normalize_tool_declarations(tool, index) {
                if is_conflicting_visual_tool(&declaration.response_name) {
                    continue;
                }
                context.push_declaration(declaration);
            }
        }

        context
    }

    pub fn has_external_tool(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn has_response_tool(&self, name: &str) -> bool {
        self.entries
            .values()
            .any(|entry| entry.response_name == name)
            || self.entries.contains_key(name)
    }

    pub fn has_any_response_tool(&self, names: &[&str]) -> bool {
        names.iter().any(|name| self.has_response_tool(name))
    }

    pub fn request_tool_items(&self) -> usize {
        self.request_tool_items
    }

    pub fn source_names(&self) -> Vec<String> {
        self.source_names.clone()
    }

    pub fn upstream_names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn ensure_codex_tool_search_bridge(&mut self) {
        if self.has_response_tool("tool_search_tool") || self.has_response_tool("tool_search") {
            return;
        }
        self.push_declaration(ToolDeclaration {
            response_name: "tool_search_tool".to_owned(),
            description: "Search deferred Codex native tool metadata, such as sub-agents, computer-use, thread, automation, or plugin runtime tools, and expose matching tools for the next model turn.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for deferred Codex tools, such as sub-agent, computer-use, thread, automation, or plugin runtime tools."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of tool matches to return."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            namespace: None,
        });
    }

    fn push_declaration(&mut self, declaration: ToolDeclaration) {
        let upstream_name = unique_tool_name(
            &declaration.response_name,
            declaration.namespace.as_deref(),
            &self.entries,
        );
        self.upstream_tools.push(json!({
            "type": "function",
            "function": {
                "name": upstream_name,
                "description": declaration.description,
                "parameters": declaration.parameters
            }
        }));
        self.source_names.push(declaration.response_name.clone());
        self.entries.insert(
            upstream_name,
            ToolEntry {
                response_name: declaration.response_name,
                namespace: declaration.namespace,
            },
        );
    }

    pub fn response_item_from_chat_call(&self, call: &ChatToolCall) -> Value {
        self.response_item_from_chat_call_with_id(call, &format!("fc_{}", Uuid::new_v4().simple()))
    }

    pub fn response_item_from_chat_call_with_id(
        &self,
        call: &ChatToolCall,
        item_id: &str,
    ) -> Value {
        let entry = self.entries.get(&call.name);
        let mut item = json!({
            "id": item_id,
            "type": "function_call",
            "status": "completed",
            "call_id": call.id,
            "name": entry.map(|value| value.response_name.as_str()).unwrap_or(call.name.as_str()),
            "arguments": call.arguments
        });
        if let Some(namespace) = entry.and_then(|value| value.namespace.as_deref()) {
            item["namespace"] = Value::String(namespace.to_owned());
        }
        item
    }
}

#[derive(Debug)]
struct ToolDeclaration {
    response_name: String,
    description: String,
    parameters: Value,
    namespace: Option<String>,
}

fn normalize_tool_declarations(tool: &Value, index: usize) -> Vec<ToolDeclaration> {
    let namespace = namespace_from_tool(tool);
    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if tool_type == "mcp" && tool.get("tools").and_then(Value::as_array).is_none() {
        return Vec::new();
    }
    if let Some(nested_tools) = tool.get("tools").and_then(Value::as_array) {
        return nested_tools
            .iter()
            .enumerate()
            .filter_map(|(nested_index, nested)| {
                normalize_single_tool(nested, nested_index, namespace.clone())
            })
            .collect();
    }

    normalize_single_tool(tool, index, namespace)
        .map(|value| vec![value])
        .unwrap_or_default()
}

fn normalize_single_tool(
    tool: &Value,
    index: usize,
    namespace: Option<String>,
) -> Option<ToolDeclaration> {
    let object = tool.as_object()?;
    let nested_tool = object.get("tool").filter(|value| value.is_object());
    let name = first_string(
        &[
            object.get("name"),
            object.get("function").and_then(|value| value.get("name")),
            nested_tool.and_then(|value| value.get("name")),
            object.get("server_label"),
        ],
        &format!("tool_{}", index + 1),
    );
    let description = first_string(
        &[
            object.get("description"),
            object
                .get("function")
                .and_then(|value| value.get("description")),
            nested_tool.and_then(|value| value.get("description")),
            object.get("title"),
        ],
        &name,
    );
    let parameters = object
        .get("parameters")
        .cloned()
        .or_else(|| {
            object
                .get("function")
                .and_then(|value| value.get("parameters"))
                .cloned()
        })
        .or_else(|| object.get("input_schema").cloned())
        .or_else(|| object.get("inputSchema").cloned())
        .or_else(|| {
            nested_tool
                .and_then(|value| value.get("input_schema"))
                .cloned()
        })
        .or_else(|| {
            nested_tool
                .and_then(|value| value.get("inputSchema"))
                .cloned()
        })
        .or_else(|| {
            nested_tool
                .and_then(|value| value.get("parameters"))
                .cloned()
        })
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));

    if !is_callable_tool(tool, &parameters) {
        return None;
    }

    Some(ToolDeclaration {
        response_name: sanitize_tool_name(&name),
        description,
        parameters: normalize_schema(parameters),
        namespace,
    })
}

fn namespace_from_tool(tool: &Value) -> Option<String> {
    for key in ["namespace", "server_namespace", "server"] {
        if let Some(value) = tool.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }

    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if tool_type == "namespace" || (tool_type == "mcp" && tool.get("tools").is_some()) {
        for key in ["name", "server_label"] {
            if let Some(value) = tool.get(key).and_then(Value::as_str) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
        }
    }
    None
}

fn is_callable_tool(tool: &Value, parameters: &Value) -> bool {
    if tool.get("type").and_then(Value::as_str) == Some("function")
        || tool.get("function").is_some()
    {
        return true;
    }
    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if tool_type == "mcp" {
        return has_callable_shape(tool, parameters);
    }
    if let Some(nested) = tool.get("tool").filter(|value| value.is_object()) {
        return has_callable_shape(nested, parameters);
    }
    tool.get("name").is_some() && has_callable_shape(tool, parameters)
}

fn has_callable_shape(tool: &Value, parameters: &Value) -> bool {
    tool.get("name").and_then(Value::as_str).is_some()
        || (tool.get("server_label").and_then(Value::as_str).is_some()
            && (tool.get("description").is_some() || parameters.is_object()))
}

fn normalize_schema(schema: Value) -> Value {
    if !schema.is_object() {
        return json!({ "type": "object", "properties": {} });
    }
    if schema.get("type").is_some() {
        return schema;
    }
    json!({
        "type": "object",
        "properties": schema.get("properties").cloned().unwrap_or_else(|| json!({})),
        "required": schema.get("required").cloned().unwrap_or_else(|| json!([])),
        "additionalProperties": schema
            .get("additionalProperties")
            .cloned()
            .unwrap_or(Value::Bool(true))
    })
}

fn first_string(candidates: &[Option<&Value>], fallback: &str) -> String {
    candidates
        .iter()
        .filter_map(|value| value.and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

fn unique_tool_name(
    response_name: &str,
    namespace: Option<&str>,
    entries: &BTreeMap<String, ToolEntry>,
) -> String {
    let base = sanitize_tool_name(response_name);
    if !entries.contains_key(&base) {
        return base;
    }
    let prefix = namespace
        .map(|value| sanitize_tool_name(&format!("{value}_{base}")))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| base.clone());
    let mut candidate = prefix.clone();
    let mut suffix = 2_u32;
    while entries.contains_key(&candidate) {
        candidate = suffixed_tool_name(&prefix, suffix);
        suffix += 1;
        if suffix > 10_000 {
            return suffixed_tool_name(&format!("tool_{}", Uuid::new_v4().simple()), 2);
        }
    }
    candidate
}

fn suffixed_tool_name(prefix: &str, suffix: u32) -> String {
    let suffix = format!("_{suffix}");
    let max_prefix_chars = 64_usize.saturating_sub(suffix.chars().count());
    let mut base = prefix.chars().take(max_prefix_chars).collect::<String>();
    if base.trim_matches('_').is_empty() {
        base = "tool".to_owned();
    }
    sanitize_tool_name(&format!("{base}{suffix}"))
}

fn sanitize_tool_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let truncated = sanitized.chars().take(64).collect::<String>();
    if truncated.trim_matches('_').is_empty() {
        "tool".to_owned()
    } else {
        truncated
    }
}

fn is_conflicting_visual_tool(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "imagegen"
            | "imagegenext"
            | "image_gen"
            | "image_generation"
            | "generate_image"
            | "image_generate"
            | "create_image"
            | "vision_analyze"
            | "vision_generate"
            | "visual_search"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_mcp_nested_tools_with_namespace() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "mcp",
                "name": "smoke_server",
                "tools": [
                    {
                        "name": "smoke_add",
                        "description": "Add two numbers",
                        "input_schema": {
                            "type": "object",
                            "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } },
                            "required": ["a", "b"]
                        }
                    }
                ]
            }
        ])));

        assert_eq!(context.upstream_tools.len(), 1);
        assert_eq!(
            context.upstream_tools[0]
                .pointer("/function/name")
                .and_then(Value::as_str),
            Some("smoke_add")
        );
        let item = context.response_item_from_chat_call(&ChatToolCall {
            id: "call_1".to_owned(),
            name: "smoke_add".to_owned(),
            arguments: "{\"a\":1,\"b\":2}".to_owned(),
        });
        assert_eq!(
            item.get("namespace").and_then(Value::as_str),
            Some("smoke_server")
        );
        assert_eq!(item.get("name").and_then(Value::as_str), Some("smoke_add"));
    }

    #[test]
    fn mcp_server_without_nested_tools_is_not_callable_function() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "mcp",
                "server_label": "node_repl"
            }
        ])));

        assert!(context.upstream_tools.is_empty());
        assert!(!context.has_external_tool("node_repl"));
    }

    #[test]
    fn flat_function_tool_search_passes_through() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "tool_search_tool",
                "description": "Search deferred tool metadata",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "number" }
                    },
                    "required": ["query"]
                }
            }
        ])));

        assert_eq!(context.request_tool_items(), 1);
        assert_eq!(context.source_names(), vec!["tool_search_tool"]);
        assert_eq!(context.upstream_names(), vec!["tool_search_tool"]);
        assert_eq!(
            context.upstream_tools[0]
                .pointer("/function/name")
                .and_then(Value::as_str),
            Some("tool_search_tool")
        );
        assert!(context.has_external_tool("tool_search_tool"));
    }

    #[test]
    fn synthetic_tool_search_bridge_can_be_added_for_codex_native_tools() {
        let mut context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "function": {
                    "name": "shell_command",
                    "description": "Run a shell command",
                    "parameters": { "type": "object", "properties": {} }
                }
            }
        ])));

        context.ensure_codex_tool_search_bridge();

        assert!(context.has_external_tool("shell_command"));
        assert!(context.has_external_tool("tool_search_tool"));
        assert_eq!(context.upstream_tools.len(), 2);
        assert!(context
            .upstream_names()
            .iter()
            .any(|name| name == "tool_search_tool"));
    }

    #[test]
    fn synthetic_tool_search_bridge_does_not_duplicate_native_declaration() {
        let mut context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "tool_search_tool",
                "description": "Search deferred tools",
                "parameters": { "type": "object", "properties": {} }
            }
        ])));

        context.ensure_codex_tool_search_bridge();

        assert_eq!(context.upstream_tools.len(), 1);
        assert_eq!(context.upstream_names(), vec!["tool_search_tool"]);
    }

    #[test]
    fn custom_tool_declaration_passes_through_as_callable_function() {
        let context = ToolContext::from_request_tools(Some(&json!([{
            "type": "custom",
            "name": "apply_patch",
            "description": "Apply a patch",
            "input_schema": {
                "type": "object",
                "properties": {
                    "patch": { "type": "string" }
                },
                "required": ["patch"],
                "additionalProperties": false
            }
        }])));

        assert_eq!(context.request_tool_items(), 1);
        assert_eq!(context.upstream_names(), vec!["apply_patch"]);
        assert_eq!(
            context.upstream_tools[0]
                .pointer("/function/name")
                .and_then(Value::as_str),
            Some("apply_patch")
        );
        assert_eq!(
            context.upstream_tools[0]
                .pointer("/function/parameters/required/0")
                .and_then(Value::as_str),
            Some("patch")
        );
    }

    #[test]
    fn nested_tools_without_namespace_still_pass_through() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "namespace",
                "tools": [
                    {
                        "name": "spawn_agent",
                        "description": "Spawn a sub-agent",
                        "input_schema": {
                            "type": "object",
                            "properties": {
                                "message": { "type": "string" }
                            }
                        }
                    }
                ]
            }
        ])));

        assert_eq!(context.request_tool_items(), 1);
        assert_eq!(context.source_names(), vec!["spawn_agent"]);
        assert_eq!(context.upstream_names(), vec!["spawn_agent"]);
        assert!(context.has_external_tool("spawn_agent"));
    }

    #[test]
    fn filters_conflicting_native_visual_tools() {
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "name": "imagegen",
                "description": "Generate images",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "type": "function",
                "name": "image_gen",
                "description": "Native-compatible image generation",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "type": "function",
                "name": "vision_generate",
                "description": "Legacy image generation",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "type": "function",
                "name": "view_image",
                "description": "View a local image",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent",
                "parameters": { "type": "object", "properties": {} }
            }
        ])));

        assert_eq!(context.request_tool_items(), 5);
        assert_eq!(context.upstream_names(), vec!["spawn_agent", "view_image"]);
        assert!(!context.has_external_tool("imagegen"));
        assert!(!context.has_external_tool("image_gen"));
        assert!(!context.has_external_tool("vision_generate"));
        assert!(context.has_external_tool("view_image"));
        assert!(context.has_external_tool("spawn_agent"));
    }

    #[test]
    fn duplicate_long_tool_names_get_distinct_bounded_names() {
        let long = "a".repeat(64);
        let context = ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "function": {
                    "name": long,
                    "description": "first",
                    "parameters": { "type": "object", "properties": {} }
                }
            },
            {
                "type": "function",
                "function": {
                    "name": long,
                    "description": "second",
                    "parameters": { "type": "object", "properties": {} }
                }
            }
        ])));
        let names = context
            .upstream_tools
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1]);
        assert!(names.iter().all(|name| name.chars().count() <= 64));
    }
}
