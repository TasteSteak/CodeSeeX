const fs = require("node:fs");
const path = require("node:path");

const DEFAULT_MAX_RESULTS = 30;
const DEFAULT_CONTEXT_LINES = 1;
const MAX_CONTEXT_LINES = 3;
const MAX_FILE_BYTES = 1024 * 1024;
const MAX_MATCH_LINE_BYTES = 120;
const MAX_QUERY_LENGTH = 1024;
const MAX_READ_LINES = 240;
const MAX_READ_BYTES = 24000;
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

function executeWorkspaceSearch(args, config = {}) {
  const parsed = parseArgs(args);
  const query = String(parsed.query ?? parsed.pattern ?? "").trim();
  const scope = workspaceScope(config);
  const searchRoot = resolveWorkspacePath(scope, parsed.path || parsed.root || parsed.cwd || parsed.directory || ".");
  if (!searchRoot.ok) return searchRoot;
  const rootDir = searchRoot.path;
  const displayRoot = searchRoot.root || scope.rootDir;
  if (!query) return { ok: false, error: "missing_query", message: "workspace_search requires a non-empty query." };
  if (query.length > MAX_QUERY_LENGTH) return { ok: false, error: "query_too_long", message: `Search query must be at most ${MAX_QUERY_LENGTH} characters.` };
  if (looksUnsafePattern(query)) return { ok: false, error: "unsafe_query", message: "The query is too broad or unsafe. Use a more specific literal string." };

  if (parsed.max_results !== undefined) {
    const mr = Number(parsed.max_results);
    if (!Number.isFinite(mr) || mr < 1) return { ok: false, error: "invalid_max_results", message: "max_results must be a positive integer." };
    if (mr > 80) return { ok: false, error: "max_results_too_large", message: "max_results must be at most 80." };
  }
  const maxResults = clampInt(parsed.max_results, DEFAULT_MAX_RESULTS, 1, 80);
  const contextLines = clampInt(parsed.context_lines, DEFAULT_CONTEXT_LINES, 0, MAX_CONTEXT_LINES);
  const includeGlobs = normalizeList(parsed.include || parsed.includes || parsed.files);
  const excludeGlobs = normalizeList(parsed.exclude || parsed.excludes);
  const caseSensitive = Boolean(parsed.case_sensitive);
  const needle = caseSensitive ? query : query.toLowerCase();
  const results = [];

  for (const filePath of walkFiles(rootDir)) {
    if (results.length >= maxResults) break;
    const rel = toPortableRelative(displayRoot, filePath);
    if (!matchesGlobs(rel, includeGlobs, true) || matchesGlobs(rel, excludeGlobs, false)) continue;
    const stat = safeStat(filePath);
    if (!stat || stat.size > MAX_FILE_BYTES || isProbablyBinary(filePath)) continue;
    const text = readUtf8(filePath);
    if (text === null) continue;
    const lines = text.split(/\r?\n/);
    for (let index = 0; index < lines.length && results.length < maxResults; index += 1) {
      const haystack = caseSensitive ? lines[index] : lines[index].toLowerCase();
      if (!haystack.includes(needle)) continue;
      const ctx = contextLines > 0
        ? contextTextSlice(lines, index - contextLines, index).concat(contextTextSlice(lines, index + 1, index + 1 + contextLines))
        : [];
      const result = {
        p: rel,
        l: index + 1,
        s: matchSnippet(lines[index], query, caseSensitive),
      };
      if (ctx.length > 0) result.c = ctx;
      results.push(result);
    }
  }

  return {
    ok: true,
    q: query,
    path: searchRoot.relative,
    n: results.length,
    r: results,
  };
}

function executeReadFileRange(args, config = {}) {
  const parsed = parseArgs(args);
  const scope = workspaceScope(config);
  const requestedPath = parsed.path || parsed.file || parsed.src;
  const resolved = resolveWorkspacePath(scope, requestedPath);
  if (!resolved.ok) return resolved;
  const stat = safeStat(resolved.path);
  if (!stat) return { ok: false, error: "not_found", message: "The requested path does not exist.", path: String(requestedPath || "") };
  if (stat.isDirectory()) {
    return {
      ok: false,
      error: "is_directory",
      message: "The requested path is a directory. Use list_directory to inspect folders, then read_file_range for files.",
      path: resolved.relative,
    };
  }
  if (!stat.isFile()) return { ok: false, error: "not_file", message: "The requested path is not a file.", path: resolved.relative };
  if (stat.size > MAX_FILE_BYTES) return { ok: false, error: "file_too_large", message: "File is too large for read_file_range.", path: resolved.relative, bytes: stat.size };
  if (isProbablyBinary(resolved.path)) return { ok: false, error: "binary_file", message: "Binary files are not readable through read_file_range.", path: resolved.relative };

  const text = readUtf8(resolved.path);
  if (text === null) return { ok: false, error: "read_failed", message: "Unable to read file as UTF-8 text.", path: resolved.relative };
  const lines = text.split(/\r?\n/);
  const rawStart = parsed.start ?? parsed.line;
  const rawEnd = parsed.end ?? parsed.stop;
  const rawCount = parsed.count;

  if (rawStart !== undefined && (!Number.isFinite(Number(rawStart)) || Number(rawStart) < 1)) {
    return { ok: false, error: "invalid_range", message: "start/line must be a positive integer." };
  }
  if (rawStart !== undefined && Number(rawStart) > lines.length) {
    return { ok: false, error: "line_out_of_range", message: `start line ${rawStart} exceeds file length of ${lines.length} lines.` };
  }
  if (rawEnd !== undefined && (!Number.isFinite(Number(rawEnd)) || Number(rawEnd) < 1)) {
    return { ok: false, error: "invalid_range", message: "end/stop must be a positive integer." };
  }
  if (rawCount !== undefined && (!Number.isFinite(Number(rawCount)) || Number(rawCount) < 1)) {
    return { ok: false, error: "invalid_range", message: "count must be a positive integer." };
  }

  const start = clampInt(rawStart ?? 1, 1, 1, Math.max(1, lines.length));

  const requestedEnd = rawEnd ?? (start + clampInt(rawCount ?? 80, 80, 1, MAX_READ_LINES) - 1);
  const end = clampInt(requestedEnd, Math.min(lines.length, start + 79), start, Math.min(lines.length, start + MAX_READ_LINES - 1));
  const selected = lines.slice(start - 1, end);
  const numbered = [];
  let bytes = 0;
  let truncatedByBytes = false;
  for (let index = 0; index < selected.length; index += 1) {
    const lineNo = start + index;
    const line = truncateLine(selected[index], 2000);
    const rendered = String(lineNo).padStart(String(end).length, " ") + ": " + line;
    const nextBytes = Buffer.byteLength(rendered + "\n", "utf8");
    if (bytes + nextBytes > MAX_READ_BYTES) {
      truncatedByBytes = true;
      break;
    }
    bytes += nextBytes;
    numbered.push(rendered);
  }

  return {
    ok: true,
    path: resolved.relative,
    start,
    end: start + numbered.length - 1,
    total_lines: lines.length,
    content: numbered.join("\n"),
    truncated: truncatedByBytes || end < Number(requestedEnd),
  };
}

function executeListDirectory(args, config = {}) {
  const parsed = parseArgs(args);
  const scope = workspaceScope(config);
  const requestedPath = String(parsed.path || "").trim();
  if (!requestedPath) return { ok: false, error: "missing_path", message: "list_directory requires a path.", path: "" };

  const resolved = resolveWorkspacePath(scope, requestedPath);
  if (!resolved.ok) return resolved;
  const stat = safeStat(resolved.path);
  if (!stat) return { ok: false, error: "not_found", message: "The requested path does not exist.", path: resolved.relative };
  if (!stat.isDirectory()) return { ok: false, error: "not_directory", message: "The requested path is not a directory. Use read_file_range for files.", path: resolved.relative };

  if (parsed.depth !== undefined && (!Number.isFinite(Number(parsed.depth)) || Number(parsed.depth) < 0)) {
    return { ok: false, error: "invalid_depth", message: "Depth must be a non-negative integer." };
  }

  const depth = clampInt(parsed.depth, 1, 0, 4);
  const includeFiles = parsed.include_files !== false;
  const includeDirs = parsed.include_dirs !== false;
  const entries = [];
  walkDirectory(resolved.path, resolved.relative, 0, depth, entries, includeFiles, includeDirs);

  return {
    ok: true,
    path: resolved.relative,
    depth,
    entries: entries.slice(0, 200),
    truncated: entries.length > 200,
  };
}

function workspaceScope(config = {}) {
  const rawScope = config.workspaceScope && typeof config.workspaceScope === "object" ? config.workspaceScope : null;
  const rootDir = normalizeRootPath((rawScope && rawScope.rootDir) || config.rootDir || process.cwd());
  const roots = (rawScope && Array.isArray(rawScope.roots) ? rawScope.roots : [rootDir])
    .map(normalizeRootPath)
    .filter(Boolean);
  if (!roots.some((root) => samePath(root, rootDir))) roots.unshift(rootDir);
  return {
    rootDir,
    roots: dedupePaths(roots.length > 0 ? roots : [rootDir]),
    allowOutsideWorkspace: Boolean(rawScope && rawScope.allowOutsideWorkspace),
  };
}

function* walkFiles(rootDir) {
  const stack = [rootDir];
  while (stack.length > 0) {
    const current = stack.pop();
    let entries = [];
    try {
      entries = fs.readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const fullPath = path.join(current, entry.name);
      if (entry.isDirectory()) {
        if (!SKIP_DIRS.has(entry.name)) stack.push(fullPath);
      } else if (entry.isFile()) {
        yield fullPath;
      }
    }
  }
}

function walkDirectory(absoluteDir, relativeDir, currentDepth, maxDepth, entries, includeFiles, includeDirs) {
  if (entries.length > 200) return;
  if (currentDepth > maxDepth) return;
  let dirEntries = [];
  try {
    dirEntries = fs.readdirSync(absoluteDir, { withFileTypes: true });
  } catch {
    return;
  }
  dirEntries.sort((left, right) => left.name.localeCompare(right.name));
  for (const entry of dirEntries) {
    if (entries.length > 200) return;
    const relPath = relativeDir ? path.posix.join(relativeDir.replace(/\\/g, "/"), entry.name) : entry.name;
    if (entry.isDirectory()) {
      if (includeDirs) entries.push({ type: "dir", path: relPath });
      if (currentDepth < maxDepth && !SKIP_DIRS.has(entry.name)) walkDirectory(path.join(absoluteDir, entry.name), relPath, currentDepth + 1, maxDepth, entries, includeFiles, includeDirs);
      continue;
    }
    if (entry.isFile() && includeFiles) entries.push({ type: "file", path: relPath });
  }
}

function resolveWorkspacePath(scope, requestedPath) {
  const raw = String(requestedPath || "").trim();
  if (!raw) return { ok: false, error: "missing_path", message: "A path is required." };
  const rootDir = scope.rootDir;
  const absoluteInput = path.isAbsolute(raw);
  const normalized = path.normalize(absoluteInput ? path.resolve(raw) : path.resolve(rootDir, raw));
  const containingRoot = findContainingRoot(scope.roots, normalized);
  if (!containingRoot && !(scope.allowOutsideWorkspace && absoluteInput)) {
    return {
      ok: false,
      error: "path_outside_workspace",
      message: "Path must stay inside the workspace unless full file access is enabled and an absolute path is provided.",
      path: raw,
      workspace_roots: scope.roots.map(toPortablePath),
    };
  }
  const displayRoot = containingRoot || rootDir;
  return {
    ok: true,
    path: normalized,
    root: displayRoot,
    relative: containingRoot ? toPortableRelative(displayRoot, normalized) : toPortablePath(normalized),
    outside_workspace: !containingRoot,
  };
}

function safeStat(filePath) {
  try {
    return fs.statSync(filePath);
  } catch {
    return null;
  }
}

function readUtf8(filePath) {
  try {
    const buffer = fs.readFileSync(filePath);
    if (buffer.includes(0)) return null;
    return buffer.toString("utf8");
  } catch {
    return null;
  }
}

function isProbablyBinary(filePath) {
  const ext = path.extname(filePath).toLowerCase();
  return [".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".zip", ".7z", ".exe", ".dll", ".node", ".bin", ".lockb"].includes(ext);
}

function contextTextSlice(lines, start, end) {
  const output = [];
  for (let index = Math.max(0, start); index < Math.min(lines.length, end); index += 1) {
    const text = truncateLine(lines[index]).trim();
    if (text) output.push(text);
  }
  return output;
}

function matchSnippet(value, query, caseSensitive, limit = MAX_MATCH_LINE_BYTES) {
  const text = String(value || "");
  if (Buffer.byteLength(text, "utf8") <= limit) return text;
  const haystack = caseSensitive ? text : text.toLowerCase();
  const needle = caseSensitive ? String(query || "") : String(query || "").toLowerCase();
  const matchIndex = needle ? haystack.indexOf(needle) : -1;
  if (matchIndex < 0) return truncateLine(text, limit);

  const matchEnd = matchIndex + needle.length;
  let start = Math.max(0, matchIndex - Math.floor((limit - needle.length) / 2));
  let end = Math.min(text.length, matchEnd + Math.floor((limit - needle.length) / 2));
  let snippet = text.slice(start, end);
  while (Buffer.byteLength((start > 0 ? "..." : "") + snippet + (end < text.length ? "..." : ""), "utf8") > limit && snippet.length > needle.length) {
    if (matchIndex - start > end - matchEnd && start < matchIndex) start += 1;
    else if (end > matchEnd) end -= 1;
    else break;
    snippet = text.slice(start, end);
  }
  return (start > 0 ? "..." : "") + snippet + (end < text.length ? "..." : "");
}

function truncateLine(value, limit = MAX_MATCH_LINE_BYTES) {
  const text = String(value || "");
  if (Buffer.byteLength(text, "utf8") <= limit) return text;
  let output = "";
  for (const char of text) {
    if (Buffer.byteLength(output + char + "...", "utf8") > limit) break;
    output += char;
  }
  return output + "...";
}

function matchesGlobs(relPath, globs, defaultValue) {
  if (!globs.length) return defaultValue;
  return globs.some((glob) => globMatches(relPath, glob));
}

function globMatches(relPath, glob) {
  const pathText = relPath.replace(/\\/g, "/");
  let pattern = String(glob || "").replace(/\\/g, "/");
  if (!pattern) return false;
  if (!pattern.includes("*")) return pathText.includes(pattern);
  let leadOpt = "";
  if (pattern.startsWith("**/")) { leadOpt = "(?:|.*/)"; pattern = pattern.slice(3); }
  else if (pattern === "**") { leadOpt = ".*"; pattern = ""; }
  let trailOpt = "";
  if (pattern.endsWith("/**")) { trailOpt = "(?:/.*)?"; pattern = pattern.slice(0, -3); }
  else if (pattern.endsWith("**")) { trailOpt = "(?:/.*)?"; pattern = pattern.slice(0, -2); }
  const parts = pattern.split(/(\*\*|\*)/g);
  let regexStr = "^" + leadOpt;
  for (const part of parts) {
    if (part === "**") { regexStr += "(?:|.*/)"; }
    else if (part === "*") { regexStr += ".*"; }
    else { regexStr += escapeRegExp(part); }
  }
  regexStr += trailOpt + "$";
  return new RegExp(regexStr, "i").test(pathText);
}

function normalizeList(value) {
  if (Array.isArray(value)) return value.map((item) => String(item || "").trim()).filter(Boolean);
  if (typeof value === "string" && value.trim()) return value.split(",").map((item) => item.trim()).filter(Boolean);
  return [];
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

function toPortableRelative(rootDir, filePath) {
  return path.relative(rootDir, filePath).replace(/\\/g, "/");
}

function toPortablePath(filePath) {
  return String(filePath || "").replace(/\\/g, "/");
}

function normalizeRootPath(value) {
  const raw = String(value || "").trim();
  if (!raw) return "";
  try {
    return path.resolve(raw);
  } catch {
    return "";
  }
}

function dedupePaths(paths) {
  const output = [];
  for (const item of paths) {
    if (!item || output.some((existing) => samePath(existing, item))) continue;
    output.push(item);
  }
  return output;
}

function findContainingRoot(roots, target) {
  const sorted = (Array.isArray(roots) ? roots : [])
    .filter(Boolean)
    .sort((left, right) => right.length - left.length);
  return sorted.find((root) => isInsidePath(target, root)) || "";
}

function isInsidePath(target, root) {
  if (samePath(target, root)) return true;
  const relative = path.relative(root, target);
  return Boolean(relative && !relative.startsWith("..") && !path.isAbsolute(relative));
}

function samePath(left, right) {
  const a = comparablePath(left);
  const b = comparablePath(right);
  return Boolean(a && b && a === b);
}

function comparablePath(value) {
  const resolved = path.resolve(String(value || ""));
  return process.platform === "win32" ? resolved.toLowerCase() : resolved;
}

function looksUnsafePattern(query) {
  return query.length < 2 || query === "*" || query === "." || query === "/";
}

function escapeRegExp(value) {
  return String(value || "").replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

module.exports = {
  executeReadFileRange,
  executeListDirectory,
  executeWorkspaceSearch,
};
