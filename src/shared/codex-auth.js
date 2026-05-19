const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const API_KEY_FIELDS = [
  "OPENAI_API_KEY",
  "DEEPSEEK_API_KEY",
  "api_key",
  "apiKey",
];

let cachedAuthorizationHeader = "";

function readCodexAuthApiKey(env = process.env, options = {}) {
  const authorizationKey = apiKeyFromAuthorization(options.authorization);
  if (authorizationKey) return authorizationKey;

  if (options.includeCachedAuthorization) {
    const cachedKey = apiKeyFromAuthorization(cachedAuthorizationHeader);
    if (cachedKey) return cachedKey;
  }

  const filePath = resolveCodexAuthPath(env);
  if (!filePath) return "";

  try {
    if (!fs.existsSync(filePath)) return "";
    const parsed = JSON.parse(fs.readFileSync(filePath, "utf8"));
    return apiKeyFromCodexAuth(parsed);
  } catch {
    return "";
  }
}

function rememberAuthorizationHeader(value) {
  const normalized = normalizeAuthorizationHeader(value);
  if (normalized) cachedAuthorizationHeader = normalized;
  return normalized;
}

function apiKeyFromAuthorization(value) {
  const normalized = normalizeAuthorizationHeader(value);
  if (!normalized) return "";
  return normalized.replace(/^Bearer\s+/i, "").trim();
}

function normalizeAuthorizationHeader(value) {
  const text = String(value || "").trim();
  if (!text || !/^Bearer\s+\S+/i.test(text)) return "";
  return text;
}

function apiKeyFromCodexAuth(auth) {
  if (!auth || typeof auth !== "object" || Array.isArray(auth)) return "";
  for (const field of API_KEY_FIELDS) {
    const value = auth[field];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

function resolveCodexAuthPath(env = process.env) {
  const candidates = codexAuthPathCandidates(env);
  return candidates.find((candidate) => safeExists(candidate)) || candidates[0] || "";
}

function codexAuthPathCandidates(env = process.env) {
  const explicit = envValue(env, "CODEX_AUTH_JSON") || envValue(env, "CODEX_AUTH_FILE");
  if (explicit) return [resolveAuthPath(explicit, env)];

  const candidates = [];
  const codexHome = envValue(env, "CODEX_HOME");
  if (codexHome) candidates.push(path.join(resolveAuthPath(codexHome, env), "auth.json"));

  const home = envValue(env, "USERPROFILE") || envValue(env, "HOME") || os.homedir();
  if (home) candidates.push(path.join(resolveAuthPath(home, env), ".codex", "auth.json"));

  const appData = envValue(env, "APPDATA");
  if (appData) candidates.push(path.join(resolveAuthPath(appData, env), "codex", "auth.json"));

  return uniquePaths(candidates);
}

function resolveAuthPath(value, env) {
  const raw = String(value || "").trim();
  if (!raw) return "";
  if (raw === "~") {
    const home = envValue(env, "USERPROFILE") || envValue(env, "HOME") || os.homedir();
    return home ? path.resolve(home) : "";
  }
  if (raw.startsWith("~/") || raw.startsWith("~\\")) {
    const home = envValue(env, "USERPROFILE") || envValue(env, "HOME") || os.homedir();
    if (home) return path.resolve(home, raw.slice(2));
  }
  return path.resolve(raw);
}

function envValue(env, key) {
  if (env && env[key]) return String(env[key]);
  if (process.env[key]) return String(process.env[key]);
  return "";
}

function uniquePaths(paths) {
  const seen = new Set();
  const output = [];
  for (const item of paths) {
    if (!item || seen.has(item)) continue;
    seen.add(item);
    output.push(item);
  }
  return output;
}

function safeExists(filePath) {
  try {
    return Boolean(filePath && fs.existsSync(filePath));
  } catch {
    return false;
  }
}

module.exports = {
  apiKeyFromAuthorization,
  apiKeyFromCodexAuth,
  codexAuthPathCandidates,
  rememberAuthorizationHeader,
  readCodexAuthApiKey,
  resolveCodexAuthPath,
};
