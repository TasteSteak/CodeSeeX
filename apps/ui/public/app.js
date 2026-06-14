const LOG_INITIAL_PAGE_SIZE = 30;
const LOG_OLDER_PAGE_SIZE = 15;
const LOG_MAX_ITEMS = 1000;
const LOG_BOTTOM_LOAD_THRESHOLD = 80;
const CONFIG_AUTOSAVE_DELAY_MS = 450;
const CONFIG_TEXT_AUTOSAVE_DELAY_MS = 2500;
const CONFIG_AUTOSAVE_RETRY_MS = 700;
const DEBUG_MANAGER_BASE_URL = "http://127.0.0.1:8787";
const DEEPSEEK_RECHARGE_URL = "https://platform.deepseek.com/top_up";
const CODEX_CONFIG_PATH_UNIX = "~/.codex/config.toml";
const CODEX_CONFIG_PATH_WINDOWS = "%USERPROFILE%\\.codex\\config.toml";
const REFRESH_RUNNING_MS = 2000;
const REFRESH_IDLE_MS = 5000;
const REFRESH_HIDDEN_MS = 10000;
const SLOW_RENDER_MS = 80;
const LANGUAGE_LOAD_TIMEOUT_MS = 1200;
const CONFIG_CHANGED_EVENT = "codeseex-config-changed";
const RUNTIME_STATUS_STARTING = "starting";
const RUNTIME_STATUS_STOPPING = "stopping";
const ENABLED_TOOLS_KEY = "ENABLED_TOOLS";
const UPDATE_NOTICE_STORAGE_KEY = "version";
const DEFAULT_TEMPERATURE_PRESET = "default";
const DEFAULT_BILLING_RATES_CNY = Object.freeze({
  flash: Object.freeze({ cached: 0.02, cacheMiss: 1, output: 2 }),
  pro: Object.freeze({ cached: 0.025, cacheMiss: 3, output: 6 }),
});
const RESTART_REQUIRED_KEYS = new Set([
  "NETWORK_PROXY_MODE",
  "PROXY_PORT",
]);
const SYSTEM_LANGUAGE = "system";
const FALLBACK_LANGUAGE = "en_us";
const DEFAULT_LANGUAGE = SYSTEM_LANGUAGE;

const els = {
  aboutStatus: byId("aboutStatus"),
  aboutUpdateDot: byId("aboutUpdateDot"),
  activeRequests: byId("activeRequests"),
  appDescription: byId("appDescription"),
  appLicense: byId("appLicense"),
  appName: byId("appName"),
  appProductName: byId("appProductName"),
  appVersion: byId("appVersion"),
  aboutVersion: byId("aboutVersion"),
  balanceGranted: byId("balanceGranted"),
  balanceStatus: byId("balanceStatus"),
  balanceToppedUp: byId("balanceToppedUp"),
  balanceTotal: byId("balanceTotal"),
  billingFlashCachedInput: byId("BILLING_FLASH_CACHED_INPUT_CNY"),
  billingFlashCacheMissInput: byId("BILLING_FLASH_CACHE_MISS_INPUT_CNY"),
  billingFlashOutput: byId("BILLING_FLASH_OUTPUT_CNY"),
  billingProCachedInput: byId("BILLING_PRO_CACHED_INPUT_CNY"),
  billingProCacheMissInput: byId("BILLING_PRO_CACHE_MISS_INPUT_CNY"),
  billingProOutput: byId("BILLING_PRO_OUTPUT_CNY"),
  completedTurns: byId("completedTurns"),
  autoStart: byId("AUTO_START"),
  configTomlCode: byId("configTomlCode"),
  configTomlCopyStatus: byId("configTomlCopyStatus"),
  configTomlStatus: byId("configTomlStatus"),
  copyTomlButton: byId("copyTomlButton"),
  failedTurns: byId("failedTurns"),
  loadingDetail: byId("loadingDetail"),
  loadingOverlay: byId("loadingOverlay"),
  loadingTitle: byId("loadingTitle"),
  logStream: byId("logStream"),
  navItems: Array.from(document.querySelectorAll(".nav-item[data-view]")),
  pageSubtitle: byId("pageSubtitle"),
  pageTitle: byId("pageTitle"),
  pid: byId("pid"),
  pidLabel: byId("pidLabel"),
  deepseekOfficialV1Compat: byId("DEEPSEEK_OFFICIAL_V1_COMPAT"),
  deepseekBaseUrl: byId("DEEPSEEK_BASE_URL"),
  proxyPort: byId("PROXY_PORT"),
  rechargeBalanceButton: byId("rechargeBalanceButton"),
  refreshBalanceButton: byId("refreshBalanceButton"),
  restartButton: byId("restartButton"),
  restartRequiredBadge: byId("restartRequiredBadge"),
  running: byId("running"),
  showThinking: byId("SHOW_THINKING"),
  startButton: byId("startButton"),
  statusPill: byId("statusPill"),
  stopButton: byId("stopButton"),
  stagePortCheck: byId("stagePortCheck"),
  stagePortState: byId("stagePortState"),
  stageBalanceCheck: byId("stageBalanceCheck"),
  stageProxyHealth: byId("stageProxyHealth"),
  stageProxyState: byId("stageProxyState"),
  toolConfigList: byId("toolConfigList"),
  uiLanguage: byId("UI_LANGUAGE"),
  usageAverageMs: byId("usageAverageMs"),
  usageCacheHitRate: byId("usageCacheHitRate"),
  usageRows: byId("usageRows"),
  usageTotalCost: byId("usageTotalCost"),
  usageTotalTurns: byId("usageTotalTurns"),
  updateButtonDot: byId("updateButtonDot"),
  workspace: byId("workspace"),
};

let appInfo = null;
let busy = false;
let autosaveTimer = null;
let configSaving = false;
let currentView = "console";
let currentConfigTab = "client";
let currentTools = [];
let currentToolsSignature = "";
let currentConfigSignature = "";
let currentAdapterSignature = "";
let currentToolValuesSignature = "";
let refreshInFlight = false;
let refreshQueuedOptions = null;
let refreshTimer = null;
let toolsLoaded = false;
let i18n = {};
let languages = [];
let systemLanguageHints = [];
let configuredLanguage = DEFAULT_LANGUAGE;
let lastSavedConfig = null;
let pendingConfig = null;
let restartRequired = false;
let latestRunning = false;
let latestStarting = true;
let latestRuntimePort = null;
let logDividers = [];
let logEvents = [];
let logHasMore = false;
let logLoadingOlder = false;
let logRenderPending = false;
let latestLogsLoadedOnce = false;
let latestLogsRefreshInFlight = false;
let lastBalanceData = null;
let lastStatusSignature = "";
let lastUsageSignature = "";
let latestUsageRuntime = null;
let usageRefreshInFlight = false;
let usageRefreshQueued = false;
let lastUsageSourceSignature = "";
let lastLogRenderSignature = "";
let latestAdapter = null;
let latestUpdateCheck = null;
let latestConfigVersion = "";
let externalConfigSyncTimer = null;
let uiLanguage = FALLBACK_LANGUAGE;
let contextMenuEl = null;
let contextMenuTarget = null;
let apiBaseUrl = null;

init();

function byId(id) {
  return document.getElementById(id);
}

async function init() {
  const config = await loadConfig({ render: false }).catch(() => ({}));
  configuredLanguage = normalizeConfiguredLanguageId(config.UI_LANGUAGE || DEFAULT_LANGUAGE);
  i18n = await loadI18n(configuredLanguage);
  bind();
  runSoon(bindDesktopConfigEvents);
  applyLanguage(configuredLanguage);
  if (els.configTomlStatus) els.configTomlStatus.textContent = codexConfigPathHint();
  renderConfig(config || {});
  setView("console");
  await Promise.allSettled([loadAppInfo(), refresh()]);
  runSoon(loadCodexAdapter);
  runSoon(() => checkForUpdates({ silent: true }));
  runSoon(refreshBalance);
}

function runSoon(task) {
  const run = () => Promise.resolve().then(task).catch(() => {});
  if (typeof requestIdleCallback === "function") {
    requestIdleCallback(run, { timeout: 1500 });
    return;
  }
  setTimeout(run, 0);
}

async function loadI18n(targetLanguage) {
  try {
    const manifestResponse = await apiFetch("/api/languages", { cache: "no-store" });
    if (!manifestResponse.ok) throw new Error("Failed to load languages");
    const manifest = await manifestResponse.json();
    systemLanguageHints = languageHintsFromManifest(manifest);
    const loadedLanguages = Array.isArray(manifest.languages) ? manifest.languages : [];
    languages = loadedLanguages.length > 0
      ? loadedLanguages.map((language) => ({ id: normalizeLanguageId(language.id), name: language.name || language.id, url: language.url || "" })).filter((language) => language.id)
      : [];
    renderLanguageOptions();
    const languageId = resolveLanguageId(targetLanguage);
    const pack = await fetchLanguagePack(languageId);
    if (pack) languages = mergeLanguageName(languages, languageId, pack.languageName || languageId);
    uiLanguage = languageId;
    configuredLanguage = normalizeConfiguredLanguageId(targetLanguage);
    i18n = pack ? { [languageId]: pack } : {};
    renderLanguageOptions();
    return i18n;
  } catch {
    configuredLanguage = normalizeConfiguredLanguageId(targetLanguage);
    uiLanguage = resolveLanguageId(targetLanguage);
    languages = [];
    systemLanguageHints = [];
    i18n = {};
    renderLanguageOptions();
    return {};
  }
}

function bind() {
  els.startButton.addEventListener("click", () => actionPost("/api/start", t("startingTitle"), t("startingDetail")));
  els.restartButton.addEventListener("click", () => actionPost("/api/restart", t("restartingTitle"), t("restartingDetail")));
  els.stopButton.addEventListener("click", () => actionPost("/api/stop", t("stoppingTitle"), t("stoppingDetail")));
  if (els.refreshBalanceButton) els.refreshBalanceButton.addEventListener("click", refreshBalance);
  if (els.rechargeBalanceButton) els.rechargeBalanceButton.addEventListener("click", openRechargePage);
  if (els.copyTomlButton) els.copyTomlButton.addEventListener("click", copyConfigToml);
  if (els.logStream) els.logStream.addEventListener("scroll", handleLogScroll);
  document.addEventListener("contextmenu", handleContextMenu);
  document.addEventListener("click", hideContextMenu);
  document.addEventListener("scroll", hideContextMenu, true);
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") hideContextMenu();
  });
  document.addEventListener("visibilitychange", () => scheduleNextRefresh(0));
  window.addEventListener(CONFIG_CHANGED_EVENT, () => scheduleExternalConfigSync());
  if (els.toolConfigList) {
    els.toolConfigList.addEventListener("input", handleConfigInput);
    els.toolConfigList.addEventListener("change", handleConfigInput);
    els.toolConfigList.addEventListener("focusout", handleConfigInput);
  }

  [els.showThinking, els.autoStart, els.deepseekOfficialV1Compat, els.uiLanguage, els.deepseekBaseUrl, els.proxyPort, ...billingInputs()].forEach((input) => {
    if (!input) return;
    input.addEventListener("input", handleConfigInput);
    input.addEventListener("change", handleConfigInput);
    input.addEventListener("focusout", handleConfigInput);
  });

  onRadioChange("CONFIG_TAB", setConfigTab);
  onRadioChange("UPSTREAM_MODEL_OVERRIDE", handleConfigInput);
  onRadioChange("DEEPSEEK_TEMPERATURE_PRESET", handleConfigInput);
  onRadioChange("DEEPSEEK_THINKING", handleConfigInput);
  onRadioChange("NETWORK_PROXY_MODE", handleConfigInput);
  onRadioChange("LOG_RETENTION_DAYS", handleConfigInput);
  onRadioChange("UI_CLOSE_BEHAVIOR", handleConfigInput);
  onRadioChange("UI_THEME", (value) => {
    applyTheme(value);
    handleConfigInput();
  });

  if (els.uiLanguage) {
    els.uiLanguage.addEventListener("change", async () => {
      await ensureLanguageLoaded(els.uiLanguage.value);
      applyLanguage(els.uiLanguage.value);
      renderLogs();
    });
  }

  els.navItems.forEach((item) => {
    item.addEventListener("click", (event) => {
      event.preventDefault();
      setView(item.dataset.view || "console");
      if (currentView === "about") markUpdateNoticeSeen();
      if (currentView === "config" && currentConfigTab === "tools") ensureToolsLoaded();
    });
  });

  document.querySelectorAll("[data-about-action]").forEach((button) => {
    button.addEventListener("click", () => handleAboutAction(button.dataset.aboutAction));
  });

  document.addEventListener("dragstart", (event) => event.preventDefault());
}

function handleContextMenu(event) {
  event.preventDefault();
  contextMenuTarget = event.target instanceof Element ? event.target : null;
  showContextMenu(event.clientX, event.clientY);
}

function showContextMenu(x, y) {
  const menu = ensureContextMenu();
  const copyButton = menu.querySelector("[data-context-action=\"copy\"]");
  if (copyButton) copyButton.disabled = !selectedText();
  menu.hidden = false;
  const rect = menu.getBoundingClientRect();
  const left = Math.min(x, window.innerWidth - rect.width - 8);
  const top = Math.min(y, window.innerHeight - rect.height - 8);
  menu.style.left = Math.max(8, left) + "px";
  menu.style.top = Math.max(8, top) + "px";
}

function hideContextMenu() {
  if (contextMenuEl) contextMenuEl.hidden = true;
}

function ensureContextMenu() {
  if (contextMenuEl) return contextMenuEl;
  const menu = document.createElement("div");
  menu.className = "context-menu";
  menu.hidden = true;
  menu.appendChild(contextMenuButton("selectAll", t("contextSelectAll")));
  menu.appendChild(contextMenuButton("copy", t("contextCopy")));
  document.body.appendChild(menu);
  contextMenuEl = menu;
  return menu;
}

function contextMenuButton(action, label) {
  const button = document.createElement("button");
  button.type = "button";
  button.dataset.contextAction = action;
  button.textContent = label;
  button.addEventListener("click", async (event) => {
    event.stopPropagation();
    if (action === "selectAll") selectContextText();
    if (action === "copy") await copySelectedText();
    hideContextMenu();
  });
  return button;
}

function updateContextMenuLabels() {
  if (!contextMenuEl) return;
  const selectAll = contextMenuEl.querySelector("[data-context-action=\"selectAll\"]");
  const copy = contextMenuEl.querySelector("[data-context-action=\"copy\"]");
  if (selectAll) selectAll.textContent = t("contextSelectAll");
  if (copy) copy.textContent = t("contextCopy");
}

function selectContextText() {
  const editable = editableTarget(contextMenuTarget || document.activeElement);
  if (editable) {
    editable.focus();
    editable.select();
    return;
  }
  const target = contextMenuTarget && contextMenuTarget.closest
    ? contextMenuTarget.closest(".selectable") || document.querySelector(".workspace")
    : document.querySelector(".workspace");
  if (!target) return;
  const range = document.createRange();
  range.selectNodeContents(target);
  const selection = window.getSelection();
  selection.removeAllRanges();
  selection.addRange(range);
}

async function copySelectedText() {
  const text = selectedText();
  if (!text) return;
  await navigator.clipboard.writeText(text).catch(() => document.execCommand("copy"));
}

function selectedText() {
  const editable = editableTarget(document.activeElement);
  if (editable && editable.selectionStart !== editable.selectionEnd) {
    return editable.value.slice(editable.selectionStart, editable.selectionEnd);
  }
  return String(window.getSelection ? window.getSelection().toString() : "").trim();
}

function editableTarget(target) {
  if (!(target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement)) return null;
  return target;
}

function onRadioChange(name, callback) {
  document.querySelectorAll(`input[name="${name}"]`).forEach((input) => {
    input.addEventListener("change", (event) => callback(event.target.value));
  });
}

function getRadioValue(name) {
  const el = document.querySelector(`input[name="${name}"]:checked`);
  return el ? el.value : null;
}

function setRadioValue(name, value) {
  const el = document.querySelector(`input[name="${name}"][value="${value}"]`);
  if (el) el.checked = true;
}

async function actionPost(url, title, detail) {
  if (busy) return;
  setBusy(true, title, detail);
  try {
    await apiFetch(url, { method: "POST" });
    if (url === "/api/restart") {
      restartRequired = false;
      renderConfigSaveState(pendingConfig ? "pending" : "clean");
    }
    await delay(450);
    await refresh({ forceLogs: true, force: true });
  } finally {
    setBusy(false);
  }
}

async function saveConfig() {
  if (!pendingConfig) return;
  if (busy || configSaving) {
    scheduleConfigSave(CONFIG_AUTOSAVE_RETRY_MS);
    return;
  }
  configSaving = true;
  renderConfigSaveState("saving");
  const payload = pendingConfig;
  const previousConfig = lastSavedConfig;
  let saveCompleted = false;
  try {
    const response = await apiFetch("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    if (!response.ok) throw new Error("Config save failed");
    const status = await response.json().catch(() => null);
    const needsRestart = hasRestartRequiredChanges(payload);
    lastSavedConfig = normalizeConfigPayload(payload);
    if (pendingConfig === payload || sameConfigPayload(normalizeConfigPayload(pendingConfig), lastSavedConfig)) {
      pendingConfig = null;
    }
    if (needsRestart) restartRequired = true;
    if (status && status.config_version) latestConfigVersion = String(status.config_version);
    renderConfigSaveState(pendingConfig ? "pending" : (restartRequired ? "savedRestart" : "saved"));
    saveCompleted = true;
    await syncDesktopConfig(payload, previousConfig).catch(() => {});
    await loadConfig();
    await loadCodexAdapter().catch(() => {});
    if (toolsLoaded) await loadTools();
    await refresh({ forceLogs: true, force: true });
    if (currentView === "usage") await refreshUsage({ force: true });
  } catch (error) {
    renderConfigSaveState("error");
  } finally {
    configSaving = false;
    if (saveCompleted && pendingConfig) scheduleConfigSave(CONFIG_AUTOSAVE_RETRY_MS);
  }
}

async function refresh(options = {}) {
  if (refreshInFlight) {
    if (options.force || options.forceLogs) {
      refreshQueuedOptions = Object.assign({}, refreshQueuedOptions || {}, options, { force: true });
    }
    return;
  }
  refreshInFlight = true;
  const started = performance.now();
  try {
    const data = await apiJson("/api/status", { cache: "no-store" });
    await syncConfigIfChanged(data.config_version);
    renderStatus(data);
    if (Array.isArray(data.events)) {
      updateLatestLogs(data.events, {
        force: Boolean(options.forceLogs),
        hasMore: data.has_more,
      });
    } else if (options.forceLogs || currentView === "logs" || !latestLogsLoadedOnce) {
      await refreshLatestLogs({ force: Boolean(options.forceLogs) });
    }
    maybeRefreshUsage(data.runtime || {}, options);
  } catch (error) {
    latestRunning = false;
    latestStarting = false;
    els.running.textContent = t("unavailable");
    els.statusPill.classList.remove("running");
    els.statusPill.classList.remove("starting");
    renderButtons();
    updateLatestLogs([{
      ts: new Date().toISOString(),
      type: "client_error",
      level: "error",
      message: error.message || String(error),
      detail: clientErrorDetail("/api/status", error),
    }], { force: true });
  } finally {
    refreshInFlight = false;
    noteSlow("refresh", performance.now() - started);
    const queued = refreshQueuedOptions;
    refreshQueuedOptions = null;
    if (queued) refresh(queued);
    else scheduleNextRefresh();
  }
}

function maybeRefreshUsage(runtime, options = {}) {
  if (currentView !== "usage") return;
  const sourceSignature = stableStringify({
    request_count: runtime.request_count || 0,
    last_request_at: runtime.last_request_at || "",
    billing: currentBillingSignature(),
  });
  if (!options.force && latestUsageRuntime && sourceSignature === lastUsageSourceSignature) return;
  lastUsageSourceSignature = sourceSignature;
  refreshUsage({ force: Boolean(options.force) }).catch(() => {});
}

async function refreshUsage(options = {}) {
  if (usageRefreshInFlight) {
    if (options.force) usageRefreshQueued = true;
    return;
  }
  usageRefreshInFlight = true;
  const started = performance.now();
  try {
    const data = await apiJson("/api/usage", { cache: "no-store" });
    latestUsageRuntime = data.runtime || {};
    renderUsage(latestUsageRuntime);
  } catch (error) {
    updateLatestLogs([{
      ts: new Date().toISOString(),
      type: "client_error",
      level: "error",
      message: error.message || String(error),
      detail: clientErrorDetail("/api/usage", error),
    }], { force: true });
  } finally {
    usageRefreshInFlight = false;
    noteSlow("refreshUsage", performance.now() - started);
    if (usageRefreshQueued) {
      usageRefreshQueued = false;
      refreshUsage({ force: true }).catch(() => {});
    }
  }
}

async function refreshLatestLogs(options = {}) {
  if (latestLogsRefreshInFlight) return;
  latestLogsRefreshInFlight = true;
  try {
    const data = await apiJson("/api/events?limit=" + LOG_INITIAL_PAGE_SIZE, { cache: "no-store" });
    latestLogsLoadedOnce = true;
    updateLatestLogs(Array.isArray(data.events) ? data.events : [], {
      force: Boolean(options.force),
      hasMore: data.has_more,
    });
  } catch (error) {
    updateLatestLogs([{
      ts: new Date().toISOString(),
      type: "client_error",
      level: "error",
      message: error.message || String(error),
      detail: clientErrorDetail("/api/events", error),
    }], { force: true });
  } finally {
    latestLogsRefreshInFlight = false;
  }
}

async function syncConfigIfChanged(configVersion) {
  const version = String(configVersion || "");
  if (!version || version === latestConfigVersion || pendingConfig || configSaving) return;
  latestConfigVersion = version;
  await loadConfig().catch(() => null);
  await loadCodexAdapter().catch(() => null);
}

function scheduleExternalConfigSync() {
  if (externalConfigSyncTimer) clearTimeout(externalConfigSyncTimer);
  externalConfigSyncTimer = setTimeout(() => {
    externalConfigSyncTimer = null;
    syncExternalConfig().catch(() => {});
  }, 40);
}

async function syncExternalConfig() {
  if (pendingConfig || configSaving) return;
  currentConfigSignature = "";
  await loadConfig();
  await loadCodexAdapter().catch(() => null);
  if (toolsLoaded) await loadTools().catch(() => null);
  await refresh({ force: true, forceLogs: true });
  if (currentView === "usage") await refreshUsage({ force: true });
}

async function bindDesktopConfigEvents() {
  const listen = window.__TAURI__ && window.__TAURI__.event && window.__TAURI__.event.listen;
  if (typeof listen !== "function") return;
  try {
    await listen(CONFIG_CHANGED_EVENT, () => {
      window.dispatchEvent(new Event(CONFIG_CHANGED_EVENT));
    });
  } catch {}
}

async function syncDesktopConfig(payload, previousConfig) {
  if (!isTauriRuntime()) return;
  const tasks = [];
  if (payload && payload.UI_THEME !== undefined) {
    tasks.push(desktopInvoke("desktop_apply_theme", { theme: payload.UI_THEME || "system" }));
  }
  if (
    payload &&
    payload.AUTO_START !== undefined &&
    (!previousConfig || String(payload.AUTO_START) !== String(previousConfig.AUTO_START))
  ) {
    tasks.push(desktopInvoke("desktop_apply_autostart", { enabled: isTruthy(payload.AUTO_START) }));
  }
  tasks.push(desktopInvoke("desktop_refresh_tray"));
  await Promise.allSettled(tasks);
}

function isTauriRuntime() {
  return Boolean(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);
}

function desktopInvoke(command, args = {}) {
  const invoke = window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
  if (typeof invoke !== "function") return Promise.reject(new Error("Tauri runtime is unavailable"));
  return invoke(command, args);
}

function isApiRequestUrl(url) {
  const value = String(url || "");
  return value === "/health" || value.startsWith("/api/");
}

function defaultApiBaseUrl() {
  const protocol = window.location && window.location.protocol;
  return protocol === "http:" || protocol === "https:" ? "" : DEBUG_MANAGER_BASE_URL;
}

async function resolveApiBaseUrl() {
  if (apiBaseUrl !== null) return apiBaseUrl;
  apiBaseUrl = defaultApiBaseUrl();
  return apiBaseUrl;
}

async function apiUrl(url) {
  const value = String(url || "");
  if (!isApiRequestUrl(value) || /^https?:\/\//i.test(value)) return value;
  const base = await resolveApiBaseUrl();
  return base ? base + value : value;
}

async function apiFetch(url, options = {}) {
  if (isTauriRuntime() && isApiRequestUrl(url)) {
    return desktopManagerFetch(url, options);
  }
  const target = await apiUrl(url);
  try {
    const response = await fetch(target, options);
    response.codeseexTargetUrl = target;
    return response;
  } catch (error) {
    const wrapped = new Error(`${String(url || "")} failed: ${error && error.message ? error.message : String(error)}`);
    wrapped.cause = error;
    wrapped.endpoint = String(url || "");
    wrapped.targetUrl = target;
    throw wrapped;
  }
}

async function desktopManagerFetch(url, options = {}) {
  const endpoint = String(url || "");
  const parsed = new URL(endpoint, "http://codeseex.local");
  const method = String(options.method || "GET").toUpperCase();
  const query = {};
  parsed.searchParams.forEach((value, key) => {
    query[key] = value;
  });
  try {
    const response = await desktopInvoke("desktop_manager_request", {
      method,
      path: parsed.pathname,
      query,
      body: parseRequestBody(options.body)
    });
    const wrapped = responseLike(response);
    wrapped.codeseexTargetUrl = "tauri://desktop_manager_request" + parsed.pathname;
    return wrapped;
  } catch (error) {
    const wrapped = new Error(`${endpoint} failed: ${error && error.message ? error.message : String(error)}`);
    wrapped.cause = error;
    wrapped.endpoint = endpoint;
    wrapped.targetUrl = "tauri://desktop_manager_request" + parsed.pathname;
    throw wrapped;
  }
}

function parseRequestBody(body) {
  if (body === undefined || body === null || body === "") return null;
  if (typeof body === "string") {
    try {
      return JSON.parse(body);
    } catch (_) {
      return { raw: body };
    }
  }
  return body;
}

function responseLike(response) {
  const status = Number(response && response.status) || 500;
  const body = response && response.body !== undefined ? response.body : null;
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: String(status),
    codeseexTargetUrl: "",
    headers: {
      get(name) {
        return String(name || "").toLowerCase() === "content-type"
          ? "application/json; charset=utf-8"
          : null;
      }
    },
    async json() {
      return body;
    },
    async text() {
      return typeof body === "string" ? body : JSON.stringify(body || {});
    }
  };
}

async function apiJson(url, options = {}) {
  const response = await apiFetch(url, options);
  if (!response.ok) {
    const body = await response.text().catch(() => "");
    const preview = body ? " " + body.slice(0, 180).replace(/\s+/g, " ") : "";
    const error = new Error(`${url} failed: HTTP ${response.status}${preview}`);
    error.endpoint = String(url || "");
    error.targetUrl = response.codeseexTargetUrl || "";
    error.status = response.status;
    throw error;
  }
  return response.json();
}

function clientErrorDetail(endpoint, error) {
  return {
    endpoint,
    target: error && error.targetUrl ? error.targetUrl : "",
    status: error && error.status !== undefined ? error.status : "",
    message: error && error.message ? error.message : String(error || ""),
    protocol: window.location && window.location.protocol ? window.location.protocol : "",
    tauri_runtime: isTauriRuntime() ? "available" : "unavailable",
  };
}

async function loadConfig(options = {}) {
  const started = performance.now();
  const config = await apiJson("/api/config", { cache: "no-store" });
  if (config && config.config_version) latestConfigVersion = String(config.config_version);
  if (options.render !== false) renderConfig(config || {});
  noteSlow("loadConfig", performance.now() - started);
  return config;
}

async function loadTools() {
  const started = performance.now();
  const data = await apiJson("/api/tools", { cache: "no-store" });
  const config = lastSavedConfig || {};
  renderTools(data.tools || [], config);
  toolsLoaded = true;
  noteSlow("loadTools", performance.now() - started);
  return data.tools || [];
}

async function loadCodexAdapter() {
  const data = await apiJson("/api/codex-adapter", { cache: "no-store" });
  renderCodexAdapter(data || {});
  return data || {};
}

async function checkForUpdates(options = {}) {
  try {
    latestUpdateCheck = await apiJson("/api/update-check", { cache: "no-store" });
  } catch (error) {
    latestUpdateCheck = { ok: false, has_update: false, error: error.message || String(error) };
  }
  renderUpdateState({ silent: Boolean(options.silent) });
  return latestUpdateCheck;
}

async function ensureToolsLoaded() {
  if (toolsLoaded && currentTools.length > 0) return currentTools;
  return loadTools();
}

async function ensureLanguageLoaded(languageId) {
  const target = resolveLanguageId(languageId);
  if (i18n[target]) return i18n[target];
  const pack = await fetchLanguagePack(target);
  if (!pack) return null;
  i18n = Object.assign({}, i18n, { [target]: pack });
  languages = mergeLanguageName(languages, target, pack.languageName || target);
  renderLanguageOptions();
  return pack;
}

async function fetchLanguagePack(languageId) {
  const target = normalizeLanguageId(languageId);
  if (!target) return null;
  let loadedLanguages = languages;
  if (!Array.isArray(loadedLanguages) || loadedLanguages.length === 0) {
    const manifest = await apiFetch("/api/languages", { cache: "no-store" }).then((response) => response.ok ? response.json() : null).catch(() => null);
    systemLanguageHints = languageHintsFromManifest(manifest);
    loadedLanguages = Array.isArray(manifest && manifest.languages)
      ? manifest.languages.map((language) => ({ id: normalizeLanguageId(language.id), name: language.name || language.id, url: language.url || "" })).filter((language) => language.id)
      : [];
    languages = loadedLanguages;
  }
  const language = Array.isArray(loadedLanguages)
    ? loadedLanguages.find((item) => normalizeLanguageId(item && item.id) === target)
    : null;
  if (!language || !language.url) return null;
  const response = await fetch(language.url, { cache: "no-store" }).catch(() => null);
  if (!response || !response.ok) return null;
  const pack = await response.json().catch(() => null);
  if (!pack || typeof pack !== "object" || Array.isArray(pack)) return null;
  return pack;
}

function mergeLanguageName(list, id, name) {
  const target = normalizeLanguageId(id);
  const next = [];
  let replaced = false;
  for (const language of Array.isArray(list) ? list : []) {
    if (normalizeLanguageId(language && language.id) === target) {
      next.push({ id: target, name: name || language.name || target, url: language.url || "" });
      replaced = true;
    } else {
      next.push(language);
    }
  }
  if (!replaced && target) next.push({ id: target, name: name || target, url: "" });
  return next.sort((left, right) => left.id.localeCompare(right.id));
}

function scheduleNextRefresh(delayMs) {
  if (refreshTimer) clearTimeout(refreshTimer);
  const delay = delayMs !== undefined ? delayMs : nextRefreshDelay();
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    refresh();
  }, delay);
}

function nextRefreshDelay() {
  if (document.hidden) return REFRESH_HIDDEN_MS;
  const active = Number(els.activeRequests && els.activeRequests.textContent ? String(els.activeRequests.textContent).replace(/\D/g, "") : 0);
  return latestRunning || active > 0 ? REFRESH_RUNNING_MS : REFRESH_IDLE_MS;
}

async function loadAppInfo() {
  try {
    appInfo = await apiJson("/api/app-info", { cache: "no-store" });
    renderAppInfo(appInfo);
  } catch (error) {
    appInfo = null;
    setAboutStatus((error.message || String(error)), true);
  }
}

async function refreshBalance() {
  if (els.refreshBalanceButton) els.refreshBalanceButton.disabled = true;
  setBalanceStage(t("balanceLoading"), "active");
  try {
    const response = await apiFetch("/api/deepseek/balance", { cache: "no-store" });
    renderBalance(await response.json());
  } catch (error) {
    renderBalance({ ok: false, error: error.message || String(error) });
  } finally {
    if (els.refreshBalanceButton) els.refreshBalanceButton.disabled = false;
  }
}

async function loadOlderLogs() {
  if (logLoadingOlder || !logHasMore) return;
  const oldest = oldestLogTs();
  if (!oldest) return;
  logLoadingOlder = true;
  const oldScrollTop = els.logStream.scrollTop;
  try {
    const url = "/api/events?limit=" + LOG_OLDER_PAGE_SIZE + "&before=" + encodeURIComponent(oldest);
    const data = await apiJson(url, { cache: "no-store" });
    const older = Array.isArray(data.events) ? data.events : [];
    const existingKeys = new Set(logEvents.map(logEventKey));
    const addedOlder = older.filter((event) => event && event.ts && !existingKeys.has(logEventKey(event)));
    logHasMore = Boolean(data.has_more);
    if (addedOlder.length > 0) {
      const newestLoaded = addedOlder[addedOlder.length - 1];
      logDividers.push({ key: logEventKey(newestLoaded), count: addedOlder.length });
    }
    logEvents = mergeEvents(older.concat(logEvents)).slice(-LOG_MAX_ITEMS);
    pruneLogDividers();
    renderLogs();
    els.logStream.scrollTop = oldScrollTop;
  } finally {
    logLoadingOlder = false;
  }
}

function renderStatus(data) {
  const runtime = data.runtime || {};
  const runtimeStatus = String(data.runtime_status || runtime.status || "").toLowerCase();
  const isStarting = !data.running && runtimeStatus === RUNTIME_STATUS_STARTING;
  const isStopping = !data.running && runtimeStatus === RUNTIME_STATUS_STOPPING;
  const signature = stableStringify({
    running: Boolean(data.running),
    runtime_status: runtimeStatus,
    pid: data.pid || "",
    process_label: data.process_label || "",
    active_requests: runtime.active_requests || 0,
    request_count: runtime.request_count || 0,
    failed_request_count: runtime.failed_request_count || 0,
    last_request_at: runtime.last_request_at || "",
  });
  if (signature === lastStatusSignature) return;
  lastStatusSignature = signature;
  latestRunning = Boolean(data.running);
  latestStarting = isStarting || isStopping;
  latestRuntimePort = runtime.port || null;
  els.statusPill.classList.toggle("running", latestRunning);
  els.statusPill.classList.toggle("starting", latestStarting);
  els.running.textContent = latestRunning
    ? t("running")
    : (isStopping ? t("stopping") : (latestStarting ? t("starting") : t("stopped")));
  els.pidLabel.textContent = data.process_label || (data.process_mode === "inline" ? t("appPid") : t("proxyPid"));
  els.pid.textContent = data.pid || "-";
  els.activeRequests.textContent = formatNumber(runtime.active_requests || 0);
  els.completedTurns.textContent = formatNumber(runtime.request_count || 0);
  els.failedTurns.textContent = formatNumber(runtime.failed_request_count || 0);
  renderDashboardReadiness(data, runtime, { isStarting, isStopping });
  renderButtons();
}

function renderDashboardReadiness(data, runtime, state) {
  const isRunning = Boolean(data.running);
  const isStarting = Boolean(state && state.isStarting);
  const isStopping = Boolean(state && state.isStopping);
  const port = runtime.port || latestRuntimePort || (lastSavedConfig && lastSavedConfig.PROXY_PORT) || "8787";
  setStageState(els.stagePortCheck, els.stagePortState, {
    done: isRunning,
    active: isStarting,
    error: !isRunning && !isStarting && !isStopping,
    text: isRunning
      ? t("dashboardStatusReady")
      : (isStarting ? t("dashboardStatusChecking") : t("dashboardPortPending").replace("{port}", port)),
  });
  setStageState(els.stageProxyHealth, els.stageProxyState, {
    done: isRunning,
    active: isStarting || isStopping,
    error: !isRunning && !isStarting && !isStopping,
    text: isRunning
      ? t("dashboardStatusRunning")
      : (isStopping ? t("stopping") : (isStarting ? t("starting") : t("dashboardStatusStopped"))),
  });
}

function setStageState(row, label, options) {
  if (!row || !label) return;
  row.classList.toggle("is-done", Boolean(options.done));
  row.classList.toggle("is-active", Boolean(options.active));
  row.classList.toggle("is-error", Boolean(options.error));
  label.textContent = options.text || "-";
}

function renderButtons() {
  els.startButton.disabled = busy || latestRunning || latestStarting;
  els.restartButton.disabled = busy || !latestRunning;
  els.stopButton.disabled = busy || (!latestRunning && !latestStarting);
  els.startButton.textContent = latestRunning ? t("started") : t("start");
  els.restartButton.textContent = t("restart");
  els.stopButton.textContent = t("stop");
}

function renderConfig(config) {
  if (pendingConfig || configSaving) return;
  const active = document.activeElement;
  const textInputs = [els.deepseekBaseUrl, els.proxyPort, ...billingInputs()];
  if (textInputs.includes(active)) return;
  const configSignature = stableStringify(normalizeConfigPayload(config));
  if (configSignature === currentConfigSignature && lastSavedConfig) return;
  currentConfigSignature = configSignature;

  setRadioValue("DEEPSEEK_THINKING", config.DEEPSEEK_THINKING || "auto");
  setRadioValue("UPSTREAM_MODEL_OVERRIDE", normalizeUpstreamModelOverride(config.UPSTREAM_MODEL_OVERRIDE));
  setRadioValue("DEEPSEEK_TEMPERATURE_PRESET", normalizeTemperaturePreset(config.DEEPSEEK_TEMPERATURE_PRESET));
  setRadioValue("NETWORK_PROXY_MODE", normalizeNetworkProxyMode(config.NETWORK_PROXY_MODE || config.WEB_SEARCH_PROXY_MODE));
  setRadioValue("LOG_RETENTION_DAYS", normalizeRetentionDays(config.LOG_RETENTION_DAYS));
  setRadioValue("UI_CLOSE_BEHAVIOR", normalizeCloseBehavior(config.UI_CLOSE_BEHAVIOR));
  const nextTheme = config.UI_THEME || "system";
  setRadioValue("UI_THEME", nextTheme);
  els.showThinking.checked = !/^(0|false|no|off|disabled)$/i.test(String(config.SHOW_THINKING || "true"));
  if (els.autoStart) els.autoStart.checked = isTruthy(config.AUTO_START || "false");
  if (els.deepseekOfficialV1Compat) els.deepseekOfficialV1Compat.checked = isTruthy(config.DEEPSEEK_OFFICIAL_V1_COMPAT || "true");
  if (els.deepseekBaseUrl && document.activeElement !== els.deepseekBaseUrl) els.deepseekBaseUrl.value = normalizeDeepSeekBaseUrl(config.DEEPSEEK_BASE_URL || "");
  if (document.activeElement !== els.proxyPort) els.proxyPort.value = normalizePort(config.PROXY_PORT || "8787");
  const nextLanguage = normalizeConfiguredLanguageId(config.UI_LANGUAGE || DEFAULT_LANGUAGE);
  if (document.activeElement !== els.uiLanguage) els.uiLanguage.value = nextLanguage;
  setBillingInputValues(config);
  currentAdapterSignature = "";
  applyTheme(nextTheme);
  if (resolveLanguageId(nextLanguage) !== uiLanguage || nextLanguage !== configuredLanguage) applyLanguage(nextLanguage);
  lastSavedConfig = normalizeConfigPayload(config);
  lastUsageSignature = "";
  if (!restartRequired) renderConfigSaveState("clean");
  renderCodexAdapter(latestAdapter || {});
}

function renderCodexAdapter(adapter) {
  latestAdapter = adapter || {};
  const signature = stableStringify({
    adapter: latestAdapter,
    model: normalizeUpstreamModelOverride(getRadioValue("UPSTREAM_MODEL_OVERRIDE") || (lastSavedConfig && lastSavedConfig.UPSTREAM_MODEL_OVERRIDE)),
  });
  if (signature === currentAdapterSignature) return;
  currentAdapterSignature = signature;
  const toml = String(latestAdapter.toml_snippet || "");
  if (els.configTomlCode) els.configTomlCode.textContent = toml || "-";
  if (els.configTomlStatus) els.configTomlStatus.textContent = codexConfigPathHint();
}

async function copyConfigToml() {
  const text = configTomlCopyText(els.configTomlCode ? els.configTomlCode.textContent : "");
  if (!text || text === "-") {
    if (els.configTomlCopyStatus) els.configTomlCopyStatus.textContent = t("codexAdapterMissing");
    return;
  }
  try {
    await navigator.clipboard.writeText(text);
    if (els.configTomlCopyStatus) {
      els.configTomlCopyStatus.textContent = t("copied");
      window.setTimeout(() => {
        if (els.configTomlCopyStatus) els.configTomlCopyStatus.textContent = "";
      }, 1800);
    }
  } catch {
    if (els.configTomlCopyStatus) els.configTomlCopyStatus.textContent = t("copyFailed");
  }
}

function configTomlCopyText(value) {
  return String(value || "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .split("\n")
    .filter((line) => !line.trimStart().startsWith("#"))
    .join("\n")
    .trim();
}

function renderUpdateState(options = {}) {
  const hasUpdate = Boolean(latestUpdateCheck && latestUpdateCheck.has_update);
  if (els.aboutUpdateDot) els.aboutUpdateDot.hidden = !hasUpdate || isUpdateNoticeSeen(latestUpdateCheck);
  if (els.updateButtonDot) els.updateButtonDot.hidden = !hasUpdate;
  if (!els.aboutStatus || !latestUpdateCheck || options.silent) return;

  if (hasUpdate) {
    setAboutStatus(renderUpdateAvailableMessage(latestUpdateCheck), false, { html: true });
  } else if (latestUpdateCheck.ok) {
    setAboutStatus(updateMessage("updateCurrent", latestUpdateCheck), false);
  } else {
    setAboutStatus(updateMessage("updateCheckFailed", latestUpdateCheck), true);
  }
}

function updateNoticeVersion(data = latestUpdateCheck) {
  return String(data && (data.latest_version || data.current_version) || "").trim();
}

function isUpdateNoticeSeen(data = latestUpdateCheck) {
  const version = updateNoticeVersion(data);
  return Boolean(version && localStorage.getItem(UPDATE_NOTICE_STORAGE_KEY) === version);
}

function markUpdateNoticeSeen() {
  const version = updateNoticeVersion();
  if (!version) return;
  localStorage.setItem(UPDATE_NOTICE_STORAGE_KEY, version);
  renderUpdateState({ silent: true });
}

function renderUpdateAvailableMessage(data = {}) {
  const url = data.url || (appInfo && appInfo.urls && appInfo.urls.releases) || "";
  const version = data.latest_version || data.current_version || "-";
  const prefix = t("updateAvailablePrefix");
  if (!url) return updateMessage("updateAvailable", data);
  return `${escapeHtml(prefix)} <a href="${escapeHtml(url)}" target="_blank" rel="noopener">${escapeHtml(version)}</a>`;
}

function updateMessage(key, data = {}) {
  return t(key)
    .replace("{version}", data.latest_version || data.current_version || "-")
    .replace("{current}", data.current_version || "-")
    .replace("{error}", data.error || t("unknownError"));
}

function renderTools(tools, config) {
  const started = performance.now();
  const nextTools = Array.isArray(tools) ? tools : [];
  const signature = JSON.stringify(nextTools.map((tool) => ({
    id: tool.id,
    name: tool.name,
    nameKey: tool.nameKey,
    description: tool.description,
    descriptionKey: tool.descriptionKey,
    icon: tool.icon,
    iconPath: tool.iconPath,
    system: Boolean(tool.system),
    configurable: tool.configurable !== false,
    labels: Array.isArray(tool.labels) ? tool.labels.map((label) => ({
      id: label.id,
      labelKey: label.labelKey,
      label: label.label,
    })) : [],
    config: (tool.config || []).map((field) => ({
      key: field.key,
      type: field.type,
      label: field.label,
      description: field.description,
      defaultValue: field.defaultValue,
      options: (field.options || []).map((option) => option.value),
    })),
  })));
  currentTools = nextTools;
  if (!els.toolConfigList) return;
  if (signature !== currentToolsSignature) {
    currentToolsSignature = signature;
    els.toolConfigList.replaceChildren(...nextTools.map(renderToolCard));
  }
  if (!pendingConfig && !configSaving) {
    const valueSignature = stableStringify(normalizeConfigPayload(config));
    if (valueSignature !== currentToolValuesSignature) {
      currentToolValuesSignature = valueSignature;
      applyToolConfigValues(config);
    }
  }
  noteSlow("renderTools", performance.now() - started);
}

function renderToolCard(tool) {
  const card = document.createElement("section");
  card.className = "tool-card";
  card.dataset.toolId = tool.id || "";
  const systemTool = isSystemTool(tool);

  const header = document.createElement("div");
  header.className = "tool-card-header";

  const icon = document.createElement("div");
  icon.className = "tool-card-icon";
  if (tool.iconPath) {
    icon.classList.add("has-svg");
    icon.style.setProperty("--tool-icon-url", `url("${tool.iconPath}")`);
  } else {
    icon.textContent = tool.icon || (tool.id || "T").slice(0, 2).toUpperCase();
  }

  const titleWrap = document.createElement("div");
  titleWrap.className = "tool-card-copy";
  const titleRow = document.createElement("div");
  titleRow.className = "tool-card-title-row";
  const title = document.createElement("div");
  title.className = "tool-card-title";
  title.textContent = translateToolText(tool.nameKey, tool.name || tool.id || "Tool");
  titleRow.appendChild(title);
  for (const label of normalizeToolLabels(tool.labels)) titleRow.appendChild(renderToolLabel(label));
  const description = document.createElement("div");
  description.className = "tool-card-description";
  description.textContent = translateToolText(tool.descriptionKey, tool.description || "");
  titleWrap.appendChild(titleRow);
  if (description.textContent) titleWrap.appendChild(description);

  header.appendChild(icon);
  header.appendChild(titleWrap);
  if (tool.configurable !== false && !systemTool) header.appendChild(renderToolEnableSwitch(tool));
  card.appendChild(header);

  const body = document.createElement("div");
  body.className = "tool-card-body";
  const fields = Array.isArray(tool.config) ? tool.config : [];
  if (fields.length > 0) {
    fields.forEach((field, index) => {
      if (index > 0) body.appendChild(settingDivider());
      body.appendChild(renderToolField(field));
    });
  }
  if (fields.length > 0) card.appendChild(body);
  return card;
}

function isSystemTool(tool) {
  return Boolean(tool && tool.system);
}

function renderToolEnableSwitch(tool) {
  const label = document.createElement("label");
  label.className = "toggle-switch tool-card-switch";
  const input = document.createElement("input");
  input.type = "checkbox";
  input.name = ENABLED_TOOLS_KEY;
  input.dataset.toolId = normalizeToolId(tool && tool.id);
  input.checked = defaultToolEnabled(tool);
  const slider = document.createElement("span");
  slider.className = "slider";
  label.appendChild(input);
  label.appendChild(slider);
  return label;
}

function normalizeToolLabels(labels) {
  const seen = new Set();
  const output = [];
  for (const label of Array.isArray(labels) ? labels : []) {
    if (!label || typeof label !== "object") continue;
    const id = String(label.id || label.label || "").trim();
    if (!id || seen.has(id)) continue;
    seen.add(id);
    output.push({
      id,
      label: translateToolText(label.labelKey, label.label || id),
    });
  }
  return output;
}

function renderToolLabel(label) {
  const element = document.createElement("span");
  element.className = "tool-label";
  element.dataset.labelId = label.id;
  element.textContent = label.label;
  return element;
}

function renderToolField(field) {
  const item = document.createElement("div");
  item.className = "setting-item";

  const labelWrap = document.createElement("span");
  const label = document.createElement("span");
  label.textContent = translateToolText(field.labelKey, field.label || field.key);
  labelWrap.appendChild(label);
  const description = translateToolText(field.descriptionKey || inferredToolTextKey(field, "Hint"), field.description || "");
  if (description) {
    const hint = document.createElement("small");
    hint.className = "muted";
    hint.textContent = description;
    labelWrap.appendChild(hint);
  }
  item.appendChild(labelWrap);

  if (field.type === "segmented") {
    item.appendChild(renderSegmentedField(field));
  } else if (field.type === "select") {
    item.appendChild(renderSelectField(field));
  } else if (field.type === "boolean") {
    item.appendChild(renderBooleanField(field));
  } else if (field.type === "textarea") {
    item.appendChild(renderTextAreaField(field));
  } else {
    const input = document.createElement("input");
    input.className = "inline-control";
    input.name = field.key;
    input.type = field.type === "number" ? "number" : (field.type === "password" ? "password" : "text");
    input.value = field.value || field.defaultValue || "";
    input.placeholder = translateToolText(field.placeholderKey, field.placeholder || "");
    item.appendChild(input);
  }
  return item;
}

function translateToolText(key, fallback) {
  if (!key) return fallback || "";
  const translated = t(key);
  return translated && translated !== key ? translated : (fallback || "");
}

function inferredToolTextKey(field, suffix) {
  const base = field && field.labelKey ? String(field.labelKey) : "";
  return base ? base + suffix : "";
}

function inferredToolOptionKey(field, option) {
  const base = field && field.labelKey ? String(field.labelKey) : "";
  const value = option && option.value !== undefined ? String(option.value) : "";
  return base && value ? `${base}_${value}` : "";
}

function renderSegmentedField(field) {
  const group = document.createElement("div");
  group.className = "segmented-control";
  group.id = "ctrl-tool-" + sanitizeDomId(field.key);
  for (const option of Array.isArray(field.options) ? field.options : []) {
    const id = sanitizeDomId(field.key + "_" + option.value);
    const input = document.createElement("input");
    input.type = "radio";
    input.name = field.key;
    input.id = id;
    input.value = option.value;
    if (option.value === (field.value || field.defaultValue)) input.checked = true;
    const label = document.createElement("label");
    label.htmlFor = id;
    label.textContent = translateToolText(option.labelKey || inferredToolOptionKey(field, option), option.label || option.value);
    group.appendChild(input);
    group.appendChild(label);
  }
  return group;
}

function renderSelectField(field) {
  const select = document.createElement("select");
  select.className = "inline-control";
  select.name = field.key;
  const value = field.value || field.defaultValue || "";
  for (const option of Array.isArray(field.options) ? field.options : []) {
    const el = document.createElement("option");
    el.value = option.value;
    el.textContent = translateToolText(option.labelKey || inferredToolOptionKey(field, option), option.label || option.value);
    el.selected = option.value === value;
    select.appendChild(el);
  }
  return select;
}

function renderBooleanField(field) {
  const label = document.createElement("label");
  label.className = "toggle-switch";
  const input = document.createElement("input");
  input.type = "checkbox";
  input.name = field.key;
  input.checked = isTruthy(field.value || field.defaultValue);
  const slider = document.createElement("span");
  slider.className = "slider";
  label.appendChild(input);
  label.appendChild(slider);
  return label;
}

function renderTextAreaField(field) {
  const textarea = document.createElement("textarea");
  textarea.className = "inline-control";
  textarea.name = field.key;
  textarea.rows = 3;
  textarea.value = field.value || field.defaultValue || "";
  textarea.placeholder = translateToolText(field.placeholderKey, field.placeholder || "");
  return textarea;
}

function applyToolConfigValues(config) {
  const enabledTools = parseEnabledTools(config && config[ENABLED_TOOLS_KEY], currentTools);
  for (const tool of currentTools) {
    if (isSystemTool(tool)) continue;
    const id = normalizeToolId(tool && tool.id);
    const input = document.querySelector(`[name="${cssEscape(ENABLED_TOOLS_KEY)}"][data-tool-id="${cssEscape(id)}"]`);
    if (input) input.checked = enabledTools.includes(id);
  }
  for (const field of toolConfigFields()) {
    const value = config[field.key] !== undefined ? String(config[field.key]) : String(field.defaultValue || "");
    if (field.type === "segmented") setRadioValue(field.key, value);
    else if (field.type === "boolean") {
      const input = document.querySelector(`[name="${cssEscape(field.key)}"]`);
      if (input) input.checked = isTruthy(value);
    }
    else {
      const input = document.querySelector(`[name="${cssEscape(field.key)}"]`);
      if (input && document.activeElement !== input) input.value = value;
    }
  }
}

function collectToolConfigPayload() {
  const payload = {};
  if (!toolsLoaded || currentTools.length === 0) return payload;
  const enabledTools = [];
  for (const tool of currentTools) {
    if (isSystemTool(tool)) continue;
    const id = normalizeToolId(tool && tool.id);
    const input = document.querySelector(`[name="${cssEscape(ENABLED_TOOLS_KEY)}"][data-tool-id="${cssEscape(id)}"]`);
    if (input && input.checked) enabledTools.push(id);
  }
  payload[ENABLED_TOOLS_KEY] = stringifyEnabledTools(enabledTools);
  for (const field of toolConfigFields()) {
    if (!field.key) continue;
    if (field.type === "segmented") payload[field.key] = getRadioValue(field.key) || field.defaultValue || "";
    else if (field.type === "boolean") {
      const input = document.querySelector(`[name="${cssEscape(field.key)}"]`);
      payload[field.key] = input && input.checked ? "true" : "false";
    }
    else {
      const input = document.querySelector(`[name="${cssEscape(field.key)}"]`);
      payload[field.key] = input ? input.value : field.defaultValue || "";
    }
  }
  return payload;
}

function toolConfigFields() {
  const fields = [];
  for (const tool of currentTools) {
    for (const field of Array.isArray(tool.config) ? tool.config : []) fields.push(field);
  }
  return fields;
}

function defaultToolEnabled(tool) {
  if (!tool || tool.enabled === false) return false;
  return String(tool.source || "").trim().toLowerCase() !== "community";
}

function parseEnabledTools(value, tools = currentTools) {
  if (value === undefined || value === null || value === "") {
    return (Array.isArray(tools) ? tools : [])
      .filter((tool) => !isSystemTool(tool) && defaultToolEnabled(tool))
      .map((tool) => normalizeToolId(tool && tool.id))
      .filter(Boolean)
      .sort();
  }
  if (Array.isArray(value)) return uniqueToolIds(value);
  const text = String(value || "").trim();
  if (!text) return [];
  try {
    const parsed = JSON.parse(text);
    if (Array.isArray(parsed)) return uniqueToolIds(parsed);
  } catch {}
  return uniqueToolIds(text.split(","));
}

function stringifyEnabledTools(ids) {
  return JSON.stringify(uniqueToolIds(ids));
}

function uniqueToolIds(ids) {
  const seen = new Set();
  const output = [];
  for (const id of Array.isArray(ids) ? ids : []) {
    const normalized = normalizeToolId(id);
    if (!normalized || seen.has(normalized)) continue;
    seen.add(normalized);
    output.push(normalized);
  }
  return output.sort();
}

function normalizeToolId(value) {
  return String(value || "").trim().toLowerCase().replace(/[^a-z0-9_-]/g, "_").slice(0, 64);
}

function settingDivider() {
  const divider = document.createElement("div");
  divider.className = "setting-divider";
  return divider;
}

function setConfigTab(value) {
  currentConfigTab = ["client", "proxy", "experimental", "tools"].includes(value) ? value : "client";
  document.querySelectorAll("[data-config-panel]").forEach((panel) => {
    panel.classList.toggle("active", panel.dataset.configPanel === currentConfigTab);
  });
  if (currentConfigTab === "tools") ensureToolsLoaded();
}

function sanitizeDomId(value) {
  return String(value || "field").replace(/[^a-zA-Z0-9_-]/g, "_");
}

function cssEscape(value) {
  if (window.CSS && typeof window.CSS.escape === "function") return window.CSS.escape(value);
  return String(value || "").replace(/["\\]/g, "\\$&");
}

function isTruthy(value) {
  return /^(1|true|yes|on|enabled)$/i.test(String(value || "").trim());
}

function renderUsage(runtime) {
  const started = performance.now();
  const turns = Array.isArray(runtime.turn_history) ? runtime.turn_history.slice(-120) : [];
  const usageSignature = stableStringify({
    locale: uiLanguage,
    billing: currentBillingSignature(),
    total_cached_input_tokens: runtime.total_cached_input_tokens || 0,
    total_cache_miss_input_tokens: runtime.total_cache_miss_input_tokens || 0,
    total_output_tokens: runtime.total_output_tokens || 0,
    turn_ids: turns.map((turn) => [
      turn.id || "",
      turn.completed_at || "",
      turn.model || "",
      turn.requested_model || "",
      turn.cached_input_tokens || 0,
      turn.cache_miss_input_tokens || 0,
      turn.output_tokens || 0,
      turn.total_tokens || 0,
      turn.request_ms || 0,
    ]),
  });
  if (usageSignature === lastUsageSignature) return;
  lastUsageSignature = usageSignature;
  const totalTurnsCount = turns.length;
  const avgMs = average(turns.map((turn) => turn.request_ms || 0).filter((value) => value > 0));
  const totalCached = runtime.total_cached_input_tokens || 0;
  const totalMiss = runtime.total_cache_miss_input_tokens || 0;
  const totalInput = totalCached + totalMiss;
  const cacheHitRate = totalInput > 0 ? (totalCached / totalInput * 100).toFixed(1) + "%" : "-";
  const totalCostVal = turns.reduce((sum, turn) => sum + costForTokens(turn), 0);

  els.usageTotalTurns.textContent = formatNumber(totalTurnsCount);
  els.usageCacheHitRate.textContent = cacheHitRate;
  els.usageAverageMs.textContent = formatDuration(avgMs);
  els.usageTotalCost.textContent = formatCost(totalCostVal);
  renderUsageRows(turns);
  noteSlow("renderUsage", performance.now() - started);
}

function renderUsageRows(turns) {
  els.usageRows.replaceChildren();
  if (turns.length === 0) {
    const empty = document.createElement("div");
    empty.className = "usage-empty";
    empty.textContent = t("noRows");
    els.usageRows.appendChild(empty);
    return;
  }
  for (const turn of turns.slice().reverse()) {
    els.usageRows.appendChild(usageRecord(turn));
  }
}

function usageRecord(turn) {
  const details = document.createElement("details");
  details.className = "usage-record";
  const summary = document.createElement("summary");
  const modelLabel = usageModelLabel(turn);
  const totalCost = formatCost(costForTokens(turn));
  summary.appendChild(usageTitle(t("usageConversationRecord"), [
    formatDateTime(turn.completed_at),
    modelLabel,
    turn.requested_model && turn.requested_model !== turn.model ? t("usageRequestedModel").replace("{model}", turn.requested_model) : "",
  ].filter(Boolean).join(" · ")));
  [
    [t("cacheHit"), formatNumber(turn.cached_input_tokens)],
    [t("cacheMiss"), formatNumber(turn.cache_miss_input_tokens)],
    [t("output"), formatNumber(turn.output_tokens)],
    [t("elapsed"), formatDuration(turn.request_ms)],
    [t("cost"), totalCost],
  ].forEach(([label, value]) => summary.appendChild(usageMetric(label, value)));
  details.appendChild(summary);

  const body = document.createElement("div");
  body.className = "usage-record-body";
  const stages = document.createElement("div");
  stages.className = "usage-stage-grid";
  stages.appendChild(usageStage(t("usageStageRequest"), modelLabel, t("usageStageRequestHint")));
  stages.appendChild(usageStage(t("usageStageInput"), formatNumber((turn.cached_input_tokens || 0) + (turn.cache_miss_input_tokens || 0)), t("usageStageInputHint")));
  stages.appendChild(usageStage(t("usageStageOutput"), formatNumber(turn.output_tokens), t("usageStageOutputHint")));
  stages.appendChild(usageStage(t("usageStageTotal"), formatNumber(turn.total_tokens), t("usageStageTotalHint").replace("{cost}", totalCost)));
  body.appendChild(stages);
  details.appendChild(body);
  return details;
}

function usageTitle(title, subtitle) {
  const wrap = document.createElement("div");
  wrap.className = "usage-record-title";
  const strong = document.createElement("strong");
  strong.textContent = title;
  const span = document.createElement("span");
  span.textContent = subtitle || "-";
  wrap.append(strong, span);
  return wrap;
}

function usageMetric(label, value) {
  const wrap = document.createElement("div");
  wrap.className = "usage-metric";
  const span = document.createElement("span");
  span.textContent = label;
  const strong = document.createElement("strong");
  strong.textContent = value || "-";
  wrap.append(span, strong);
  return wrap;
}

function usageStage(label, value, hint) {
  const wrap = document.createElement("div");
  wrap.className = "usage-stage";
  const span = document.createElement("span");
  span.textContent = label;
  const strong = document.createElement("strong");
  strong.textContent = value || "-";
  const p = document.createElement("p");
  p.textContent = hint || "";
  wrap.append(span, strong, p);
  return wrap;
}

function usageModelLabel(turn) {
  const model = String(turn && turn.model || "").trim();
  const requested = String(turn && turn.requested_model || "").trim();
  return model || requested || "-";
}

function updateLatestLogs(events, options = {}) {
  const next = Array.isArray(events) ? events : [];
  const hasMore = options.hasMore === undefined ? null : Boolean(options.hasMore);
  const shouldFollow = options.force || logEvents.length === 0 || isAtLogTop();
  const nextEvents = logEvents.length === 0 ? next.slice(-LOG_INITIAL_PAGE_SIZE) : eventsAfterNewestLog(next);
  if (!options.force && nextEvents.length === 0) {
    logHasMore = hasMore === null ? (next.length >= LOG_INITIAL_PAGE_SIZE || logHasMore) : hasMore;
    if (logRenderPending && shouldFollow) {
      logRenderPending = false;
      renderLogs();
      els.logStream.scrollTop = 0;
    }
    return;
  }
  if (shouldFollow) {
    logEvents = mergeEvents(logEvents.concat(nextEvents)).slice(-LOG_MAX_ITEMS);
    logHasMore = hasMore === null ? (next.length >= LOG_INITIAL_PAGE_SIZE || logHasMore) : hasMore;
    pruneLogDividers();
    logRenderPending = false;
    renderLogs();
    els.logStream.scrollTop = 0;
  } else {
    logEvents = mergeEvents(logEvents.concat(nextEvents)).slice(-LOG_MAX_ITEMS);
    logHasMore = hasMore === null ? (next.length >= LOG_INITIAL_PAGE_SIZE || logHasMore) : hasMore;
    pruneLogDividers();
    logRenderPending = true;
  }
}

function renderLogs() {
  const started = performance.now();
  const shouldFollow = isAtLogTop();
  const previousScrollTop = els.logStream ? els.logStream.scrollTop : 0;
  const signature = stableStringify({
    locale: uiLanguage,
    events: logEvents.map(logEventKey),
    dividers: logDividers,
  });
  if (signature === lastLogRenderSignature) return;
  lastLogRenderSignature = signature;
  els.logStream.replaceChildren();
  if (logEvents.length === 0) {
    els.logStream.appendChild(logEntry({
      time: "--:--:--",
      prefix: "SYS",
      message: t("noLogs"),
      detail: t("noLogsDetail"),
      baseClass: "log-type-system",
    }));
    return;
  }
  for (const item of logRenderItems()) {
    if (item.kind === "divider") {
      els.logStream.appendChild(logDivider(item.count));
      continue;
    }
    const event = item.event;
    els.logStream.appendChild(logEntry(normalizeLogEvent(event)));
  }
  els.logStream.scrollTop = shouldFollow ? 0 : previousScrollTop;
  noteSlow("renderLogs", performance.now() - started);
}

function handleLogScroll() {
  if (isAtLogTop() && logRenderPending) {
    logRenderPending = false;
    renderLogs();
    els.logStream.scrollTop = 0;
  }
  if (isAtLogBottom()) loadOlderLogs();
}

function logRenderItems() {
  const dividerMap = new Map(logDividers.map((divider) => [divider.key, divider]));
  const items = [];
  for (const event of logEvents.slice().reverse()) {
    const divider = dividerMap.get(logEventKey(event));
    if (divider) items.push({ kind: "divider", key: "divider|" + divider.key, count: divider.count });
    items.push({ kind: "event", key: logEventKey(event), event });
  }
  return items;
}

function normalizeLogEvent(event) {
  const type = event.type || "event";
  const level = event.level || "info";
  let prefix = "SYS";
  let baseClass = "log-type-system";
  if (level === "error") prefix = "ERR";
  else if (level === "warn") prefix = "WRN";
  else if (type.includes("tool")) {
    prefix = "TOOL";
    baseClass = "log-type-tool";
  } else if (type.includes("request")) {
    prefix = "REQ";
    baseClass = "log-type-request";
  }
  return {
    time: event.ts ? formatTimeOnly(event.ts) : formatTimeOnly(new Date()),
    prefix,
    message: userLogMessage(type, event.message || ""),
    detail: event.detail ? formatLogDetail(type, event.detail) : "",
    baseClass: `${baseClass} log-level-${level}`,
  };
}

function userLogMessage(type, fallback) {
  const key = {
    client_error: "clientError",
    manager_config_saved: "managerConfigSaved",
    manager_restart_requested: "managerRestartRequested",
    manager_start_requested: "managerStartRequested",
    manager_started: "managerStarted",
    manager_stop_requested: "managerStopRequested",
    manager_stopped: "managerStopped",
    context_compaction_completed: "contextCompactionCompleted",
    context_compaction_failed: "contextCompactionFailed",
    context_compaction_started: "contextCompactionStarted",
    context_compacted: "contextCompacted",
    model_alias_applied: "modelAliasApplied",
    process_stderr: "processError",
    process_stdout: "processOutput",
    proxy_start_failed: "proxyStartFailed",
    proxy_started: "proxyStarted",
    proxy_stopped: "proxyStopped",
    request_completed: "requestCompleted",
    request_failed: "requestFailed",
    request_started: "requestStarted",
    tool_call: "toolCall",
    tool_result: "toolResult",
  }[type];
  if (key) {
    const translated = t(key);
    if (translated !== key) return translated;
  }
  const message = String(fallback || "").trim();
  return message || t("runtimeEvent");
}

function logEntry(item) {
  const wrap = document.createElement("div");
  wrap.className = `log-entry ${item.baseClass || ""}`;
  const header = document.createElement("div");
  header.className = "log-header";
  header.innerHTML = `<span class="log-time">${escapeHtml(item.time)}</span><span class="log-badge">${escapeHtml(item.prefix)}</span><span class="log-msg">${escapeHtml(item.message)}</span>`;
  wrap.appendChild(header);
  if (item.detail) {
    const detail = document.createElement("div");
    detail.className = "log-detail selectable";
    detail.textContent = item.detail;
    wrap.appendChild(detail);
  }
  return wrap;
}

function logDivider(count) {
  const wrap = document.createElement("div");
  wrap.className = "log-divider";
  wrap.textContent = t("loadedOlderLogs").replace("{count}", formatNumber(count));
  return wrap;
}

function renderAppInfo(info) {
  const productName = info.product_name || "CodeSeeX";
  const version = info.version || "-";
  appInfo = info;
  document.querySelectorAll("[data-product-name]").forEach((element) => {
    element.textContent = productName;
  });
  document.title = productName;
  els.appProductName.textContent = productName;
  els.appDescription.textContent = info.description || t("productDescription");
  els.appVersion.textContent = "v" + version;
  els.appName.textContent = productName;
  els.aboutVersion.textContent = version;
  els.appLicense.textContent = info.license || t("notDeclared");
}

function renderBalance(data) {
  lastBalanceData = data || null;
  if (!data || !data.ok) {
    const code = data && data.code;
    const message = code === "missing_api_key" ? t("balanceNoApiKey") : t("balanceFailed");
    els.balanceTotal.textContent = "-";
    els.balanceGranted.textContent = "-";
    els.balanceToppedUp.textContent = "-";
    setBalanceStage(message, "error");
    return;
  }

  const totals = sumBalances(data.balance_infos || []);
  const totalStr = formatCurrencyMap(totals.total);
  els.balanceTotal.textContent = totalStr;
  els.balanceGranted.textContent = formatCurrencyMap(totals.granted);
  els.balanceToppedUp.textContent = formatCurrencyMap(totals.toppedUp);
  setBalanceStage(data.is_available ? t("balanceAvailable") : t("balanceUnavailable"), data.is_available ? "done" : "active");
}

function setBalanceStage(text, state) {
  setStageState(els.stageBalanceCheck, els.balanceStatus, {
    done: state === "done",
    active: state === "active",
    error: state === "error",
    text,
  });
}

function setView(viewName) {
  const view = ["console", "usage", "logs", "config", "about"].includes(viewName) ? viewName : "console";
  currentView = view;
  els.workspace.className = "workspace view-" + view;
  els.navItems.forEach((item) => item.classList.toggle("active", item.dataset.view === view));
  const name = view.charAt(0).toUpperCase() + view.slice(1);
  els.pageTitle.textContent = t("view" + name + "Title");
  els.pageSubtitle.textContent = t("view" + name + "Subtitle");
  if (view === "usage") refreshUsage({ force: true }).catch(() => {});
  if (view === "logs") refreshLatestLogs({ force: true }).catch(() => {});
}

function handleAboutAction(action) {
  if (!appInfo) return setAboutStatus(t("appInfoLoading"), true);
  const urls = appInfo.urls || {};
  if (action === "feedback") return openOrExplain(urls.feedback, t("feedbackUnavailable"));
  if (action === "source") return openOrExplain(urls.source, t("sourceUnavailable"));
  if (action === "license") return openOrExplain(urls.license, t("licenseUnavailable"));
  if (action === "update") return handleUpdateCheck();
}

async function handleUpdateCheck() {
  markUpdateNoticeSeen();
  setAboutStatus(t("checkingUpdate"), false);
  const update = await checkForUpdates();
  renderUpdateState();
  return update;
}

async function handleWindowAction(action) {
  if (!["minimize", "maximize", "close"].includes(action)) return;
  try {
    if (isTauriRuntime()) await desktopInvoke("desktop_window_action", { action });
  } catch {}
}

async function openOrExplain(url, fallback) {
  if (!url) return setAboutStatus(fallback, true);
  try {
    await openExternalUrl(url);
    setAboutStatus(t("openExternal"), false);
  } catch (error) {
    window.open(url, "_blank", "noopener");
    setAboutStatus(error && error.message ? error.message : String(error), true);
  }
}

async function openRechargePage() {
  try {
    await openExternalUrl(DEEPSEEK_RECHARGE_URL);
  } catch (error) {
    window.open(DEEPSEEK_RECHARGE_URL, "_blank", "noopener");
    setBalanceStage(error && error.message ? error.message : String(error), "error");
  }
}

async function openExternalUrl(url) {
  if (isTauriRuntime()) {
    await desktopInvoke("desktop_open_external", { url });
  } else {
    window.open(url, "_blank", "noopener");
  }
}

function setAboutStatus(message, warning, options = {}) {
  if (options.html) els.aboutStatus.innerHTML = message;
  else els.aboutStatus.textContent = message;
  els.aboutStatus.classList.toggle("warning", Boolean(warning));
}

function codexConfigPathHint() {
  const platform = [
    navigator.userAgentData && navigator.userAgentData.platform,
    navigator.platform,
    navigator.userAgent,
  ].filter(Boolean).join(" ").toLowerCase();
  return platform.includes("win") ? CODEX_CONFIG_PATH_WINDOWS : CODEX_CONFIG_PATH_UNIX;
}

function handleConfigInput(event) {
  if (!lastSavedConfig) return;
  const nextPayload = buildConfigPayload();
  const next = normalizeConfigPayload(nextPayload);
  if (sameConfigPayload(next, lastSavedConfig)) {
    pendingConfig = null;
    clearAutosaveTimer();
    renderConfigSaveState(restartRequired ? "savedRestart" : "clean");
    return;
  }
  pendingConfig = nextPayload;
  renderConfigSaveState("pending");
  scheduleConfigSave(configAutosaveDelayForEvent(event));
}

function scheduleConfigSave(delay = CONFIG_AUTOSAVE_DELAY_MS) {
  clearAutosaveTimer();
  autosaveTimer = setTimeout(() => {
    autosaveTimer = null;
    saveConfig();
  }, delay);
}

function configAutosaveDelayForEvent(event) {
  if (!event) return CONFIG_AUTOSAVE_DELAY_MS;
  if (event.type === "change" || event.type === "focusout") return CONFIG_AUTOSAVE_DELAY_MS;
  return isTextConfigInput(event.target) ? CONFIG_TEXT_AUTOSAVE_DELAY_MS : CONFIG_AUTOSAVE_DELAY_MS;
}

function isTextConfigInput(target) {
  if (!target || !target.tagName) return false;
  const tag = target.tagName.toLowerCase();
  if (tag === "textarea") return true;
  if (tag !== "input") return false;
  const type = String(target.type || "text").toLowerCase();
  return ["email", "number", "password", "search", "tel", "text", "url"].includes(type);
}

function buildConfigPayload() {
  return {
    ...collectToolConfigPayload(),
    DEEPSEEK_THINKING: getRadioValue("DEEPSEEK_THINKING") || "auto",
    UPSTREAM_MODEL_OVERRIDE: normalizeUpstreamModelOverride(getRadioValue("UPSTREAM_MODEL_OVERRIDE")),
    DEEPSEEK_TEMPERATURE_PRESET: normalizeTemperaturePreset(getRadioValue("DEEPSEEK_TEMPERATURE_PRESET")),
    NETWORK_PROXY_MODE: normalizeNetworkProxyMode(getRadioValue("NETWORK_PROXY_MODE")),
    DEEPSEEK_OFFICIAL_V1_COMPAT: els.deepseekOfficialV1Compat && els.deepseekOfficialV1Compat.checked ? "true" : "false",
    AUTO_START: els.autoStart && els.autoStart.checked ? "true" : "false",
    COMMUNITY_TOOL_CODE_ENABLED: "false",
    SHOW_THINKING: els.showThinking && els.showThinking.checked ? "true" : "false",
    UI_THEME: getRadioValue("UI_THEME") || "system",
    UI_CLOSE_BEHAVIOR: normalizeCloseBehavior(getRadioValue("UI_CLOSE_BEHAVIOR")),
    UI_LANGUAGE: els.uiLanguage ? normalizeConfiguredLanguageId(els.uiLanguage.value) : DEFAULT_LANGUAGE,
    DEEPSEEK_BASE_URL: normalizeDeepSeekBaseUrl(els.deepseekBaseUrl ? els.deepseekBaseUrl.value : ""),
    PROXY_PORT: normalizePort(els.proxyPort ? els.proxyPort.value : "", 8787),
    LOG_RETENTION_DAYS: getRadioValue("LOG_RETENTION_DAYS") || "7",
    BILLING_FLASH_CACHED_INPUT_CNY: normalizeRateInput(els.billingFlashCachedInput ? els.billingFlashCachedInput.value : "", DEFAULT_BILLING_RATES_CNY.flash.cached),
    BILLING_FLASH_CACHE_MISS_INPUT_CNY: normalizeRateInput(els.billingFlashCacheMissInput ? els.billingFlashCacheMissInput.value : "", DEFAULT_BILLING_RATES_CNY.flash.cacheMiss),
    BILLING_FLASH_OUTPUT_CNY: normalizeRateInput(els.billingFlashOutput ? els.billingFlashOutput.value : "", DEFAULT_BILLING_RATES_CNY.flash.output),
    BILLING_PRO_CACHED_INPUT_CNY: normalizeRateInput(els.billingProCachedInput ? els.billingProCachedInput.value : "", DEFAULT_BILLING_RATES_CNY.pro.cached),
    BILLING_PRO_CACHE_MISS_INPUT_CNY: normalizeRateInput(els.billingProCacheMissInput ? els.billingProCacheMissInput.value : "", DEFAULT_BILLING_RATES_CNY.pro.cacheMiss),
    BILLING_PRO_OUTPUT_CNY: normalizeRateInput(els.billingProOutput ? els.billingProOutput.value : "", DEFAULT_BILLING_RATES_CNY.pro.output),
  };
}

function normalizeConfigPayload(payload) {
  const output = {};
  for (const [key, value] of Object.entries(payload || {})) {
    if (Array.isArray(value)) {
      output[key] = key === ENABLED_TOOLS_KEY
        ? stringifyEnabledTools(value)
        : JSON.stringify(value.map((item) => String(item)));
    } else {
      output[key] = String(value);
    }
  }
  return output;
}

function sameConfigPayload(left, right) {
  const leftKeys = Object.keys(left || {}).sort();
  const rightKeys = Object.keys(right || {}).sort();
  if (leftKeys.length !== rightKeys.length) return false;
  for (let index = 0; index < leftKeys.length; index += 1) {
    const key = leftKeys[index];
    if (key !== rightKeys[index]) return false;
    if (String(left[key]) !== String(right[key])) return false;
  }
  return true;
}

function hasRestartRequiredChanges(payload) {
  if (!latestRunning) return false;
  const current = normalizeConfigPayload(payload);
  for (const key of RESTART_REQUIRED_KEYS) {
    if (lastSavedConfig && current[key] !== undefined && current[key] !== lastSavedConfig[key]) return true;
  }
  return false;
}

function hasSavedRestartRequiredChanges() {
  if (!lastSavedConfig || !latestRunning || !latestRuntimePort) return false;
  return String(normalizePort(lastSavedConfig.PROXY_PORT, 8787)) !== String(latestRuntimePort);
}

function clearAutosaveTimer() {
  if (!autosaveTimer) return;
  clearTimeout(autosaveTimer);
  autosaveTimer = null;
}

function renderConfigSaveState(state) {
  const restartState = state === "savedRestart";
  if (els.restartRequiredBadge) els.restartRequiredBadge.hidden = !(restartRequired || restartState);
}

function setBusy(nextBusy, title, detail) {
  busy = Boolean(nextBusy);
  els.loadingOverlay.hidden = !busy;
  if (busy) {
    els.loadingTitle.textContent = title || t("busyTitle");
    els.loadingDetail.textContent = detail || t("busyDetail");
  }
  renderButtons();
}

function applyTheme(value) {
  const theme = value === "light" || value === "dark" ? value : "system";
  if (document.documentElement.dataset.theme === theme) return;
  document.documentElement.classList.add("theme-changing");
  document.documentElement.dataset.theme = theme;
  previewWindowTheme(theme);
  window.setTimeout(() => {
    document.documentElement.classList.remove("theme-changing");
  }, 240);
}

async function previewWindowTheme(theme) {
  try {
    await desktopInvoke("desktop_apply_theme", { theme });
  } catch {}
}

function applyLanguage(value) {
  const previousLanguage = uiLanguage;
  const previousConfiguredLanguage = configuredLanguage;
  const toolValues = collectToolConfigPayload();
  const requested = normalizeConfiguredLanguageId(value);
  const resolved = resolveLanguageId(requested);
  configuredLanguage = requested;
  uiLanguage = resolved;
  if (uiLanguage === previousLanguage && configuredLanguage === previousConfiguredLanguage && document.documentElement.lang === uiLanguage) return;
  document.documentElement.lang = uiLanguage;
  document.querySelectorAll("[data-i18n]").forEach((element) => {
    element.textContent = t(element.dataset.i18n);
  });
  if (els.uiLanguage && els.uiLanguage.value !== configuredLanguage) els.uiLanguage.value = configuredLanguage;
  setView(currentView);
  renderButtons();
  if (lastBalanceData) renderBalance(lastBalanceData);
  lastStatusSignature = "";
  lastUsageSignature = "";
  lastLogRenderSignature = "";
  currentAdapterSignature = "";
  if (latestUsageRuntime) renderUsage(latestUsageRuntime);
  renderCodexAdapter(latestAdapter || {});
  renderUpdateState({ silent: true });
  updateContextMenuLabels();
  if (currentTools.length > 0) {
    currentToolsSignature = "";
    renderTools(currentTools, toolValues);
    applyToolConfigValues(toolValues);
  }
}

function renderLanguageOptions() {
  if (!els.uiLanguage) return;
  const previous = normalizeConfiguredLanguageId(els.uiLanguage.value || configuredLanguage || DEFAULT_LANGUAGE);
  els.uiLanguage.replaceChildren();
  const systemOption = document.createElement("option");
  systemOption.value = SYSTEM_LANGUAGE;
  systemOption.textContent = systemLanguageLabel();
  els.uiLanguage.appendChild(systemOption);
  for (const language of languages) {
    const option = document.createElement("option");
    option.value = language.id;
    option.textContent = language.name;
    els.uiLanguage.appendChild(option);
  }
  els.uiLanguage.value = previous === SYSTEM_LANGUAGE || languages.some((language) => language.id === previous) ? previous : DEFAULT_LANGUAGE;
}

function languageHintsFromManifest(manifest) {
  const hints = [];
  const add = (value) => {
    const normalized = normalizeLocaleId(value);
    if (normalized && !hints.includes(normalized)) hints.push(normalized);
  };
  add(manifest && manifest.system_locale);
  if (Array.isArray(manifest && manifest.system_locales)) manifest.system_locales.forEach(add);
  return hints;
}

function normalizeLanguageId(value) {
  const normalized = String(value || FALLBACK_LANGUAGE).trim().replace(/-/g, "_").toLowerCase();
  return normalized && normalized !== SYSTEM_LANGUAGE ? normalized : FALLBACK_LANGUAGE;
}

function normalizeLocaleId(value) {
  return String(value || "").trim().replace(/-/g, "_").toLowerCase();
}

function normalizeConfiguredLanguageId(value) {
  const normalized = String(value || DEFAULT_LANGUAGE).trim().replace(/-/g, "_").toLowerCase();
  return normalized || DEFAULT_LANGUAGE;
}

function resolveLanguageId(value) {
  const requested = normalizeConfiguredLanguageId(value);
  if (requested !== SYSTEM_LANGUAGE) return normalizeLanguageId(requested);
  const available = languages.map((language) => normalizeLanguageId(language && language.id)).filter(Boolean);
  const availableSet = new Set(available);
  for (const locale of systemLanguageIds()) {
    if (availableSet.has(locale)) return locale;
    const preferred = preferredLanguageForPrefix(locale, availableSet);
    if (preferred) return preferred;
  }
  return availableSet.has(FALLBACK_LANGUAGE) ? FALLBACK_LANGUAGE : (available[0] || FALLBACK_LANGUAGE);
}

function preferredLanguageForPrefix(locale, availableSet) {
  const prefix = String(locale || "").split("_")[0];
  if (!prefix) return "";
  const preferredByPrefix = {
    zh: ["zh_cn", "zh_hans", "zh_tw", "zh_hk"],
    en: ["en_us", "en_gb"],
    ja: ["ja_jp"],
    ko: ["ko_kr"],
    fr: ["fr_fr"],
    de: ["de_de"],
    ru: ["ru_ru"],
  };
  for (const id of preferredByPrefix[prefix] || []) {
    if (availableSet.has(id)) return id;
  }
  return Array.from(availableSet).find((id) => id === prefix || id.startsWith(prefix + "_")) || "";
}

function navigatorLanguageIds() {
  const values = [];
  if (Array.isArray(navigator.languages)) values.push(...navigator.languages);
  values.push(navigator.language || navigator.userLanguage || "");
  return values.map(normalizeLocaleId).filter(Boolean);
}

function systemLanguageIds() {
  const output = [];
  for (const id of systemLanguageHints.concat(navigatorLanguageIds())) {
    const normalized = normalizeLocaleId(id);
    if (!normalized || output.includes(normalized)) continue;
    output.push(normalized);
  }
  return output;
}

function systemLanguageLabel() {
  const resolved = resolveLanguageId(SYSTEM_LANGUAGE);
  const matched = languages.find((language) => normalizeLanguageId(language && language.id) === resolved);
  const label = t("languageSystem");
  return label + (matched && matched.name ? " (" + matched.name + ")" : "");
}

function t(key) {
  return (i18n[uiLanguage] && i18n[uiLanguage][key]) || key;
}

function billingInputs() {
  return [
    els.billingFlashCachedInput,
    els.billingFlashCacheMissInput,
    els.billingFlashOutput,
    els.billingProCachedInput,
    els.billingProCacheMissInput,
    els.billingProOutput,
  ];
}

function setBillingInputValues(config = {}) {
  setInputValue(els.billingFlashCachedInput, config.BILLING_FLASH_CACHED_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.flash.cached);
  setInputValue(els.billingFlashCacheMissInput, config.BILLING_FLASH_CACHE_MISS_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.flash.cacheMiss);
  setInputValue(els.billingFlashOutput, config.BILLING_FLASH_OUTPUT_CNY, DEFAULT_BILLING_RATES_CNY.flash.output);
  setInputValue(els.billingProCachedInput, config.BILLING_PRO_CACHED_INPUT_CNY || config.BILLING_CACHED_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.pro.cached);
  setInputValue(els.billingProCacheMissInput, config.BILLING_PRO_CACHE_MISS_INPUT_CNY || config.BILLING_CACHE_MISS_INPUT_CNY, DEFAULT_BILLING_RATES_CNY.pro.cacheMiss);
  setInputValue(els.billingProOutput, config.BILLING_PRO_OUTPUT_CNY || config.BILLING_OUTPUT_CNY, DEFAULT_BILLING_RATES_CNY.pro.output);
}

function setInputValue(input, value, fallback) {
  if (!input || document.activeElement === input) return;
  input.value = String(normalizeRateInput(value, fallback));
}

function currentBillingSignature() {
  return stableStringify({
    flash: currentBillingRates("deepseek-v4-flash"),
    pro: currentBillingRates("deepseek-v4-pro"),
  });
}

function currentBillingRates(model) {
  const group = String(model || "").toLowerCase().includes("flash") ? "flash" : "pro";
  if (group === "flash") {
    return {
      cached: normalizeRateInput(els.billingFlashCachedInput ? els.billingFlashCachedInput.value : "", DEFAULT_BILLING_RATES_CNY.flash.cached),
      cacheMiss: normalizeRateInput(els.billingFlashCacheMissInput ? els.billingFlashCacheMissInput.value : "", DEFAULT_BILLING_RATES_CNY.flash.cacheMiss),
      output: normalizeRateInput(els.billingFlashOutput ? els.billingFlashOutput.value : "", DEFAULT_BILLING_RATES_CNY.flash.output),
    };
  }
  return {
    cached: normalizeRateInput(els.billingProCachedInput ? els.billingProCachedInput.value : "", DEFAULT_BILLING_RATES_CNY.pro.cached),
    cacheMiss: normalizeRateInput(els.billingProCacheMissInput ? els.billingProCacheMissInput.value : "", DEFAULT_BILLING_RATES_CNY.pro.cacheMiss),
    output: normalizeRateInput(els.billingProOutput ? els.billingProOutput.value : "", DEFAULT_BILLING_RATES_CNY.pro.output),
  };
}

function normalizeRateInput(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function normalizePort(value, fallback = 8787) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return String(fallback);
  return String(Math.min(65535, Math.max(1, Math.floor(parsed))));
}

function normalizeDeepSeekBaseUrl(value) {
  const raw = String(value || "").trim().replace(/\/+$/, "");
  if (!raw) return "";
  try {
    const url = new URL(raw);
    if (url.protocol !== "http:" && url.protocol !== "https:") return "";
    return url.toString().replace(/\/+$/, "");
  } catch {
    return raw;
  }
}

function normalizeRetentionDays(value) {
  const raw = String(value || "7");
  return raw === "1" || raw === "3" || raw === "7" || raw === "30" ? raw : "7";
}

function normalizeUpstreamModelOverride(value) {
  const normalized = String(value || "default").trim().toLowerCase();
  if (normalized === "flash" || normalized === "deepseek-v4-flash") return "deepseek-v4-flash";
  if (normalized === "pro" || normalized === "deepseek-v4-pro") return "deepseek-v4-pro";
  return "default";
}

function normalizeTemperaturePreset(value) {
  const normalized = String(value || DEFAULT_TEMPERATURE_PRESET).trim().toLowerCase();
  if (normalized === "precise" || normalized === "strict" || normalized === "rigorous") return "strict";
  if (normalized === "balanced" || normalized === "balance") return "balanced";
  if (normalized === "general" || normalized === "chat" || normalized === "translation") return "general";
  if (normalized === "creative" || normalized === "creation") return "creative";
  return DEFAULT_TEMPERATURE_PRESET;
}

function normalizeNetworkProxyMode(value) {
  const normalized = String(value || "system").trim().toLowerCase();
  return normalized === "none" || normalized === "no_proxy" || normalized === "direct" ? "none" : "system";
}

function normalizeCloseBehavior(value) {
  return String(value || "exit") === "tray" ? "tray" : "exit";
}

function costForTokens(tokens) {
  const rates = currentBillingRates(tokens && (tokens.model || tokens.requested_model));
  const cached = Number(tokens.cached_input_tokens || tokens.cachedInputTokens || 0);
  const cacheMiss = Number(tokens.cache_miss_input_tokens || tokens.cacheMissInputTokens || 0);
  const output = Number(tokens.output_tokens || tokens.outputTokens || 0);
  return (cached * rates.cached + cacheMiss * rates.cacheMiss + output * rates.output) / 1000000;
}

function sumBalances(infos) {
  const totals = { total: {}, granted: {}, toppedUp: {} };
  for (const item of Array.isArray(infos) ? infos : []) {
    const currency = item && item.currency ? String(item.currency) : "CNY";
    addCurrency(totals.total, currency, item.total_balance);
    addCurrency(totals.granted, currency, item.granted_balance);
    addCurrency(totals.toppedUp, currency, item.topped_up_balance);
  }
  return totals;
}

function addCurrency(target, currency, value) {
  target[currency] = (target[currency] || 0) + (Number(value) || 0);
}

function formatCurrencyMap(values) {
  const entries = Object.entries(values || {});
  if (entries.length === 0) return "-";
  return entries.map(([currency, value]) => currency + " " + formatDecimal(value)).join(" / ");
}

function formatDetail(detail) {
  if (!detail || typeof detail !== "object") return String(detail || "");
  return Object.entries(detail)
    .filter(([, value]) => value !== "" && value !== null && value !== undefined)
    .map(([key, value]) => key + ": " + (typeof value === "object" ? JSON.stringify(value) : String(value)))
    .join("\n");
}

function formatLogDetail(type, detail) {
  if (!detail || typeof detail !== "object") return String(detail || "");
  if (type === "request_started") {
    return [
      detail.endpoint ? t("logApi") + ": " + formatEndpointLabel(detail.endpoint) : "",
      modelDetailLine(detail),
      detail.previous_response_id ? t("logPreviousResponseId") + ": " + compactLogValue(detail.previous_response_id, 80) : "",
    ].filter(Boolean).join("\n");
  }
  if (type === "request_completed") {
    return [
      detail.status !== undefined ? t("logHttp") + ": " + detail.status : "",
      modelDetailLine(detail),
      detail.duration_ms !== undefined ? t("elapsed") + ": " + formatDuration(detail.duration_ms) : "",
      detail.cost_cny !== undefined ? t("cost") + ": " + formatCost(detail.cost_cny) : "",
    ].filter(Boolean).join("\n");
  }
  if (type === "request_failed") {
    return [
      detail.status !== undefined ? t("logHttp") + ": " + detail.status : "",
      modelDetailLine(detail),
      errorDetailLine(detail),
    ].filter(Boolean).join("\n");
  }
  if (type === "tool_call") return toolDetailLines(detail).join("\n");
  if (type === "tool_result") {
    return toolDetailLines(detail).concat([
      detail.ok !== undefined ? t("logStatus") + ": " + (detail.ok ? t("logStatusOk") : t("logStatusFailed")) : "",
      detail.summary ? t("logSummary") + ": " + compactLogValue(detail.summary, 180) : "",
    ]).filter(Boolean).join("\n");
  }
  if (type === "model_alias_applied") {
    return [
      modelDetailLine(detail),
      detail.source ? t("logSource") + ": " + detail.source : "",
    ].filter(Boolean).join("\n");
  }
  if (type === "context_compacted") {
    return [
      detail.mode ? t("mode") + ": " + detail.mode : "",
      detail.estimated_tokens !== undefined ? t("logEstimatedTokens") + ": " + detail.estimated_tokens : "",
      detail.threshold_tokens !== undefined ? t("logThresholdTokens") + ": " + detail.threshold_tokens : "",
    ].filter(Boolean).join("\n");
  }
  return formatUserLevelDetail(detail);
}

function formatEndpointLabel(value) {
  const endpoint = String(value || "").trim();
  if (endpoint === "/v1/responses") return "Responses";
  if (endpoint === "/v1/chat/completions") return "Chat completions";
  return compactLogValue(endpoint, 100);
}

function modelDetailLine(detail) {
  const requested = String(detail && detail.requested_model || "").trim();
  const model = String(detail && detail.model || "").trim();
  if (requested && model && requested !== model) return t("model") + ": " + requested + " -> " + model;
  const value = model || requested;
  return value ? t("model") + ": " + value : "";
}

function toolDetailLines(detail) {
  return [
    detail.name ? t("toolName") + ": " + compactLogValue(detail.name, 80) : "",
    detail.scope ? t("toolScope") + ": " + compactLogValue(detail.scope, 80) : "",
  ].filter(Boolean);
}

function errorDetailLine(detail) {
  const upstream = detail.upstream_error;
  const message = detail.message || detail.error
    || (upstream && (upstream.message || upstream.error || upstream.code || upstream.type));
  return message ? t("logError") + ": " + compactLogValue(message, 220) : "";
}

function formatUserLevelDetail(detail) {
  const allowed = ["endpoint", "status", "model", "requested_model", "action", "mode", "path", "base_url", "host", "port", "error", "message"];
  return allowed
    .map((key) => detail[key] !== undefined && detail[key] !== null && detail[key] !== "" ? logDetailLabel(key) + ": " + compactLogValue(detail[key], 180) : "")
    .filter(Boolean)
    .join("\n");
}

function logDetailLabel(key) {
  const labelKey = {
    endpoint: "logEndpoint",
    status: "logStatus",
    model: "model",
    requested_model: "logRequestedModel",
    action: "logAction",
    mode: "mode",
    path: "logPath",
    base_url: "logBaseUrl",
    host: "logHost",
    port: "logPort",
    error: "logError",
    message: "logMessage",
  }[key];
  return labelKey ? t(labelKey) : key;
}

function compactLogValue(value, limit) {
  const text = typeof value === "object" ? JSON.stringify(value) : String(value || "");
  const cleaned = text.replace(/\s+/g, " ").trim();
  const max = Math.max(20, Number(limit) || 160);
  return cleaned.length > max ? cleaned.slice(0, max - 1) + "..." : cleaned;
}

function mergeEvents(events) {
  const seen = new Set();
  const output = [];
  for (const event of events) {
    if (!event || !event.ts) continue;
    const key = logEventKey(event);
    if (seen.has(key)) continue;
    seen.add(key);
    output.push(event);
  }
  return output.sort((left, right) => String(left.ts).localeCompare(String(right.ts)));
}

function eventsAfterNewestLog(events) {
  const newest = newestLogTs();
  const existingKeys = new Set(logEvents.map(logEventKey));
  return events.filter((event) => event && event.ts && String(event.ts) > newest && !existingKeys.has(logEventKey(event)));
}

function logEventKey(event) {
  return [event.ts, event.type || "", event.message || "", JSON.stringify(event.detail || null)].join("|");
}

function pruneLogDividers() {
  const eventKeys = new Set(logEvents.map(logEventKey));
  logDividers = logDividers.filter((divider) => eventKeys.has(divider.key));
}

function oldestLogTs() {
  return logEvents.length > 0 ? logEvents[0].ts : null;
}

function newestLogTs() {
  return logEvents.length > 0 ? String(logEvents[logEvents.length - 1].ts || "") : "";
}

function isAtLogTop() {
  if (!els.logStream) return true;
  return els.logStream.scrollTop <= 2;
}

function isAtLogBottom() {
  if (!els.logStream) return false;
  const gap = els.logStream.scrollHeight - els.logStream.scrollTop - els.logStream.clientHeight;
  return gap <= LOG_BOTTOM_LOAD_THRESHOLD;
}
