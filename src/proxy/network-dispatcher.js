const { execFileSync } = require("node:child_process");

let ProxyAgentCtor = null;

function resolveDispatcher(config = {}) {
  const proxy = resolveProxyUrl(config);
  if (!proxy) return undefined;
  try {
    const ProxyAgent = getProxyAgent();
    const allowInsecureTls = /^(1|true|yes|on|enabled)$/i.test(String((config && config.ALLOW_INSECURE_PROXY_TLS) || process.env.ALLOW_INSECURE_PROXY_TLS || "").trim());
    return allowInsecureTls
      ? new ProxyAgent({ uri: proxy, requestTls: { rejectUnauthorized: false } })
      : new ProxyAgent({ uri: proxy });
  } catch {
    return undefined;
  }
}

function getProxyAgent() {
  if (!ProxyAgentCtor) {
    ProxyAgentCtor = require("undici").ProxyAgent;
  }
  return ProxyAgentCtor;
}

function resolveProxyUrl(config = {}) {
  const explicit = normalizeProxyUrl(config.HTTPS_PROXY || config.HTTP_PROXY || process.env.HTTPS_PROXY || process.env.HTTP_PROXY || process.env.https_proxy || process.env.http_proxy);
  if (explicit) return explicit;
  if (process.platform === "win32") return resolveWindowsProxyUrl();
  return "";
}

function resolveWindowsProxyUrl() {
  try {
    if (!isWindowsProxyEnabled()) return "";
    const raw = queryWindowsInternetSetting("ProxyServer");
    if (!raw) return "";
    return parseWindowsProxyServer(raw);
  } catch {
    return "";
  }
}

function isWindowsProxyEnabled() {
  const value = queryWindowsInternetSetting("ProxyEnable");
  if (!value) return false;
  return Number(value.trim()) === 1;
}

function queryWindowsInternetSetting(name) {
  const output = execFileSync("reg", ["query", "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings", "/v", name], {
    encoding: "utf8",
    windowsHide: true,
    timeout: 3000,
  });
  const match = output.match(new RegExp(escapeRegExp(name) + "\\s+REG_\\w+\\s+(.+)", "i"));
  return match ? match[1].trim() : "";
}

function parseWindowsProxyServer(raw) {
  const value = String(raw || "").trim();
  if (!value) return "";
  if (!value.includes("=")) return normalizeProxyUrl(value);

  const entries = {};
  for (const part of value.split(";")) {
    const match = String(part || "").trim().match(/^([a-z][a-z0-9+.-]*)=(.+)$/i);
    if (!match) continue;
    entries[match[1].toLowerCase()] = match[2].trim();
  }

  // Undici ProxyAgent supports HTTP(S) CONNECT proxies. Do not reinterpret
  // socks=host:port as http://host:port; that causes fast "fetch failed" errors.
  return normalizeProxyUrl(entries.https || entries.http || "");
}

function normalizeProxyUrl(value) {
  const raw = String(value || "").trim();
  if (!raw) return "";
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(raw)) {
    return /^https?:\/\//i.test(raw) ? raw : "";
  }
  return "http://" + raw;
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

module.exports = {
  resolveDispatcher,
};
