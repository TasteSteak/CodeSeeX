"use strict";

const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const zlib = require("node:zlib");

const DEFAULT_BASE_MODELS = ["gpt-5.5", "gpt-5.4", "gpt-5.2"];
const DEFAULT_CONTEXT_WINDOW = 1000000;
const DEFAULT_EFFECTIVE_CONTEXT_PERCENT = 90;
const DEFAULT_CODEX_CATALOG_TIMEOUT_MS = 8000;
const FALLBACK_BASE_MODEL_SLUG = "codeseex-fallback";
const PACKAGED_CATALOG_SEED_FILE = "catalog-seed.c6";
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
  const recipe = resolveCatalogRecipe(options);
  const sourceInfo = resolveCatalogSource(options, recipe);
  const source = sourceInfo.model;
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
    baseModel: sourceInfo.baseModel,
    fallback: sourceInfo.fallback,
    source: sourceInfo.source,
    warning: sourceInfo.warning,
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
  for (const dir of codexSearchDirs()) {
    for (const name of names) {
      const candidate = path.join(dir, name);
      if (isExecutableFile(candidate)) return candidate;
    }
  }
  return "codex";
}

function codexSearchDirs() {
  const dirs = pathList(process.env.PATH);
  if (process.platform === "win32") {
    dirs.push(...windowsCodexInstallDirs());
  }
  return uniquePaths(dirs);
}

function windowsCodexInstallDirs() {
  const dirs = [];
  const localAppData = process.env.LOCALAPPDATA;
  const programFiles = [process.env.ProgramFiles, process.env["ProgramFiles(x86)"]].filter(Boolean);
  if (localAppData) {
    dirs.push(path.join(localAppData, "Programs", "Codex", "resources"));
    dirs.push(path.join(localAppData, "Programs", "Codex", "resources", "app"));
  }
  for (const base of programFiles) {
    dirs.push(...globCodexDirs(path.join(base, "WindowsApps")));
    dirs.push(path.join(base, "Codex", "resources"));
    dirs.push(path.join(base, "OpenAI", "Codex", "resources"));
  }
  return dirs;
}

function globCodexDirs(parent) {
  const dirs = [];
  try {
    if (!parent || !fs.existsSync(parent)) return dirs;
    for (const entry of fs.readdirSync(parent, { withFileTypes: true })) {
      if (!entry.isDirectory() || !/^OpenAI\.Codex_/i.test(entry.name)) continue;
      const root = path.join(parent, entry.name, "app", "resources");
      dirs.push(root);
      dirs.push(path.join(root, "app"));
    }
  } catch {}
  return dirs;
}

function pathList(value) {
  return String(value || "")
    .split(path.delimiter)
    .map((part) => part.trim())
    .filter(Boolean);
}

function uniquePaths(paths) {
  const seen = new Set();
  const output = [];
  for (const item of paths) {
    const normalized = path.resolve(String(item || ""));
    const key = process.platform === "win32" ? normalized.toLowerCase() : normalized;
    if (!normalized || seen.has(key)) continue;
    seen.add(key);
    output.push(normalized);
  }
  return output;
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

function resolveCatalogSource(options = {}, recipe = null) {
  const candidates = baseModelCandidates(options);
  let nativeCatalog = null;
  let nativeError = null;
  if (Object.prototype.hasOwnProperty.call(options, "nativeCatalog")) {
    nativeCatalog = options.nativeCatalog;
    nativeError = options.nativeError || null;
  } else {
    try {
      nativeCatalog = readNativeCatalog();
    } catch (error) {
      nativeError = error;
    }
  }

  const source = findFirstModelOrNull(nativeCatalog, candidates);
  if (source) {
    return {
      model: source,
      baseModel: source.slug || "",
      fallback: false,
      source: "native",
      warning: "",
    };
  }

  const seedInfo = readSeedCatalogSource(options, candidates);
  if (seedInfo.model) {
    return {
      model: seedInfo.model,
      baseModel: seedInfo.model.slug || "",
      fallback: false,
      source: "seed",
      warning: nativeError
        ? "Native Codex catalog unavailable; using packaged CodeSeeX catalog seed."
        : "Using packaged CodeSeeX catalog seed.",
    };
  }

  if (options.allowFallback === false) {
    throw modelNotFoundError(nativeCatalog, candidates, nativeError || seedInfo.error);
  }

  return {
    model: fallbackBaseModel(recipe),
    baseModel: FALLBACK_BASE_MODEL_SLUG,
    fallback: true,
    source: "fallback",
    warning: nativeError
      ? "Native Codex catalog unavailable: " + (nativeError.message || String(nativeError))
      : "Native Codex catalog unavailable; using CodeSeeX fallback catalog.",
  };
}

function readSeedCatalogSource(options = {}, candidates = DEFAULT_BASE_MODELS) {
  if (options.allowSeed === false) return { catalog: null, model: null, path: "", error: null };
  let lastError = null;
  for (const seedPath of seedCatalogSearchPaths(options)) {
    try {
      const catalog = readSeedCatalogFile(seedPath);
      const model = findFirstModelOrNull(catalog, candidates);
      if (model) return { catalog, model, path: seedPath, error: null };
      lastError = modelNotFoundError(catalog, candidates, null);
    } catch (error) {
      lastError = error;
    }
  }
  return { catalog: null, model: null, path: "", error: lastError };
}

function seedCatalogSearchPaths(options = {}) {
  const rootDir = options.rootDir || projectRoot();
  const paths = [];
  if (options.seedCatalogPath) paths.push(options.seedCatalogPath);
  if (process.env.CODESEEX_CATALOG_SEED_FILE) paths.push(process.env.CODESEEX_CATALOG_SEED_FILE);
  paths.push(path.join(rootDir, PRIVATE_CATALOG_DIR, "c6.br"));
  paths.push(path.join(rootDir, PRIVATE_CATALOG_DIR, "model-catalog.br"));
  paths.push(path.join(rootDir, PRIVATE_CATALOG_DIR, "model-catalog.json"));
  paths.push(path.join(rootDir, "build", "private", "catalog", "c6.br"));
  paths.push(path.join(rootDir, "build", "private", "catalog", "model-catalog.br"));
  paths.push(path.join(rootDir, "build", "private", "catalog", "model-catalog.json"));
  if (process.resourcesPath) paths.push(path.join(process.resourcesPath, PACKAGED_CATALOG_SEED_FILE));
  paths.push(path.join(__dirname, PACKAGED_CATALOG_SEED_FILE));
  return uniquePaths(paths).filter((item) => {
    try {
      return fs.existsSync(item) && fs.statSync(item).isFile();
    } catch {
      return false;
    }
  });
}

function readSeedCatalogFile(seedPath) {
  const raw = fs.readFileSync(seedPath);
  const text = /\.json$/i.test(seedPath)
    ? raw.toString("utf8")
    : zlib.brotliDecompressSync(raw).toString("utf8");
  return JSON.parse(stripBom(text));
}

function findFirstModel(catalog, slugs) {
  const model = findFirstModelOrNull(catalog, slugs);
  if (model) return model;
  throw modelNotFoundError(catalog, slugs, null);
}

function findFirstModelOrNull(catalog, slugs) {
  const models = catalog && Array.isArray(catalog.models) ? catalog.models : [];
  for (const slug of slugs) {
    const model = models.find((item) => item && item.slug === slug);
    if (model) return model;
  }
  return null;
}

function modelNotFoundError(catalog, slugs, nativeError) {
  if (nativeError) {
    return new Error("Unable to read native Codex model catalog: " + (nativeError.message || String(nativeError)));
  }
  const models = catalog && Array.isArray(catalog.models) ? catalog.models : [];
  const available = models.map((item) => item.slug).filter(Boolean).join(", ");
  return new Error("No supported base Codex model was found. Tried: " + slugs.join(", ") + ". Available: " + available);
}

function fallbackBaseModel(recipe = null) {
  const baseInstructions = fallbackBaseInstructions(recipe);
  return {
    slug: FALLBACK_BASE_MODEL_SLUG,
    display_name: "CodeSeeX Fallback Base",
    description: "Local CodeSeeX fallback catalog base used when the native Codex catalog is unavailable.",
    default_reasoning_level: "medium",
    supported_reasoning_levels: defaultReasoningLevels(),
    shell_type: "shell_command",
    visibility: "list",
    supported_in_api: true,
    priority: 2,
    additional_speed_tiers: ["fast"],
    availability_nux: null,
    upgrade: null,
    base_instructions: baseInstructions,
    model_messages: {
      instructions_template: baseInstructions + "\n\n{{ personality }}",
      instructions_variables: {
        personality_default: "",
      },
    },
    supports_reasoning_summaries: true,
    default_reasoning_summary: "none",
    support_verbosity: true,
    default_verbosity: "low",
    apply_patch_tool_type: "freeform",
    web_search_tool_type: "text_and_image",
    truncation_policy: {
      mode: "tokens",
      limit: 10000,
    },
    supports_parallel_tool_calls: true,
    supports_image_detail_original: true,
    context_window: DEFAULT_CONTEXT_WINDOW,
    max_context_window: DEFAULT_CONTEXT_WINDOW,
    effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_PERCENT,
    experimental_supported_tools: [],
    input_modalities: ["text", "image"],
    supports_search_tool: true,
    available_in_plans: DEFAULT_AVAILABLE_PLANS.slice(),
    minimal_client_version: "0.98.0",
  };
}

function fallbackBaseInstructions(recipe = null) {
  const identity = recipe && recipe.identity ? recipe.identity : DEFAULT_CODESEEX_IDENTITY;
  return [
    identity,
    "",
    "# General",
    "You are an expert coding agent operating inside the Codex client through CodeSeeX.",
    "Use the workspace, terminal, and structured tools provided by the client to help the user safely complete software tasks.",
    "Prefer concise progress updates, precise file edits, and verifiable results.",
  ].join("\n");
}

function defaultReasoningLevels() {
  return [
    { effort: "low", description: "Fast responses with lighter reasoning" },
    { effort: "medium", description: "Balances speed and reasoning depth for everyday tasks" },
    { effort: "high", description: "Greater reasoning depth for complex problems" },
    { effort: "xhigh", description: "Extra high reasoning depth for complex problems" },
  ];
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

function validateCodeSeeXCatalog(catalog, targets = TARGET_MODELS) {
  const models = catalog && Array.isArray(catalog.models) ? catalog.models : [];
  const slugs = models.map((model) => model && model.slug).filter(Boolean);
  const missing = [];
  const invalid = [];
  for (const target of targets) {
    const model = models.find((item) => item && item.slug === target.slug);
    if (!model) {
      missing.push(target.slug);
      continue;
    }
    if (!isValidTargetModel(model, target)) invalid.push(target.slug);
  }
  if (missing.length > 0) {
    return { ok: false, models: slugs, error: "Missing catalog models: " + missing.join(", ") };
  }
  if (invalid.length > 0) {
    return { ok: false, models: slugs, error: "Invalid catalog models: " + invalid.join(", ") };
  }
  return { ok: true, models: slugs, error: "" };
}

function isValidTargetModel(model, target) {
  const contextWindow = readPositiveInteger(model.context_window, 0);
  return model.slug === target.slug
    && model.id === target.slug
    && model.model === target.slug
    && Boolean(model.display_name || model.displayName)
    && model.supported_in_api === true
    && model.visibility === "list"
    && contextWindow >= DEFAULT_CONTEXT_WINDOW
    && hasUsableInstructions(model);
}

function hasUsableInstructions(model) {
  if (typeof model.base_instructions === "string" && model.base_instructions.trim()) return true;
  return Boolean(model.model_messages
    && typeof model.model_messages.instructions_template === "string"
    && model.model_messages.instructions_template.trim());
}

module.exports = {
  DEFAULT_BASE_MODELS,
  FALLBACK_BASE_MODEL_SLUG,
  PACKAGED_CATALOG_SEED_FILE,
  PRIVATE_CATALOG_DIR,
  TARGET_MODELS,
  buildCodeSeeXCatalog,
  codeSeeXUserDir,
  codexAdapterCatalogPath,
  codexCliInvocation,
  generatedCatalogPath,
  resolveCatalogRecipe,
  validateCodeSeeXCatalog,
};
