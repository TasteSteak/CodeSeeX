const LOG_INITIAL_PAGE_SIZE = 30;
const LOG_OLDER_PAGE_SIZE = 15;
const LOG_MAX_ITEMS = 1000;
const LOG_RENDER_WINDOW = 120;
const LOG_RENDER_BUFFER = 30;
const LOG_ESTIMATED_ROW_HEIGHT = 58;
const LOG_BOTTOM_LOAD_THRESHOLD = 80;
const CONFIG_AUTOSAVE_DELAY_MS = 450;
const REFRESH_RUNNING_MS = 2000;
const REFRESH_IDLE_MS = 5000;
const REFRESH_HIDDEN_MS = 10000;
const SLOW_RENDER_MS = 80;
const LANGUAGE_LOAD_TIMEOUT_MS = 1200;
const CONFIG_CHANGED_EVENT = "codeseex-config-changed";
const RUNTIME_STATUS_STARTING = "starting";
const ENABLED_TOOLS_KEY = "ENABLED_TOOLS";
const UPDATE_NOTICE_STORAGE_KEY = "version";
const RESTART_REQUIRED_KEYS = new Set([
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
  balanceAvailability: byId("balanceAvailability"),
  balanceGranted: byId("balanceGranted"),
  balanceStatus: byId("balanceStatus"),
  balanceToppedUp: byId("balanceToppedUp"),
  balanceTotal: byId("balanceTotal"),
  billingCachedInput: byId("BILLING_CACHED_INPUT_CNY"),
  billingCacheMissInput: byId("BILLING_CACHE_MISS_INPUT_CNY"),
  billingOutput: byId("BILLING_OUTPUT_CNY"),
  completedTurns: byId("completedTurns"),
  autoStart: byId("AUTO_START"),
  configTomlCode: byId("configTomlCode"),
  configTomlCopyStatus: byId("configTomlCopyStatus"),
  configTomlStatus: byId("configTomlStatus"),
  copyTomlButton: byId("copyTomlButton"),
  failedTurns: byId("failedTurns"),
  lastCompletedAt: byId("lastCompletedAt"),
  lastTurnCard: byId("lastTurnCard"),
  loadingDetail: byId("loadingDetail"),
  loadingOverlay: byId("loadingOverlay"),
  loadingTitle: byId("loadingTitle"),
  logStream: byId("logStream"),
  navItems: Array.from(document.querySelectorAll(".nav-item[data-view]")),
  pageSubtitle: byId("pageSubtitle"),
  pageTitle: byId("pageTitle"),
  pid: byId("pid"),
  pidLabel: byId("pidLabel"),
  deepseekBaseUrl: byId("DEEPSEEK_BASE_URL"),
  proxyPort: byId("PROXY_PORT"),
  refreshBalanceButton: byId("refreshBalanceButton"),
  restartButton: byId("restartButton"),
  restartRequiredBadge: byId("restartRequiredBadge"),
  running: byId("running"),
  showThinking: byId("SHOW_THINKING"),
  startButton: byId("startButton"),
  statusPill: byId("statusPill"),
  stopButton: byId("stopButton"),
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
let logDividers = [];
let logEvents = [];
let logHasMore = false;
let logLoadingOlder = false;
let logRenderState = { signature: "", start: 0, end: 0, scrollTop: 0 };
let lastBalanceData = null;
let lastStatusSignature = "";
let lastUsageSignature = "";
let lastLogRenderSignature = "";
let lastTurnSignature = "";
let latestAdapter = null;
let latestUpdateCheck = null;
let latestConfigVersion = "";
let externalConfigSyncTimer = null;
let themePreviewSeq = 0;
let uiLanguage = FALLBACK_LANGUAGE;

init();

function byId(id) {
  return document.getElementById(id);
}

async function init() {
  const config = await loadConfig({ render: false }).catch(() => ({}));
  configuredLanguage = normalizeConfiguredLanguageId(config.UI_LANGUAGE || DEFAULT_LANGUAGE);
  i18n = await loadI18n(configuredLanguage);
  bind();
  applyLanguage(configuredLanguage);
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
    const manifestResponse = await fetch("/api/languages", { cache: "no-store" });
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
  if (els.copyTomlButton) els.copyTomlButton.addEventListener("click", copyConfigToml);
  if (els.logStream) els.logStream.addEventListener("scroll", handleLogScroll);
  document.querySelector(".windowbar")?.addEventListener("contextmenu", (event) => event.preventDefault());
  document.addEventListener("visibilitychange", () => scheduleNextRefresh(0));
  window.addEventListener(CONFIG_CHANGED_EVENT, () => scheduleExternalConfigSync());
  if (els.toolConfigList) {
    els.toolConfigList.addEventListener("input", handleConfigInput);
    els.toolConfigList.addEventListener("change", handleConfigInput);
  }

  [els.showThinking, els.autoStart, els.uiLanguage, els.deepseekBaseUrl, els.proxyPort, els.billingCachedInput, els.billingCacheMissInput, els.billingOutput].forEach((input) => {
    if (!input) return;
    input.addEventListener("input", handleConfigInput);
    input.addEventListener("change", handleConfigInput);
  });

  onRadioChange("CONFIG_TAB", setConfigTab);
  onRadioChange("CATALOG_MODE", handleConfigInput);
  onRadioChange("UPSTREAM_MODEL_OVERRIDE", handleConfigInput);
  onRadioChange("DEEPSEEK_THINKING", handleConfigInput);
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

  document.querySelectorAll("[data-window-action]").forEach((button) => {
    button.addEventListener("click", () => handleWindowAction(button.dataset.windowAction));
  });

  document.addEventListener("dragstart", (event) => event.preventDefault());
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
    await fetch(url, { method: "POST" });
    if (url === "/api/restart") {
      restartRequired = hasSavedRestartRequiredChanges();
      renderConfigSaveState(pendingConfig ? "pending" : "clean");
    }
    await delay(450);
    await refresh({ forceLogs: true, force: true });
  } finally {
    setBusy(false);
  }
}

async function saveConfig() {
  if (busy || configSaving || !pendingConfig) return;
  configSaving = true;
  renderConfigSaveState("saving");
  const payload = pendingConfig;
  try {
    const response = await fetch("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    if (!response.ok) throw new Error("Config save failed");
    const status = await response.json().catch(() => null);
    const needsRestart = hasRestartRequiredChanges(payload);
    lastSavedConfig = normalizeConfigPayload(payload);
    pendingConfig = null;
    if (needsRestart) restartRequired = true;
    if (status && status.config_version) latestConfigVersion = String(status.config_version);
    renderConfigSaveState(restartRequired ? "savedRestart" : "saved");
    configSaving = false;
    await loadConfig();
    await loadCodexAdapter().catch(() => {});
    if (toolsLoaded) await loadTools();
    await refresh({ forceLogs: true, force: true });
  } catch (error) {
    renderConfigSaveState("error");
  } finally {
    configSaving = false;
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
    const data = await (await fetch("/api/status", { cache: "no-store" })).json();
    await syncConfigIfChanged(data.config_version);
    renderStatus(data);
    renderUsage(data.runtime || {});
    updateLatestLogs(data.events || [], { force: Boolean(options.forceLogs) });
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
      detail: null,
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
}

async function loadConfig(options = {}) {
  const started = performance.now();
  const config = await (await fetch("/api/config", { cache: "no-store" })).json();
  if (config && config.config_version) latestConfigVersion = String(config.config_version);
  if (options.render !== false) renderConfig(config || {});
  noteSlow("loadConfig", performance.now() - started);
  return config;
}

async function loadTools() {
  const started = performance.now();
  const data = await (await fetch("/api/tools", { cache: "no-store" })).json();
  const config = lastSavedConfig || {};
  renderTools(data.tools || [], config);
  toolsLoaded = true;
  noteSlow("loadTools", performance.now() - started);
  return data.tools || [];
}

async function loadCodexAdapter() {
  const data = await (await fetch("/api/codex-adapter", { cache: "no-store" })).json();
  renderCodexAdapter(data || {});
  return data || {};
}

async function checkForUpdates(options = {}) {
  try {
    latestUpdateCheck = await (await fetch("/api/update-check", { cache: "no-store" })).json();
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
    const manifest = await fetch("/api/languages", { cache: "no-store" }).then((response) => response.ok ? response.json() : null).catch(() => null);
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
    appInfo = await (await fetch("/api/app-info", { cache: "no-store" })).json();
    renderAppInfo(appInfo);
  } catch (error) {
    appInfo = null;
    setAboutStatus((error.message || String(error)), true);
  }
}

async function refreshBalance() {
  if (els.refreshBalanceButton) els.refreshBalanceButton.disabled = true;
  if (els.balanceStatus) els.balanceStatus.textContent = t("balanceLoading");
  try {
    const response = await fetch("/api/deepseek/balance", { cache: "no-store" });
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
    const data = await (await fetch(url, { cache: "no-store" })).json();
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
  latestStarting = isStarting;
  els.statusPill.classList.toggle("running", latestRunning);
  els.statusPill.classList.toggle("starting", latestStarting);
  els.running.textContent = latestRunning ? t("running") : (latestStarting ? t("starting") : t("stopped"));
  els.pidLabel.textContent = data.process_label || (data.process_mode === "inline" ? t("appPid") : t("proxyPid"));
  els.pid.textContent = data.pid || "-";
  els.activeRequests.textContent = formatNumber(runtime.active_requests || 0);
  els.completedTurns.textContent = formatNumber(runtime.request_count || 0);
  els.failedTurns.textContent = formatNumber(runtime.failed_request_count || 0);
  els.lastCompletedAt.textContent = formatDateTime(runtime.last_request_at);
  renderLastTurn(runtime.last_turn || null);
  renderButtons();
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
  const textInputs = [els.deepseekBaseUrl, els.proxyPort, els.billingCachedInput, els.billingCacheMissInput, els.billingOutput];
  if (textInputs.includes(active)) return;
  const configSignature = stableStringify(normalizeConfigPayload(config));
  if (configSignature === currentConfigSignature && lastSavedConfig) return;
  currentConfigSignature = configSignature;

  setRadioValue("DEEPSEEK_THINKING", config.DEEPSEEK_THINKING || "auto");
  setRadioValue("CATALOG_MODE", normalizeCatalogMode(config.CATALOG_MODE));
  setRadioValue("UPSTREAM_MODEL_OVERRIDE", normalizeUpstreamModelOverride(config.UPSTREAM_MODEL_OVERRIDE));
  setRadioValue("LOG_RETENTION_DAYS", normalizeRetentionDays(config.LOG_RETENTION_DAYS));
  setRadioValue("UI_CLOSE_BEHAVIOR", normalizeCloseBehavior(config.UI_CLOSE_BEHAVIOR));
  const nextTheme = config.UI_THEME || "system";
  setRadioValue("UI_THEME", nextTheme);
  els.showThinking.checked = !/^(0|false|no|off|disabled)$/i.test(String(config.SHOW_THINKING || "true"));
  if (els.autoStart) els.autoStart.checked = isTruthy(config.AUTO_START || "false");
  if (els.deepseekBaseUrl && document.activeElement !== els.deepseekBaseUrl) els.deepseekBaseUrl.value = normalizeDeepSeekBaseUrl(config.DEEPSEEK_BASE_URL || "");
  if (document.activeElement !== els.proxyPort) els.proxyPort.value = normalizePort(config.PROXY_PORT || "8787");
  const nextLanguage = normalizeConfiguredLanguageId(config.UI_LANGUAGE || DEFAULT_LANGUAGE);
  if (document.activeElement !== els.uiLanguage) els.uiLanguage.value = nextLanguage;
  els.billingCachedInput.value = config.BILLING_CACHED_INPUT_CNY || "0.025";
  els.billingCacheMissInput.value = config.BILLING_CACHE_MISS_INPUT_CNY || "3";
  els.billingOutput.value = config.BILLING_OUTPUT_CNY || "6";
  currentAdapterSignature = "";
  applyTheme(nextTheme);
  if (resolveLanguageId(nextLanguage) !== uiLanguage || nextLanguage !== configuredLanguage) applyLanguage(nextLanguage);
  lastSavedConfig = normalizeConfigPayload(buildConfigPayload());
  lastUsageSignature = "";
  lastTurnSignature = "";
  if (!restartRequired) renderConfigSaveState("clean");
  renderCodexAdapter(latestAdapter || {});
}

function renderCodexAdapter(adapter) {
  latestAdapter = adapter || {};
  const signature = stableStringify({
    adapter: latestAdapter,
    model: normalizeUpstreamModelOverride(getRadioValue("UPSTREAM_MODEL_OVERRIDE") || (lastSavedConfig && lastSavedConfig.UPSTREAM_MODEL_OVERRIDE)),
    catalogMode: normalizeCatalogMode(getRadioValue("CATALOG_MODE") || (lastSavedConfig && lastSavedConfig.CATALOG_MODE)),
  });
  if (signature === currentAdapterSignature) return;
  currentAdapterSignature = signature;
  const toml = String(latestAdapter.toml_snippet || "");
  if (els.configTomlCode) els.configTomlCode.textContent = toml || "-";
  if (els.configTomlStatus) {
    if (latestAdapter.ready) {
      els.configTomlStatus.textContent = [
        t("codexAdapterReady"),
        latestAdapter.catalog_mode ? t("catalogMode") + ": " + catalogModeLabel(latestAdapter.catalog_mode) : "",
        latestAdapter.catalog_path ? latestAdapter.catalog_path : "",
      ].filter(Boolean).join(" · ");
    } else {
      els.configTomlStatus.textContent = latestAdapter.error || t("codexAdapterMissing");
    }
  }
}

function catalogModeLabel(value) {
  const mode = normalizeCatalogMode(value);
  if (mode === "auto") return t("catalogModeAuto");
  if (mode === "builtin") return t("catalogModeBuiltin");
  return t("catalogModeDefault");
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

function escapeHtml(value) {
  return String(value || "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function renderTools(tools, config) {
  const started = performance.now();
  const nextTools = Array.isArray(tools) ? tools : [];
  const signature = JSON.stringify(nextTools.map((tool) => ({
    id: tool.id,
    name: tool.name,
    description: tool.description,
    icon: tool.icon,
    iconPath: tool.iconPath,
    systemTool: isSystemTool(tool),
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
  title.textContent = tool.name || tool.id || "Tool";
  titleRow.appendChild(title);
  for (const label of normalizeToolLabels(tool.labels)) titleRow.appendChild(renderToolLabel(label));
  const description = document.createElement("div");
  description.className = "tool-card-description";
  description.textContent = tool.description || "";
  titleWrap.appendChild(titleRow);
  if (description.textContent) titleWrap.appendChild(description);

  header.appendChild(icon);
  header.appendChild(titleWrap);
  if (!systemTool) header.appendChild(renderToolEnableSwitch(tool));
  card.appendChild(header);

  const body = document.createElement("div");
  body.className = "tool-card-body";
  const fields = systemTool ? [] : (Array.isArray(tool.config) ? tool.config : []);
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
  const id = String(tool && tool.id || "");
  return id === "apply_patch" || id === "web_search";
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
  const node = document.createElement("span");
  node.className = "tool-label";
  node.dataset.labelId = label.id;
  node.textContent = label.label;
  return node;
}

function renderToolField(field) {
  const item = document.createElement("div");
  item.className = "setting-item";

  const labelWrap = document.createElement("span");
  const label = document.createElement("span");
  label.textContent = translateToolText(field.labelKey, field.label || field.key);
  labelWrap.appendChild(label);
  const helpText = translateToolText(field.descriptionKey, field.description || "");
  if (helpText) {
    const small = document.createElement("small");
    small.className = "muted";
    small.textContent = helpText;
    labelWrap.appendChild(document.createElement("br"));
    labelWrap.appendChild(small);
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
    label.textContent = translateToolText(option.labelKey, option.label || option.value);
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
    el.textContent = translateToolText(option.labelKey, option.label || option.value);
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
    if (isSystemTool(tool)) continue;
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
  currentConfigTab = ["client", "proxy", "tools"].includes(value) ? value : "client";
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
  const totalCostVal = costForTokens({
    cached_input_tokens: totalCached,
    cache_miss_input_tokens: totalMiss,
    output_tokens: runtime.total_output_tokens || 0,
  });

  els.usageTotalTurns.textContent = formatNumber(totalTurnsCount);
  els.usageCacheHitRate.textContent = cacheHitRate;
  els.usageAverageMs.textContent = formatDuration(avgMs);
  els.usageTotalCost.textContent = formatCost(totalCostVal);
  renderUsageRows(turns);
  noteSlow("renderUsage", performance.now() - started);
}

function renderLastTurn(turn) {
  const signature = stableStringify({ locale: uiLanguage, turn: turn || null });
  if (signature === lastTurnSignature) return;
  lastTurnSignature = signature;
  els.lastTurnCard.replaceChildren();
  if (!turn) {
    els.lastTurnCard.appendChild(infoRow(t("noTurn"), "-"));
    return;
  }
  els.lastTurnCard.appendChild(infoRow(t("completedAt"), formatDateTime(turn.completed_at)));
  els.lastTurnCard.appendChild(infoRow(t("cacheHit"), formatNumber(turn.cached_input_tokens)));
  els.lastTurnCard.appendChild(infoRow(t("cacheMiss"), formatNumber(turn.cache_miss_input_tokens)));
  els.lastTurnCard.appendChild(infoRow(t("output"), formatNumber(turn.output_tokens)));
  els.lastTurnCard.appendChild(infoRow(t("total"), formatNumber(turn.total_tokens)));
  els.lastTurnCard.appendChild(infoRow(t("elapsed"), formatDuration(turn.request_ms)));
  els.lastTurnCard.appendChild(infoRow(t("cost"), formatCost(costForTokens(turn))));
}

function renderUsageRows(turns) {
  els.usageRows.replaceChildren();
  for (const turn of turns.slice().reverse()) {
    const row = document.createElement("tr");
    [
      formatDateTime(turn.completed_at),
      usageModelLabel(turn),
      formatNumber(turn.cached_input_tokens),
      formatNumber(turn.cache_miss_input_tokens),
      formatNumber(turn.output_tokens),
      formatNumber(turn.total_tokens),
      formatDuration(turn.request_ms),
      formatCost(costForTokens(turn)),
    ].forEach((value, index) => {
      const cell = document.createElement("td");
      cell.textContent = value;
      if (index === 1) cell.title = value;
      row.appendChild(cell);
    });
    els.usageRows.appendChild(row);
  }
  if (turns.length === 0) {
    const row = document.createElement("tr");
    row.innerHTML = `<td colspan="8" style="text-align: center; color: var(--text-muted)">${escapeHtml(t("noRows"))}</td>`;
    els.usageRows.appendChild(row);
  }
}

function usageModelLabel(turn) {
  const model = String(turn && turn.model || "").trim();
  const requested = String(turn && turn.requested_model || "").trim();
  return model || requested || "-";
}

function updateLatestLogs(events, options = {}) {
  const next = Array.isArray(events) ? events : [];
  const shouldFollow = options.force || logEvents.length === 0 || isAtLogTop();
  const nextEvents = logEvents.length === 0 ? next.slice(-LOG_INITIAL_PAGE_SIZE) : eventsAfterNewestLog(next);
  if (!options.force && nextEvents.length === 0) {
    logHasMore = next.length >= LOG_INITIAL_PAGE_SIZE || logHasMore;
    return;
  }
  if (shouldFollow) {
    logEvents = mergeEvents(logEvents.concat(nextEvents)).slice(-LOG_MAX_ITEMS);
    logHasMore = next.length >= LOG_INITIAL_PAGE_SIZE || logHasMore;
    pruneLogDividers();
    renderLogs();
    els.logStream.scrollTop = 0;
  } else {
    logEvents = mergeEvents(logEvents.concat(nextEvents)).slice(-LOG_MAX_ITEMS);
    logHasMore = next.length >= LOG_INITIAL_PAGE_SIZE || logHasMore;
    pruneLogDividers();
  }
}

function renderLogs() {
  const started = performance.now();
  const shouldFollow = isAtLogTop();
  const previousScrollTop = els.logStream ? els.logStream.scrollTop : 0;
  const windowRange = logWindowRange();
  const signature = stableStringify({
    locale: uiLanguage,
    events: logEvents.map(logEventKey),
    dividers: logDividers,
    range: windowRange,
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
  const rendered = logRenderItems().slice(windowRange.start, windowRange.end);
  els.logStream.appendChild(logSpacer(windowRange.topSpacer));
  for (const item of rendered) {
    if (item.kind === "divider") {
      els.logStream.appendChild(logDivider(item.count));
      continue;
    }
    const event = item.event;
    els.logStream.appendChild(logEntry(normalizeLogEvent(event)));
  }
  els.logStream.appendChild(logSpacer(windowRange.bottomSpacer));
  logRenderState = {
    signature,
    start: windowRange.start,
    end: windowRange.end,
    scrollTop: els.logStream.scrollTop,
  };
  els.logStream.scrollTop = shouldFollow ? 0 : previousScrollTop;
  logRenderState.scrollTop = els.logStream.scrollTop;
  noteSlow("renderLogs", performance.now() - started);
}

function handleLogScroll() {
  const nextRange = logWindowRange();
  if (nextRange.start !== logRenderState.start || nextRange.end !== logRenderState.end) renderLogs();
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

function logWindowRange() {
  const total = logRenderItems().length;
  const visibleCount = Math.max(LOG_RENDER_WINDOW, Math.ceil((els.logStream ? els.logStream.clientHeight : 0) / LOG_ESTIMATED_ROW_HEIGHT) + LOG_RENDER_BUFFER * 2);
  const firstVisible = Math.max(0, Math.floor((els.logStream ? els.logStream.scrollTop : 0) / LOG_ESTIMATED_ROW_HEIGHT) - LOG_RENDER_BUFFER);
  const start = Math.min(Math.max(0, firstVisible), Math.max(0, total - visibleCount));
  const end = Math.min(total, start + visibleCount);
  return {
    start,
    end,
    topSpacer: start * LOG_ESTIMATED_ROW_HEIGHT,
    bottomSpacer: Math.max(0, total - end) * LOG_ESTIMATED_ROW_HEIGHT,
  };
}

function logSpacer(height) {
  const spacer = document.createElement("div");
  spacer.className = "log-spacer";
  spacer.style.height = Math.max(0, Number(height) || 0) + "px";
  spacer.setAttribute("aria-hidden", "true");
  return spacer;
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
    model_alias_applied: "modelAliasApplied",
    process_stderr: "processError",
    process_stdout: "processOutput",
    proxy_started: "proxyStarted",
    proxy_stopped: "proxyStopped",
    request_completed: "requestCompleted",
    request_failed: "requestFailed",
    request_started: "requestStarted",
    tool_call: "toolCall",
    tool_result: "toolResult",
  }[type];
  if (key) return t(key);
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
  document.querySelectorAll("[data-product-name]").forEach((node) => {
    node.textContent = productName;
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
    els.balanceAvailability.textContent = code === "missing_api_key" ? t("balanceNoApiKey") : t("balanceUnavailable");
    els.balanceTotal.textContent = "-";
    els.balanceGranted.textContent = "-";
    els.balanceToppedUp.textContent = "-";
    els.balanceStatus.textContent = message;
    return;
  }

  const totals = sumBalances(data.balance_infos || []);
  const totalStr = formatCurrencyMap(totals.total);
  els.balanceAvailability.textContent = data.is_available ? t("balanceAvailable") : t("balanceUnavailable");
  els.balanceTotal.textContent = totalStr;
  els.balanceGranted.textContent = formatCurrencyMap(totals.granted);
  els.balanceToppedUp.textContent = formatCurrencyMap(totals.toppedUp);
  els.balanceStatus.textContent = t("balanceUpdated");
}

function setView(viewName) {
  const view = ["console", "usage", "logs", "config", "about"].includes(viewName) ? viewName : "console";
  currentView = view;
  els.workspace.className = "workspace view-" + view;
  els.navItems.forEach((item) => item.classList.toggle("active", item.dataset.view === view));
  const name = view.charAt(0).toUpperCase() + view.slice(1);
  els.pageTitle.textContent = t("view" + name + "Title");
  els.pageSubtitle.textContent = t("view" + name + "Subtitle");
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
    await fetch("/api/window/" + action, { method: "POST" });
  } catch {}
}

function openOrExplain(url, fallback) {
  if (!url) return setAboutStatus(fallback, true);
  window.open(url, "_blank", "noopener");
  setAboutStatus(t("openExternal"), false);
}

function setAboutStatus(message, warning, options = {}) {
  if (options.html) els.aboutStatus.innerHTML = message;
  else els.aboutStatus.textContent = message;
  els.aboutStatus.classList.toggle("warning", Boolean(warning));
}

function handleConfigInput() {
  if (!lastSavedConfig) return;
  const next = normalizeConfigPayload(buildConfigPayload());
  if (sameConfigPayload(next, lastSavedConfig)) {
    pendingConfig = null;
    clearAutosaveTimer();
    renderConfigSaveState(restartRequired ? "savedRestart" : "clean");
    return;
  }
  pendingConfig = next;
  renderConfigSaveState("pending");
  clearAutosaveTimer();
  autosaveTimer = setTimeout(() => {
    autosaveTimer = null;
    saveConfig();
  }, CONFIG_AUTOSAVE_DELAY_MS);
}

function buildConfigPayload() {
  return {
    ...collectToolConfigPayload(),
    DEEPSEEK_THINKING: getRadioValue("DEEPSEEK_THINKING") || "auto",
    CATALOG_MODE: normalizeCatalogMode(getRadioValue("CATALOG_MODE")),
    UPSTREAM_MODEL_OVERRIDE: normalizeUpstreamModelOverride(getRadioValue("UPSTREAM_MODEL_OVERRIDE")),
    AUTO_START: els.autoStart && els.autoStart.checked ? "true" : "false",
    COMMUNITY_TOOL_CODE_ENABLED: "false",
    SHOW_THINKING: els.showThinking && els.showThinking.checked ? "true" : "false",
    UI_THEME: getRadioValue("UI_THEME") || "system",
    UI_CLOSE_BEHAVIOR: normalizeCloseBehavior(getRadioValue("UI_CLOSE_BEHAVIOR")),
    UI_LANGUAGE: els.uiLanguage ? normalizeConfiguredLanguageId(els.uiLanguage.value) : DEFAULT_LANGUAGE,
    DEEPSEEK_BASE_URL: normalizeDeepSeekBaseUrl(els.deepseekBaseUrl ? els.deepseekBaseUrl.value : ""),
    PROXY_PORT: normalizePort(els.proxyPort ? els.proxyPort.value : "", 8787),
    LOG_RETENTION_DAYS: getRadioValue("LOG_RETENTION_DAYS") || "7",
    BILLING_CACHED_INPUT_CNY: normalizeRateInput(els.billingCachedInput ? els.billingCachedInput.value : "", 0.025),
    BILLING_CACHE_MISS_INPUT_CNY: normalizeRateInput(els.billingCacheMissInput ? els.billingCacheMissInput.value : "", 3),
    BILLING_OUTPUT_CNY: normalizeRateInput(els.billingOutput ? els.billingOutput.value : "", 6),
  };
}

function normalizeConfigPayload(payload) {
  const output = {};
  for (const [key, value] of Object.entries(payload || {})) {
    output[key] = String(value);
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
  const current = normalizeConfigPayload(payload);
  for (const key of RESTART_REQUIRED_KEYS) {
    if (lastSavedConfig && current[key] !== undefined && current[key] !== lastSavedConfig[key]) return true;
  }
  return false;
}

function hasSavedRestartRequiredChanges() {
  if (!lastSavedConfig) return false;
  const currentPort = normalizePort(location.port || "", 8787);
  return lastSavedConfig.PROXY_PORT !== undefined && String(lastSavedConfig.PROXY_PORT) !== String(currentPort);
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
  const seq = ++themePreviewSeq;
  try {
    await fetch("/api/window/theme", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ theme, seq }),
    });
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
  document.querySelectorAll("[data-i18n]").forEach((node) => {
    node.textContent = t(node.dataset.i18n);
  });
  if (els.uiLanguage && els.uiLanguage.value !== configuredLanguage) els.uiLanguage.value = configuredLanguage;
  setView(currentView);
  renderButtons();
  if (lastBalanceData) renderBalance(lastBalanceData);
  lastStatusSignature = "";
  lastUsageSignature = "";
  lastLogRenderSignature = "";
  lastTurnSignature = "";
  currentAdapterSignature = "";
  renderCodexAdapter(latestAdapter || {});
  renderUpdateState({ silent: true });
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

function infoRow(label, value) {
  const row = document.createElement("div");
  row.className = "info-row";
  const left = document.createElement("span");
  const right = document.createElement("strong");
  left.textContent = label;
  right.textContent = value;
  row.appendChild(left);
  row.appendChild(right);
  return row;
}

function currentBillingRates() {
  return {
    cached: normalizeRateInput(els.billingCachedInput ? els.billingCachedInput.value : "", 0.025),
    cacheMiss: normalizeRateInput(els.billingCacheMissInput ? els.billingCacheMissInput.value : "", 3),
    output: normalizeRateInput(els.billingOutput ? els.billingOutput.value : "", 6),
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

function normalizeCloseBehavior(value) {
  return String(value || "exit") === "tray" ? "tray" : "exit";
}

function costForTokens(tokens) {
  const rates = currentBillingRates();
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
  if (type === "request_completed") {
    return [
      detail.duration_ms !== undefined ? t("elapsed") + ": " + formatDuration(detail.duration_ms) : "",
      detail.cost_cny !== undefined ? t("cost") + ": " + formatCost(detail.cost_cny) : "",
    ].filter(Boolean).join("\n");
  }
  if (type === "tool_call" || type === "tool_result") {
    return [
      detail.name ? t("toolName") + ": " + detail.name : "",
      detail.scope ? t("toolScope") + ": " + detail.scope : "",
      detail.call_id ? "call_id: " + detail.call_id : "",
      detail.bytes !== undefined ? "bytes: " + detail.bytes : "",
    ].filter(Boolean).join("\n");
  }
  if (type === "model_alias_applied") {
    return [
      detail.requested_model ? "requested_model: " + detail.requested_model : "",
      detail.model ? t("model") + ": " + detail.model : "",
      detail.source ? "source: " + detail.source : "",
    ].filter(Boolean).join("\n");
  }
  return formatDetail(detail);
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

function escapeHtml(value) {
  return String(value || "").replace(/[&<>"']/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;" }[ch]));
}

function formatCost(value) {
  const amount = Number(value) || 0;
  return "CNY " + amount.toFixed(4);
}

function formatDecimal(value) {
  const amount = Number(value) || 0;
  return amount.toFixed(Math.abs(amount) < 0.01 && amount !== 0 ? 6 : 2);
}

function formatNumber(value) {
  try {
    return new Intl.NumberFormat(formatLocaleId(uiLanguage)).format(Number(value) || 0);
  } catch {
    return String(Number(value) || 0);
  }
}

function formatDateTime(value) {
  return value && !Number.isNaN(new Date(value).getTime()) ? new Date(value).toLocaleString(formatLocaleId(uiLanguage), { hour12: false }) : "-";
}

function formatTimeOnly(value) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "--:--:--" : date.toTimeString().split(" ")[0];
}

function formatDuration(value) {
  const ms = Number(value);
  if (!Number.isFinite(ms) || ms <= 0) return "-";
  return ms < 1000 ? Math.round(ms) + " ms" : (ms / 1000).toFixed(ms < 10000 ? 1 : 0) + " s";
}

function average(values) {
  return values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : 0;
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function formatLocaleId(value) {
  return normalizeLanguageId(value).replace(/_/g, "-");
}

function stableStringify(value) {
  try {
    return JSON.stringify(value || null);
  } catch {
    return String(value || "");
  }
}

function noteSlow(label, durationMs) {
  if (!isDevDiagnosticsEnabled()) return;
  if (!Number.isFinite(durationMs) || durationMs < SLOW_RENDER_MS) return;
  console.debug("[CodeSeeX perf] " + label + " took " + Math.round(durationMs) + "ms");
}

function isDevDiagnosticsEnabled() {
  try {
    return location.search.includes("debug=1") || localStorage.getItem("codeseex_perf_debug") === "1";
  } catch {
    return false;
  }
}
