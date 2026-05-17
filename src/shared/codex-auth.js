const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const API_KEY_FIELDS = [
  "OPENAI_API_KEY",
  "DEEPSEEK_API_KEY",
  "api_key",
  "apiKey",
];

function readCodexAuthApiKey(env = process.env) {
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
  apiKeyFromCodexAuth,
  codexAuthPathCandidates,
  readCodexAuthApiKey,
  resolveCodexAuthPath,
};
