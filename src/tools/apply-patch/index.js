const { makeId } = require("../../shared/http");

const TOOL_NAME = "apply_patch";
const LEGACY_TOOL_NAME = "apply_patch_proxy";

function modelTool() {
  return {
    type: "function",
    function: {
      name: TOOL_NAME,
      description: [
        "Apply a Codex-style patch to create, update, move, or delete local text files.",
        "Use this as the default path for text file edits, including Markdown reports, source code, config files, and docs.",
        "Use exact operation headers: *** Add File: <path>, *** Update File: <path>, and *** Delete File: <path>. Never write Create:, Create File:, Add:, Modify:, or other invented patch headers.",
        "For new files, every content line must begin with + after the *** Add File: <path> header.",
        "Do not use shell redirection, Out-File, Set-Content, WriteAllText, or similar full-file rewrites for routine text edits unless the user explicitly requests a non-patch repair or binary/non-text handling.",
        "Large edits are valid: replace or add whole logical sections in one patch; if a patch conflicts or is too large, split by file or section rather than switching to shell rewriting.",
        "Preserve existing encoding and line endings where possible, and avoid introducing UTF-8 BOMs or hidden formatting artifacts.",
      ].join(" "),
      parameters: {
        type: "object",
        properties: {
          patch: {
            type: "string",
            description: "Patch text beginning with *** Begin Patch and ending with *** End Patch. New files must use *** Add File: <path>, not Create: or Create File:.",
          },
        },
        required: ["patch"],
        additionalProperties: false,
      },
    },
  };
}

function matchesInputTool(tool) {
  const name = toolNameFromDeclaration(tool);
  return isToolName(name) || String((tool && tool.type) || "").toLowerCase() === TOOL_NAME;
}

function registerInputTool(tool, state) {
  const name = toolNameFromDeclaration(tool);
  if (name === LEGACY_TOOL_NAME) {
    state.byName.set(LEGACY_TOOL_NAME, { kind: "internal_apply_patch_legacy", nativeTool: tool || null });
    return true;
  }
  state.byName.set(TOOL_NAME, { kind: "internal_apply_patch", nativeTool: tool || null });
  return true;
}

function registerLegacyAlias(state) {
  state.byName.set(TOOL_NAME, { kind: "internal_apply_patch", nativeTool: null });
  state.byName.set(LEGACY_TOOL_NAME, { kind: "internal_apply_patch_legacy", nativeTool: null });
}

function normalizeToolChoiceName(name) {
  return name === LEGACY_TOOL_NAME ? TOOL_NAME : name;
}

function isToolName(name) {
  return String(name || "") === TOOL_NAME || String(name || "") === LEGACY_TOOL_NAME;
}

function isShellPatchCall(toolCall, getToolName, getToolArguments) {
  return getToolName(toolCall) === "shell" && patchTextFromToolCall("shell", getToolArguments(toolCall)) !== null;
}

function responseItemFromChatTool(toolCall, context) {
  const name = context.getToolName(toolCall);
  const args = context.getToolArguments(toolCall);
  const patch = patchTextFromToolCall(name, args);
  return responseShellApplyPatchItem(toolCall, patch === null ? normalizeApplyPatchInput(args) : patch);
}

function chatToolCallFromResponseItem(item, helpers) {
  if (item && item.type === "apply_patch_call") {
    return chatToolCall(item, patchFromApplyPatchOperation(item.operation));
  }
  if (item && item.type === "custom_tool_call" && item.name === TOOL_NAME) {
    return chatToolCall(item, normalizePatchText(item.input));
  }
  if (item && item.type === "function_call" && isToolName(item.name)) {
    return chatToolCall(item, normalizeApplyPatchInput(helpers.normalizeResponseToolArguments(item)));
  }
  if (item && item.type === "function_call" && item.name === "shell") {
    const patch = patchTextFromToolCall("shell", helpers.normalizeResponseToolArguments(item));
    if (patch !== null) return chatToolCall(item, patch);
  }
  return null;
}

function chatToolCall(item, patch) {
  return {
    id: item.call_id || item.id || makeId("call"),
    type: "function",
    function: {
      name: TOOL_NAME,
      arguments: JSON.stringify({ patch }),
    },
  };
}

function responseShellApplyPatchItem(toolCall, patch) {
  return {
    id: makeId("fc"),
    type: "function_call",
    call_id: toolCall.id || makeId("call"),
    name: "shell",
    arguments: JSON.stringify({ command: ["apply_patch", normalizePatchText(repairApplyPatch(patch))] }),
    status: "completed",
  };
}

function patchTextFromToolCall(name, argsText) {
  if (isToolName(name)) return normalizeApplyPatchInput(argsText);
  if (name === "shell") {
    const parsed = parseJson(argsText);
    const command = parsed && Array.isArray(parsed.command) ? parsed.command : null;
    if (command && command[0] === TOOL_NAME) return normalizePatchText(command[1]);
  }
  return null;
}

function repairApplyPatch(patch) {
  const text = normalizePatchText(patch).trim();
  if (!text) return text;

  const lines = text.split("\n");
  const hasOperation = lines.some(isPatchOperationHeader);
  if (hasOperation && lines[0] !== "*** Begin Patch") lines.unshift("*** Begin Patch");
  if (hasOperation && lines[lines.length - 1] !== "*** End Patch") lines.push("*** End Patch");

  const repaired = [];
  let operation = "";
  for (let index = 0; index < lines.length; index += 1) {
    const line = normalizePatchOperationHeader(lines[index]);
    if (line.startsWith("*** Add File: ")) {
      operation = "add";
      repaired.push(line);
      continue;
    }
    if (line.startsWith("*** Update File: ")) {
      operation = "update";
      repaired.push(line);
      continue;
    }
    if (line.startsWith("*** Delete File: ")) {
      operation = "delete";
      repaired.push(line);
      continue;
    }
    if (line === "*** Begin Patch" || line === "*** End Patch") {
      operation = "";
      repaired.push(line);
      continue;
    }

    if (operation === "add") {
      if (isLooseAddContentInstruction(line)) continue;
      repaired.push(line.startsWith("+") ? line : "+" + line);
      continue;
    }

    if (operation === "update" && line.startsWith("@@")) {
      repaired.push(repairUpdateHunkHeader(line, nextPatchLine(lines, index + 1)));
      continue;
    }

    repaired.push(line);
  }

  return repaired.join("\n");
}

function normalizePatchOperationHeader(line) {
  const text = String(line || "");
  if (text.startsWith("*** Create File: ")) return "*** Add File: " + text.slice("*** Create File: ".length).trim();
  if (text.startsWith("*** Add: ")) return "*** Add File: " + text.slice("*** Add: ".length).trim();
  if (text.startsWith("*** Modify File: ")) return "*** Update File: " + text.slice("*** Modify File: ".length).trim();
  return text;
}

function isPatchOperationHeader(line) {
  return /^\*\*\*\s+(?:Add|Create|Delete|Update|Modify)(?:\s+File)?:\s+/.test(String(line || ""));
}

function isLooseAddContentInstruction(line) {
  return /^\*\*\*\s*(?:add|insert|write)\s+(?:the\s+)?(?:following\s+)?content\s*:?\s*$/i.test(String(line || "").trim());
}

function repairUpdateHunkHeader(line, nextLine) {
  const header = String(line || "").slice(2).trim();
  if (!header) return "@@";
  if (isUnifiedDiffRangeHeader(header)) return "@@";
  if (nextLine && nextLine.startsWith("-") && nextLine.slice(1).trimEnd() === header.trimEnd()) return "@@";
  if (nextLine && nextLine.startsWith(" ") && nextLine.slice(1).trimEnd() === header.trimEnd()) return "@@";
  return line;
}

function isUnifiedDiffRangeHeader(header) {
  return /^[-+]?\d+(?:,\d+)?(?:\s+[-+]?\d+(?:,\d+)?)?\s*@@?$/.test(String(header || ""))
    || /^-\d+(?:,\d+)?\s+\+\d+(?:,\d+)?\s*@@?$/.test(String(header || ""));
}

function nextPatchLine(lines, startIndex) {
  for (let index = startIndex; index < lines.length; index += 1) {
    const line = lines[index];
    if (line === "") continue;
    return line;
  }
  return "";
}

function patchFromApplyPatchOperation(operation) {
  const op = operation && typeof operation === "object" ? operation : {};
  const type = op.type || "update_file";
  const filePath = op.path || "";
  const diff = normalizePatchText(op.diff || "");
  const lines = ["*** Begin Patch"];
  if (type === "create_file") lines.push("*** Add File: " + filePath);
  else if (type === "delete_file") lines.push("*** Delete File: " + filePath);
  else lines.push("*** Update File: " + filePath);
  if (type !== "delete_file" && diff) lines.push(diff);
  lines.push("*** End Patch");
  return lines.join("\n");
}

function normalizeShellArguments(name, argsText) {
  if (name !== "shell") return argsText || "{}";
  const parsed = parseJson(argsText);
  if (!parsed || typeof parsed !== "object") return argsText || "{}";

  if (typeof parsed.command === "string") {
    parsed.command = parseJsonStringArray(parsed.command) || platformShellCommand(unwrapPowerShellCommandText(parsed.command) || parsed.command);
  } else if (Array.isArray(parsed.command)) {
    parsed.command = unwrapNestedShellCommand(parsed.command) || parsed.command;
  }
  if (Array.isArray(parsed.command) && parsed.command[0] === TOOL_NAME) {
    parsed.command = [TOOL_NAME, normalizePatchText(parsed.command[1])];
  }
  return JSON.stringify(parsed);
}

function platformShellCommand(commandText) {
  if (process.platform === "win32") return ["powershell.exe", "-Command", commandText];
  return ["sh", "-lc", commandText];
}

function parseJsonStringArray(value) {
  if (typeof value !== "string") return null;
  try {
    const parsed = JSON.parse(value);
    return Array.isArray(parsed) && parsed.every((item) => typeof item === "string") ? parsed : null;
  } catch {
    return null;
  }
}

function unwrapNestedShellCommand(command) {
  if (!Array.isArray(command) || !command.every((item) => typeof item === "string")) return null;
  if (command.length === 1) return parseJsonStringArray(command[0]);

  if (command.length >= 3 && isPowerShellExecutable(command[0]) && /^-Command$/i.test(command[1])) {
    const innerArray = parseJsonStringArray(command[2]);
    if (innerArray) return innerArray;
    const innerScript = unwrapPowerShellCommandText(command[2]);
    if (innerScript) return [command[0], command[1], innerScript];
  }
  return null;
}

function isPowerShellExecutable(value) {
  return /(?:^|[\\/])(?:powershell|pwsh)(?:\.exe)?$/i.test(String(value || ""));
}

function unwrapPowerShellCommandText(value) {
  const text = String(value || "").trim();
  const match = text.match(/^(?:&\s*)?(?:powershell|pwsh)(?:\.exe)?\s+-Command\s+([\s\S]+)$/i);
  if (!match) return null;
  return unquoteShellText(match[1].trim());
}

function unquoteShellText(value) {
  const text = String(value || "").trim();
  if (text.length >= 2 && text[0] === "\"" && text[text.length - 1] === "\"") {
    return text.slice(1, -1).replace(/\\"/g, "\"");
  }
  if (text.length >= 2 && text[0] === "'" && text[text.length - 1] === "'") {
    return text.slice(1, -1).replace(/''/g, "'");
  }
  return text;
}

function normalizeApplyPatchInput(argsText) {
  const parsed = parseJson(argsText);
  if (parsed && Array.isArray(parsed.command) && parsed.command[0] === TOOL_NAME) return normalizePatchText(parsed.command[1]);
  if (parsed && typeof parsed.patch === "string") return normalizePatchText(parsed.patch);
  if (parsed && typeof parsed.input === "string") return normalizePatchText(parsed.input);
  return normalizePatchText(typeof argsText === "string" ? argsText : String(argsText || ""));
}

function normalizePatchText(value) {
  return String(value || "").replace(/\r\n/g, "\n").replace(/\r/g, "\n").replace(/\\r\\n/g, "\n").replace(/\\n/g, "\n");
}

function parseJson(value) {
  if (!value) return null;
  if (typeof value === "object" && !Array.isArray(value)) return value;
  if (typeof value !== "string") return null;
  try {
    const parsed = JSON.parse(value);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : null;
  } catch {
    const repaired = repairTruncatedJson(value);
    if (!repaired) return null;
    try {
      const parsed = JSON.parse(repaired);
      return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : null;
    } catch {
      return null;
    }
  }
}

function repairTruncatedJson(value) {
  const text = String(value || "").trim();
  if (!text.startsWith("{")) return null;
  const counts = countUnclosedDelimiters(text);
  if (!counts.unclosedBrackets && !counts.unclosedBraces) return null;

  let repaired = text;
  if (counts.unclosedBrackets > 0 && repaired.endsWith("}")) {
    repaired = repaired.slice(0, -1) + "]".repeat(counts.unclosedBrackets) + "}";
  }
  if (counts.unclosedBraces > 0) {
    repaired += "}".repeat(counts.unclosedBraces);
  }
  return repaired === text ? null : repaired;
}

function countUnclosedDelimiters(value) {
  let unclosedBrackets = 0;
  let unclosedBraces = 0;
  let inString = false;
  let escaped = false;

  for (const char of String(value || "")) {
    if (escaped) {
      escaped = false;
      continue;
    }
    if (char === "\\") {
      escaped = true;
      continue;
    }
    if (char === "\"") {
      inString = !inString;
      continue;
    }
    if (inString) continue;
    if (char === "[") unclosedBrackets += 1;
    else if (char === "]" && unclosedBrackets > 0) unclosedBrackets -= 1;
    else if (char === "{") unclosedBraces += 1;
    else if (char === "}" && unclosedBraces > 0) unclosedBraces -= 1;
  }

  return { unclosedBrackets, unclosedBraces };
}

function toolNameFromDeclaration(tool) {
  if (!tool || typeof tool !== "object") return "";
  return String(tool.name || (tool.function ? tool.function.name : "") || tool.server_label || tool.type || "").trim();
}

module.exports = {
  LEGACY_TOOL_NAME,
  TOOL_NAME,
  chatToolCallFromResponseItem,
  isShellPatchCall,
  isToolName,
  matchesInputTool,
  modelTool,
  normalizeShellArguments,
  normalizeToolChoiceName,
  parseJson,
  patchFromApplyPatchOperation,
  patchTextFromToolCall,
  registerInputTool,
  registerLegacyAlias,
  repairApplyPatch,
  responseItemFromChatTool,
};
