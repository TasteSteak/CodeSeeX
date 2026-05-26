const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { eventLogPath } = require("./event-log");

const ROOT_DIR = resolveRootDir();
const DATA_DIR = resolveDataDir(process.env, ROOT_DIR);
const DEFAULT_BILLING_RATES_CNY = Object.freeze({
  flash: Object.freeze({ cachedInput: 0.02, cacheMissInput: 1, output: 2 }),
  pro: Object.freeze({ cachedInput: 0.025, cacheMissInput: 3, output: 6 }),
});
const DEFAULT_MAX_RESPONSE_CHAIN_DEPTH = 10000;

function parseBool(value, fallback) {
  if (value === undefined || value === null || value === "") return fallback;
  return /^(1|true|yes|on|enabled)$/i.test(String(value).trim());
}

function clampInt(value, fallback, min, max) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.min(max, Math.max(min, Math.floor(parsed)));
}

function splitList(value) {
  return String(value || "").split(",").map((part) => part.trim()).filter(Boolean);
}

function normalizeDeepSeekBaseUrl(value) {
  const raw = String(value || "https://api.deepseek.com/").replace(/\/+$/, "");
  try {
    const url = new URL(raw);
    if (isOfficialDeepSeekUrl(url)) url.pathname = "/";
    return url.toString().replace(/\/+$/, "");
  } catch {
    return raw;
  }
}

function isOfficialDeepSeekUrl(url) {
  if (!url || url.protocol !== "https:" || url.hostname.toLowerCase() !== "api.deepseek.com") return false;
  const pathname = String(url.pathname || "/").replace(/\/+$/, "").toLowerCase();
  return pathname === "" || pathname === "/" || pathname === "/v1";
}

function loadProxyConfig(env = process.env) {
  const rootDir = resolveRootDir(env);
  const dataDir = resolveDataDir(env, rootDir);
  const eventLogFile = env.PROXY_EVENT_LOG_FILE || defaultEventLogFile(rootDir, dataDir);
  return {
    rootDir,
    dataDir,
    extensionDir: env.PROXY_EXTENSION_DIR || path.join(dataDir, "extension"),
    host: env.PROXY_HOST || "127.0.0.1",
    port: clampInt(env.PROXY_PORT, 8787, 1, 65535),
    deepseekBaseUrl: normalizeDeepSeekBaseUrl(env.DEEPSEEK_BASE_URL || "https://api.deepseek.com/"),
    requestTimeoutMs: clampInt(env.UPSTREAM_REQUEST_TIMEOUT_MS, 120000, 5000, 1800000),
    requestBodyLimitBytes: clampInt(env.REQUEST_BODY_LIMIT_BYTES, 10 * 1024 * 1024, 1024, 100 * 1024 * 1024),
    maxStoredResponses: clampInt(env.MAX_STORED_RESPONSES, 200, 10, 5000),
    maxResponseChainDepth: normalizeMaxResponseChainDepth(env.MAX_RESPONSE_CHAIN_DEPTH),
    stateFile: env.PROXY_STATE_FILE || path.join(dataDir, "proxy-state.json"),
    runtimeFile: env.PROXY_RUNTIME_FILE || path.join(dataDir, "runtime.json"),
    eventLogFile,
    logRetentionDays: normalizeRetentionDays(env.LOG_RETENTION_DAYS),
    debugDir: env.PROXY_DEBUG_DIR || path.join(dataDir, "debug"),
    debugEnabled: parseBool(env.PROXY_DEBUG, false),
    deepseekOfficialV1Compat: parseBool(env.DEEPSEEK_OFFICIAL_V1_COMPAT, true),
    temperaturePreset: normalizeTemperaturePreset(env.DEEPSEEK_TEMPERATURE_PRESET),
    thinkingMode: String(env.DEEPSEEK_THINKING || "auto"),
    visibleThinkingEnabled: parseBool(env.SHOW_THINKING, true),
    thinkingTitle: env.THINKING_TITLE || "DeepSeek Thinking",
    contextWindow: clampInt(env.CODESEEX_CONTEXT_WINDOW, 1000000, 8192, 2000000),
    effectiveContextWindowPercent: clampInt(env.CODESEEX_EFFECTIVE_CONTEXT_WINDOW_PERCENT, 90, 10, 100),
    contextReservedOutputTokens: clampInt(env.CODESEEX_CONTEXT_RESERVED_OUTPUT_TOKENS, 64000, 1024, 512000),
    contextMaxToolOutputBytes: clampInt(env.CODESEEX_CONTEXT_MAX_TOOL_OUTPUT_BYTES, 512 * 1024, 4096, 4 * 1024 * 1024),
    compactionSecret: String(env.CODESEEX_COMPACTION_SECRET || loadOrCreateCompactionSecret(dataDir)),
    catalogMode: normalizeCatalogMode(env.CATALOG_MODE),
    upstreamModelOverride: normalizeUpstreamModelOverride(env.UPSTREAM_MODEL_OVERRIDE),
    autoStart: parseBool(env.AUTO_START, false),
    billingRatesCny: billingRatesFromEnv(env),
    billingCachedInputCny: numberOrDefault(env.BILLING_PRO_CACHED_INPUT_CNY || env.BILLING_CACHED_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.pro.cachedInput),
    billingCacheMissInputCny: numberOrDefault(env.BILLING_PRO_CACHE_MISS_INPUT_CNY || env.BILLING_CACHE_MISS_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.pro.cacheMissInput),
    billingOutputCny: numberOrDefault(env.BILLING_PRO_OUTPUT_CNY || env.BILLING_OUTPUT_CNY, DEFAULT_BILLING_RATES_CNY.pro.output),
    availableModels: splitList(env.AVAILABLE_MODELS || "deepseek-v4-flash,deepseek-v4-pro"),
    communityToolCodeEnabled: parseBool(env.COMMUNITY_TOOL_CODE_ENABLED, false),
    ENABLED_TOOLS: env.ENABLED_TOOLS || "",
    workspaceToolFileAccess: String(env.WORKSPACE_TOOL_FILE_ACCESS || "auto").trim().toLowerCase() || "auto",
    workspaceRoots: splitPathList(env.WORKSPACE_ROOTS),
    parentPid: Number(env.PROXY_PARENT_PID || "0") || null,
  };
}

function normalizeCatalogMode(value) {
  const normalized = String(value || "default").trim().toLowerCase();
  if (normalized === "auto" || normalized === "dynamic") return "auto";
  if (normalized === "builtin") return "builtin";
  return "default";
}

function normalizeUpstreamModelOverride(value) {
  const normalized = String(value || "default").trim().toLowerCase();
  if (normalized === "flash" || normalized === "deepseek-v4-flash") return "deepseek-v4-flash";
  if (normalized === "pro" || normalized === "deepseek-v4-pro") return "deepseek-v4-pro";
  return "default";
}

function normalizeMaxResponseChainDepth(value) {
  const raw = String(value === undefined || value === null ? "" : value).trim().toLowerCase();
  if (!raw) return DEFAULT_MAX_RESPONSE_CHAIN_DEPTH;
  if (raw === "0" || raw === "unlimited" || raw === "infinite" || raw === "none") return 0;
  return clampInt(raw, DEFAULT_MAX_RESPONSE_CHAIN_DEPTH, 100, 500000);
}

function normalizeTemperaturePreset(value) {
  const normalized = String(value || "default").trim().toLowerCase();
  if (normalized === "precise" || normalized === "strict" || normalized === "rigorous") return "strict";
  if (normalized === "balanced" || normalized === "balance") return "balanced";
  if (normalized === "general" || normalized === "chat" || normalized === "translation") return "general";
  if (normalized === "creative" || normalized === "creation") return "creative";
  return "default";
}

function temperatureForPreset(value) {
  const preset = normalizeTemperaturePreset(value);
  if (preset === "strict") return 0;
  if (preset === "balanced") return 1;
  if (preset === "general") return 1.3;
  if (preset === "creative") return 1.5;
  return undefined;
}

function normalizeRetentionDays(value) {
  const raw = String(value || "7").trim();
  return ["1", "3", "7", "30"].includes(raw) ? Number(raw) : 7;
}

function numberOrDefault(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function billingRatesFromEnv(env = {}) {
  return {
    flash: {
      cachedInput: numberOrDefault(env.BILLING_FLASH_CACHED_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.flash.cachedInput),
      cacheMissInput: numberOrDefault(env.BILLING_FLASH_CACHE_MISS_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.flash.cacheMissInput),
      output: numberOrDefault(env.BILLING_FLASH_OUTPUT_CNY, DEFAULT_BILLING_RATES_CNY.flash.output),
    },
    pro: {
      cachedInput: numberOrDefault(env.BILLING_PRO_CACHED_INPUT_CNY || env.BILLING_CACHED_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.pro.cachedInput),
      cacheMissInput: numberOrDefault(env.BILLING_PRO_CACHE_MISS_INPUT_CNY || env.BILLING_CACHE_MISS_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.pro.cacheMissInput),
      output: numberOrDefault(env.BILLING_PRO_OUTPUT_CNY || env.BILLING_OUTPUT_CNY, DEFAULT_BILLING_RATES_CNY.pro.output),
    },
  };
}

function splitPathList(value) {
  return String(value || "")
    .split(/[;\n]/)
    .map((part) => part.trim())
    .filter(Boolean);
}

function resolveRootDir(env = process.env) {
  return path.resolve(env.PROXY_ROOT_DIR || path.resolve(__dirname, "..", ".."));
}

function resolveDataDir(env = process.env, rootDir = resolveRootDir(env)) {
  if (env.PROXY_DATA_DIR) return path.resolve(env.PROXY_DATA_DIR);
  if (env.PORTABLE_EXECUTABLE_DIR) return path.resolve(env.PORTABLE_EXECUTABLE_DIR);
  return path.join(os.homedir() || rootDir, ".codeseex");
}

function defaultEventLogFile(rootDir, dataDir) {
  return eventLogPath(rootDir, dataDir);
}

function loadOrCreateCompactionSecret(dataDir) {
  const keyPath = path.join(dataDir, "compact.key");
  try {
    const existing = fs.readFileSync(keyPath, "utf8").trim();
    if (existing.length >= 32) return existing;
  } catch {}
  const secret = crypto.randomBytes(32).toString("base64url");
  try {
    fs.mkdirSync(dataDir, { recursive: true });
    fs.writeFileSync(keyPath, secret + "\n", { encoding: "utf8", mode: 0o600, flag: "wx" });
    return secret;
  } catch {
    try {
      const existing = fs.readFileSync(keyPath, "utf8").trim();
      if (existing.length >= 32) return existing;
    } catch {}
    return secret;
  }
}

module.exports = {
  DATA_DIR,
  DEFAULT_BILLING_RATES_CNY,
  ROOT_DIR,
  defaultEventLogFile,
  loadProxyConfig,
  normalizeDeepSeekBaseUrl,
  normalizeCatalogMode,
  normalizeMaxResponseChainDepth,
  normalizeTemperaturePreset,
  normalizeUpstreamModelOverride,
  parseBool,
  resolveDataDir,
  resolveRootDir,
  splitList,
  temperatureForPreset,
};
