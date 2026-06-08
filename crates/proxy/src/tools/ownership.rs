use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexNativeTool {
    ApplyPatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostedTool {
    WebSearch,
    BuiltIn,
    Community,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolOwner {
    CodexNative(CodexNativeTool),
    CodeseexHosted(HostedTool),
    External,
    Unknown,
}

#[derive(Debug, Default)]
pub(crate) struct ToolCallPartition {
    pub(crate) code: Vec<ChatToolCall>,
    pub(crate) hosted: Vec<ChatToolCall>,
    pub(crate) native: Vec<ChatToolCall>,
    pub(crate) external: Vec<ChatToolCall>,
    pub(crate) unknown: Vec<ChatToolCall>,
}

impl ToolCallPartition {
    pub(crate) fn has_proxy_executed_calls(&self) -> bool {
        !self.code.is_empty() || !self.hosted.is_empty()
    }

    pub(crate) fn has_client_executed_calls(&self) -> bool {
        !self.native.is_empty() || !self.external.is_empty()
    }
}

pub(crate) fn partition_tool_calls(
    tool_calls: Vec<ChatToolCall>,
    community_tools: &crate::community_tools::CommunityToolSet,
    external_tool_context: &crate::tool_passthrough::ToolContext,
) -> ToolCallPartition {
    let mut partition = ToolCallPartition::default();
    for mut call in tool_calls {
        call.name = canonical_tool_name(&call.name).to_owned();
        match resolve_tool_owner(&call.name, community_tools, external_tool_context) {
            ToolOwner::CodexNative(_) => partition.native.push(call),
            ToolOwner::CodeseexHosted(HostedTool::WebSearch) => partition.hosted.push(call),
            ToolOwner::CodeseexHosted(HostedTool::BuiltIn | HostedTool::Community) => {
                partition.code.push(call);
            }
            ToolOwner::External => partition.external.push(call),
            ToolOwner::Unknown => partition.unknown.push(call),
        }
    }
    partition
}

pub(crate) fn resolve_tool_owner(
    name: &str,
    community_tools: &crate::community_tools::CommunityToolSet,
    external_tool_context: &crate::tool_passthrough::ToolContext,
) -> ToolOwner {
    if is_native_apply_patch_tool(name) {
        return ToolOwner::CodexNative(CodexNativeTool::ApplyPatch);
    }
    if is_web_search_tool(name) {
        return ToolOwner::CodeseexHosted(HostedTool::WebSearch);
    }
    if crate::tools::is_known_code_tool(name) {
        return ToolOwner::CodeseexHosted(HostedTool::BuiltIn);
    }
    if community_tools.is_known_tool(name) {
        return ToolOwner::CodeseexHosted(HostedTool::Community);
    }
    if external_tool_context.has_external_tool(name) {
        return ToolOwner::External;
    }
    ToolOwner::Unknown
}

pub(crate) fn is_native_apply_patch_tool(name: &str) -> bool {
    name == "apply_patch"
}

pub(crate) fn is_web_search_tool(name: &str) -> bool {
    canonical_tool_name(name) == "web_search"
}

pub(crate) fn canonical_tool_name(name: &str) -> &str {
    match name {
        "web_search_preview" => "web_search",
        "visual_search" => "vision_analyze",
        _ => name,
    }
}

pub(crate) fn proxy_executed_calls_in_order(
    all_tool_calls: &[ChatToolCall],
    partition: &ToolCallPartition,
) -> Vec<ChatToolCall> {
    let ids = partition
        .code
        .iter()
        .chain(partition.hosted.iter())
        .map(|call| call.id.as_str())
        .collect::<HashSet<_>>();
    all_tool_calls
        .iter()
        .filter(|call| ids.contains(call.id.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str) -> ChatToolCall {
        ChatToolCall {
            id: format!("call_{name}"),
            name: name.to_owned(),
            arguments: "{}".to_owned(),
        }
    }

    #[test]
    fn codeseex_known_tool_wins_over_same_name_external_tool() {
        let external = crate::tool_passthrough::ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "function": {
                    "name": "list_directory",
                    "description": "client-side collision",
                    "parameters": { "type": "object", "properties": {} }
                }
            }
        ])));

        let owner = resolve_tool_owner(
            "list_directory",
            &crate::community_tools::CommunityToolSet::default(),
            &external,
        );

        assert_eq!(owner, ToolOwner::CodeseexHosted(HostedTool::BuiltIn));
    }

    #[test]
    fn external_mcp_style_tool_stays_client_executed() {
        let external = crate::tool_passthrough::ToolContext::from_request_tools(Some(&json!([
            {
                "type": "mcp",
                "name": "node_repl",
                "tools": [
                    {
                        "name": "js",
                        "description": "Run JavaScript",
                        "input_schema": { "type": "object", "properties": {} }
                    }
                ]
            }
        ])));

        let partition = partition_tool_calls(
            vec![call("js")],
            &crate::community_tools::CommunityToolSet::default(),
            &external,
        );

        assert_eq!(partition.external.len(), 1);
        assert!(partition.code.is_empty());
        assert!(partition.native.is_empty());
        assert!(partition.unknown.is_empty());
    }

    #[test]
    fn native_apply_patch_wins_over_external_collision() {
        let external = crate::tool_passthrough::ToolContext::from_request_tools(Some(&json!([
            {
                "type": "function",
                "function": {
                    "name": "apply_patch",
                    "description": "client-side native declaration",
                    "parameters": { "type": "object", "properties": {} }
                }
            }
        ])));

        let partition = partition_tool_calls(
            vec![call("apply_patch")],
            &crate::community_tools::CommunityToolSet::default(),
            &external,
        );

        assert_eq!(partition.native.len(), 1);
        assert!(partition.external.is_empty());
        assert!(partition.code.is_empty());
    }
}
