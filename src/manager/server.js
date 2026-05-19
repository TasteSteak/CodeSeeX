const http = require("node:http");
const { execFile } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const { readCodexAuthApiKey } = require("../shared/codex-auth");
const { DATA_DIR, ROOT_DIR, defaultEventLogFile, loadProxyConfig } = require("../shared/config");
const { addCorsHeaders, enforceLocalAccess, handleHttpError, httpError, parseJsonResponse, readJsonBody, sendJson } = require("../shared/http");
const { appendJsonl, readJson } = require("../shared/json-store");
const { appendEventLog, eventLogDir, eventLogPath: datedEventLogPath, readEventLogTail } = require("../shared/event-log");
const { buildCodeSeeXCatalog, codeSeeXUserDir, codexAdapterCatalogPath, codexCliInvocation, TARGET_MODELS, validateCodeSeeXCatalog } = require("../codex/model-catalog");
const { createProxyContext, handleRequest: handleProxyRequest, markProxyRunning, markProxyStopped } = require("../proxy/server");
const { PRODUCT_DESCRIPTION, PRODUCT_NAME } = require("../shared/product");
const { repairMojibakeText } = require("../shared/text-encoding");
const { listPublicTools, sanitizeToolConfig, toolDefaultConfig } = require("../shared/tool-registry");
const { toolAssetFilePath } = require("../tools");
const { resolveDispatcher } = require("../proxy/web-search-executor");
const { mergeEnv, readEnvFile, writeEnvFile } = require("./env-file");
const { DEFAULT_LANGUAGE_ID, LANG_DIR, languageFilePath, listLanguages } = require("./languages");
const { contentTypeFor, readIndexPage, staticFilePath } = require("./page");
const { cleanupStaleProxyProcesses } = require("./process-cleanup");

const HOST = "127.0.0.1";
const PORT = 8787;
const FIXED_DEEPSEEK_BASE_URL = "https://api.deepseek.com/v1";
const FIXED_THINKING_TITLE = "DeepSeek Thinking";
const CODEX_ADAPTER_PROVIDER_ID = "custom";
const CODEX_ADAPTER_STATUS_IDLE = "idle";
const CODEX_ADAPTER_STATUS_READY = "ready";
const CODEX_ADAPTER_STATUS_GENERATING = "generating";
const CODEX_ADAPTER_STATUS_ERROR = "error";
const LICENSE_DISPLAY_NAMES = {
  "GPL-3.0-only": "GPLv3",
  "AGPL-3.0-only": "AGPLv3",
};
let cachedPackageJson = null;
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
  const initialEnv = loadProxyEnv(dataDir, rootDir);
  const host = options.host || initialEnv.PROXY_HOST || process.env.PROXY_HOST || HOST;
  const requestedPort = options.port !== undefined ? options.port : initialEnv.PROXY_PORT;
  const port = clampPort(requestedPort, PORT);
  const exitOnClose = options.exitOnClose !== false;

  ensureRuntimeDirectories(dataDir);
  ensureProxyEnv(dataDir, rootDir);
  ensureCodexAdapterBootstrapCatalog(dataDir, rootDir);
  const proxyEnv = loadProxyEnv(dataDir, rootDir);
  cleanupStaleSinglePortProcesses({ rootDir, dataDir, port });
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
      embeddedProxy.start({ host, port: actualPort });
      scheduleCodexAdapterMaintenance(dataDir, rootDir);
      appendManagerEvent(dataDir, rootDir, "manager_started", "success", "Manager service started.", {
        url,
      });
      console.log("[manager] UI available at " + url);
      resolve({ close: () => closeManager(exitOnClose), controller, dataDir, server, url });
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

function createEmbeddedProxy(rootDir, dataDir, env) {
  let proxyContext = createProxyContext(loadProxyConfigForManager(rootDir, dataDir, env));
  let listenEndpoint = { host: proxyContext.config.host, port: proxyContext.config.port };
  let running = false;
  let lastError = null;
  const stdout = [];
  const stderr = [];

  function start(nextEnv = {}) {
    if (running) return status();
    if (nextEnv && Object.keys(nextEnv).length > 0 && !nextEnv.host && !nextEnv.port) {
      proxyContext = createProxyContext(loadProxyConfigForManager(rootDir, dataDir, nextEnv));
    }
    if (nextEnv.host || nextEnv.port) {
      listenEndpoint = {
        host: nextEnv.host || listenEndpoint.host || proxyContext.config.host,
        port: nextEnv.port || listenEndpoint.port || proxyContext.config.port,
      };
    }
    const host = listenEndpoint.host || proxyContext.config.host;
    const port = listenEndpoint.port || proxyContext.config.port;
    markProxyRunning(proxyContext, { host, port, message: "Embedded proxy service started." });
    running = true;
    lastError = null;
    pushControllerLine(stdout, "[proxy] Embedded proxy service started at " + proxyContext.runtime.base_url);
    return status();
  }

  function stop() {
    if (!running) return status();
    markProxyStopped(proxyContext, { message: "Embedded proxy service stopped." });
    running = false;
    pushControllerLine(stdout, "[proxy] Embedded proxy service stopped");
    return status();
  }

  function restart(nextEnv = {}) {
    stop();
    if (nextEnv && Object.keys(nextEnv).length > 0) {
      proxyContext = createProxyContext(loadProxyConfigForManager(rootDir, dataDir, nextEnv));
    }
    return start({});
  }

  function updateConfig(nextEnv = {}) {
    const nextConfig = loadProxyConfigForManager(rootDir, dataDir, nextEnv);
    Object.assign(proxyContext.config, nextConfig);
    return status();
  }

  function status() {
    const runtime = proxyContext.runtime || readJson(path.join(dataDir, "runtime.json"), null);
    return {
      mode: "embedded",
      running: Boolean(running && runtime && runtime.status === "running"),
      pid: running ? process.pid : null,
      last_error: lastError,
      stdout: stdout.slice(-80),
      stderr: stderr.slice(-80),
      runtime,
    };
  }

  function handleRequest(req, res) {
    if (!running) {
      sendJson(res, 503, { error: { message: "Proxy service is stopped.", type: "server_error", code: "proxy_stopped" } });
      return;
    }
    return handleProxyRequest(req, res, proxyContext);
  }

  return { dataDir, rootDir, handleRequest, restart, start, status, stop, updateConfig };
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

function cleanupStaleSinglePortProcesses({ rootDir, dataDir, port }) {
  try {
    if (!port) return;
    cleanupStaleProxyProcesses({
      rootDir,
      dataDir,
      runtimeFile: path.join(dataDir, "runtime.json"),
      ports: [port],
      includeProxy: true,
      includeManager: true,
      includeDesktop: false,
      excludePids: [process.pid],
    });
  } catch {}
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

    if (req.method === "GET" && url.pathname === "/api/config") {
      sendJson(res, 200, publicConfig(loadProxyEnv(dataDir, context.rootDir)));
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/config") {
      const body = await readJsonBody(req, 1024 * 1024);
      const next = mergeEnv(loadProxyEnv(dataDir, context.rootDir), sanitizeConfig(body, context.rootDir, dataDir));
      writeEnvFile(proxyEnvPath(dataDir), next, proxyEnvOptions(dataDir, context.rootDir));
      appendManagerEvent(dataDir, context.rootDir, "manager_config_saved", "success", "Configuration saved.", null);
      notifyConfigChanged(context, publicConfig(next));
      if (context.embeddedProxy && typeof context.embeddedProxy.updateConfig === "function") context.embeddedProxy.updateConfig(next);
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

function scheduleCodexAdapterMaintenance(dataDir, rootDir = ROOT_DIR) {
  const key = path.resolve(dataDir);
  if (catalogMaintenanceByDataDir.has(key)) return;
  const timer = setImmediate(() => {
    generateCodexAdapterCatalog(dataDir, rootDir, { force: false }).catch(() => {});
  });
  catalogMaintenanceByDataDir.set(key, timer);
}

function ensureCodexAdapterBootstrapCatalog(dataDir, rootDir = ROOT_DIR) {
  if (isCodexAdapterReady(dataDir) && isCodexAdapterKnownSource(dataDir)) return;
  try {
    const catalogPath = codexAdapterCatalogPath(dataDir);
    const result = buildCodeSeeXCatalog({
      outputPath: catalogPath,
      rootDir,
      nativeCatalog: null,
      nativeError: new Error("Native Codex catalog has not been loaded yet."),
    });
    writeCodexAdapterStatus(dataDir, {
      status: CODEX_ADAPTER_STATUS_READY,
      updated_at: new Date().toISOString(),
      error: "",
      base_model: result.baseModel || "",
      fallback: Boolean(result.fallback),
      source: result.source || "",
      warning: result.warning || "",
      target_models: result.targetModels || [],
    });
  } catch (error) {
    writeCodexAdapterError(dataDir, error);
  }
}

async function generateCodexAdapterCatalog(dataDir, rootDir = ROOT_DIR, options = {}) {
  const catalogPath = codexAdapterCatalogPath(dataDir);
  if (!options.force && isCodexAdapterReady(dataDir) && isCodexAdapterNative(dataDir)) return gatherCodexAdapter(dataDir, rootDir);
  writeCodexAdapterStatus(dataDir, {
    status: CODEX_ADAPTER_STATUS_GENERATING,
    updated_at: new Date().toISOString(),
    error: "",
    base_model: "",
    target_models: TARGET_MODELS.map((model) => model.slug),
  });
  let nativeCatalog = null;
  let nativeError = null;
  try {
    nativeCatalog = await readNativeCodexCatalogAsync();
  } catch (error) {
    nativeError = error;
  }
  try {
    const result = buildCodeSeeXCatalog({ outputPath: catalogPath, rootDir, nativeCatalog, nativeError });
    writeCodexAdapterStatus(dataDir, {
      status: CODEX_ADAPTER_STATUS_READY,
      updated_at: new Date().toISOString(),
      error: "",
      base_model: result.baseModel || "",
      fallback: Boolean(result.fallback),
      source: result.source || "",
      warning: result.warning || "",
      target_models: result.targetModels || [],
    });
  } catch (error) {
    writeCodexAdapterError(dataDir, error);
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
  const config = publicConfig(loadProxyEnv(dataDir, rootDir));
  const status = readCodexAdapterStatus(dataDir);
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
    provider_id: CODEX_ADAPTER_PROVIDER_ID,
    models,
    context_window: 1000000,
    effective_context_window_percent: 90,
    base_model: baseModel,
    fallback,
    source,
    warning,
    toml_snippet: codexTomlSnippet(catalogPath, proxyBaseUrl(config)),
    error: ready ? "" : error,
  };
}

function readCodexAdapterStatus(dataDir) {
  return readJson(codexAdapterStatusPath(dataDir), { status: CODEX_ADAPTER_STATUS_IDLE, error: "", base_model: "" }) || { status: CODEX_ADAPTER_STATUS_IDLE, error: "", base_model: "" };
}

function writeCodexAdapterStatus(dataDir, value) {
  try {
    const statusPath = codexAdapterStatusPath(dataDir);
    fs.mkdirSync(path.dirname(statusPath), { recursive: true });
    fs.writeFileSync(statusPath, JSON.stringify(value, null, 2) + "\n", "utf8");
  } catch {}
}

function writeCodexAdapterError(dataDir, error) {
  writeCodexAdapterStatus(dataDir, {
    updated_at: new Date().toISOString(),
    status: CODEX_ADAPTER_STATUS_ERROR,
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

function codexTomlSnippet(catalogPath, baseUrl) {
  return [
    'model_provider = "' + CODEX_ADAPTER_PROVIDER_ID + '"',
    'model = "deepseek-v4-pro"',
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
    "# To use the flash model, change:",
    '# model = "deepseek-v4-flash"',
  ].join("\n");
}

function tomlLiteral(value) {
  return "'" + String(value || "").replace(/'/g, "''") + "'";
}

function gatherLanguages(dataDir) {
  return {
    default_language: DEFAULT_LANGUAGE_ID,
    languages: listLanguages(languageDirs(dataDir)),
  };
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
  const config = publicConfig(loadProxyEnv(dataDir, rootDir));
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
  const output = Object.assign({}, config || {});
  for (const key of Object.keys(output)) {
    if (isSensitiveConfigKey(key)) delete output[key];
  }
  return output;
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
    else result[key] = String(value);
  }
  return result;
}

function normalizeLanguageId(value) {
  return String(value || DEFAULT_LANGUAGE_ID).trim().replace(/-/g, "_").toLowerCase() || DEFAULT_LANGUAGE_ID;
}

function normalizeCloseBehavior(value) {
  return String(value || "exit") === "tray" ? "tray" : "exit";
}

function ensureProxyEnv(dataDir, rootDir = ROOT_DIR) {
  const filePath = proxyEnvPath(dataDir);
  const envOptions = proxyEnvOptions(dataDir, rootDir);
  const current = readEnvFile(filePath, envOptions);
  const defaults = Object.assign({
    DEEPSEEK_BASE_URL: FIXED_DEEPSEEK_BASE_URL,
    PROXY_HOST: "127.0.0.1",
    PROXY_PORT: "8787",
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
  }, toolDefaultConfig({ rootDir, extensionDir: extensionDir(dataDir) }));
  const merged = Object.assign(mergeEnv(defaults, current), {
    DEEPSEEK_BASE_URL: FIXED_DEEPSEEK_BASE_URL,
    THINKING_TITLE: FIXED_THINKING_TITLE,
    COMMUNITY_TOOL_CODE_ENABLED: "false",
    PROXY_EXTENSION_DIR: extensionDir(dataDir),
    UI_LANGUAGE: normalizeLanguageId(current.UI_LANGUAGE || defaults.UI_LANGUAGE),
    UI_CLOSE_BEHAVIOR: normalizeCloseBehavior(current.UI_CLOSE_BEHAVIOR || defaults.UI_CLOSE_BEHAVIOR),
    LOG_RETENTION_DAYS: normalizeRetentionDays(current.LOG_RETENTION_DAYS || defaults.LOG_RETENTION_DAYS),
  });
  writeEnvFile(filePath, merged, envOptions);
}

function loadProxyEnv(dataDir, rootDir = ROOT_DIR) {
  const envOptions = proxyEnvOptions(dataDir, rootDir);
  const rootEnv = readEnvFile(path.join(rootDir, "proxy.env"), envOptions);
  const dataEnv = readEnvFile(proxyEnvPath(dataDir), envOptions);
  return Object.assign({}, rootEnv, dataEnv, {
    DEEPSEEK_BASE_URL: FIXED_DEEPSEEK_BASE_URL,
    THINKING_TITLE: FIXED_THINKING_TITLE,
    PROXY_EXTENSION_DIR: extensionDir(dataDir),
    COMMUNITY_TOOL_CODE_ENABLED: dataEnv.COMMUNITY_TOOL_CODE_ENABLED || rootEnv.COMMUNITY_TOOL_CODE_ENABLED || "false",
    UI_CLOSE_BEHAVIOR: normalizeCloseBehavior(dataEnv.UI_CLOSE_BEHAVIOR || rootEnv.UI_CLOSE_BEHAVIOR || "exit"),
    LOG_RETENTION_DAYS: normalizeRetentionDays(dataEnv.LOG_RETENTION_DAYS || rootEnv.LOG_RETENTION_DAYS || "7"),
  });
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
  const dispatcher = resolveDispatcher(config);
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

function extensionDirFromRoot(rootDir) {
  return path.join(rootDir, "extension");
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
  if (!["minimize", "maximize", "close", "theme"].includes(action)) {
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

function normalizeRetentionDays(value) {
  const parsed = String(value || "").trim();
  return ["1", "3", "7", "30"].includes(parsed) ? parsed : "7";
}
