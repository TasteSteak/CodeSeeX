const fs = require("node:fs");
const path = require("node:path");

const applyPatch = require("./apply-patch");

// Tool contract:
// - manifest.json: client-facing metadata. Tool name and description are the single display source.
// - assets/icon.svg or assets/icon.png: optional fixed-name icon, auto-discovered.
// - modelTool(): optional model-facing function schema with hardcoded English descriptions.
// - matchesInputTool/registerInputTool: optional proxy registration hooks.
// - matchesChatTool/responseItemFromChatTool: optional chat-completions to Responses mapping.
// - matchesResponseItem/chatToolCallFromResponseItem: optional history replay mapping.
// Drop community tools into <dataDir>/extension/tools/<tool>/.
// Development can point PROXY_DATA_DIR at a scratch runtime directory.
function listToolModules(options = {}) {
  const includeCommunity = options.includeCommunity !== false;
  const rootDir = options.rootDir || process.env.PROXY_ROOT_DIR || path.resolve(__dirname, "..", "..");
  const extensionDir = options.extensionDir || process.env.PROXY_EXTENSION_DIR || path.join(rootDir, "extension");
  const modules = discoverBuiltInToolModules(options);
  if (includeCommunity) modules.push(...discoverCommunityToolModules(extensionDir));
  return dedupeToolModules(modules);
}

function listToolAdapters(options = {}) {
  return listToolModules(options).filter(hasAdapterHooks);
}

function listToolManifests(options = {}) {
  return listToolModules(options)
    .map((tool) => normalizeManifest(tool))
    .filter(Boolean);
}

function toolAssetFilePath(requestPath, options = {}) {
  const match = String(requestPath || "").match(/^\/tool-assets\/([^/]+)\/(icon\.(?:svg|png))$/i);
  if (!match) return null;
  const requestedId = normalizeToolId(decodeURIComponent(match[1]));
  const requestedFile = match[2].toLowerCase();
  for (const tool of listToolModules(options)) {
    const manifest = normalizeManifest(tool);
    if (!manifest || manifest.id !== requestedId) continue;
    const filePath = path.join(tool.__toolDir || "", "assets", requestedFile);
    if (!isSafeToolAssetPath(tool.__toolDir, filePath) || !fs.existsSync(filePath)) return null;
    return filePath;
  }
  return null;
}

function discoverBuiltInToolModules(options = {}) {
  return discoverToolPackageDirs(__dirname)
    .map((toolDir) => loadToolPackage(toolDir, "built-in", options))
    .filter(Boolean);
}

function discoverCommunityToolModules(extensionDir) {
  const toolDirs = discoverToolPackageDirs(path.join(extensionDir, "tools"));
  return toolDirs.map((toolDir) => loadToolPackage(toolDir, "community")).filter(Boolean);
}

function discoverToolPackageDirs(toolsDir) {
  let entries = [];
  try {
    entries = fs.readdirSync(toolsDir, { withFileTypes: true });
  } catch {
    return [];
  }

  return entries
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort((left, right) => left.localeCompare(right))
    .map((name) => path.join(toolsDir, name));
}

function loadToolPackage(toolDir, source, options = {}) {
  const manifestPath = path.join(toolDir, "manifest.json");
  const entryPath = path.join(toolDir, "index.js");
  const manifest = readManifest(manifestPath);
  if (source === "community" && !manifest) return null;
  if (!manifest && !fs.existsSync(entryPath)) return null;
  const allowCode = source !== "community" || communityToolCodeEnabled(options);
  const metadata = manifest && manifest.metadata && typeof manifest.metadata === "object" ? Object.assign({}, manifest.metadata) : {};
  if (source === "community" && fs.existsSync(entryPath) && !allowCode) {
    metadata.code_present = true;
    metadata.code_enabled = false;
    return {
      manifest: Object.assign({}, manifest, { metadata }),
      source,
      __toolDir: toolDir,
    };
  }

  try {
    const loaded = allowCode && fs.existsSync(entryPath) ? require(entryPath) : {};
    if (!loaded || typeof loaded !== "object") return null;
    return Object.assign({}, loaded, {
      manifest: manifest || loaded.manifest || fallbackManifest(toolDir),
      source,
      __toolDir: toolDir,
    });
  } catch (error) {
    const failedManifest = manifest || fallbackManifest(toolDir);
    return {
      manifest: Object.assign({}, failedManifest, {
        source,
        enabled: false,
        description: "Tool failed to load: " + (error && error.message ? error.message : String(error)),
        metadata: Object.assign({}, failedManifest.metadata || {}, { load_error: true }),
      }),
      source,
      __toolDir: toolDir,
    };
  }
}

function communityToolCodeEnabled(options = {}) {
  if (options.communityToolCodeEnabled !== undefined) return Boolean(options.communityToolCodeEnabled);
  return /^(1|true|yes|on|enabled)$/i.test(String(process.env.COMMUNITY_TOOL_CODE_ENABLED || "").trim());
}

function readManifest(filePath) {
  try {
    const parsed = JSON.parse(fs.readFileSync(filePath, "utf8"));
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function fallbackManifest(toolDir) {
  const id = path.basename(toolDir);
  return {
    id,
    kind: "tool",
    name: id,
    description: "",
  };
}

function dedupeToolModules(modules) {
  const seen = new Set();
  const output = [];
  for (const tool of modules) {
    const id = normalizeToolId(tool && tool.manifest && tool.manifest.id);
    if (!id || seen.has(id)) continue;
    seen.add(id);
    output.push(tool);
  }
  return output;
}

function normalizeManifest(tool) {
  if (!tool || typeof tool !== "object") return null;
  const manifest = normalizeManifestSource(tool.manifest, tool.source || "community");
  if (!manifest || typeof manifest !== "object") return null;
  const id = normalizeToolId(manifest.id);
  if (!id) return null;
  const iconPath = fixedIconPath(tool.__toolDir, id) || manifest.iconPath || "";
  return Object.assign({}, manifest, { id, iconPath });
}

function normalizeManifestSource(manifest, fallbackSource) {
  if (!manifest || typeof manifest !== "object") return null;
  return Object.assign({}, manifest, {
    source: manifest.source || fallbackSource || "community",
  });
}

function fixedIconPath(toolDir, toolId) {
  if (!toolDir || !toolId) return "";
  const svgPath = path.join(toolDir, "assets", "icon.svg");
  if (fs.existsSync(svgPath)) return "/tool-assets/" + encodeURIComponent(toolId) + "/icon.svg";
  const pngPath = path.join(toolDir, "assets", "icon.png");
  if (fs.existsSync(pngPath)) return "/tool-assets/" + encodeURIComponent(toolId) + "/icon.png";
  return "";
}

function isSafeToolAssetPath(toolDir, filePath) {
  if (!toolDir || !filePath) return false;
  const assetDir = path.join(toolDir, "assets");
  const relative = path.relative(assetDir, filePath);
  return relative && !relative.startsWith("..") && !path.isAbsolute(relative);
}

function normalizeToolId(value) {
  return String(value || "").trim().toLowerCase().replace(/[^a-z0-9_-]/g, "_").slice(0, 64);
}

function hasAdapterHooks(tool) {
  if (!tool || typeof tool !== "object") return false;
  return (
    (typeof tool.matchesInputTool === "function" && typeof tool.registerInputTool === "function")
    || (typeof tool.matchesChatTool === "function" && typeof tool.responseItemFromChatTool === "function")
    || (typeof tool.matchesResponseItem === "function" && typeof tool.chatToolCallFromResponseItem === "function")
    || (typeof tool.matchesResponseItem === "function" && typeof tool.emitOutputEvents === "function")
  );
}

module.exports = {
  applyPatch,
  listToolAdapters,
  listToolManifests,
  listToolModules,
  toolAssetFilePath,
};

Object.defineProperties(module.exports, {
  toolAdapters: { enumerable: true, get: () => listToolAdapters() },
  toolManifests: { enumerable: true, get: () => listToolManifests() },
  toolModules: { enumerable: true, get: () => listToolModules() },
});
