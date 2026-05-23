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

  const allTools = (Array.isArray(tools) ? tools : []).concat(Array.isArray(options.extraTools) ? options.extraTools : []);
  for (let index = 0; index < allTools.length; index += 1) {
    const tool = allTools[index];
    if (!tool || typeof tool !== "object") continue;

    if (applyPatch.matchesInputTool(tool)) {
      applyPatch.registerInputTool(tool, state, options);
      continue;
    }

    if (registerInputToolAdapter(tool, state, options)) continue;

    const declarations = normalizeModelFunctionTools(tool, index);
    for (const declaration of declarations) {
      const responseName = declaration.name;
      const name = selectModelToolName(responseName, declaration, byName, upstreamTools);
      if (applyPatch.isToolName(name)) {
        applyPatch.registerInputTool(tool, state, options);
        continue;
      }
      upstreamTools.push({
        type: "function",
        function: {
          name,
          description: declaration.description,
          parameters: declaration.parameters,
        },
      });
      byName.set(name, {
        kind: declaration.kind || "function",
        nativeTool: tool,
        namespace: declaration.namespace || "",
        responseName,
        mcpServer: declaration.mcpServer || "",
      });
    }
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
  const entry = toolContext && toolContext.byName ? toolContext.byName.get(name) : null;
  const responseName = (entry && entry.responseName) || name;
  const item = {
    id: makeId("fc"),
    type: "function_call",
    call_id: toolCall.id || makeId("call"),
    name: responseName,
    arguments: entry && isHostedKind(entry.kind) ? args : applyPatch.normalizeShellArguments(responseName, args),
    status: "completed",
  };
  if (entry && entry.namespace) item.namespace = entry.namespace;
  if (entry && isHostedKind(entry.kind)) {
    item.type = "proxy_tool_call";
    item.mcp_server = entry.mcpServer || entry.namespace || "";
  }
  return item;
}

function chatToolCallFromResponseItem(item) {
  const patchCall = applyPatch.chatToolCallFromResponseItem(item, { normalizeResponseToolArguments });
  if (patchCall) return patchCall;

  const adaptedToolCall = chatToolCallFromAdaptedResponseItem(item, { normalizeResponseToolArguments, parseJson });
  if (adaptedToolCall) return adaptedToolCall;

  const toolCall = {
    id: item.call_id || item.id || makeId("call"),
    type: "function",
    function: { name: item.name || "tool_call", arguments: normalizeResponseToolArguments(item) },
  };
  if (item.namespace) toolCall.namespace = item.namespace;
  return toolCall;
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
  return Boolean(entry && isHostedKind(entry.kind));
}

function isHostedKind(kind) {
  return String(kind || "").startsWith("hosted_");
}

function internalPatchFromToolCall(toolCall, toolContext) {
  if (!isInternalToolCall(toolCall, toolContext)) return null;
  const patch = applyPatch.patchTextFromToolCall(getToolName(toolCall), getToolArguments(toolCall));
  return patch === null ? "" : patch;
}

function getToolName(toolCall) {
  if (!toolCall || typeof toolCall !== "object") return "";
  if (toolCall.function) return toolCall.function.name || "";
  return toolCall.name || "";
}

function getToolArguments(toolCall) {
  const args = toolCall && toolCall.function ? toolCall.function.arguments : toolCall && toolCall.arguments;
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

function normalizeModelFunctionTools(tool, index = 0) {
  if (!tool || typeof tool !== "object") return [];
  const namespace = namespaceFromTool(tool);
  const nestedTools = Array.isArray(tool.tools) ? tool.tools : [];
  if (namespace && nestedTools.length > 0) {
    return nestedTools
      .map((nestedTool, nestedIndex) => normalizeModelFunctionTool(nestedTool, nestedIndex, namespace))
      .filter(Boolean);
  }
  const declaration = normalizeModelFunctionTool(tool, index, namespace);
  return declaration ? [declaration] : [];
}

function normalizeModelFunctionTool(tool, index = 0, namespace = "") {
  if (!tool || typeof tool !== "object") return null;
  const nestedTool = tool.tool && typeof tool.tool === "object" ? tool.tool : null;
  const name = sanitizeToolName(
    tool.name
    || (tool.function ? tool.function.name : "")
    || (nestedTool ? nestedTool.name : "")
    || tool.server_label
    || "tool_" + (index + 1)
  );
  const description = tool.description
    || (tool.function ? tool.function.description : "")
    || (nestedTool ? nestedTool.description : "")
    || tool.title
    || name;
  const parameters = tool.parameters
    || (tool.function ? tool.function.parameters : null)
    || tool.input_schema
    || tool.inputSchema
    || schemaFromMcpTool(tool)
    || { type: "object", properties: {} };
  if (!isFunctionTool(tool) && !isModelCallableTool(tool, parameters)) return null;
  return {
    name,
    description,
    parameters: normalizeSchema(parameters),
    namespace,
    kind: tool.mcp_helper ? "hosted_mcp_helper" : (tool.mcp_server || tool.server ? "hosted_mcp" : ""),
    mcpServer: tool.mcp_server || tool.server || namespace,
  };
}

function namespaceFromTool(tool) {
  const namespace = tool.namespace || tool.server_namespace || tool.server || "";
  if (namespace) return String(namespace);
  const type = String(tool.type || "").toLowerCase();
  if (type === "namespace") return String(tool.name || tool.server_label || "");
  if (type === "mcp" && Array.isArray(tool.tools)) return String(tool.name || tool.server_label || "");
  return "";
}

function isFunctionTool(tool) {
  return tool.type === "function" || Boolean(tool.function);
}

function isModelCallableTool(tool, parameters) {
  if (!tool || typeof tool !== "object") return false;
  const type = String(tool.type || "").toLowerCase();
  if (type === "mcp") return hasCallableShape(tool, parameters);
  if (tool.name || tool.server_label) return hasCallableShape(tool, parameters);
  return false;
}

function hasCallableShape(tool, parameters) {
  if (!tool || typeof tool !== "object") return false;
  if (tool.name) return true;
  if (tool.server_label && (tool.description || parameters)) return true;
  return false;
}

function schemaFromMcpTool(tool) {
  if (!tool || typeof tool !== "object") return null;
  if (tool.tool && typeof tool.tool === "object") {
    return tool.tool.input_schema || tool.tool.inputSchema || tool.tool.parameters || null;
  }
  return null;
}

function sanitizeToolName(name) {
  return String(name || "tool").replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 64) || "tool";
}

function uniqueToolName(name, namespace, byName) {
  const base = sanitizeToolName(name);
  if (!byName || !byName.has(base)) return base;
  const prefix = namespace ? sanitizeToolName(namespace + "_" + base) : base;
  let candidate = prefix;
  let suffix = 2;
  while (byName.has(candidate)) {
    candidate = sanitizeToolName(prefix + "_" + suffix);
    suffix += 1;
  }
  return candidate;
}

function selectModelToolName(responseName, declaration, byName, upstreamTools) {
  const base = sanitizeToolName(responseName);
  if (declaration && declaration.kind === "hosted_mcp_helper") {
    const existing = byName && byName.get(base);
    if (!existing || existing.kind === "function") {
      if (existing) removeUpstreamTool(upstreamTools, base);
      if (byName) byName.delete(base);
      return base;
    }
  }
  return uniqueToolName(responseName, declaration.namespace, byName);
}

function removeUpstreamTool(upstreamTools, name) {
  const index = (Array.isArray(upstreamTools) ? upstreamTools : []).findIndex((tool) => (
    tool && tool.function && tool.function.name === name
  ));
  if (index >= 0) upstreamTools.splice(index, 1);
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
