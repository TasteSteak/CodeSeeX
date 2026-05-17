const { makeId } = require("../../shared/http");

const TOOL_NAME = "workspace_search";

function matchesInputTool(tool) {
  return String((tool && tool.type) || "").toLowerCase() === TOOL_NAME;
}

function registerInputTool(tool, state) {
  if (!state.byName.has(TOOL_NAME)) state.upstreamTools.push(modelTool(tool));
  state.byName.set(TOOL_NAME, { kind: "hosted_workspace_search", nativeTool: tool });
}

function modelTool(tool = {}) {
  return {
    type: "function",
    function: {
      name: TOOL_NAME,
      description: tool.description || "Search local workspace text files. Returns compact model payload: q=query, n=count, r[]=matches, p=path, l=line, s=match-centered snippet, c=context lines. Use this before reading files when you need to locate code.",
      parameters: {
        type: "object",
        properties: {
          query: { type: "string" },
          include: { type: "array", items: { type: "string" } },
          exclude: { type: "array", items: { type: "string" } },
          max_results: { type: "integer" },
          context_lines: { type: "integer" },
          case_sensitive: { type: "boolean" },
        },
        required: ["query"],
        additionalProperties: false,
      },
    },
  };
}

function matchesChatTool(toolCall, context) {
  return hostedKind(context.getToolName(toolCall), context) === "hosted_workspace_search";
}

function responseItemFromChatTool(toolCall, context, helpers) {
  return {
    id: makeId("wss"),
    type: "proxy_tool_call",
    call_id: toolCall.id || makeId("call"),
    name: TOOL_NAME,
    arguments: JSON.stringify(helpers.parseJson(context.getToolArguments(toolCall)) || {}),
    status: "completed",
  };
}

function matchesResponseItem(item) {
  return Boolean(item && (item.type === "function_call" || item.type === "proxy_tool_call") && item.name === TOOL_NAME);
}

function chatToolCallFromResponseItem(item, helpers) {
  return {
    id: item.call_id || item.id || makeId("call"),
    type: "function",
    function: {
      name: TOOL_NAME,
      arguments: helpers.normalizeResponseToolArguments(item),
    },
  };
}

function hostedKind(name, context) {
  const entry = context && context.byName ? context.byName.get(name) : null;
  return entry ? entry.kind : "";
}

module.exports = {
  chatToolCallFromResponseItem,
  matchesChatTool,
  matchesInputTool,
  matchesResponseItem,
  modelTool,
  registerInputTool,
  responseItemFromChatTool,
};
