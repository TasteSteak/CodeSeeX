const fs = require("node:fs");
const path = require("node:path");

const RUNTIME_ENV_KEYS = [
  "DEEPSEEK_BASE_URL",
  "DEEPSEEK_OFFICIAL_V1_COMPAT",
  "PROXY_HOST",
  "PROXY_PORT",
  "LOG_RETENTION_DAYS",
  "DEEPSEEK_TEMPERATURE_PRESET",
  "DEEPSEEK_THINKING",
  "SHOW_THINKING",
  "THINKING_TITLE",
  "CATALOG_MODE",
  "UPSTREAM_MODEL_OVERRIDE",
  "AUTO_START",
  "UI_THEME",
  "UI_LANGUAGE",
  "UI_CLOSE_BEHAVIOR",
  "BILLING_FLASH_CACHED_INPUT_CNY",
  "BILLING_FLASH_CACHE_MISS_INPUT_CNY",
  "BILLING_FLASH_OUTPUT_CNY",
  "BILLING_PRO_CACHED_INPUT_CNY",
  "BILLING_PRO_CACHE_MISS_INPUT_CNY",
  "BILLING_PRO_OUTPUT_CNY",
  "BILLING_CACHED_INPUT_CNY",
  "BILLING_CACHE_MISS_INPUT_CNY",
  "BILLING_OUTPUT_CNY",
  "COMMUNITY_TOOL_CODE_ENABLED",
  "ENABLED_TOOLS",
  "WORKSPACE_TOOL_FILE_ACCESS",
  "WORKSPACE_ROOTS",
];

function readEnvFile(filePath, options = {}) {
  const values = {};
  if (!fs.existsSync(filePath)) return values;

  for (const line of fs.readFileSync(filePath, "utf8").split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const index = trimmed.indexOf("=");
    if (index === -1) continue;
    const key = trimmed.slice(0, index).trim();
    if (!isRuntimeEnvKey(key, options)) continue;
    let value = trimmed.slice(index + 1).trim();
    if ((value.startsWith("\"") && value.endsWith("\"")) || (value.startsWith("'") && value.endsWith("'"))) value = value.slice(1, -1);
    value = decodeEnvValue(value);
    if (key) values[key] = value;
  }

  return values;
}

function writeEnvFile(filePath, values, options = {}) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  const lines = ["# Proxy runtime configuration"];
  const written = new Set();
  const orderedKeys = envKeyOrder(options);

  for (const key of orderedKeys) {
    if (values[key] === undefined) continue;
    lines.push(key + "=" + encodeEnvValue(values[key]));
    written.add(key);
  }

  for (const key of Object.keys(values).sort()) {
    if (!isRuntimeEnvKey(key, options)) continue;
    if (written.has(key)) continue;
    lines.push(key + "=" + encodeEnvValue(values[key]));
  }

  fs.writeFileSync(filePath, lines.join("\n") + "\n", "utf8");
}

function mergeEnv(base, overrides) {
  const result = Object.assign({}, base);
  for (const [key, value] of Object.entries(overrides || {})) {
    if (value === undefined || value === null) continue;
    result[key] = String(value);
  }
  return result;
}

function encodeEnvValue(value) {
  return String(value).replace(/\r\n/g, "\n").replace(/\r/g, "\n").replace(/\n/g, "\\n");
}

function decodeEnvValue(value) {
  return String(value).replace(/\\n/g, "\n").replace(/\\r/g, "\r");
}

function isRuntimeEnvKey(key, options = {}) {
  const normalized = String(key || "");
  return envKeySet(options).has(normalized);
}

function envKeyOrder(options = {}) {
  const seen = new Set();
  const output = [];
  for (const key of RUNTIME_ENV_KEYS.concat(extraEnvKeys(options))) {
    const normalized = String(key || "");
    if (!normalized || seen.has(normalized)) continue;
    seen.add(normalized);
    output.push(normalized);
  }
  return output;
}

function envKeySet(options = {}) {
  return new Set(envKeyOrder(options));
}

function extraEnvKeys(options = {}) {
  const keys = Array.isArray(options.allowedKeys) ? options.allowedKeys : [];
  return keys.map((key) => String(key || "").trim()).filter(Boolean);
}

module.exports = {
  decodeEnvValue,
  encodeEnvValue,
  RUNTIME_ENV_KEYS,
  isRuntimeEnvKey,
  mergeEnv,
  readEnvFile,
  writeEnvFile,
};
