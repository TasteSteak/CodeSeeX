const { makeId } = require("../../shared/http");

const TOOL_NAME = "read_file_range";

function matchesInputTool(tool) {
  return String((tool && tool.type) || "").toLowerCase() === TOOL_NAME;
}

function registerInputTool(tool, state) {
  if (!state.byName.has(TOOL_NAME)) state.upstreamTools.push(modelTool(tool));
  state.byName.set(TOOL_NAME, { kind: "hosted_read_file_range", nativeTool: tool });
}

function modelTool(tool = {}) {
  return {
    type: "function",
    function: {
      name: TOOL_NAME,
      description: tool.description || "Read a limited line range from a local workspace text file. Use path plus start/end or count. The file must stay inside the workspace.",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string" },
          start: { type: "integer" },
          end: { type: "integer" },
          count: { type: "integer" },
        },
        required: ["path"],
        additionalProperties: false,
      },
    },
  };
}

function matchesChatTool(toolCall, context) {
  return hostedKind(context.getToolName(toolCall), context) === "hosted_read_file_range";
}

function responseItemFromChatTool(toolCall, context, helpers) {
  return {
    id: makeId("rfr"),
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
