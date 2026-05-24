const { makeId } = require("../../shared/http");

const TOOL_NAME = "apply_patch";
const PATCH_FORMAT_DESCRIPTION = [
  "CodeSeeX adapts this Chat Completions function to Codex's native freeform apply_patch tool.",
  "Call this function with JSON arguments containing exactly the patch string in the `patch` field; CodeSeeX forwards only that raw patch text to Codex.",
  "Patch grammar:",
  "1. The first line must be `*** Begin Patch` and the last line must be `*** End Patch`.",
  "2. File operations are `*** Add File: <path>`, `*** Update File: <path>`, `*** Delete File: <path>`, and optional `*** Move to: <path>` after an update header.",
  "3. For added files, every content line must start with `+`.",
  "4. For updates, provide enough exact unchanged context lines starting with a space, removed lines starting with `-`, and added lines starting with `+`.",
  "5. Use a bare `@@` line only to separate update hunks when needed; do not put line numbers or anchor text after `@@`.",
  "6. Do not use unified diff headers such as `---`, `+++`, or `@@ -1,3 +1,3 @@`; do not invent headers such as `Create File`, `Edit File`, `Modify File`, or `Rename File`.",
  "Reliability rules: prefer small patches, use exact current file contents for unchanged context lines, and re-read the target file before retrying after a context mismatch. Do not retry from remembered context.",
].join(" ");

function modelTool(tool = {}) {
  const nativeFunction = tool && tool.function && typeof tool.function === "object" ? tool.function : {};
  return {
    type: "function",
    function: {
      name: TOOL_NAME,
      description: buildToolDescription(nativeFunction.description || tool.description),
      parameters: normalizeModelParameters(nativeFunction.parameters || tool.parameters),
    },
  };
}

function buildToolDescription(nativeDescription) {
  const text = sanitizeNativeToolDescription(nativeDescription);
  return text ? text + " " + PATCH_FORMAT_DESCRIPTION : PATCH_FORMAT_DESCRIPTION;
}

function sanitizeNativeToolDescription(nativeDescription) {
  return String(nativeDescription || "")
    .replace(/\bThis is a FREEFORM tool,\s*so do not wrap the patch in JSON\.?/gi, "")
    .replace(/\bdo not wrap the patch in JSON\.?/gi, "")
    .replace(/\s+/g, " ")
    .trim();
}

function normalizeModelParameters(parameters) {
  const source = parameters && typeof parameters === "object" && !Array.isArray(parameters) ? parameters : {};
  const patch = source.properties && source.properties.patch && typeof source.properties.patch === "object"
    ? source.properties.patch
    : {};
  return {
    type: "object",
    properties: {
      patch: Object.assign({}, patch, {
        type: "string",
        description: PATCH_FORMAT_DESCRIPTION,
      }),
    },
    required: ["patch"],
    additionalProperties: false,
  };
}

function matchesInputTool(tool) {
  const name = toolNameFromDeclaration(tool);
  return isToolName(name) || String((tool && tool.type) || "").toLowerCase() === TOOL_NAME;
}

function registerInputTool(tool, state) {
  upsertModelTool(state, modelTool(tool));
  state.byName.set(TOOL_NAME, { kind: "internal_apply_patch", nativeTool: tool || null });
  return true;
}

function matchesChatTool(toolCall, context) {
  return isToolName(context.getToolName(toolCall));
}

function responseItemFromChatTool(toolCall, context) {
  const patch = normalizeApplyPatchInput(context.getToolArguments(toolCall));
  return {
    id: makeId("ctc"),
    type: "custom_tool_call",
    call_id: toolCall.id || makeId("call"),
    name: TOOL_NAME,
    input: patch,
    status: "completed",
  };
}

function matchesResponseItem(item) {
  return Boolean(item && !item.namespace && item.name === TOOL_NAME && (
    item.type === "custom_tool_call"
    || item.type === "function_call"
  ));
}

function chatToolCallFromResponseItem(item, helpers) {
  if (!matchesResponseItem(item)) return null;
  const patch = item.type === "custom_tool_call"
    ? normalizePatchText(item.input)
    : normalizeApplyPatchInput(helpers.normalizeResponseToolArguments(item));
  return {
    id: item.call_id || item.id || makeId("call"),
    type: "function",
    function: {
      name: TOOL_NAME,
      arguments: JSON.stringify({ patch }),
    },
  };
}

function isToolName(name) {
  return String(name || "") === TOOL_NAME;
}

function normalizeToolChoiceName(name) {
  return isToolName(name) ? TOOL_NAME : name;
}

function normalizeApplyPatchInput(argsText) {
  const parsed = parseJson(argsText);
  if (parsed && typeof parsed.patch === "string") return normalizePatchText(parsed.patch);
  if (parsed && typeof parsed.input === "string") return normalizePatchText(parsed.input);
  if (parsed && Array.isArray(parsed.command) && parsed.command[0] === TOOL_NAME) {
    return normalizePatchText(parsed.command[1]);
  }
  return normalizePatchText(typeof argsText === "string" ? argsText : String(argsText || ""));
}

function normalizePatchText(value) {
  return String(value || "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n");
}

function parseJson(value) {
  if (!value) return null;
  if (typeof value === "object" && !Array.isArray(value)) return value;
  if (typeof value !== "string") return null;
  try {
    const parsed = JSON.parse(value);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function toolNameFromDeclaration(tool) {
  if (!tool || typeof tool !== "object") return "";
  return String(tool.name || (tool.function ? tool.function.name : "") || tool.server_label || tool.type || "").trim();
}

function upsertModelTool(state, tool) {
  if (!state || !Array.isArray(state.upstreamTools) || !tool || !tool.function) return;
  const index = state.upstreamTools.findIndex((entry) => entry && entry.function && entry.function.name === TOOL_NAME);
  if (index >= 0) state.upstreamTools[index] = tool;
  else state.upstreamTools.push(tool);
}

module.exports = {
  TOOL_NAME,
  chatToolCallFromResponseItem,
  isToolName,
  matchesChatTool,
  matchesInputTool,
  matchesResponseItem,
  modelTool,
  normalizeApplyPatchInput,
  normalizePatchText,
  normalizeToolChoiceName,
  parseJson,
  registerInputTool,
  responseItemFromChatTool,
  sanitizeNativeToolDescription,
};
