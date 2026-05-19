const fs = require("node:fs");
const path = require("node:path");

const { makeId } = require("../../shared/http");

const TOOL_NAME = "list_directory";
const MAX_DEPTH = 4;
const MAX_ENTRIES = 200;
const SKIP_DIRS = new Set([
  ".git",
  "node_modules",
  "dist",
  "build",
  "coverage",
  "debug",
  "logs",
  ".next",
  ".cache",
  "tmp",
  "temp",
]);

function matchesInputTool(tool) {
  return String((tool && tool.type) || "").toLowerCase() === TOOL_NAME;
}

function registerInputTool(tool, state) {
  if (!state.byName.has(TOOL_NAME)) state.upstreamTools.push(modelTool(tool));
  state.byName.set(TOOL_NAME, { kind: "hosted_list_directory", nativeTool: tool });
}

function modelTool(tool = {}) {
  return {
    type: "function",
    function: {
      name: TOOL_NAME,
      description: tool.description || "List files and folders in a local directory. Relative paths resolve from the current workspace; absolute paths are accepted only when the host has full file access. Use this before read_file_range when the user asks about a directory or folder structure.",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string" },
          depth: { type: "integer" },
          include_files: { type: "boolean" },
          include_dirs: { type: "boolean" },
        },
        required: ["path"],
        additionalProperties: false,
      },
    },
  };
}

function matchesChatTool(toolCall, context) {
  return hostedKind(context.getToolName(toolCall), context) === "hosted_list_directory";
}

function responseItemFromChatTool(toolCall, context, helpers) {
  return {
    id: makeId("ld"),
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

function executeListDirectory(args, config = {}) {
  const parsed = parseArgs(args);
  const rootDir = workspaceRoot(config);
  const requestedPath = String(parsed.path || "").trim();
  if (!requestedPath) return { ok: false, error: "missing_path", message: "list_directory requires a path.", path: "" };

  const resolved = resolveInsideRoot(rootDir, requestedPath);
  if (!resolved.ok) return resolved;
  const stat = safeStat(resolved.path);
  if (!stat) return { ok: false, error: "not_found", message: "The requested path does not exist.", path: resolved.relative };
  if (!stat.isDirectory()) {
    return {
      ok: false,
      error: "not_directory",
      message: "The requested path is not a directory. Use read_file_range for files.",
      path: resolved.relative,
    };
  }

  if (parsed.depth !== undefined && (!Number.isFinite(Number(parsed.depth)) || Number(parsed.depth) < 0)) {
    return { ok: false, error: "invalid_depth", message: "Depth must be a non-negative integer." };
  }

  const depth = clampInt(parsed.depth, 1, 0, MAX_DEPTH);
  const includeFiles = parsed.include_files !== false;
  const includeDirs = parsed.include_dirs !== false;
  const entries = [];
  walkDirectory(resolved.path, resolved.relative, 0, depth, entries, includeFiles, includeDirs);

  return {
    ok: true,
    path: resolved.relative,
    depth,
    entries: entries.slice(0, MAX_ENTRIES),
    truncated: entries.length > MAX_ENTRIES,
  };
}

function walkDirectory(absoluteDir, relativeDir, currentDepth, maxDepth, entries, includeFiles, includeDirs) {
  if (entries.length > MAX_ENTRIES) return;
  if (currentDepth > maxDepth) return;
  let dirEntries = [];
  try {
    dirEntries = fs.readdirSync(absoluteDir, { withFileTypes: true });
  } catch {
    return;
  }
  dirEntries.sort((left, right) => left.name.localeCompare(right.name));
  for (const entry of dirEntries) {
    if (entries.length > MAX_ENTRIES) return;
    const relPath = relativeDir ? path.posix.join(relativeDir.replace(/\\/g, "/"), entry.name) : entry.name;
    if (entry.isDirectory()) {
      if (includeDirs) entries.push({ type: "dir", path: relPath });
      if (currentDepth < maxDepth && !SKIP_DIRS.has(entry.name)) walkDirectory(path.join(absoluteDir, entry.name), relPath, currentDepth + 1, maxDepth, entries, includeFiles, includeDirs);
      continue;
    }
    if (entry.isFile() && includeFiles) entries.push({ type: "file", path: relPath });
  }
}

function hostedKind(name, context) {
  const entry = context && context.byName ? context.byName.get(name) : null;
  return entry ? entry.kind : "";
}

function workspaceRoot(config) {
  const configured = (config && config.rootDir) || process.cwd();
  return path.resolve(String(configured || process.cwd()));
}

function resolveInsideRoot(rootDir, requestedPath) {
  const raw = String(requestedPath || "").trim();
  if (!raw) return { ok: false, error: "missing_path", message: "list_directory requires a path." };
  const resolved = path.resolve(rootDir, raw);
  const normalized = path.normalize(resolved);
  const relative = path.relative(rootDir, normalized);
  if (relative.startsWith("..") || path.isAbsolute(relative)) {
    return { ok: false, error: "path_outside_workspace", message: "Path must stay inside the workspace.", path: raw };
  }
  return { ok: true, path: normalized, relative: relative.replace(/\\/g, "/") };
}

function safeStat(filePath) {
  try {
    return fs.statSync(filePath);
  } catch {
    return null;
  }
}

function parseArgs(args) {
  if (!args) return {};
  if (typeof args === "object" && !Array.isArray(args)) return args;
  try {
    const parsed = JSON.parse(String(args));
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

function clampInt(value, fallback, min, max) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.min(max, Math.max(min, Math.floor(parsed)));
}

module.exports = {
  chatToolCallFromResponseItem,
  executeListDirectory,
  matchesChatTool,
  matchesInputTool,
  matchesResponseItem,
  modelTool,
  registerInputTool,
  responseItemFromChatTool,
};
