use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    entries: BTreeMap<String, ToolEntry>,
    pub upstream_tools: Vec<Value>,
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

        for (index, tool) in items.iter().enumerate() {
            for declaration in normalize_tool_declarations(tool, index) {
                let upstream_name = unique_tool_name(
                    &declaration.response_name,
                    declaration.namespace.as_deref(),
                    &context.entries,
                );
                context.upstream_tools.push(json!({
                    "type": "function",
                    "function": {
                        "name": upstream_name,
                        "description": declaration.description,
                        "parameters": declaration.parameters
                    }
                }));
                context.entries.insert(
                    upstream_name,
                    ToolEntry {
                        response_name: declaration.response_name,
                        namespace: declaration.namespace,
                    },
                );
            }
        }

        context
    }

    pub fn has_external_tool(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn response_item_from_chat_call(&self, call: &crate::server::ChatToolCall) -> Value {
        let entry = self.entries.get(&call.name);
        let mut item = json!({
            "id": format!("fc_{}", Uuid::new_v4().simple()),
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
    if let Some(nested_tools) = tool.get("tools").and_then(Value::as_array) {
        if namespace.is_some() {
            return nested_tools
                .iter()
                .enumerate()
                .filter_map(|(nested_index, nested)| {
                    normalize_single_tool(nested, nested_index, namespace.clone())
                })
                .collect();
        }
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
        candidate = sanitize_tool_name(&format!("{prefix}_{suffix}"));
        suffix += 1;
    }
    candidate
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
        let item = context.response_item_from_chat_call(&crate::server::ChatToolCall {
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
}
