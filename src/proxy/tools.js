const { makeId } = require("../shared/http");
const { isToolEnabledByConfig } = require("../shared/tool-registry");
const { applyPatch, listToolModules } = require("../tools");
const { chatToolCallFromAdaptedResponseItem, registerInputToolAdapter, responseItemFromAdaptedChatTool } = require("./tool-adapters");

function createToolContext(tools, options = {}) {
  const normalized = normalizeTools(tools, options);
  const context = {
    rootDir: normalized.rootDir,
    internalApplyPatchToolName: applyPatch.TOOL_NAME,
    upstreamTools: normalized.upstreamTools,
    passthroughTools: normalized.passthroughTools,
    byName: normalized.byName,
    getToolArguments,
    getToolName,
    isInternalToolCall(toolCall) {
      return isInternalToolCall(toolCall, context);
    },
    internalPatchFromToolCall(toolCall) {
      return internalPatchFromToolCall(toolCall, context);
    },
    normalizeToolChoice(toolChoice) {
      return normalizeToolChoice(toolChoice, context);
    },
    responseToolItemFromChat(toolCall) {
      return responseToolItemFromChat(toolCall, context);
    },
  };
  return context;
}

function normalizeTools(tools, options = {}) {
  const rootDir = options.rootDir || process.env.PROXY_ROOT_DIR || null;
  const upstreamTools = [applyPatch.modelTool()];
  const byName = new Map();
  const passthroughTools = [];
  const state = { upstreamTools, passthroughTools, byName };

  applyPatch.registerLegacyAlias(state);
  registerAutoTools(state, options);

  for (let index = 0; index < (Array.isArray(tools) ? tools.length : 0); index += 1) {
    const tool = tools[index];
    if (!tool || typeof tool !== "object") continue;

    if (applyPatch.matchesInputTool(tool)) {
      applyPatch.registerInputTool(tool, state, options);
      continue;
    }

    if (registerInputToolAdapter(tool, state, options)) continue;
    if (!isFunctionTool(tool)) continue;

    const name = sanitizeToolName(tool.name || (tool.function ? tool.function.name : "") || tool.server_label || "tool_" + (index + 1));
    if (applyPatch.isToolName(name)) {
      applyPatch.registerInputTool(tool, state, options);
      continue;
    }
    const parameters = tool.parameters || (tool.function ? tool.function.parameters : null) || tool.input_schema || { type: "object", properties: {} };
    upstreamTools.push({
      type: "function",
      function: {
        name,
        description: tool.description || (tool.function ? tool.function.description : "") || name,
        parameters: normalizeSchema(parameters),
      },
    });
    byName.set(name, { kind: "function", nativeTool: tool });
  }

  return {
    upstreamTools,
    byName,
    passthroughTools,
    rootDir,
  };
}

function registerAutoTools(state, options = {}) {
  for (const tool of listToolModules({
    rootDir: options.rootDir,
    extensionDir: options.extensionDir,
    communityToolCodeEnabled: options.communityToolCodeEnabled,
  })) {
    if (!tool || !tool.manifest || normalizeToolId(tool.manifest.id) === applyPatch.TOOL_NAME) continue;
    if (!toolEnabled(tool.manifest, options.toolConfig || {})) continue;
    if (tool.autoRegister === false) continue;
    if (typeof tool.registerInputTool !== "function") continue;
    const declaration = tool.defaultInputTool || { type: tool.manifest.id };
    tool.registerInputTool(declaration, state, options);
  }
}

function toolEnabled(manifest, config = {}) {
  return isToolEnabledByConfig(manifest, config);
}

function normalizeToolChoice(toolChoice, toolContext) {
  if (toolChoice === undefined || toolChoice === null) return undefined;
  if (toolChoice === "auto" || toolChoice === "none" || toolChoice === "required") return toolChoice;
  if (typeof toolChoice === "object") {
    let name = toolChoice.name || (toolChoice.function ? toolChoice.function.name : null) || toolChoice.type;
    name = applyPatch.normalizeToolChoiceName(name);
    if (name === "web_search_preview" && toolContext.byName.has("web_search")) name = "web_search";
    if (name && toolContext.byName.has(name)) return { type: "function", function: { name } };
  }
  return undefined;
}

function splitToolCalls(toolCalls, toolContext) {
  const external = [];
  const internal = [];
  const hosted = [];
  for (const call of Array.isArray(toolCalls) ? toolCalls : []) {
    if (isInternalToolCall(call, toolContext)) {
      internal.push(call);
      continue;
    }
    if (isHostedToolCall(call, toolContext)) {
      hosted.push(call);
      continue;
    }
    external.push(call);
  }
  return { external, internal, hosted };
}

function responseToolItemFromChat(toolCall, toolContext) {
  if (isInternalToolCall(toolCall, toolContext)) {
    return applyPatch.responseItemFromChatTool(toolCall, Object.assign({ getToolArguments, getToolName }, toolContext));
  }

  const adaptedItem = responseItemFromAdaptedChatTool(toolCall, Object.assign({ getToolArguments, getToolName }, toolContext), { parseJson });
  if (adaptedItem) return adaptedItem;

  const name = getToolName(toolCall);
  const args = getToolArguments(toolCall);
  return {
    id: makeId("fc"),
    type: "function_call",
    call_id: toolCall.id || makeId("call"),
    name,
    arguments: applyPatch.normalizeShellArguments(name, args),
    status: "completed",
  };
}

function chatToolCallFromResponseItem(item) {
  const patchCall = applyPatch.chatToolCallFromResponseItem(item, { normalizeResponseToolArguments });
  if (patchCall) return patchCall;

  const adaptedToolCall = chatToolCallFromAdaptedResponseItem(item, { normalizeResponseToolArguments, parseJson });
  if (adaptedToolCall) return adaptedToolCall;

  return {
    id: item.call_id || item.id || makeId("call"),
    type: "function",
    function: { name: item.name || "tool_call", arguments: normalizeResponseToolArguments(item) },
  };
}

function expandApplyPatchToolCalls(toolCalls) {
  return Array.isArray(toolCalls) ? toolCalls : [];
}

function isInternalToolCall(toolCall, toolContext) {
  const name = getToolName(toolCall);
  if (applyPatch.isToolName(name)) return true;
  return applyPatch.isShellPatchCall(toolCall, getToolName, getToolArguments);
}

function isHostedToolCall(toolCall, toolContext) {
  const name = getToolName(toolCall);
  const entry = toolContext && toolContext.byName ? toolContext.byName.get(name) : null;
  return Boolean(entry && String(entry.kind || "").startsWith("hosted_"));
}

function internalPatchFromToolCall(toolCall, toolContext) {
  if (!isInternalToolCall(toolCall, toolContext)) return null;
  const patch = applyPatch.patchTextFromToolCall(getToolName(toolCall), getToolArguments(toolCall));
  return patch === null ? "" : patch;
}

function getToolName(toolCall) {
  return toolCall && toolCall.function ? toolCall.function.name || "" : "";
}

function getToolArguments(toolCall) {
  const args = toolCall && toolCall.function ? toolCall.function.arguments : "";
  if (typeof args === "string") return args;
  return JSON.stringify(args || {});
}

function normalizeResponseToolArguments(item) {
  if (typeof item.arguments === "string") return item.arguments;
  if (item.arguments && typeof item.arguments === "object") return JSON.stringify(item.arguments);
  if (typeof item.input === "string") return JSON.stringify({ input: item.input });
  if (item.input && typeof item.input === "object") return JSON.stringify(item.input);
  return "{}";
}

function normalizeSchema(schema) {
  if (!schema || typeof schema !== "object") return { type: "object", properties: {} };
  if (!schema.type) {
    return {
      type: "object",
      properties: schema.properties || {},
      required: schema.required || [],
      additionalProperties: schema.additionalProperties !== undefined ? schema.additionalProperties : true,
    };
  }
  return schema;
}

function isFunctionTool(tool) {
  return tool.type === "function" || Boolean(tool.function);
}

function sanitizeToolName(name) {
  return String(name || "tool").replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 64) || "tool";
}

function normalizeToolId(value) {
  return String(value || "").trim().toLowerCase().replace(/[^a-z0-9_-]/g, "_").slice(0, 64);
}

const parseJson = applyPatch.parseJson;
const patchFromApplyPatchOperation = applyPatch.patchFromApplyPatchOperation;
const repairApplyPatch = applyPatch.repairApplyPatch;

module.exports = {
  chatToolCallFromResponseItem,
  createToolContext,
  expandApplyPatchToolCalls,
  getToolArguments,
  getToolName,
  isHostedToolCall,
  normalizeToolChoice,
  normalizeTools,
  patchFromApplyPatchOperation,
  parseJson,
  repairApplyPatch,
  responseToolItemFromChat,
  splitToolCalls,
};
