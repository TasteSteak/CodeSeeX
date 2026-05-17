"use strict";

const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const DEFAULT_BASE_MODELS = ["gpt-5.5", "gpt-5.4", "gpt-5.2"];
const DEFAULT_CONTEXT_WINDOW = 1000000;
const DEFAULT_EFFECTIVE_CONTEXT_PERCENT = 90;
const DEFAULT_CODEX_CATALOG_TIMEOUT_MS = 8000;
const PRIVATE_CATALOG_DIR = path.join(".codeseex-private", "catalog");
const TARGET_MODELS = [
  {
    slug: "deepseek-v4-flash",
    displayName: "DeepSeek-V4 Flash",
    description: "DeepSeek-V4 Flash coding model served through CodeSeeX.",
  },
  {
    slug: "deepseek-v4-pro",
    displayName: "DeepSeek-V4 Pro",
    description: "DeepSeek-V4 Pro coding model served through CodeSeeX.",
  },
];
const DEFAULT_AVAILABLE_PLANS = [
  "business",
  "edu",
  "education",
  "enterprise",
  "enterprise_cbp_usage_based",
  "finserv",
  "free",
  "free_workspace",
  "go",
  "hc",
  "k12",
  "plus",
  "pro",
  "prolite",
  "quorum",
  "self_serve_business_usage_based",
  "team",
];
const DEFAULT_CODESEEX_IDENTITY = [
  "You are Codex, a coding agent based on DeepSeek-V4 and running through the local CodeSeeX proxy inside the Codex environment.",
  "You and the user share the same workspace and collaborate to achieve the user's goals.",
  "",
  "Codex provides the host UI, workspace, terminal, and tool execution layer. CodeSeeX bridges the model runtime to DeepSeek-V4 while preserving Codex-compatible tool and response behavior.",
].join(" ");
const ORIGINAL_CODEX_IDENTITY = "You are Codex, a coding agent based on GPT-5. You and the user share the same workspace and collaborate to achieve the user's goals.";
const PROXY_COMPATIBILITY_HEADER = "# CodeSeeX Proxy Compatibility";
const DEFAULT_PROXY_COMPATIBILITY_INSTRUCTIONS = [
  "# CodeSeeX Proxy Compatibility",
  "",
  "- Use the provided tools through structured tool calls. Do not print DSML, XML, or markdown-like tool call markup as plain text.",
  "- For web requests, call `web_search`. CodeSeeX executes it and returns tool results so you can answer in the same turn.",
  "- For local text edits, use `apply_patch` with Codex-style patch text beginning with `*** Begin Patch` and ending with `*** End Patch`.",
  "- In `apply_patch`, new files must use the exact header `*** Add File: <path>`; do not write `Create:`, `Create File:`, `Add:`, or other invented operation headers.",
  "- In `apply_patch` add-file hunks, every new content line must begin with `+`.",
  "- Do not use shell redirection, Out-File, Set-Content, WriteAllText, or similar full-file rewrites for routine text edits unless the user explicitly requests a non-patch repair or binary/non-text handling.",
].join("\n");

function buildCodeSeeXCatalog(options = {}) {
  const outputPath = options.outputPath || "";
  const nativeCatalog = options.nativeCatalog || readNativeCatalog();
  const recipe = resolveCatalogRecipe(options);
  const source = findFirstModel(nativeCatalog, baseModelCandidates(options));
  const contextWindow = readPositiveInteger(options.contextWindow || process.env.CODESEEX_MAX_CONTEXT_WINDOW, DEFAULT_CONTEXT_WINDOW);
  const effectiveContextPercent = readPositiveInteger(options.effectiveContextPercent || process.env.CODESEEX_CONTEXT_PERCENT, DEFAULT_EFFECTIVE_CONTEXT_PERCENT);
  const targetModels = targetModelDefinitions(options, recipe).map((target) => targetModelFromSource(source, target, {
    contextWindow,
    effectiveContextPercent,
    recipe,
  }));
  const catalog = { models: targetModels };

  if (outputPath) writeCatalogAtomic(outputPath, catalog);
  return {
    outputPath,
    baseModel: source.slug,
    targetModels: targetModels.map((model) => model.slug),
    catalog,
  };
}

function codexAdapterCatalogPath(dataDir) {
  return path.join(codeSeeXUserDir(dataDir), "model-catalog.json");
}

function generatedCatalogPath() {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "codeseex-model-catalog-"));
  registerTempCleanup(tempDir);
  return path.join(tempDir, "model-catalog.json");
}

function codeSeeXUserDir(fallbackDir) {
  return path.join(os.homedir() || fallbackDir || os.tmpdir(), ".codeseex");
}

function readNativeCatalog() {
  return readNativeCatalogWithArgs(["debug", "models", "--bundled"]);
}

function readNativeCatalogWithArgs(args) {
  const invocation = codexCliInvocation();
  const timeout = readPositiveInteger(process.env.CODESEEX_CATALOG_TIMEOUT_MS, DEFAULT_CODEX_CATALOG_TIMEOUT_MS);
  const output = execFileSync(invocation.command, invocation.args.concat(args), {
    encoding: "utf8",
    env: process.env,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
    maxBuffer: 16 * 1024 * 1024,
    timeout,
  });
  return JSON.parse(stripBom(output));
}

function codexCliInvocation() {
  const command = findCodexCommand();
  if (process.platform === "win32" && /\.(?:cmd|bat)$/i.test(command)) {
    return { command: process.env.ComSpec || "cmd.exe", args: ["/d", "/c", "call", command] };
  }
  return { command, args: [] };
}

function findCodexCommand() {
  const names = process.platform === "win32"
    ? ["codex.exe", "codex.cmd", "codex.bat", "codex"]
    : ["codex"];
  for (const dir of pathList(process.env.PATH)) {
    for (const name of names) {
      const candidate = path.join(dir, name);
      if (isExecutableFile(candidate)) return candidate;
    }
  }
  return "codex";
}

function pathList(value) {
  return String(value || "")
    .split(path.delimiter)
    .map((part) => part.trim())
    .filter(Boolean);
}

function isExecutableFile(filePath) {
  try {
    const stat = fs.statSync(filePath);
    return stat.isFile();
  } catch {
    return false;
  }
}

function baseModelCandidates(options = {}) {
  if (Array.isArray(options.baseModels) && options.baseModels.length > 0) return options.baseModels.map(String).filter(Boolean);
  if (options.baseModel) return [String(options.baseModel)];
  if (process.env.CODESEEX_BASE_CATALOG_MODEL) return splitList(process.env.CODESEEX_BASE_CATALOG_MODEL);
  return DEFAULT_BASE_MODELS.slice();
}

function targetModelDefinitions(options = {}, recipe = null) {
  if (Array.isArray(options.targetModels) && options.targetModels.length > 0) return options.targetModels;
  if (recipe && Array.isArray(recipe.targetModels) && recipe.targetModels.length > 0) return recipe.targetModels;
  return TARGET_MODELS.slice();
}

function findFirstModel(catalog, slugs) {
  const models = catalog && Array.isArray(catalog.models) ? catalog.models : [];
  for (const slug of slugs) {
    const model = models.find((item) => item && item.slug === slug);
    if (model) return model;
  }
  const available = models.map((item) => item.slug).filter(Boolean).join(", ");
  throw new Error("No supported base Codex model was found. Tried: " + slugs.join(", ") + ". Available: " + available);
}

function targetModelFromSource(source, target, options) {
  const model = deepClone(source);
  model.slug = target.slug;
  model.display_name = target.displayName || target.slug;
  model.description = target.description || (target.slug + " coding model served through CodeSeeX.");
  model.visibility = "list";
  model.supported_in_api = true;
  model.context_window = options.contextWindow;
  model.max_context_window = options.contextWindow;
  model.effective_context_window_percent = options.effectiveContextPercent;
  model.auto_compact_token_limit = Math.floor(options.contextWindow * options.effectiveContextPercent / 100);
  applyDesktopModelListFields(model, target);
  adaptInstructions(model, options.recipe);
  return model;
}

function applyDesktopModelListFields(model, target) {
  const supportedReasoningEfforts = normalizeReasoningEfforts(model.supported_reasoning_levels);
  model.id = target.slug;
  model.model = target.slug;
  model.displayName = target.displayName || target.slug;
  model.defaultReasoningEffort = model.default_reasoning_level || "medium";
  model.supportedReasoningEfforts = supportedReasoningEfforts;
  model.hidden = model.visibility === "hide";
  model.isDefault = target.slug === "deepseek-v4-pro";
  if (!model.minimal_client_version) model.minimal_client_version = "0.98.0";
  if (!Array.isArray(model.available_in_plans) || model.available_in_plans.length === 0) {
    model.available_in_plans = DEFAULT_AVAILABLE_PLANS.slice();
  }
}

function normalizeReasoningEfforts(levels) {
  const efforts = (Array.isArray(levels) ? levels : [])
    .map((level) => typeof level === "string" ? level : level && level.effort)
    .map((effort) => String(effort || "").trim())
    .filter(Boolean);
  return efforts.length > 0 ? Array.from(new Set(efforts)) : ["low", "medium", "high", "xhigh"];
}

function adaptInstructions(model, recipe) {
  if (typeof model.base_instructions === "string") {
    model.base_instructions = appendProxyCompatibility(rewriteIdentity(model.base_instructions, recipe), recipe);
  }
  if (model.model_messages && typeof model.model_messages === "object") {
    if (typeof model.model_messages.instructions_template === "string") {
      model.model_messages.instructions_template = appendProxyCompatibility(rewriteIdentity(model.model_messages.instructions_template, recipe), recipe);
    }
  }
}

function rewriteIdentity(value, recipe) {
  const identity = recipe && recipe.identity ? recipe.identity : DEFAULT_CODESEEX_IDENTITY;
  return String(value || "")
    .replaceAll(ORIGINAL_CODEX_IDENTITY, identity)
    .replace(/You are Codex, a coding agent based on GPT-5/g, "You are Codex, a coding agent based on DeepSeek-V4");
}

function appendProxyCompatibility(value, recipe) {
  const compatibility = recipe && recipe.compatibility ? recipe.compatibility : DEFAULT_PROXY_COMPATIBILITY_INSTRUCTIONS;
  const text = stripProxyCompatibility(value).trimEnd();
  return text + "\n\n" + compatibility;
}

function stripProxyCompatibility(value) {
  const text = String(value || "");
  const index = text.indexOf(PROXY_COMPATIBILITY_HEADER);
  return index === -1 ? text : text.slice(0, index);
}

function resolveCatalogRecipe(options = {}) {
  if (options.catalogRecipe) return normalizeCatalogRecipe(options.catalogRecipe);
  const rootDir = options.rootDir || projectRoot();
  const dirRecipe = readCatalogRecipeDir(options.privateCatalogDir || path.join(rootDir, PRIVATE_CATALOG_DIR));
  if (dirRecipe) return normalizeCatalogRecipe(dirRecipe);
  return normalizeCatalogRecipe({});
}

function readCatalogRecipeDir(dir) {
  try {
    if (!dir || !fs.existsSync(dir)) return null;
    const recipe = {};
    const identityPath = path.join(dir, "identity.md");
    const compatibilityPath = path.join(dir, "compatibility.md");
    const modelsPath = path.join(dir, "models.json");
    if (fs.existsSync(identityPath)) recipe.identity = fs.readFileSync(identityPath, "utf8").trim();
    if (fs.existsSync(compatibilityPath)) recipe.compatibility = fs.readFileSync(compatibilityPath, "utf8").trim();
    if (fs.existsSync(modelsPath)) {
      const parsed = JSON.parse(fs.readFileSync(modelsPath, "utf8"));
      recipe.targetModels = Array.isArray(parsed) ? parsed : parsed && parsed.targetModels;
    }
    return Object.keys(recipe).length > 0 ? recipe : null;
  } catch {
    return null;
  }
}

function normalizeCatalogRecipe(recipe = {}) {
  return {
    identity: textOrDefault(recipe.identity, DEFAULT_CODESEEX_IDENTITY),
    compatibility: textOrDefault(recipe.compatibility, DEFAULT_PROXY_COMPATIBILITY_INSTRUCTIONS),
    targetModels: normalizeTargetModels(recipe.targetModels),
  };
}

function normalizeTargetModels(models) {
  const items = Array.isArray(models) ? models : TARGET_MODELS;
  const normalized = items
    .map((model) => ({
      slug: String(model && model.slug || "").trim(),
      displayName: String(model && (model.displayName || model.display_name) || "").trim(),
      description: String(model && model.description || "").trim(),
    }))
    .filter((model) => model.slug);
  return normalized.length > 0 ? normalized : TARGET_MODELS.slice();
}

function textOrDefault(value, fallback) {
  const text = String(value || "").trim();
  return text || fallback;
}

function projectRoot() {
  return path.resolve(__dirname, "..", "..");
}

function writeCatalogAtomic(outputPath, catalog) {
  const dir = path.dirname(outputPath);
  fs.mkdirSync(dir, { recursive: true });
  const tempPath = path.join(dir, "." + path.basename(outputPath) + "." + process.pid + "." + Date.now() + ".tmp");
  fs.writeFileSync(tempPath, JSON.stringify(catalog, null, 2) + "\n", "utf8");
  fs.renameSync(tempPath, outputPath);
}

function deepClone(value) {
  return JSON.parse(JSON.stringify(value || null));
}

function splitList(value) {
  return String(value || "").split(",").map((part) => part.trim()).filter(Boolean);
}

function registerTempCleanup(tempDir) {
  process.once("exit", () => {
    try {
      fs.rmSync(tempDir, { recursive: true, force: true });
    } catch {}
  });
}

function stripBom(value) {
  return String(value || "").replace(/^\uFEFF/, "");
}

function readPositiveInteger(value, fallback) {
  const number = Number(value);
  if (!Number.isFinite(number) || number <= 0) return fallback;
  return Math.floor(number);
}

module.exports = {
  DEFAULT_BASE_MODELS,
  PRIVATE_CATALOG_DIR,
  TARGET_MODELS,
  buildCodeSeeXCatalog,
  codeSeeXUserDir,
  codexAdapterCatalogPath,
  codexCliInvocation,
  generatedCatalogPath,
  resolveCatalogRecipe,
};
