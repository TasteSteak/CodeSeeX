#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const DEFAULT_PORT = 9222;
const DEFAULT_TIMEOUT_MS = 30_000;
const DEFAULT_MAX_ASSETS = 90;
const DEFAULT_MAX_SNIPPETS = 4;

const args = parseArgs(process.argv.slice(2));
let debugPort = numberArg(args.port, DEFAULT_PORT);
const timeoutMs = numberArg(args.timeoutMs ?? args.timeout, DEFAULT_TIMEOUT_MS);
const maxAssets = numberArg(args.maxAssets, DEFAULT_MAX_ASSETS);
const maxSnippets = numberArg(args.maxSnippets, DEFAULT_MAX_SNIPPETS);
const outDir = path.resolve(
  args.outDir || path.join(".private", "codex-app-diagnostics"),
);
const probeModules = Boolean(args.probeModules);
const traceModelList = Boolean(args.traceModelList);
const launch = Boolean(args.launch);
const connectOnly = Boolean(args.connectOnly);

if (connectOnly && launch) {
  fail("--connect-only and --launch cannot be used together.");
}

if (typeof WebSocket !== "function") {
  fail("This script requires Node.js with global WebSocket support. Node 22+ is recommended.");
}

const startedAt = new Date();
const runId = startedAt
  .toISOString()
  .replaceAll(":", "")
  .replaceAll(".", "-");

const report = {
  startedAt: startedAt.toISOString(),
  debugPort,
  probeModules,
  traceModelList,
  launchRequested: launch,
  connectOnly,
  environment: {
    platform: process.platform,
    node: process.version,
    cwd: process.cwd(),
  },
  launch: null,
  cdp: null,
  targets: [],
  conclusion: [],
};

async function main() {
try {
  if (args.scanPorts) {
    const ports = parsePortRange(args.scanPorts, debugPort);
    const found = await findReachableCdpPort(ports);
    if (found) {
      debugPort = found;
      report.debugPort = found;
      report.scannedPorts = ports;
      report.portDetected = true;
    } else {
      report.scannedPorts = ports;
      report.portDetected = false;
    }
  }
  let cdpReady = await waitForCdp(debugPort, 1_000).catch(() => false);
  if (!cdpReady && launch && !connectOnly) {
    report.launch = await launchCodex(debugPort, args.codexPath);
    cdpReady = await waitForCdp(debugPort, timeoutMs).catch(() => false);
  } else if (!cdpReady) {
    report.launch = {
      attempted: false,
      reason: "CDP was not reachable and --launch was not provided.",
    };
  }

  if (!cdpReady) {
    report.cdp = {
      reachable: false,
      error: `http://127.0.0.1:${debugPort}/json/version was not reachable.`,
      runningCodexProcesses: windowsRunningCodexProcesses(),
    };
    addConclusion(
      "CDP is not reachable. Fully quit Codex and rerun with --launch, or start Codex with --remote-debugging-port first.",
    );
  } else {
    report.cdp = {
      reachable: true,
      version: await httpJson(`http://127.0.0.1:${debugPort}/json/version`, 5_000).catch(
        (error) => ({ error: String(error.message || error) }),
      ),
    };
    const targets = await waitForTargets(debugPort, timeoutMs);
    report.targets = await inspectTargets(targets);
    summarizeFindings(report);
  }
} catch (error) {
  report.error = String(error && (error.stack || error.message) || error);
  addConclusion(`Diagnostic failed: ${String(error && (error.message || error) || error)}`);
}

mkdirSync(outDir, { recursive: true });
const jsonPath = path.join(outDir, `codex-app-model-list-${runId}.json`);
const mdPath = path.join(outDir, `codex-app-model-list-${runId}.md`);
writeFileSync(jsonPath, JSON.stringify(report, null, 2), "utf8");
writeFileSync(mdPath, renderMarkdown(report, jsonPath), "utf8");

console.log(`JSON report: ${jsonPath}`);
console.log(`Markdown report: ${mdPath}`);
if (report.conclusion.length) {
  console.log("");
  console.log("Conclusion:");
  for (const item of report.conclusion) console.log(`- ${item}`);
}
}

function parseArgs(argv) {
  const parsed = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) continue;
    const key = arg.slice(2).replace(/-([a-z])/g, (_, ch) => ch.toUpperCase());
    const next = argv[index + 1];
    if (!next || next.startsWith("--")) {
      parsed[key] = true;
    } else {
      parsed[key] = next;
      index += 1;
    }
  }
  return parsed;
}

function numberArg(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function parsePortRange(value, fallbackPort) {
  if (value === true) {
    return range(fallbackPort, fallbackPort + 20);
  }
  const text = String(value || "").trim();
  if (!text) return range(fallbackPort, fallbackPort + 20);
  if (text.includes("-")) {
    const [start, end] = text.split("-", 2).map((part) => Number(part.trim()));
    if (Number.isFinite(start) && Number.isFinite(end) && start > 0 && end >= start) {
      return range(start, Math.min(end, start + 100));
    }
  }
  return text
    .split(",")
    .map((part) => Number(part.trim()))
    .filter((port) => Number.isFinite(port) && port > 0);
}

function range(start, end) {
  const ports = [];
  for (let port = start; port <= end; port += 1) ports.push(port);
  return ports;
}

async function findReachableCdpPort(ports) {
  for (const port of ports) {
    try {
      await httpJson(`http://127.0.0.1:${port}/json/version`, 250);
      return port;
    } catch {}
  }
  return null;
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

function addConclusion(message) {
  if (!report.conclusion.includes(message)) report.conclusion.push(message);
}

async function waitForCdp(port, timeout) {
  const deadline = Date.now() + timeout;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      await httpJson(`http://127.0.0.1:${port}/json/version`, 1_000);
      return true;
    } catch (error) {
      lastError = error;
      await sleep(350);
    }
  }
  throw lastError || new Error("CDP wait timed out.");
}

async function waitForTargets(port, timeout) {
  const deadline = Date.now() + timeout;
  let lastTargets = [];
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      const targets = await httpJson(`http://127.0.0.1:${port}/json`, 2_000);
      lastTargets = Array.isArray(targets) ? targets : [];
      if (lastTargets.some((target) => target.type === "page" && target.webSocketDebuggerUrl)) {
        return lastTargets;
      }
    } catch (error) {
      lastError = error;
    }
    await sleep(350);
  }
  if (lastTargets.length > 0) return lastTargets;
  if (lastError) throw lastError;
  return [];
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function httpJson(url, timeout) {
  const response = await fetchWithTimeout(url, { timeout });
  if (!response.ok) throw new Error(`${url} returned HTTP ${response.status}`);
  return await response.json();
}

async function fetchWithTimeout(url, { timeout }) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeout);
  try {
    return await fetch(url, { signal: controller.signal });
  } finally {
    clearTimeout(timer);
  }
}

async function launchCodex(port, explicitPath) {
  const args = [
    `--remote-debugging-port=${port}`,
    `--remote-allow-origins=http://127.0.0.1:${port}`,
  ];
  const executable = explicitPath || findCodexExecutable();
  if (executable) {
    try {
      const child = spawn(executable, args, {
        detached: true,
        stdio: "ignore",
        windowsHide: true,
      });
      child.unref();
      return {
        attempted: true,
        mode: "process",
        executable,
        args,
        pid: child.pid,
      };
    } catch (error) {
      if (explicitPath) throw error;
      const packaged = activatePackagedCodex(port, args);
      return {
        attempted: true,
        mode: "process_failed_then_packaged_activation",
        executable,
        processError: String(error && (error.message || error) || error),
        args,
        ...packaged,
      };
    }
  }

  const packaged = activatePackagedCodex(port, args);
  return {
    attempted: true,
    mode: "packaged_activation",
    args,
    ...packaged,
  };
}

function findCodexExecutable() {
  const candidates = [];
  for (const key of ["CODESEEX_CODEX_APP_EXE", "CODEX_APP_EXE", "CODEX_APP_PATH"]) {
    if (process.env[key]) candidates.push(process.env[key]);
  }
  if (process.platform === "win32") {
    const local = process.env.LOCALAPPDATA || "";
    for (const process of windowsRunningCodexProcesses()) {
      if (process.path && /\\app\\Codex\.exe$/i.test(process.path)) {
        candidates.push(process.path);
      }
    }
    candidates.push(
      path.join(local, "OpenAI", "Codex", "app", "Codex.exe"),
      path.join(local, "OpenAI", "Codex", "Codex.exe"),
      path.join(local, "Programs", "OpenAI", "Codex", "Codex.exe"),
    );
    candidates.push(...windowsPackagedCodexExecutables());
  } else if (process.platform === "darwin") {
    candidates.push(
      "/Applications/Codex.app/Contents/MacOS/Codex",
      path.join(process.env.HOME || "", "Applications", "Codex.app", "Contents", "MacOS", "Codex"),
      "/usr/local/bin/codex",
      "/opt/homebrew/bin/codex",
    );
  } else {
    candidates.push(
      path.join(process.env.HOME || "", ".local", "bin", "codex"),
      "/usr/local/bin/codex",
      "/usr/bin/codex",
    );
  }
  return candidates.find((candidate) => candidate && existsSync(candidate)) || "";
}

function windowsPackagedCodexExecutables() {
  if (process.platform !== "win32") return [];
  const result = spawnSync(
    "powershell",
    [
      "-NoProfile",
      "-Command",
      "Get-AppxPackage | Where-Object { $_.Name -eq 'OpenAI.Codex' -or $_.Name -eq 'OpenAI.CodexBeta' } | ForEach-Object { Join-Path $_.InstallLocation 'app\\Codex.exe' } | ConvertTo-Json -Compress",
    ],
    { encoding: "utf8", windowsHide: true, timeout: 10_000 },
  );
  if (result.status !== 0 || !result.stdout.trim()) return [];
  try {
    const parsed = JSON.parse(result.stdout.trim());
    return Array.isArray(parsed) ? parsed : [parsed];
  } catch {
    return result.stdout
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
  }
}

function activatePackagedCodex(port, codexArgs) {
  if (process.platform !== "win32") {
    throw new Error("Codex executable was not found and packaged activation is Windows-only.");
  }
  const script = String.raw`
$ErrorActionPreference = 'Stop'
$arguments = [string]$env:CODESEEX_CODEX_ARGS
$aumid = [string]$env:CODESEEX_CODEX_AUMID
if ([string]::IsNullOrWhiteSpace($aumid)) {
  $app = Get-StartApps | Where-Object { $_.AppID -like 'OpenAI.Codex*!*' } | Select-Object -First 1
  if ($null -ne $app) { $aumid = [string]$app.AppID }
}
if ([string]::IsNullOrWhiteSpace($aumid)) {
  $pkg = Get-AppxPackage | Where-Object { $_.Name -eq 'OpenAI.Codex' -or $_.Name -eq 'OpenAI.CodexBeta' } | Select-Object -First 1
  if ($null -ne $pkg) { $aumid = "$($pkg.PackageFamilyName)!App" }
}
if ([string]::IsNullOrWhiteSpace($aumid)) { throw 'Codex packaged app AUMID was not found.' }
$source = @'
using System;
using System.Runtime.InteropServices;
public enum ActivateOptions { None = 0 }
[ComImport]
[Guid("2e941141-7f97-4756-ba1d-9decde894a3d")]
[InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IApplicationActivationManager
{
    [PreserveSig]
    int ActivateApplication(
        [MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
        [MarshalAs(UnmanagedType.LPWStr)] string arguments,
        ActivateOptions options,
        out uint processId);
}
[ComImport]
[Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
class ApplicationActivationManager {}
public static class CodeSeeXCodexActivator
{
    public static uint Activate(string appUserModelId, string arguments)
    {
        var manager = (IApplicationActivationManager)new ApplicationActivationManager();
        uint processId;
        int hr = manager.ActivateApplication(appUserModelId, arguments ?? "", ActivateOptions.None, out processId);
        if (hr < 0) Marshal.ThrowExceptionForHR(hr);
        return processId;
    }
}
'@
Add-Type -TypeDefinition $source -Language CSharp
$activatedPid = [CodeSeeXCodexActivator]::Activate($aumid, $arguments)
[pscustomobject]@{ appUserModelId = $aumid; pid = $activatedPid } | ConvertTo-Json -Compress
`;
  const result = spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script], {
    encoding: "utf8",
    windowsHide: true,
    env: {
      ...process.env,
      CODESEEX_CODEX_ARGS: codexArgs.join(" "),
      CODESEEX_CODEX_AUMID: process.env.CODESEEX_CODEX_APP_AUMID || process.env.CODEX_APP_AUMID || "",
    },
    timeout: 20_000,
  });
  if (result.status !== 0) {
    throw new Error(`Packaged Codex activation failed: ${(result.stderr || result.stdout || "").trim()}`);
  }
  try {
    return JSON.parse(result.stdout.trim());
  } catch {
    return { raw: result.stdout.trim() };
  }
}

function windowsRunningCodexProcesses() {
  if (process.platform !== "win32") return [];
  const result = spawnSync(
    "powershell",
    [
      "-NoProfile",
      "-Command",
      "Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -ieq 'Codex' } | Select-Object @{Name='pid';Expression={$_.Id}},@{Name='name';Expression={[string]$_.ProcessName}},@{Name='path';Expression={if ($_.Path) {[string]$_.Path} else {''}}} | ConvertTo-Json -Compress",
    ],
    { encoding: "utf8", windowsHide: true, timeout: 10_000 },
  );
  if (result.status !== 0 || !result.stdout.trim()) return [];
  try {
    const parsed = JSON.parse(result.stdout.trim());
    return Array.isArray(parsed) ? parsed : [parsed];
  } catch {
    return [{ raw: result.stdout.trim() }];
  }
}

async function inspectTargets(targets) {
  const pageTargets = (Array.isArray(targets) ? targets : []).filter(
    (target) => target.type === "page" && target.webSocketDebuggerUrl,
  );
  const inspected = [];
  for (const target of pageTargets) {
    const item = {
      id: target.id,
      type: target.type,
      title: target.title || "",
      url: target.url || "",
      attached: false,
      inspection: null,
    };
    try {
      item.inspection = await evaluateRendererDiagnostic(target.webSocketDebuggerUrl, {
        maxAssets,
        maxSnippets,
        probeModules,
        traceModelList,
      });
      item.attached = true;
    } catch (error) {
      item.error = String(error && (error.stack || error.message) || error);
    }
    inspected.push(item);
  }
  return inspected;
}

async function evaluateRendererDiagnostic(webSocketUrl, options) {
  const expression = `(${rendererDiagnosticSource})(${JSON.stringify(options)})`;
  const session = await CdpSession.open(webSocketUrl);
  try {
    await session.send("Runtime.enable", {});
    const response = await session.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
      timeout: timeoutMs,
    });
    if (response.exceptionDetails) {
      throw new Error(`Runtime.evaluate exception: ${JSON.stringify(response.exceptionDetails)}`);
    }
    return response.result && "value" in response.result ? response.result.value : response;
  } finally {
    await session.close().catch(() => {});
  }
}

class CdpSession {
  constructor(ws) {
    this.ws = ws;
    this.nextId = 1;
    this.pending = new Map();
    ws.addEventListener("message", (event) => this.onMessage(event));
    ws.addEventListener("error", (event) => this.rejectAll(new Error(String(event.message || "WebSocket error"))));
    ws.addEventListener("close", () => this.rejectAll(new Error("WebSocket closed")));
  }

  static open(url) {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
      const timer = setTimeout(() => reject(new Error(`WebSocket open timed out: ${url}`)), 10_000);
      ws.addEventListener("open", () => {
        clearTimeout(timer);
        resolve(new CdpSession(ws));
      }, { once: true });
      ws.addEventListener("error", () => {
        clearTimeout(timer);
        reject(new Error(`WebSocket open failed: ${url}`));
      }, { once: true });
    });
  }

  send(method, params) {
    const id = this.nextId++;
    const payload = JSON.stringify({ id, method, params });
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(payload);
    });
  }

  onMessage(event) {
    let message;
    try {
      message = JSON.parse(event.data);
    } catch {
      return;
    }
    if (!message.id || !this.pending.has(message.id)) return;
    const pending = this.pending.get(message.id);
    this.pending.delete(message.id);
    if (message.error) pending.reject(new Error(JSON.stringify(message.error)));
    else pending.resolve(message.result);
  }

  rejectAll(error) {
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear();
  }

  async close() {
    this.ws.close();
  }
}

async function rendererDiagnosticSource(options) {
  const patternStrings = [
    "list-models-for-host",
    "send-cli-request-for-host",
    "model/list",
    "use-host-config-",
    "model-queries-",
    "sendRequest",
    "setMessageHandler",
    "messageHandler",
    "codex-message-from-view",
    "mcp-request",
    "mcp-response",
    "available_models",
    "availableModels",
    "default_model",
    "defaultModel",
    "getDynamicConfig",
    "107580212",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
  ];

  function unique(values) {
    return Array.from(new Set(values.filter(Boolean)));
  }

  function label(url) {
    try {
      const parsed = new URL(String(url || ""), location.href);
      return parsed.pathname.split("/").filter(Boolean).pop() || parsed.href;
    } catch {
      return String(url || "").slice(0, 220);
    }
  }

  function countOccurrences(text, needle) {
    if (!needle) return 0;
    let count = 0;
    let index = 0;
    while ((index = text.indexOf(needle, index)) !== -1) {
      count += 1;
      index += needle.length;
    }
    return count;
  }

  function snippets(text, needle, maxSnippets) {
    const out = [];
    let index = 0;
    while (out.length < maxSnippets && (index = text.indexOf(needle, index)) !== -1) {
      const start = Math.max(0, index - 220);
      const end = Math.min(text.length, index + needle.length + 260);
      out.push(text.slice(start, end).replace(/\s+/g, " ").trim());
      index += needle.length;
    }
    return out;
  }

  function importSpecifiers(text) {
    const imports = [];
    const re = /from\s*["'](\.\/[^"']+\.js)["']/g;
    let match;
    while ((match = re.exec(text)) && imports.length < 80) imports.push(match[1]);
    return unique(imports);
  }

  function relativeJsSpecifiers(text) {
    const refs = [];
    const re = /["'](\.\/[^"']+\.js)["']/g;
    let match;
    while ((match = re.exec(text)) && refs.length < 400) refs.push(match[1]);
    return unique(refs);
  }

  function exportSpecifiers(text) {
    const exports = [];
    const re = /export\s*\{([^}]+)\}/g;
    let match;
    while ((match = re.exec(text)) && exports.length < 120) {
      for (const part of match[1].split(",")) {
        const trimmed = part.trim();
        if (trimmed) exports.push(trimmed.slice(0, 120));
      }
    }
    return unique(exports);
  }

  function looksRelevant(url, text) {
    const haystack = `${label(url)}\n${text.slice(0, 2_000)}`;
    return /model|host|config|mcp|bridge|codex|app-server|query/i.test(haystack)
      || patternStrings.some((pattern) => text.includes(pattern));
  }

  function shouldFollowSpecifier(specifier) {
    return /app-main|model|host|config|mcp|bridge|codex|query|rpc|thread-context|vscode-api|statsig|capability|service-tier|reasoning/i.test(specifier);
  }

  async function fetchText(url) {
    const response = await fetch(url, { cache: "force-cache" });
    const text = await response.text();
    return { ok: response.ok, status: response.status, text };
  }

  async function probeModule(url) {
    try {
      const mod = await import(url);
      const keys = Object.keys(mod || {});
      return {
        url,
        label: label(url),
        ok: true,
        exportKeys: keys,
        exportSummaries: keys.slice(0, 220).map((key) => {
          const value = mod[key];
          const summary = { key, type: typeof value };
          if (value && typeof value === "object") {
            summary.objectKeys = Object.keys(value).slice(0, 40);
            summary.hasSendRequest = typeof value.sendRequest === "function";
          }
          if (typeof value === "function") {
            summary.functionName = value.name || "";
            const source = Function.prototype.toString.call(value);
            const matches = patternStrings.filter((pattern) => source.includes(pattern));
            if (matches.length > 0 || /sendRequest|messageHandler|model\/list|list-models-for-host/.test(source)) {
              summary.sourceLength = source.length;
              summary.sourceMatches = matches;
              summary.sourceSnippet = source.slice(0, 1200).replace(/\s+/g, " ").trim();
            }
          }
          return summary;
        }),
      };
    } catch (error) {
      return {
        url,
        label: label(url),
        ok: false,
        error: String(error && (error.message || error) || error),
      };
    }
  }

  function summarizeModelListResult(value) {
    const summary = {
      type: Array.isArray(value) ? "array" : typeof value,
      keys: value && typeof value === "object" && !Array.isArray(value) ? Object.keys(value).slice(0, 30) : [],
      count: 0,
      models: [],
      defaultModels: [],
      rawShape: ""
    };
    const arrays = [];
    if (Array.isArray(value)) arrays.push(value);
    if (Array.isArray(value && value.data)) arrays.push(value.data);
    if (Array.isArray(value && value.models)) arrays.push(value.models);
    if (Array.isArray(value && value.result)) arrays.push(value.result);
    if (Array.isArray(value && value.result && value.result.data)) arrays.push(value.result.data);
    if (Array.isArray(value && value.result && value.result.models)) arrays.push(value.result.models);
    const selected = arrays.find((items) => items.length > 0) || arrays[0] || [];
    summary.count = selected.length;
    summary.models = selected
      .map((item) => typeof item === "string" ? item : item && (item.model || item.id || item.slug))
      .filter(Boolean)
      .slice(0, 50);
    summary.defaultModels = selected
      .filter((item) => item && typeof item === "object" && item.isDefault)
      .map((item) => item.model || item.id || item.slug)
      .filter(Boolean)
      .slice(0, 10);
    if (Array.isArray(value)) summary.rawShape = "array";
    else if (Array.isArray(value && value.data)) summary.rawShape = "object.data";
    else if (Array.isArray(value && value.models)) summary.rawShape = "object.models";
    else if (Array.isArray(value && value.result && value.result.data)) summary.rawShape = "object.result.data";
    else if (Array.isArray(value && value.result && value.result.models)) summary.rawShape = "object.result.models";
    else summary.rawShape = summary.keys.join(",");
    return summary;
  }

  function redactValue(key, value) {
    if (/api.?key|token|secret|password|authorization|bearer/i.test(String(key || ""))) {
      return "[redacted]";
    }
    return value;
  }

  function summarizeConfigResult(value) {
    const config = value && typeof value === "object" ? value.config || value : null;
    const providers = config && typeof config === "object" ? config.model_providers || {} : {};
    const custom = providers && typeof providers === "object" ? providers.custom || {} : {};
    const origins = value && typeof value === "object" ? value.origins || {} : {};
    return {
      type: Array.isArray(value) ? "array" : typeof value,
      keys: value && typeof value === "object" && !Array.isArray(value) ? Object.keys(value).slice(0, 40) : [],
      model: config && config.model,
      modelProvider: config && config.model_provider,
      modelCatalogJson: config && config.model_catalog_json,
      reasoningEffort: config && config.model_reasoning_effort,
      disableResponseStorage: config && config.disable_response_storage,
      customProvider: custom && typeof custom === "object" ? {
        name: custom.name,
        baseUrl: custom.base_url,
        wireApi: custom.wire_api,
        requiresOpenAiAuth: custom.requires_openai_auth,
        envKey: redactValue("env_key", custom.env_key),
        auth: redactValue("auth", custom.auth),
        experimentalBearerToken: redactValue("experimental_bearer_token", custom.experimental_bearer_token),
      } : null,
      origins: origins && typeof origins === "object" ? {
        model: origins.model || null,
        modelProvider: origins.model_provider || null,
        modelCatalogJson: origins.model_catalog_json || null,
        customBaseUrl: origins["model_providers.custom.base_url"] || null,
      } : null,
    };
  }

  function visibleText(element) {
    return String(element && (element.innerText || element.textContent) || "")
      .replace(/\s+/g, " ")
      .trim();
  }

  function modelDomProbe() {
    const interesting = /gpt|deepseek|flash|pro|5\.5|模型|model|reasoning|超高|high|xhigh|medium|low/i;
    const buttons = Array.from(document.querySelectorAll("button"))
      .map((button, index) => ({
        index,
        text: visibleText(button).slice(0, 160),
        ariaLabel: button.getAttribute("aria-label"),
        title: button.getAttribute("title"),
        ariaExpanded: button.getAttribute("aria-expanded"),
        ariaHasPopup: button.getAttribute("aria-haspopup"),
        role: button.getAttribute("role"),
        dataState: button.getAttribute("data-state"),
      }))
      .filter((item) => interesting.test(`${item.text} ${item.ariaLabel || ""} ${item.title || ""}`))
      .slice(0, 80);
    const menuItems = Array.from(document.querySelectorAll([
      "[role=\"menuitem\"]",
      "[role=\"option\"]",
      "[cmdk-item]",
      "[data-radix-collection-item]",
    ].join(",")))
      .map((element, index) => ({
        index,
        tag: element.tagName,
        role: element.getAttribute("role"),
        text: visibleText(element).slice(0, 180),
        ariaSelected: element.getAttribute("aria-selected"),
        dataState: element.getAttribute("data-state"),
        dataValue: element.getAttribute("data-value"),
      }))
      .filter((item) => interesting.test(`${item.text} ${item.dataValue || ""}`))
      .slice(0, 120);
    return {
      buttonCount: document.querySelectorAll("button").length,
      menuCandidateCount: document.querySelectorAll("[role=\"menuitem\"],[role=\"option\"],[cmdk-item],[data-radix-collection-item]").length,
      buttons,
      menuItems,
      bodyHasDeepSeek: document.body ? /deepseek/i.test(document.body.innerText || "") : false,
      bodyHasGpt: document.body ? /gpt|5\.5/i.test(document.body.innerText || "") : false,
    };
  }

  async function runtimeModelListProbe(candidateUrls) {
    const output = {
      attempted: true,
      url: "",
      exportFound: false,
      hasSendRequest: false,
      calls: [],
      configCalls: [],
      dom: modelDomProbe(),
    };
    const hostConfigUrl = candidateUrls.find((url) => /use-host-config-/i.test(label(url)));
    if (!hostConfigUrl) {
      output.error = "use-host-config asset was not found in candidate URLs.";
      return output;
    }
    output.url = label(hostConfigUrl);
    try {
      const module = await import(hostConfigUrl);
      const bridge = module && module.Vt;
      output.exportFound = !!bridge;
      output.hasSendRequest = !!(bridge && typeof bridge.sendRequest === "function");
      if (!output.hasSendRequest) {
        output.error = "Vt.sendRequest was not available.";
        return output;
      }
      const callInputs = [
        {
          method: "list-models-for-host",
          params: { hostId: "local", includeHidden: true, cursor: null, limit: 100 }
        },
        {
          method: "list-models-for-host",
          params: { hostId: "local", cursor: null, limit: 100 }
        }
      ];
      for (const input of callInputs) {
        const started = performance.now();
        try {
          const result = await bridge.sendRequest(input.method, input.params);
          output.calls.push({
            method: input.method,
            params: input.params,
            ok: true,
            durationMs: Math.round(performance.now() - started),
            result: summarizeModelListResult(result)
          });
        } catch (error) {
          output.calls.push({
            method: input.method,
            params: input.params,
            ok: false,
            durationMs: Math.round(performance.now() - started),
            error: String(error && (error.message || error) || error).slice(0, 500)
          });
        }
      }
      const configCallInputs = [
        {
          method: "read-config-for-host",
          params: { hostId: "local" }
        },
        {
          method: "send-cli-request-for-host",
          params: { hostId: "local", method: "read-config-for-host", params: { hostId: "local" } }
        }
      ];
      for (const input of configCallInputs) {
        const started = performance.now();
        try {
          const result = await bridge.sendRequest(input.method, input.params);
          output.configCalls.push({
            method: input.method,
            params: input.params,
            ok: true,
            durationMs: Math.round(performance.now() - started),
            result: summarizeConfigResult(result)
          });
        } catch (error) {
          output.configCalls.push({
            method: input.method,
            params: input.params,
            ok: false,
            durationMs: Math.round(performance.now() - started),
            error: String(error && (error.message || error) || error).slice(0, 500)
          });
        }
      }
    } catch (error) {
      output.error = String(error && (error.message || error) || error).slice(0, 500);
    }
    output.dom = modelDomProbe();
    return output;
  }

  const scripts = Array.from(document.scripts || []).map((script) => script.src).filter(Boolean);
  const links = Array.from(document.querySelectorAll("link[href]") || []).map((link) => link.href).filter(Boolean);
  const resources = performance.getEntriesByType("resource").map((entry) => ({
    name: entry.name,
    label: label(entry.name),
    initiatorType: entry.initiatorType,
    duration: Math.round(entry.duration || 0),
    transferSize: entry.transferSize || 0,
    decodedBodySize: entry.decodedBodySize || 0,
  }));
  const initialJsUrls = unique([
    ...scripts,
    ...links.filter((url) => String(url).split("?")[0].endsWith(".js")),
    ...resources.map((entry) => entry.name).filter((url) => String(url).split("?")[0].endsWith(".js")),
  ]);

  const assets = [];
  const candidateUrls = new Set();
  const jsUrls = [];
  const queuedUrls = new Set();
  const queue = [];
  const maxAssetCount = options.maxAssets || 90;

  function enqueueJsUrl(url) {
    const normalized = String(url || "");
    if (!normalized || queuedUrls.has(normalized) || !normalized.split("?")[0].endsWith(".js")) return;
    queuedUrls.add(normalized);
    queue.push(normalized);
  }

  for (const url of initialJsUrls) enqueueJsUrl(url);

  while (queue.length > 0 && assets.length < maxAssetCount) {
    const url = queue.shift();
    jsUrls.push(url);
    const item = { url, label: label(url), fetched: false };
    try {
      const fetched = await fetchText(url);
      item.fetched = true;
      item.status = fetched.status;
      item.ok = fetched.ok;
      item.bytes = fetched.text.length;
      item.imports = importSpecifiers(fetched.text);
      item.relativeJsRefs = relativeJsSpecifiers(fetched.text);
      item.exports = exportSpecifiers(fetched.text);
      item.patterns = {};
      item.snippets = {};
      for (const pattern of patternStrings) {
        const count = countOccurrences(fetched.text, pattern);
        if (count > 0) {
          item.patterns[pattern] = count;
          item.snippets[pattern] = snippets(fetched.text, pattern, options.maxSnippets || 4);
        }
      }
      item.relevant = looksRelevant(url, fetched.text);
      if (item.relevant) candidateUrls.add(url);
      for (const specifier of unique([...item.imports, ...item.relativeJsRefs])) {
        if (shouldFollowSpecifier(specifier)) {
          try {
            const resolved = new URL(specifier, url).toString();
            candidateUrls.add(resolved);
            enqueueJsUrl(resolved);
          } catch {}
        }
      }
    } catch (error) {
      item.error = String(error && (error.message || error) || error);
    }
    assets.push(item);
  }

  const candidateList = Array.from(candidateUrls).slice(0, 30);
  const moduleProbes = options.probeModules
    ? await Promise.all(candidateList.map((url) => probeModule(url)))
    : [];
  const runtimeModelList = options.traceModelList
    ? await runtimeModelListProbe(candidateList)
    : { attempted: false };

  const globals = Object.getOwnPropertyNames(window)
    .filter((key) => /codex|model|mcp|host|statsig|app/i.test(key))
    .slice(0, 200);

  return {
    location: location.href,
    title: document.title,
    readyState: document.readyState,
    scripts: scripts.map((url) => ({ url, label: label(url) })),
    resourceCount: resources.length,
    resources: resources.slice(0, 200),
    jsAssetCount: jsUrls.length,
    assets,
    candidateUrls: candidateList.map((url) => ({ url, label: label(url) })),
    moduleProbes,
    runtimeModelList,
    modelDom: modelDomProbe(),
    globals,
    existingCodeSeeXState: window.__codeseexModelCatalogUnlock || null,
  };
}

function summarizeFindings(data) {
  const inspected = data.targets.filter((target) => target.attached && target.inspection);
  if (inspected.length === 0) {
    addConclusion("No inspectable Codex page target was found.");
    return;
  }

  const allAssets = inspected.flatMap((target) => target.inspection.assets || []);
  const hasListModels = allAssets.some((asset) => asset.patterns && asset.patterns["list-models-for-host"]);
  const hasModelList = allAssets.some((asset) => asset.patterns && asset.patterns["model/list"]);
  const hasUseHostConfig = allAssets.some(
    (asset) => /use-host-config/i.test(asset.label) || (asset.patterns && asset.patterns["use-host-config-"]),
  );
  const bridgeProbe = inspected
    .flatMap((target) => target.inspection.moduleProbes || [])
    .flatMap((probe) => probe.exportSummaries || [])
    .find((summary) => summary.hasSendRequest);

  if (hasListModels) addConclusion("Static scan found `list-models-for-host` in renderer assets.");
  else addConclusion("Static scan did not find `list-models-for-host`; Codex may have renamed the app-server method or moved the model list path.");

  if (hasUseHostConfig) addConclusion("Static scan found a `use-host-config` candidate asset.");
  else addConclusion("Static scan did not find a `use-host-config` asset; the previous injection point is likely stale.");

  if (probeModules) {
    if (bridgeProbe) addConclusion(`Module probe found an object export with sendRequest: ${bridgeProbe.key}.`);
    else addConclusion("Module probe did not find any export object with sendRequest in candidate modules.");
  } else {
    addConclusion("Module export shape was not probed. Rerun with --probe-modules for export keys and sendRequest evidence.");
  }

  if (hasModelList) addConclusion("Static scan found `model/list`, so the MCP/app-server model list path may still exist.");
}

function renderMarkdown(data, jsonPath) {
  const lines = [];
  lines.push("# Codex App Model List Diagnostic");
  lines.push("");
  lines.push(`- Started: ${data.startedAt}`);
  lines.push(`- Debug port: ${data.debugPort}`);
  lines.push(`- Probe modules: ${data.probeModules ? "yes" : "no"}`);
  lines.push(`- Raw JSON: ${jsonPath}`);
  if (data.scannedPorts) {
    lines.push(`- Scanned ports: ${data.scannedPorts[0]}-${data.scannedPorts[data.scannedPorts.length - 1]}`);
    lines.push(`- Existing CDP detected before launch: ${data.portDetected ? "yes" : "no"}`);
  }
  lines.push("");
  lines.push("## Conclusion");
  lines.push("");
  for (const item of data.conclusion || []) lines.push(`- ${item}`);
  if (!data.conclusion || data.conclusion.length === 0) lines.push("- No conclusion generated.");
  lines.push("");

  lines.push("## Launch");
  lines.push("");
  lines.push("```json");
  lines.push(JSON.stringify(data.launch, null, 2));
  lines.push("```");
  lines.push("");

  lines.push("## CDP");
  lines.push("");
  lines.push("```json");
  lines.push(JSON.stringify(data.cdp, null, 2));
  lines.push("```");
  lines.push("");

  lines.push("## Targets");
  lines.push("");
  for (const target of data.targets || []) {
    lines.push(`### ${target.title || "(untitled)"}`);
    lines.push("");
    lines.push(`- ID: \`${target.id}\``);
    lines.push(`- URL: ${target.url || "(empty)"}`);
    lines.push(`- Attached: ${target.attached ? "yes" : "no"}`);
    if (target.error) lines.push(`- Error: ${target.error}`);
    const inspection = target.inspection;
    if (!inspection) {
      lines.push("");
      continue;
    }
    lines.push(`- Renderer location: ${inspection.location}`);
    lines.push(`- Ready state: ${inspection.readyState}`);
    lines.push(`- JS assets scanned: ${inspection.jsAssetCount}`);
    lines.push(`- Candidate URLs: ${(inspection.candidateUrls || []).length}`);
    lines.push("");

    const relevantAssets = (inspection.assets || []).filter((asset) => asset.relevant);
    lines.push("#### Relevant Assets");
    lines.push("");
    if (relevantAssets.length === 0) {
      lines.push("- None.");
    } else {
      for (const asset of relevantAssets.slice(0, 30)) {
        const patterns = Object.entries(asset.patterns || {})
          .map(([key, count]) => `${key}=${count}`)
          .join(", ");
        lines.push(`- \`${asset.label}\` (${asset.bytes || 0} chars) ${patterns ? `- ${patterns}` : ""}`);
        for (const [pattern, snippetsForPattern] of Object.entries(asset.snippets || {}).slice(0, 8)) {
          for (const snippet of snippetsForPattern.slice(0, 2)) {
            lines.push(`  - ${pattern}: \`${escapeBackticks(snippet)}\``);
          }
        }
        if (asset.imports && asset.imports.length) {
          lines.push(`  - imports: ${asset.imports.slice(0, 12).map((item) => `\`${item}\``).join(", ")}`);
        }
        if (asset.relativeJsRefs && asset.relativeJsRefs.length) {
          lines.push(`  - js refs: ${asset.relativeJsRefs.slice(0, 12).map((item) => `\`${item}\``).join(", ")}`);
        }
        if (asset.exports && asset.exports.length) {
          lines.push(`  - exports: ${asset.exports.slice(0, 20).map((item) => `\`${item}\``).join(", ")}`);
        }
      }
    }
    lines.push("");

    if (inspection.moduleProbes && inspection.moduleProbes.length) {
      lines.push("#### Module Probes");
      lines.push("");
      for (const probe of inspection.moduleProbes) {
        lines.push(`- \`${probe.label}\`: ${probe.ok ? "ok" : "failed"}`);
        if (probe.error) lines.push(`  - error: ${probe.error}`);
        if (probe.exportKeys) lines.push(`  - export keys: ${probe.exportKeys.map((key) => `\`${key}\``).join(", ")}`);
        for (const summary of (probe.exportSummaries || []).filter((item) => item.hasSendRequest)) {
          lines.push(`  - sendRequest export candidate: \`${summary.key}\`, object keys: ${(summary.objectKeys || []).map((key) => `\`${key}\``).join(", ")}`);
        }
        for (const summary of (probe.exportSummaries || []).filter((item) => item.sourceSnippet)) {
          lines.push(`  - function source candidate: \`${summary.key}\` (${summary.sourceMatches?.join(", ") || "sendRequest-like"})`);
          lines.push(`    - \`${escapeBackticks(summary.sourceSnippet)}\``);
        }
      }
      lines.push("");
    }

    if (inspection.runtimeModelList && inspection.runtimeModelList.attempted) {
      lines.push("#### Runtime Model List Probe");
      lines.push("");
      const probe = inspection.runtimeModelList;
      lines.push(`- URL: ${probe.url || "(none)"}`);
      lines.push(`- Export found: ${probe.exportFound ? "yes" : "no"}`);
      lines.push(`- Has sendRequest: ${probe.hasSendRequest ? "yes" : "no"}`);
      if (probe.error) lines.push(`- Error: ${probe.error}`);
      for (const call of probe.calls || []) {
        const result = call.result || {};
        lines.push(`- \`${call.method}\` ${call.ok ? "ok" : "failed"} (${call.durationMs} ms)`);
        if (call.error) lines.push(`  - error: ${call.error}`);
        if (call.ok) {
          lines.push(`  - shape: ${result.rawShape || result.type || "unknown"}`);
          lines.push(`  - count: ${result.count || 0}`);
          lines.push(`  - models: ${(result.models || []).map((item) => `\`${item}\``).join(", ") || "(none)"}`);
          lines.push(`  - defaults: ${(result.defaultModels || []).map((item) => `\`${item}\``).join(", ") || "(none)"}`);
        }
      }
      if (probe.configCalls && probe.configCalls.length) {
        lines.push("");
        lines.push("#### Runtime Config Probe");
        lines.push("");
        for (const call of probe.configCalls || []) {
          lines.push(`- \`${call.method}\` ${call.ok ? "ok" : "failed"} (${call.durationMs} ms)`);
          if (call.error) lines.push(`  - error: ${call.error}`);
          if (call.ok) {
            const result = call.result || {};
            lines.push(`  - model: \`${result.model || "(none)"}\``);
            lines.push(`  - model provider: \`${result.modelProvider || "(none)"}\``);
            lines.push(`  - model catalog json: \`${result.modelCatalogJson || "(none)"}\``);
            lines.push(`  - reasoning effort: \`${result.reasoningEffort || "(none)"}\``);
            if (result.customProvider) {
              lines.push(`  - custom provider: \`${result.customProvider.name || "(none)"}\``);
              lines.push(`  - custom base url: \`${result.customProvider.baseUrl || "(none)"}\``);
              lines.push(`  - wire api: \`${result.customProvider.wireApi || "(none)"}\``);
              lines.push(`  - requires OpenAI auth: \`${String(result.customProvider.requiresOpenAiAuth)}\``);
            }
          }
        }
      }
      const dom = probe.dom || inspection.modelDom;
      if (dom) {
        lines.push("");
        lines.push("#### Runtime Model DOM Probe");
        lines.push("");
        lines.push(`- Buttons scanned: ${dom.buttonCount || 0}`);
        lines.push(`- Menu candidates scanned: ${dom.menuCandidateCount || 0}`);
        lines.push(`- Body has DeepSeek text: ${dom.bodyHasDeepSeek ? "yes" : "no"}`);
        lines.push(`- Body has GPT text: ${dom.bodyHasGpt ? "yes" : "no"}`);
        for (const button of (dom.buttons || []).slice(0, 20)) {
          lines.push(`- button #${button.index}: \`${escapeBackticks(button.text || button.ariaLabel || "(empty)")}\``);
        }
        for (const item of (dom.menuItems || []).slice(0, 30)) {
          lines.push(`- menu #${item.index}: \`${escapeBackticks(item.text || item.dataValue || "(empty)")}\``);
        }
      }
      lines.push("");
    }

    lines.push("#### Global Keys");
    lines.push("");
    lines.push((inspection.globals || []).map((key) => `\`${key}\``).join(", ") || "None.");
    lines.push("");
  }

  lines.push("## Usage");
  lines.push("");
  lines.push("```powershell");
  lines.push("node scripts/codex-app-model-list-diagnostic.mjs --launch");
  lines.push("node scripts/codex-app-model-list-diagnostic.mjs --connect-only --probe-modules");
  lines.push("node scripts/codex-app-model-list-diagnostic.mjs --connect-only --probe-modules --trace-model-list");
  lines.push("```");
  lines.push("");
  return lines.join("\n");
}

function escapeBackticks(value) {
  return String(value).replaceAll("`", "\\`");
}

await main();
