const os = require("node:os");
const path = require("node:path");
const { eventLogPath } = require("./event-log");

const ROOT_DIR = resolveRootDir();
const DATA_DIR = resolveDataDir(process.env, ROOT_DIR);

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
    stateFile: env.PROXY_STATE_FILE || path.join(dataDir, "proxy-state.json"),
    runtimeFile: env.PROXY_RUNTIME_FILE || path.join(dataDir, "runtime.json"),
    eventLogFile,
    logRetentionDays: normalizeRetentionDays(env.LOG_RETENTION_DAYS),
    debugDir: env.PROXY_DEBUG_DIR || path.join(dataDir, "debug"),
    debugEnabled: parseBool(env.PROXY_DEBUG, false),
    thinkingMode: String(env.DEEPSEEK_THINKING || "auto"),
    visibleThinkingEnabled: parseBool(env.SHOW_THINKING, true),
    thinkingTitle: env.THINKING_TITLE || "DeepSeek Thinking",
    catalogMode: normalizeCatalogMode(env.CATALOG_MODE),
    upstreamModelOverride: normalizeUpstreamModelOverride(env.UPSTREAM_MODEL_OVERRIDE),
    autoStart: parseBool(env.AUTO_START, false),
    billingCachedInputCny: numberOrDefault(env.BILLING_CACHED_INPUT_CNY, 0.025),
    billingCacheMissInputCny: numberOrDefault(env.BILLING_CACHE_MISS_INPUT_CNY, 3),
    billingOutputCny: numberOrDefault(env.BILLING_OUTPUT_CNY, 6),
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

function normalizeRetentionDays(value) {
  const raw = String(value || "7").trim();
  return ["1", "3", "7", "30"].includes(raw) ? Number(raw) : 7;
}

function numberOrDefault(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
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

module.exports = {
  DATA_DIR,
  ROOT_DIR,
  defaultEventLogFile,
  loadProxyConfig,
  normalizeDeepSeekBaseUrl,
  normalizeCatalogMode,
  normalizeUpstreamModelOverride,
  parseBool,
  resolveDataDir,
  resolveRootDir,
  splitList,
};
