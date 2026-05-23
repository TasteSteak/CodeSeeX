const http = require("node:http");
const { execFile } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { readCodexAuthApiKey } = require("../shared/codex-auth");
const { DATA_DIR, ROOT_DIR, loadProxyConfig, normalizeCatalogMode, normalizeDeepSeekBaseUrl, normalizeUpstreamModelOverride } = require("../shared/config");
const { addCorsHeaders, enforceLocalAccess, handleHttpError, httpError, parseJsonResponse, readJsonBody, sendJson } = require("../shared/http");
const { readJson } = require("../shared/json-store");
const { appendEventLog, eventLogDir, eventLogPath: datedEventLogPath, readEventLogTail } = require("../shared/event-log");
const { buildCodeSeeXCatalog, codeSeeXUserDir, codexAdapterCatalogPath, codexCliInvocation, TARGET_MODELS, validateCodeSeeXCatalog } = require("../codex/model-catalog");
const { createProxyContext, handleRequest: handleProxyRequest, markProxyRunning, markProxyStopped } = require("../proxy/server");
const { PRODUCT_DESCRIPTION, PRODUCT_NAME } = require("../shared/product");
const { repairMojibakeText } = require("../shared/text-encoding");
const { listPublicTools, normalizeToolRuntimeConfig, sanitizeToolConfig, toolDefaultConfig } = require("../shared/tool-registry");
const { toolAssetFilePath } = require("../tools");
const { mergeEnv, readEnvFile, writeEnvFile } = require("./env-file");
const { DEFAULT_LANGUAGE_ID, LANG_DIR, SYSTEM_LANGUAGE_ID, languageFilePath, listLanguages } = require("./languages");
const { contentTypeFor, readIndexPage, staticFilePath } = require("./page");

const HOST = "127.0.0.1";
const PORT = 8787;
const FIXED_DEEPSEEK_BASE_URL = "https://api.deepseek.com/";
const FIXED_THINKING_TITLE = "DeepSeek Thinking";
const CODEX_ADAPTER_PROVIDER_ID = "custom";
const CODEX_ADAPTER_STATUS_IDLE = "idle";
const CODEX_ADAPTER_STATUS_READY = "ready";
const CODEX_ADAPTER_STATUS_GENERATING = "generating";
const CODEX_ADAPTER_STATUS_ERROR = "error";
const UPDATE_CHECK_TTL_MS = 30 * 60 * 1000;
const UPDATE_CHECK_TIMEOUT_MS = 8000;
const BACKGROUND_MAINTENANCE_DELAY_MS = 3000;
const DEFAULT_CATALOG_MODE = "default";
const DEFAULT_UPSTREAM_MODEL_OVERRIDE = "default";
const DEFAULT_AUTO_START = "false";
const LICENSE_DISPLAY_NAMES = {
  "GPL-3.0-only": "GPLv3",
  "AGPL-3.0-only": "AGPLv3",
};
let cachedPackageJson = null;
let cachedUpdateCheck = null;
const catalogMaintenanceByDataDir = new Map();

function main(options = {}) {
  return startManager(options).catch((error) => {
    console.error("[manager] Failed to start:", error && error.message ? error.message : error);
    process.exit(1);
  });
}

function startManager(options = {}) {
  const rootDir = options.rootDir || ROOT_DIR;
  const dataDir = options.dataDir || DATA_DIR;
  const bootstrapEnv = loadBootstrapEnv(dataDir, rootDir);
  const host = options.host || bootstrapEnv.PROXY_HOST || process.env.PROXY_HOST || HOST;
  const requestedPort = options.port !== undefined ? options.port : bootstrapEnv.PROXY_PORT;
  const port = clampPort(requestedPort, PORT);
  const exitOnClose = options.exitOnClose !== false;

  ensureRuntimeDirectories(dataDir);
  ensureProxyEnv(dataDir, rootDir);
  const proxyEnv = loadLightProxyEnv(dataDir, rootDir);
  notifyConfigChanged({ onConfigChanged: options.onConfigChanged }, publicConfig(proxyEnv));
  const embeddedProxy = options.embeddedProxy || createEmbeddedProxy(rootDir, dataDir, proxyEnv);
  const controller = options.controller || embeddedProxy;

  controller.dataDir = dataDir;
  controller.rootDir = rootDir;
  const context = { controller, dataDir, rootDir, embeddedProxy, onConfigChanged: options.onConfigChanged, onWindowAction: options.onWindowAction };
  const server = http.createServer((req, res) => routeCombinedRequest(req, res, context));
  let stopped = false;
  let exitTimer = null;
  const processHandlers = {
    sigint: () => closeManager(true),
    sigterm: () => closeManager(true),
  };
  process.on("SIGINT", processHandlers.sigint);
  process.on("SIGTERM", processHandlers.sigterm);
  server.once("close", cleanupProcessHandlers);

  const ready = new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => {
      server.off("error", reject);
      const address = server.address();
      const actualPort = typeof address === "object" && address ? address.port : port;
      const url = "http://" + host + ":" + actualPort;
      scheduleStartupBackgroundTasks({ dataDir, rootDir });
      embeddedProxy.start({ host, port: actualPort });
      appendManagerEvent(dataDir, rootDir, "manager_started", "success", "Manager service started.", {
        url,
      });
      console.log("[manager] UI available at " + url);
      resolve({
        close: () => closeManager(exitOnClose),
        controller,
        dataDir,
        server,
        url,
      });
    });
  });

  return ready;

  function cleanupProcessHandlers() {
    process.off("SIGINT", processHandlers.sigint);
    process.off("SIGTERM", processHandlers.sigterm);
    if (exitTimer) clearTimeout(exitTimer);
    exitTimer = null;
  }

  function closeManager(exitProcess = exitOnClose) {
    markStopped();
    return new Promise((resolve) => closeServer(() => {
      resolve();
      if (exitProcess) process.exit(0);
    }, exitProcess));
  }

  function markStopped() {
    if (stopped) return;
    stopped = true;
    appendManagerEvent(dataDir, rootDir, "manager_stopped", "info", "Manager service stopped.", null);
    controller.stop();
    if (controller !== embeddedProxy) embeddedProxy.stop();
  }

  function closeServer(callback, exitProcess) {
    if (!server.listening) {
      cleanupProcessHandlers();
      callback();
      return;
    }
    server.close(callback);
    if (exitProcess) {
      exitTimer = setTimeout(() => process.exit(0), 500);
      exitTimer.unref();
    }
  }
}

async function routeCombinedRequest(req, res, context) {
  if (isProxyRoute(req)) {
    return context.embeddedProxy.handleRequest(req, res);
  }
  return handleRequest(req, res, context);
}

function isProxyRoute(req) {
  const pathname = String((req && req.url) || "").split("?")[0] || "/";
  if (pathname === "/v1" || pathname.startsWith("/v1/")) return true;
  if (req && req.method === "OPTIONS") {
    return pathname === "/healthz"
      || pathname === "/models"
      || pathname === "/responses"
      || pathname.startsWith("/responses/");
  }
  return pathname === "/healthz"
    || pathname === "/models"
    || pathname === "/responses"
    || pathname.startsWith("/responses/");
}

function scheduleStartupBackgroundTasks(options = {}) {
  const { dataDir, rootDir } = options;
  const bootstrap = setImmediate(() => {
    ensureCodexAdapterBootstrapCatalog(dataDir, rootDir);
  });
  if (bootstrap && typeof bootstrap.unref === "function") bootstrap.unref();

  const timer = setTimeout(() => {
    scheduleCodexAdapterMaintenance(dataDir, rootDir);
  }, BACKGROUND_MAINTENANCE_DELAY_MS);
  if (timer && typeof timer.unref === "function") timer.unref();
}

function embeddedStatusRuntime(runtime, state = {}) {
  const endpoint = state.endpoint || {};
  const baseRuntime = runtime && typeof runtime === "object" ? Object.assign({}, runtime) : {};
  if (state.running && baseRuntime.status === "running") return baseRuntime;
  if (state.starting) {
    return Object.assign(baseRuntime, {
      status: "starting",
      pid: process.pid,
      host: baseRuntime.host || endpoint.host || "127.0.0.1",
      port: baseRuntime.port || endpoint.port || null,
      base_url: baseRuntime.base_url || (endpoint.host && endpoint.port ? "http://" + endpoint.host + ":" + endpoint.port + "/v1" : null),
    });
  }
  if (baseRuntime.status === "running") {
    return Object.assign(baseRuntime, {
      status: "stopped",
      pid: null,
      active_requests: 0,
    });
  }
  return Object.keys(baseRuntime).length > 0 ? baseRuntime : null;
}

function createEmbeddedProxy(rootDir, dataDir, env) {
  let proxyContext = null;
  let initialEnv = Object.assign({}, env || {});
  let listenEndpoint = {
    host: initialEnv.PROXY_HOST || "127.0.0.1",
    port: clampPort(initialEnv.PROXY_PORT, PORT),
  };
  let running = false;
  let starting = false;
  let lastError = null;
  const stdout = [];
  const stderr = [];

  function start(nextEnv = {}) {
    if (running) return status();
    if (starting) return status();
    starting = true;
    mergeStartOptions(nextEnv);
    try {
      refreshProxyEnvForStart();
      ensureProxyContext();
      const host = listenEndpoint.host || proxyContext.config.host;
      const port = listenEndpoint.port || proxyContext.config.port;
      markProxyRunning(proxyContext, { host, port, message: "Embedded proxy service started." });
      running = true;
      lastError = null;
      pushControllerLine(stdout, "[proxy] Embedded proxy service started at " + proxyContext.runtime.base_url);
    } catch (error) {
      lastError = error && error.message ? error.message : String(error);
      markEmbeddedProxyError(proxyContext, lastError);
      pushControllerLine(stderr, "[proxy] Failed to start embedded proxy: " + lastError);
    } finally {
      starting = false;
    }
    return status();
  }

  function stop() {
    if (!running && !starting) return status();
    starting = false;
    if (!proxyContext) return status();
    markProxyStopped(proxyContext, { message: "Embedded proxy service stopped." });
    running = false;
    pushControllerLine(stdout, "[proxy] Embedded proxy service stopped");
    return status();
  }

  function restart(nextEnv = {}) {
    stop();
    if (nextEnv && Object.keys(nextEnv).length > 0) {
      initialEnv = Object.assign({}, initialEnv, nextEnv);
      proxyContext = null;
    }
    return start({});
  }

  function updateConfig(nextEnv = {}) {
    initialEnv = Object.assign({}, initialEnv, nextEnv);
    if (proxyContext) {
      const nextConfig = loadProxyConfigForManager(rootDir, dataDir, initialEnv);
      Object.assign(proxyContext.config, nextConfig);
    }
    return status();
  }

  function status() {
    const runtime = proxyContext ? proxyContext.runtime : readJson(path.join(dataDir, "runtime.json"), null);
    const runningRuntime = runtime && runtime.status === "running";
    const statusRuntime = embeddedStatusRuntime(runtime, { running, starting, endpoint: listenEndpoint });
    return {
      mode: "embedded",
      running: Boolean(running && runningRuntime),
      pid: running || starting ? process.pid : null,
      last_error: lastError,
      stdout: stdout.slice(-80),
      stderr: stderr.slice(-80),
      runtime: statusRuntime,
    };
  }

  function handleRequest(req, res) {
    if (!running) {
      start({});
    }
    if (!running) {
      sendJson(res, 503, { error: { message: "Proxy service is stopped.", type: "server_error", code: "proxy_stopped" } });
      return;
    }
    return handleProxyRequest(req, res, proxyContext);
  }

  function mergeStartOptions(nextEnv = {}) {
    if (nextEnv && Object.keys(nextEnv).length > 0 && !nextEnv.host && !nextEnv.port) {
      initialEnv = Object.assign({}, initialEnv, nextEnv);
      proxyContext = null;
    }
    if (nextEnv && (nextEnv.host || nextEnv.port)) {
      listenEndpoint = {
        host: nextEnv.host || listenEndpoint.host,
        port: nextEnv.port || listenEndpoint.port,
      };
    }
  }

  function ensureProxyContext() {
    if (!proxyContext) {
      proxyContext = createProxyContext(loadProxyConfigForManager(rootDir, dataDir, initialEnv));
    }
    return proxyContext;
  }

  function refreshProxyEnvForStart() {
    initialEnv = Object.assign({}, initialEnv, loadLightProxyEnv(dataDir, rootDir));
    proxyContext = null;
  }

  return { dataDir, rootDir, handleRequest, restart, start, status, stop, updateConfig };
}

function markEmbeddedProxyError(proxyContext, message) {
  try {
    if (!proxyContext || !proxyContext.runtime || !proxyContext.config) return;
    proxyContext.runtime.status = "error";
    proxyContext.runtime.stopped_at = new Date().toISOString();
    proxyContext.runtime.error = { message: message || "Embedded proxy failed to start.", code: "embedded_proxy_start_failed" };
    require("../proxy/runtime").writeRuntime(proxyContext.config, proxyContext.runtime);
  } catch {}
}

function loadProxyConfigForManager(rootDir, dataDir, env = {}) {
  return loadProxyConfig(Object.assign({}, env, {
    PROXY_ROOT_DIR: rootDir,
    PROXY_DATA_DIR: dataDir,
    PROXY_RUNTIME_FILE: env.PROXY_RUNTIME_FILE || path.join(dataDir, "runtime.json"),
    PROXY_STATE_FILE: env.PROXY_STATE_FILE || path.join(dataDir, "proxy-state.json"),
    PROXY_DEBUG_DIR: env.PROXY_DEBUG_DIR || path.join(dataDir, "debug"),
    PROXY_PARENT_PID: "0",
  }));
}

function pushControllerLine(target, line) {
  target.push({ at: new Date().toISOString(), line: repairMojibakeText(line) });
  if (target.length > 200) target.splice(0, target.length - 200);
}

async function handleRequest(req, res, context) {
  const { controller, dataDir } = context;
  if (!enforceLocalAccess(req, res)) return;
  if (req.method === "OPTIONS") {
    addCorsHeaders(req, res);
    res.writeHead(204);
    res.end();
    return;
  }

  try {
    const url = new URL(req.url, "http://" + (req.headers.host || "localhost"));

    if (req.method === "GET" && isStaticRoute(url.pathname)) {
      sendStaticFile(res, url.pathname, context);
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/status") {
      sendJson(res, 200, gatherStatus(controller, dataDir, context.rootDir));
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/app-info") {
      sendJson(res, 200, gatherAppInfo(context));
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/events") {
      sendJson(res, 200, gatherEvents(dataDir, context.rootDir, url.searchParams));
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/tools") {
      sendJson(res, 200, gatherTools(dataDir, context.rootDir));
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/languages") {
      sendJson(res, 200, gatherLanguages(dataDir));
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/deepseek/balance") {
      sendJson(res, 200, await fetchDeepSeekBalance(loadProxyEnv(dataDir, context.rootDir)));
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/codex-adapter") {
      sendJson(res, 200, gatherCodexAdapter(dataDir, context.rootDir));
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/codex-adapter/generate") {
      sendJson(res, 200, await generateCodexAdapterCatalog(dataDir, context.rootDir, { force: true }));
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/update-check") {
      sendJson(res, 200, await gatherUpdateCheck(context));
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/start") {
      appendManagerEvent(dataDir, context.rootDir, "manager_start_requested", "info", "User requested proxy start.", null);
      await Promise.resolve(controller.start(loadProxyEnv(dataDir, context.rootDir)));
      sendJson(res, 200, gatherStatus(controller, dataDir, context.rootDir));
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/stop") {
      appendManagerEvent(dataDir, context.rootDir, "manager_stop_requested", "info", "User requested proxy stop.", null);
      await Promise.resolve(controller.stop());
      sendJson(res, 200, gatherStatus(controller, dataDir, context.rootDir));
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/restart") {
      appendManagerEvent(dataDir, context.rootDir, "manager_restart_requested", "info", "User requested proxy restart.", null);
      await Promise.resolve(controller.restart(loadProxyEnv(dataDir, context.rootDir)));
      sendJson(res, 200, gatherStatus(controller, dataDir, context.rootDir));
      return;
    }

    if (req.method === "POST" && url.pathname.startsWith("/api/window/")) {
      const action = url.pathname.slice("/api/window/".length);
      const body = action === "theme" ? await readJsonBody(req, 4096) : null;
      handleWindowAction(action, context, body);
      sendJson(res, 200, { ok: true });
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/desktop/login-item") {
      const body = await readJsonBody(req, 4096);
      const enabled = normalizeBoolString(body && body.enabled);
      const next = mergeEnv(loadProxyEnv(dataDir, context.rootDir), { AUTO_START: enabled });
      writeEnvFile(proxyEnvPath(dataDir), next, proxyEnvOptions(dataDir, context.rootDir));
      notifyConfigChanged(context, publicConfig(next));
      handleWindowAction("login-item", context, { enabled: enabled === "true" });
      sendJson(res, 200, { ok: true, enabled: enabled === "true" });
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/config") {
      sendJson(res, 200, Object.assign(publicConfig(loadLightProxyEnv(dataDir, context.rootDir)), {
        config_version: configVersion(dataDir),
      }));
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/config") {
      const body = await readJsonBody(req, 1024 * 1024);
      const previous = loadProxyEnv(dataDir, context.rootDir);
      const next = mergeEnv(previous, sanitizeConfig(body, context.rootDir, dataDir));
      writeEnvFile(proxyEnvPath(dataDir), next, proxyEnvOptions(dataDir, context.rootDir));
      appendManagerEvent(dataDir, context.rootDir, "manager_config_saved", "success", "Configuration saved.", null);
      notifyConfigChanged(context, publicConfig(next));
      if (context.embeddedProxy && typeof context.embeddedProxy.updateConfig === "function") context.embeddedProxy.updateConfig(next);
      if (normalizeCatalogMode(previous.CATALOG_MODE) !== normalizeCatalogMode(next.CATALOG_MODE)) {
        await generateCodexAdapterCatalog(dataDir, context.rootDir, { force: true });
      }
      sendJson(res, 200, gatherStatus(controller, dataDir, context.rootDir));
      return;
    }

    sendJson(res, 404, { error: { message: "Route not found.", type: "invalid_request_error", code: "not_found" } });
  } catch (error) {
    handleHttpError(res, error);
  }
}

function gatherAppInfo(context) {
  const packageJson = readPackageJson(context.rootDir);
  const repositoryUrl = normalizeRepositoryUrl(packageJson.repository);
  const bugsUrl = packageJson.bugs && typeof packageJson.bugs === "object" ? packageJson.bugs.url : null;
  const homepage = typeof packageJson.homepage === "string" ? packageJson.homepage : null;
  const releaseUrl = repositoryUrl ? repositoryUrl.replace(/\/$/, "") + "/releases" : null;

  return {
    name: packageJson.name || "codeseex",
    product_name: PRODUCT_NAME,
    version: packageJson.version || "0.0.0",
    description: packageJson.description || PRODUCT_DESCRIPTION,
    default_language: DEFAULT_LANGUAGE_ID,
    license: displayLicenseName(packageJson.license),
    urls: {
      feedback: bugsUrl || null,
      source: repositoryUrl || homepage || null,
      license: repositoryUrl && packageJson.license ? repositoryUrl.replace(/\/$/, "") + "/blob/main/LICENSE" : null,
      releases: releaseUrl,
    },
  };
}

async function gatherUpdateCheck(context) {
  const packageJson = readPackageJson(context.rootDir);
  const currentVersion = normalizeVersion(packageJson.version || "0.0.0");
  const repositoryUrl = normalizeRepositoryUrl(packageJson.repository);
  const releasesUrl = repositoryUrl ? repositoryUrl.replace(/\/$/, "") + "/releases" : null;
  const apiUrl = githubLatestReleaseApiUrl(repositoryUrl);
  const now = Date.now();
  if (!apiUrl) {
    return updateCheckResult(false, currentVersion, "", releasesUrl, "No release API is configured.");
  }
  if (cachedUpdateCheck && cachedUpdateCheck.apiUrl === apiUrl && now - cachedUpdateCheck.checkedAtMs < UPDATE_CHECK_TTL_MS) {
    return cachedUpdateCheck.value;
  }

  const checkedAt = new Date().toISOString();
  const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
  const timeout = setTimeout(() => controller && controller.abort(), UPDATE_CHECK_TIMEOUT_MS);
  try {
    const response = await fetch(apiUrl, {
      headers: {
        Accept: "application/vnd.github+json",
        "User-Agent": "CodeSeeX/" + currentVersion,
      },
      signal: controller ? controller.signal : undefined,
    });
    const body = await response.json().catch(() => null);
    if (!response.ok) {
      throw new Error(body && body.message ? body.message : "Release request failed with HTTP " + response.status + ".");
    }
    const latestVersion = normalizeVersion(body && (body.tag_name || body.name) || "");
    const result = {
      ok: true,
      has_update: compareVersions(latestVersion, currentVersion) > 0,
      latest_version: latestVersion,
      current_version: currentVersion,
      url: (body && body.html_url) || releasesUrl || "",
      checked_at: checkedAt,
      error: "",
    };
    cachedUpdateCheck = { apiUrl, checkedAtMs: now, value: result };
    return result;
  } catch (error) {
    const result = updateCheckResult(false, currentVersion, "", releasesUrl, error && error.message ? error.message : String(error), checkedAt);
    cachedUpdateCheck = { apiUrl, checkedAtMs: now, value: result };
    return result;
  } finally {
    clearTimeout(timeout);
  }
}

function updateCheckResult(ok, currentVersion, latestVersion, url, error, checkedAt = new Date().toISOString()) {
  return {
    ok,
    has_update: false,
    latest_version: latestVersion || "",
    current_version: currentVersion || "",
    url: url || "",
    checked_at: checkedAt,
    error: error || "",
  };
}

function githubLatestReleaseApiUrl(repositoryUrl) {
  const match = /^https:\/\/github\.com\/([^/]+)\/([^/]+)$/i.exec(String(repositoryUrl || "").replace(/\/$/, ""));
  if (!match) return "";
  return "https://api.github.com/repos/" + encodeURIComponent(match[1]) + "/" + encodeURIComponent(match[2]) + "/releases/latest";
}

function scheduleCodexAdapterMaintenance(dataDir, rootDir = ROOT_DIR) {
  const key = path.resolve(dataDir);
  if (catalogMaintenanceByDataDir.has(key)) return;
  const timer = setImmediate(() => {
    generateCodexAdapterCatalog(dataDir, rootDir, { force: false }).catch(() => {});
  });
  catalogMaintenanceByDataDir.set(key, timer);
}

function ensureCodexAdapterBootstrapCatalog(dataDir, rootDir = ROOT_DIR) {
  const env = loadProxyEnv(dataDir, rootDir);
  const catalogMode = normalizeCatalogMode(env.CATALOG_MODE);
  if (isCodexAdapterReady(dataDir) && isCodexAdapterKnownSource(dataDir) && readCodexAdapterStatus(dataDir).catalog_mode === catalogMode) return;
  try {
    const catalogPath = codexAdapterCatalogPath(dataDir);
    const result = buildCodeSeeXCatalog({
      outputPath: catalogPath,
      rootDir,
      nativeCatalog: null,
      nativeError: new Error("Native Codex catalog has not been loaded yet."),
      allowFallback: true,
    });
    writeCodexAdapterStatus(dataDir, {
      status: CODEX_ADAPTER_STATUS_READY,
      updated_at: new Date().toISOString(),
      catalog_mode: catalogMode,
      error: "",
      base_model: result.baseModel || "",
      fallback: Boolean(result.fallback),
      source: result.source || "",
      warning: result.warning || "",
      target_models: result.targetModels || [],
    });
  } catch (error) {
    writeCodexAdapterError(dataDir, error, rootDir);
  }
}

async function generateCodexAdapterCatalog(dataDir, rootDir = ROOT_DIR, options = {}) {
  const catalogPath = codexAdapterCatalogPath(dataDir);
  const env = loadProxyEnv(dataDir, rootDir);
  const catalogMode = normalizeCatalogMode(env.CATALOG_MODE);
  const status = readCodexAdapterStatus(dataDir);
  if (!options.force && isCodexAdapterReady(dataDir) && isCodexAdapterKnownSource(dataDir) && status.catalog_mode === catalogMode) {
    if (catalogMode === "builtin" || isCodexAdapterNative(dataDir)) return gatherCodexAdapter(dataDir, rootDir);
  }
  writeCodexAdapterStatus(dataDir, {
    status: CODEX_ADAPTER_STATUS_GENERATING,
    updated_at: new Date().toISOString(),
    catalog_mode: catalogMode,
    error: "",
    base_model: status.base_model || "",
    fallback: Boolean(status.fallback),
    source: status.source || "",
    warning: status.warning || "",
    target_models: TARGET_MODELS.map((model) => model.slug),
  });
  let nativeCatalog = null;
  let nativeError = null;
  if (catalogMode === "auto" || catalogMode === "default") {
    try {
      nativeCatalog = await readNativeCodexCatalogAsync();
    } catch (error) {
      nativeError = error;
    }
  } else {
    nativeError = new Error("Catalog mode is builtin; native Codex catalog lookup skipped.");
  }
  try {
    const result = buildCodeSeeXCatalog({ outputPath: catalogPath, rootDir, nativeCatalog, nativeError, allowFallback: true });
    writeCodexAdapterStatus(dataDir, {
      status: CODEX_ADAPTER_STATUS_READY,
      updated_at: new Date().toISOString(),
      catalog_mode: catalogMode,
      error: "",
      base_model: result.baseModel || "",
      fallback: Boolean(result.fallback),
      source: result.source || "",
      warning: result.warning || "",
      target_models: result.targetModels || [],
    });
  } catch (error) {
    writeCodexAdapterError(dataDir, error, rootDir);
  }
  return gatherCodexAdapter(dataDir, rootDir);
}

function readNativeCodexCatalogAsync(timeoutMs = 8000) {
  const invocation = codexCliInvocation();
  return new Promise((resolve, reject) => {
    execFile(invocation.command, invocation.args.concat(["debug", "models", "--bundled"]), {
      encoding: "utf8",
      env: process.env,
      windowsHide: true,
      maxBuffer: 16 * 1024 * 1024,
      timeout: timeoutMs,
    }, (error, stdout) => {
      if (error) {
        reject(error);
        return;
      }
      try {
        resolve(JSON.parse(String(stdout || "").replace(/^\uFEFF/, "")));
      } catch (parseError) {
        reject(parseError);
      }
    });
  });
}

function isCodexAdapterReady(dataDir) {
  try {
    const catalogPath = codexAdapterCatalogPath(dataDir);
    const catalog = JSON.parse(fs.readFileSync(catalogPath, "utf8"));
    return validateCodeSeeXCatalog(catalog).ok;
  } catch {
    return false;
  }
}

function isCodexAdapterFallback(dataDir) {
  const status = readCodexAdapterStatus(dataDir);
  return Boolean(status && status.fallback);
}

function isCodexAdapterKnownSource(dataDir) {
  const status = readCodexAdapterStatus(dataDir);
  return Boolean(status && ["native", "seed"].includes(status.source) && !status.fallback);
}

function isCodexAdapterNative(dataDir) {
  const status = readCodexAdapterStatus(dataDir);
  return Boolean(status && status.source === "native" && !status.fallback);
}

function gatherCodexAdapter(dataDir, rootDir = ROOT_DIR) {
  const catalogPath = codexAdapterCatalogPath(dataDir);
  const config = publicConfig(loadLightProxyEnv(dataDir, rootDir));
  const status = readCodexAdapterStatus(dataDir);
  const catalogMode = normalizeCatalogMode(config.CATALOG_MODE);
  let ready = false;
  let baseModel = "";
  let models = TARGET_MODELS.map((model) => model.slug);
  let error = status.error || "";
  let warning = status.warning || "";
  let fallback = Boolean(status.fallback);
  let source = status.source || "";
  let statusValue = status.status || CODEX_ADAPTER_STATUS_IDLE;
  try {
    const catalog = JSON.parse(fs.readFileSync(catalogPath, "utf8"));
    const catalogModels = Array.isArray(catalog.models) ? catalog.models : [];
    const validation = validateCodeSeeXCatalog(catalog);
    ready = validation.ok;
    models = catalogModels.map((model) => model.slug).filter(Boolean);
    baseModel = status.base_model || "";
    if (ready && statusValue !== CODEX_ADAPTER_STATUS_GENERATING) statusValue = CODEX_ADAPTER_STATUS_READY;
    if (!ready && !error) error = validation.error;
    if (ready && !source) source = fallback ? "fallback" : "unknown";
  } catch (readError) {
    if (!error) error = readError && readError.message ? readError.message : String(readError);
    if (statusValue !== CODEX_ADAPTER_STATUS_GENERATING && error) statusValue = CODEX_ADAPTER_STATUS_ERROR;
  }
  return {
    ready,
    status: statusValue,
    catalog_path: catalogPath,
    catalog_mode: catalogMode,
    provider_id: CODEX_ADAPTER_PROVIDER_ID,
    models,
    context_window: 1000000,
    effective_context_window_percent: 90,
    base_model: baseModel,
    fallback,
    source,
    warning,
    toml_snippet: codexTomlSnippet(catalogPath, proxyBaseUrl(config), config.UPSTREAM_MODEL_OVERRIDE),
    error: ready ? "" : error,
  };
}

function readCodexAdapterStatus(dataDir) {
  const status = readJson(codexAdapterStatusPath(dataDir), { status: CODEX_ADAPTER_STATUS_IDLE, error: "", base_model: "" }) || { status: CODEX_ADAPTER_STATUS_IDLE, error: "", base_model: "" };
  if (!status.catalog_mode) status.catalog_mode = DEFAULT_CATALOG_MODE;
  return status;
}

function writeCodexAdapterStatus(dataDir, value) {
  try {
    const statusPath = codexAdapterStatusPath(dataDir);
    fs.mkdirSync(path.dirname(statusPath), { recursive: true });
    fs.writeFileSync(statusPath, JSON.stringify(value, null, 2) + "\n", "utf8");
  } catch {}
}

function writeCodexAdapterError(dataDir, error, rootDir = ROOT_DIR) {
  writeCodexAdapterStatus(dataDir, {
    updated_at: new Date().toISOString(),
    status: CODEX_ADAPTER_STATUS_ERROR,
    catalog_mode: normalizeCatalogMode(loadProxyEnv(dataDir, rootDir).CATALOG_MODE),
    error: error && error.message ? error.message : String(error),
    base_model: "",
    fallback: false,
    source: "",
    warning: "",
  });
}

function codexAdapterStatusPath(dataDir) {
  return path.join(codeSeeXUserDir(dataDir), "model-catalog.status.json");
}

function codexTomlSnippet(catalogPath, baseUrl, upstreamModelOverride = DEFAULT_UPSTREAM_MODEL_OVERRIDE) {
  const model = tomlModelForOverride(upstreamModelOverride);
  return [
    'model_provider = "' + CODEX_ADAPTER_PROVIDER_ID + '"',
    'model = "' + model + '"',
    "disable_response_storage = true",
    'model_reasoning_effort = "xhigh"',
    "model_catalog_json = " + tomlLiteral(catalogPath),
    "",
    "[model_providers." + CODEX_ADAPTER_PROVIDER_ID + "]",
    'name = "DeepSeek"',
    'wire_api = "responses"',
    "requires_openai_auth = true",
    'base_url = "' + String(baseUrl || "http://127.0.0.1:8787/v1").replace(/"/g, '\\"') + '"',
    "",
    "# CodeSeeX upstream override is configured in the CodeSeeX app, not in this TOML.",
    "# To use the flash model in Codex UI, change:",
    '# model = "deepseek-v4-flash"',
  ].join("\n");
}

function tomlLiteral(value) {
  return "'" + String(value || "").replace(/'/g, "''") + "'";
}

function tomlModelForOverride(value) {
  const normalized = normalizeUpstreamModelOverride(value);
  return normalized === "deepseek-v4-flash" ? "deepseek-v4-flash" : "deepseek-v4-pro";
}

function gatherLanguages(dataDir) {
  return {
    default_language: DEFAULT_LANGUAGE_ID,
    system_language: SYSTEM_LANGUAGE_ID,
    system_locale: normalizeSystemLocale(osLocale()),
    system_locales: systemLocaleCandidates(),
    languages: listLanguages(languageDirs(dataDir)),
  };
}

function osLocale() {
  return Intl.DateTimeFormat().resolvedOptions().locale
    || process.env.LC_ALL
    || process.env.LC_MESSAGES
    || process.env.LANG
    || "";
}

function systemLocaleCandidates() {
  const values = [
    osLocale(),
    process.env.LC_ALL,
    process.env.LC_MESSAGES,
    process.env.LANG,
    process.env.LANGUAGE,
  ].map(normalizeSystemLocale).filter(Boolean);
  return Array.from(new Set(values));
}

function normalizeSystemLocale(value) {
  return String(value || "")
    .split(/[.:]/)[0]
    .trim()
    .replace(/-/g, "_")
    .toLowerCase();
}

function readPackageJson(rootDir) {
  if (cachedPackageJson && cachedPackageJson.rootDir === rootDir) return cachedPackageJson.value;
  try {
    const value = JSON.parse(fs.readFileSync(path.join(rootDir, "package.json"), "utf8"));
    cachedPackageJson = { rootDir, value };
    return value;
  } catch {
    return {};
  }
}

function normalizeRepositoryUrl(repository) {
  const raw = typeof repository === "string" ? repository : (repository && typeof repository.url === "string" ? repository.url : "");
  if (!raw) return null;
  return raw.replace(/^git\+/, "").replace(/^git:/, "https:").replace(/\.git$/, "");
}

function displayLicenseName(license) {
  return LICENSE_DISPLAY_NAMES[license] || license || "";
}

function gatherStatus(controller, dataDir, rootDir = ROOT_DIR) {
  const proxy = controller.status();
  const runtime = proxy.runtime || readJson(path.join(dataDir, "runtime.json"), null);
  const config = publicConfig(loadLightProxyEnv(dataDir, rootDir));
  const events = collectRuntimeEvents(runtime, proxy, dataDir, rootDir, { limit: 30, audience: "user" });
  return {
    running: proxy.running,
    pid: proxy.pid,
    process_mode: proxy.mode || "child",
    process_label: proxy.mode === "inline" ? "App process PID" : "Proxy process PID",
    last_error: proxy.last_error,
    runtime: publicRuntime(runtime),
    runtime_status: runtime ? runtime.status : "unknown",
    base_url: runtime && runtime.status === "running" ? runtime.base_url : proxyBaseUrl(config),
    config_version: configVersion(dataDir),
    events,
  };
}

function publicRuntime(runtime) {
  if (!runtime || typeof runtime !== "object") {
    return {
      status: "unknown",
      pid: null,
      host: "",
      port: null,
      base_url: null,
      started_at: null,
      stopped_at: null,
      active_requests: 0,
      request_count: 0,
      failed_request_count: 0,
      last_started_at: null,
      last_request_at: null,
      last_request_ms: 0,
      total_cached_input_tokens: 0,
      total_cache_miss_input_tokens: 0,
      total_output_tokens: 0,
      total_reasoning_output_tokens: 0,
      total_tokens: 0,
      last_turn: null,
      turn_history: [],
      error: null,
    };
  }
  return {
    status: runtime.status || "unknown",
    pid: runtime.pid || null,
    host: runtime.host || "",
    port: runtime.port || null,
    base_url: runtime.base_url || null,
    started_at: runtime.started_at || null,
    stopped_at: runtime.stopped_at || null,
    active_requests: Number(runtime.active_requests || 0),
    request_count: Number(runtime.request_count || 0),
    failed_request_count: Number(runtime.failed_request_count || 0),
    last_started_at: runtime.last_started_at || null,
    last_request_at: runtime.last_request_at || null,
    last_request_ms: Number(runtime.last_request_ms || 0),
    total_cached_input_tokens: Number(runtime.total_cached_input_tokens || 0),
    total_cache_miss_input_tokens: Number(runtime.total_cache_miss_input_tokens || 0),
    total_output_tokens: Number(runtime.total_output_tokens || 0),
    total_reasoning_output_tokens: Number(runtime.total_reasoning_output_tokens || 0),
    total_tokens: Number(runtime.total_tokens || 0),
    last_turn: runtime.last_turn || null,
    turn_history: Array.isArray(runtime.turn_history) ? runtime.turn_history.slice(-80) : [],
    error: runtime.error || null,
  };
}

function gatherTools(dataDir, rootDir = ROOT_DIR) {
  const config = publicConfig(loadProxyEnv(dataDir, rootDir));
  return {
    tools: listPublicTools(config, { rootDir, extensionDir: extensionDir(dataDir) }),
  };
}

function proxyBaseUrl(config) {
  const host = (config && config.PROXY_HOST) || "127.0.0.1";
  const port = clampPort(config && config.PROXY_PORT, 8787);
  return "http://" + host + ":" + port + "/v1";
}

function gatherEvents(dataDir, rootDir, params) {
  const limit = clampInt(params.get("limit"), 80, 1, 200);
  const before = params.get("before") || null;
  const audience = normalizeAudienceParam(params.get("audience"));
  const fileEvents = readFilteredEventLogTail(rootDir, dataDir, limit, before, audience);
  return {
    events: fileEvents,
    has_more: fileEvents.length === limit,
    limit,
    before,
    audience,
  };
}

function collectRuntimeEvents(runtime, proxy, dataDir, rootDir, options = {}) {
  const limit = clampInt(options.limit, 80, 1, 200);
  const audience = normalizeAudienceParam(options.audience);
  const events = [];
  for (const item of readFilteredEventLogTail(rootDir, dataDir, limit, null, audience)) events.push(repairEventText(item));
  if (events.length === 0) {
    for (const item of Array.isArray(runtime && runtime.events) ? runtime.events : []) {
      if (eventMatchesAudience(item, audience)) events.push(repairEventText(item));
    }
  }
  if (audience === "all") {
    for (const item of (proxy.stdout || []).slice(-30)) {
      events.push({ ts: item.at, type: "process_stdout", level: "info", audience: "diagnostic", message: repairMojibakeText(item.line), detail: null });
    }
    for (const item of (proxy.stderr || []).slice(-30)) {
      events.push({ ts: item.at, type: "process_stderr", level: "error", audience: "diagnostic", message: repairMojibakeText(item.line), detail: null });
    }
  }
  return events
    .filter((item) => item && item.ts)
    .sort((left, right) => String(left.ts).localeCompare(String(right.ts)))
    .slice(-limit);
}

function readFilteredEventLogTail(rootDir, dataDir, limit, before, audience) {
  if (audience === "all") return readEventLogTail(rootDir, dataDir, limit, before).map(repairEventText);
  const output = [];
  let cursor = before || null;
  const batchSize = Math.min(200, Math.max(limit * 3, limit));
  for (let round = 0; round < 8 && output.length < limit; round += 1) {
    const batch = readEventLogTail(rootDir, dataDir, batchSize, cursor);
    if (batch.length === 0) break;
    for (let index = batch.length - 1; index >= 0 && output.length < limit; index -= 1) {
      const item = batch[index];
      if (eventMatchesAudience(item, audience)) output.push(repairEventText(item));
    }
    cursor = batch[0] && batch[0].ts ? batch[0].ts : null;
    if (!cursor || batch.length < batchSize) break;
  }
  return output.reverse();
}

function normalizeAudienceParam(value) {
  return String(value || "user") === "all" ? "all" : "user";
}

function eventMatchesAudience(event, audience) {
  if (audience === "all") return true;
  return eventAudience(event) === "user";
}

function eventAudience(event) {
  if (event && event.audience === "diagnostic") return "diagnostic";
  if (event && event.audience === "user") return "user";
  return isDiagnosticEventType(event && event.type) ? "diagnostic" : "user";
}

function isDiagnosticEventType(type) {
  return type === "context_diagnostic"
    || type === "context_response_diagnostic"
    || type === "tool_lifecycle"
    || type === "models_requested"
    || type === "process_stdout"
    || type === "process_stderr";
}

function repairEventText(event) {
  if (!event || typeof event !== "object") return event;
  return Object.assign({}, event, {
    message: repairMojibakeText(event.message || ""),
    detail: repairEventDetail(event.detail || null),
  });
}

function repairEventDetail(detail) {
  if (typeof detail === "string") return repairMojibakeText(detail);
  if (!detail || typeof detail !== "object") return detail;
  if (Array.isArray(detail)) return detail.map(repairEventDetail);
  const output = {};
  for (const [key, value] of Object.entries(detail)) output[key] = repairEventDetail(value);
  return output;
}

function publicConfig(config) {
  const output = normalizeToolRuntimeConfig(config || {}, { rootDir: config && config.PROXY_ROOT_DIR || ROOT_DIR, extensionDir: config && config.PROXY_EXTENSION_DIR || extensionDir(DATA_DIR) });
  for (const key of Object.keys(output)) {
    if (isSensitiveConfigKey(key)) delete output[key];
  }
  output.DEEPSEEK_BASE_URL = displayDeepSeekBaseUrl(output.DEEPSEEK_BASE_URL);
  output.CATALOG_MODE = normalizeCatalogMode(output.CATALOG_MODE || DEFAULT_CATALOG_MODE);
  output.UPSTREAM_MODEL_OVERRIDE = normalizeUpstreamModelOverride(output.UPSTREAM_MODEL_OVERRIDE || DEFAULT_UPSTREAM_MODEL_OVERRIDE);
  output.AUTO_START = normalizeBoolString(output.AUTO_START || DEFAULT_AUTO_START);
  return output;
}

function configVersion(dataDir) {
  try {
    const stat = fs.statSync(proxyEnvPath(dataDir));
    return String(stat.mtimeMs);
  } catch {
    return "";
  }
}

function isSensitiveConfigKey(key) {
  return /(?:KEY|TOKEN|SECRET|PASSWORD|AUTH|PROXY)$/i.test(String(key || ""))
    || /(?:^|_)AUTH(?:_|$)/i.test(String(key || ""))
    || /^(?:HTTP_PROXY|HTTPS_PROXY|ALL_PROXY|NO_PROXY)$/i.test(String(key || ""));
}

function sanitizeConfig(body, rootDir = ROOT_DIR, dataDir = rootDir) {
  const allowed = new Set([
    "PROXY_HOST",
    "PROXY_PORT",
    "DEEPSEEK_BASE_URL",
    "CATALOG_MODE",
    "UPSTREAM_MODEL_OVERRIDE",
    "AUTO_START",
    "DEEPSEEK_THINKING",
    "SHOW_THINKING",
    "UI_THEME",
    "UI_LANGUAGE",
    "UI_CLOSE_BEHAVIOR",
    "BILLING_CACHED_INPUT_CNY",
    "BILLING_CACHE_MISS_INPUT_CNY",
    "BILLING_OUTPUT_CNY",
    "LOG_RETENTION_DAYS",
    "COMMUNITY_TOOL_CODE_ENABLED",
  ]);
  const result = sanitizeToolConfig(body, { rootDir, extensionDir: extensionDir(dataDir) });
  for (const [key, value] of Object.entries(body || {})) {
    if (!allowed.has(key)) continue;
    if (key === "UI_LANGUAGE") result[key] = normalizeLanguageId(value);
    else if (key === "UI_CLOSE_BEHAVIOR") result[key] = normalizeCloseBehavior(value);
    else if (key === "DEEPSEEK_BASE_URL") result[key] = normalizeStoredDeepSeekBaseUrl(value);
    else if (key === "CATALOG_MODE") result[key] = normalizeCatalogMode(value);
    else if (key === "UPSTREAM_MODEL_OVERRIDE") result[key] = normalizeUpstreamModelOverride(value);
    else if (key === "AUTO_START") result[key] = normalizeBoolString(value);
    else result[key] = String(value);
  }
  return result;
}

function normalizeLanguageId(value) {
  const normalized = String(value || DEFAULT_LANGUAGE_ID).trim().replace(/-/g, "_").toLowerCase();
  return normalized || DEFAULT_LANGUAGE_ID;
}

function normalizeStoredLanguageId(value, fallback = DEFAULT_LANGUAGE_ID) {
  const normalized = normalizeLanguageId(value || fallback);
  return normalized === "zh_cn" && !value ? DEFAULT_LANGUAGE_ID : normalized;
}

function normalizeCloseBehavior(value) {
  return String(value || "exit") === "tray" ? "tray" : "exit";
}

function normalizeBoolString(value) {
  return /^(1|true|yes|on|enabled)$/i.test(String(value || "").trim()) ? "true" : "false";
}

function ensureProxyEnv(dataDir, rootDir = ROOT_DIR) {
  const filePath = proxyEnvPath(dataDir);
  if (fs.existsSync(filePath)) return;
  const current = {};
  const defaults = defaultProxyEnv(rootDir, dataDir);
  const merged = Object.assign(mergeEnv(defaults, current), {
    DEEPSEEK_BASE_URL: normalizeStoredDeepSeekBaseUrl(current.DEEPSEEK_BASE_URL || defaults.DEEPSEEK_BASE_URL),
    THINKING_TITLE: FIXED_THINKING_TITLE,
    COMMUNITY_TOOL_CODE_ENABLED: "false",
    PROXY_EXTENSION_DIR: extensionDir(dataDir),
    CATALOG_MODE: normalizeCatalogMode(current.CATALOG_MODE || defaults.CATALOG_MODE),
    UPSTREAM_MODEL_OVERRIDE: normalizeUpstreamModelOverride(current.UPSTREAM_MODEL_OVERRIDE || defaults.UPSTREAM_MODEL_OVERRIDE),
    AUTO_START: normalizeBoolString(current.AUTO_START || defaults.AUTO_START),
    UI_LANGUAGE: normalizeStoredLanguageId(current.UI_LANGUAGE, defaults.UI_LANGUAGE),
    UI_CLOSE_BEHAVIOR: normalizeCloseBehavior(current.UI_CLOSE_BEHAVIOR || defaults.UI_CLOSE_BEHAVIOR),
    LOG_RETENTION_DAYS: normalizeRetentionDays(current.LOG_RETENTION_DAYS || defaults.LOG_RETENTION_DAYS),
  });
  writeEnvFile(filePath, merged);
}

function loadBootstrapEnv(dataDir, rootDir = ROOT_DIR) {
  return Object.assign(defaultProxyEnv(rootDir, dataDir), readEnvFile(path.join(rootDir, "proxy.env")), readEnvFile(proxyEnvPath(dataDir)));
}

function loadLightProxyEnv(dataDir, rootDir = ROOT_DIR) {
  const rootEnv = readEnvFile(path.join(rootDir, "proxy.env"));
  const dataEnv = readEnvFile(proxyEnvPath(dataDir));
  return normalizeLoadedProxyEnv(Object.assign({}, defaultProxyEnv(rootDir, dataDir), rootEnv, dataEnv), rootEnv, dataEnv, dataDir, rootDir);
}

function loadProxyEnv(dataDir, rootDir = ROOT_DIR) {
  const envOptions = proxyEnvOptions(dataDir, rootDir);
  const rootEnv = readEnvFile(path.join(rootDir, "proxy.env"), envOptions);
  const dataEnv = readEnvFile(proxyEnvPath(dataDir), envOptions);
  return normalizeLoadedProxyEnv(Object.assign({}, defaultProxyEnv(rootDir, dataDir), rootEnv, dataEnv), rootEnv, dataEnv, dataDir, rootDir);
}

function normalizeLoadedProxyEnv(env, rootEnv = {}, dataEnv = {}, dataDir = DATA_DIR, rootDir = ROOT_DIR) {
  return normalizeToolRuntimeConfig(Object.assign({}, env, {
    DEEPSEEK_BASE_URL: normalizeManagerDeepSeekBaseUrl(dataEnv.DEEPSEEK_BASE_URL || rootEnv.DEEPSEEK_BASE_URL || env.DEEPSEEK_BASE_URL),
    THINKING_TITLE: FIXED_THINKING_TITLE,
    PROXY_EXTENSION_DIR: env.PROXY_EXTENSION_DIR || extensionDir(dataDir),
    CATALOG_MODE: normalizeCatalogMode(dataEnv.CATALOG_MODE || rootEnv.CATALOG_MODE || DEFAULT_CATALOG_MODE),
    UPSTREAM_MODEL_OVERRIDE: normalizeUpstreamModelOverride(dataEnv.UPSTREAM_MODEL_OVERRIDE || rootEnv.UPSTREAM_MODEL_OVERRIDE || DEFAULT_UPSTREAM_MODEL_OVERRIDE),
    AUTO_START: normalizeBoolString(dataEnv.AUTO_START || rootEnv.AUTO_START || DEFAULT_AUTO_START),
    COMMUNITY_TOOL_CODE_ENABLED: dataEnv.COMMUNITY_TOOL_CODE_ENABLED || rootEnv.COMMUNITY_TOOL_CODE_ENABLED || "false",
    UI_LANGUAGE: normalizeStoredLanguageId(dataEnv.UI_LANGUAGE || rootEnv.UI_LANGUAGE, DEFAULT_LANGUAGE_ID),
    UI_CLOSE_BEHAVIOR: normalizeCloseBehavior(dataEnv.UI_CLOSE_BEHAVIOR || rootEnv.UI_CLOSE_BEHAVIOR || "exit"),
    LOG_RETENTION_DAYS: normalizeRetentionDays(dataEnv.LOG_RETENTION_DAYS || rootEnv.LOG_RETENTION_DAYS || "7"),
  }), { rootDir, extensionDir: extensionDir(dataDir) });
}

function defaultProxyEnv(rootDir = ROOT_DIR, dataDir = DATA_DIR) {
  return Object.assign({
    DEEPSEEK_BASE_URL: "",
    PROXY_HOST: "127.0.0.1",
    PROXY_PORT: "8787",
    CATALOG_MODE: DEFAULT_CATALOG_MODE,
    UPSTREAM_MODEL_OVERRIDE: DEFAULT_UPSTREAM_MODEL_OVERRIDE,
    AUTO_START: DEFAULT_AUTO_START,
    DEEPSEEK_THINKING: "auto",
    SHOW_THINKING: "true",
    THINKING_TITLE: FIXED_THINKING_TITLE,
    UI_THEME: "system",
    UI_LANGUAGE: DEFAULT_LANGUAGE_ID,
    UI_CLOSE_BEHAVIOR: "exit",
    BILLING_CACHED_INPUT_CNY: "0.025",
    BILLING_CACHE_MISS_INPUT_CNY: "3",
    BILLING_OUTPUT_CNY: "6",
    LOG_RETENTION_DAYS: "7",
    COMMUNITY_TOOL_CODE_ENABLED: "false",
  }, toolDefaultConfig({ rootDir, extensionDir: extensionDir(dataDir) }));
}

function normalizeManagerDeepSeekBaseUrl(value) {
  const raw = String(value || "").trim();
  if (!raw) return FIXED_DEEPSEEK_BASE_URL;
  try {
    const normalized = normalizeDeepSeekBaseUrl(raw);
    const url = new URL(normalized);
    if (url.protocol !== "http:" && url.protocol !== "https:") return FIXED_DEEPSEEK_BASE_URL;
    return url.toString().replace(/\/+$/, "");
  } catch {
    return FIXED_DEEPSEEK_BASE_URL;
  }
}

function isOfficialDeepSeekBaseUrl(value) {
  try {
    const normalized = normalizeDeepSeekBaseUrl(value || FIXED_DEEPSEEK_BASE_URL);
    const url = new URL(normalized);
    return url.protocol === "https:"
      && url.hostname.toLowerCase() === "api.deepseek.com"
      && (!url.pathname || url.pathname === "/")
      && !url.search
      && !url.hash;
  } catch {
    return false;
  }
}

function normalizeStoredDeepSeekBaseUrl(value) {
  const raw = String(value || "").trim();
  if (!raw) return "";
  const normalized = normalizeManagerDeepSeekBaseUrl(raw);
  return isOfficialDeepSeekBaseUrl(normalized) ? "" : normalized;
}

function displayDeepSeekBaseUrl(value) {
  return normalizeStoredDeepSeekBaseUrl(value);
}

function proxyEnvOptions(dataDir, rootDir = ROOT_DIR) {
  return {
    allowedKeys: Object.keys(toolDefaultConfig({ rootDir, extensionDir: extensionDir(dataDir) })),
  };
}

async function fetchDeepSeekBalance(config) {
  const apiKey = resolveApiKey(config);
  if (!apiKey) {
    return { ok: false, code: "missing_api_key", message: "API key is not configured." };
  }

  const url = deepSeekBalanceUrl(config.DEEPSEEK_BASE_URL || FIXED_DEEPSEEK_BASE_URL);
  const dispatcher = resolveBalanceDispatcher(config);
  const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
  const timeout = setTimeout(() => controller && controller.abort(), 30000);
  try {
    const response = await fetch(url, {
      method: "GET",
      headers: {
        Accept: "application/json",
        Authorization: "Bearer " + apiKey,
      },
      signal: controller ? controller.signal : undefined,
      dispatcher,
    });
    const body = await parseJsonResponse(response);
    if (!response.ok) {
      return {
        ok: false,
        code: (body && body.error && body.error.code) || "deepseek_balance_error",
        status: response.status,
        message: (body && body.error && body.error.message) || (body && body.message) || "DeepSeek balance request failed.",
      };
    }
    return {
      ok: true,
      is_available: Boolean(body.is_available),
      balance_infos: Array.isArray(body.balance_infos) ? body.balance_infos.map(normalizeBalanceInfo) : [],
      checked_at: new Date().toISOString(),
    };
  } catch (error) {
    if (error && error.name === "AbortError") {
      return { ok: false, code: "deepseek_balance_timeout", message: "DeepSeek balance request timed out." };
    }
    return { ok: false, code: "deepseek_balance_failed", message: error && error.message ? error.message : String(error) };
  } finally {
    clearTimeout(timeout);
  }
}

function resolveApiKey(config) {
  const codexKey = String(readCodexAuthApiKey(config, { includeCachedAuthorization: true }) || "").trim();
  return codexKey;
}

function resolveBalanceDispatcher(config) {
  try {
    return require("../proxy/network-dispatcher").resolveDispatcher(config);
  } catch {
    return undefined;
  }
}

function deepSeekBalanceUrl(baseUrl) {
  try {
    const url = new URL(baseUrl || FIXED_DEEPSEEK_BASE_URL);
    url.pathname = url.pathname.replace(/\/+$/, "").replace(/\/v1$/i, "") + "/user/balance";
    url.search = "";
    url.hash = "";
    return url.toString();
  } catch {
    throw httpError(500, "Invalid DeepSeek base URL.", "server_error", "invalid_deepseek_base_url");
  }
}

function normalizeBalanceInfo(item) {
  return {
    currency: item && item.currency ? String(item.currency) : "",
    total_balance: item && item.total_balance !== undefined ? String(item.total_balance) : "0",
    granted_balance: item && item.granted_balance !== undefined ? String(item.granted_balance) : "0",
    topped_up_balance: item && item.topped_up_balance !== undefined ? String(item.topped_up_balance) : "0",
  };
}

function proxyEnvPath(dataDir) {
  return path.join(dataDir, "proxy.env");
}

function eventLogPath(rootDir, dataDir = rootDir) {
  return datedEventLogPath(rootDir, dataDir);
}

function eventLogDirectory(rootDir, dataDir = rootDir) {
  return eventLogDir(rootDir, dataDir);
}

function extensionDir(dataDir) {
  return path.join(dataDir, "extension");
}

function languageDirs(dataDir) {
  return [LANG_DIR, path.join(dataDir, "lang")];
}

function ensureRuntimeDirectories(dataDir) {
  for (const dir of [
    dataDir,
    path.join(dataDir, "lang"),
    path.join(dataDir, "logs"),
    path.join(dataDir, "extension"),
    path.join(dataDir, "extension", "tools"),
  ]) {
    fs.mkdirSync(dir, { recursive: true });
  }
}

function appendManagerEvent(dataDir, rootDir, type, level, message, detail) {
  try {
    const config = loadProxyEnv(dataDir, rootDir);
    appendEventLog(rootDir, dataDir, {
      ts: new Date().toISOString(),
      type,
      level,
      audience: "user",
      message,
      detail,
    }, { retentionDays: config.LOG_RETENTION_DAYS });
  } catch {}
}

function notifyConfigChanged(context, config) {
  try {
    if (context && typeof context.onConfigChanged === "function") context.onConfigChanged(config || {});
  } catch {}
}

function handleWindowAction(action, context, body) {
  if (!["minimize", "maximize", "close", "theme", "login-item"].includes(action)) {
    throw httpError(404, "Window action not found.", "invalid_request_error", "not_found");
  }
  if (!context || typeof context.onWindowAction !== "function") {
    throw httpError(400, "Window actions are only available in the desktop app.", "invalid_request_error", "window_action_unavailable");
  }
  context.onWindowAction(action, body || {});
}

module.exports = {
  main,
  startManager,
};

function isStaticRoute(pathname) {
  if (pathname === "/" || pathname === "/index.html") return true;
  if (pathname === "/styles.css" || pathname === "/app.js") return true;
  return pathname.startsWith("/assets/") || pathname.startsWith("/lang/") || pathname.startsWith("/tool-assets/");
}

function sendStaticFile(res, pathname, context = {}) {
  if (pathname === "/" || pathname === "/index.html") {
    const html = readIndexPage();
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8", "Content-Length": Buffer.byteLength(html) });
    res.end(html);
    return;
  }

  const filePath = resolveStaticFile(pathname, context.rootDir || ROOT_DIR, context.dataDir || context.rootDir || ROOT_DIR);
  if (!filePath || !fs.existsSync(filePath)) {
    sendJson(res, 404, { error: { message: "Static file not found.", type: "invalid_request_error", code: "not_found" } });
    return;
  }
  const body = fs.readFileSync(filePath);
  res.writeHead(200, { "Content-Type": contentTypeFor(filePath), "Content-Length": body.length });
  res.end(body);
}

function resolveStaticFile(pathname, rootDir = ROOT_DIR, dataDir = rootDir) {
  if (pathname.startsWith("/tool-assets/")) return toolAssetFilePath(pathname, { rootDir, extensionDir: extensionDir(dataDir) });
  if (pathname.startsWith("/lang/")) {
    const filename = path.basename(pathname);
    const id = filename.replace(/\.json$/i, "");
    return languageFilePath(id, languageDirs(dataDir));
  }
  return staticFilePath(pathname);
}

function clampPort(value, fallback) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  if (parsed === 0) return 0;
  return Math.min(65535, Math.max(1, Math.floor(parsed)));
}

function clampInt(value, fallback, min, max) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.min(max, Math.max(min, Math.floor(parsed)));
}

function normalizeVersion(value) {
  const match = String(value || "").trim().match(/v?(\d+(?:\.\d+){0,3})/i);
  return match ? match[1] : "0.0.0";
}

function compareVersions(left, right) {
  const a = normalizeVersion(left).split(".").map((part) => Number(part) || 0);
  const b = normalizeVersion(right).split(".").map((part) => Number(part) || 0);
  const length = Math.max(a.length, b.length, 3);
  for (let index = 0; index < length; index += 1) {
    const diff = (a[index] || 0) - (b[index] || 0);
    if (diff !== 0) return diff > 0 ? 1 : -1;
  }
  return 0;
}

function normalizeRetentionDays(value) {
  const parsed = String(value || "").trim();
  return ["1", "3", "7", "30"].includes(parsed) ? parsed : "7";
}
