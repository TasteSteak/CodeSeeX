const LOG_INITIAL_PAGE_SIZE = 60;
const LOG_OLDER_PAGE_SIZE = 30;
const LOG_RENDER_WINDOW_SIZE = 60;
const LOG_MEMORY_MAX_ITEMS = 500;
const LOG_BOTTOM_LOAD_THRESHOLD = 80;
const CONFIG_AUTOSAVE_DELAY_MS = 450;
const CONFIG_TEXT_AUTOSAVE_DELAY_MS = 2500;
const CONFIG_AUTOSAVE_RETRY_MS = 700;
const SENSITIVE_CONFIG_INPUT_IDS = new Set(["DEEPSEEK_BASE_URL", "PROXY_PORT"]);
const DEBUG_MANAGER_BASE_URL = "http://127.0.0.1:8787";
const DEEPSEEK_RECHARGE_URL = "https://platform.deepseek.com/top_up";
const CCS_IMPORT_URL = "ccswitch://v1/import";
const DEFAULT_CCS_ENDPOINT = "http://127.0.0.1:8787/v1";
const DEFAULT_CCS_MODEL = "deepseek-v4-pro";
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
  aboutVersionMeta: byId("aboutVersionMeta"),
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
  configSaveStatus: byId("configSaveStatus"),
  configTomlCopyStatus: byId("configTomlCopyStatus"),
  configTomlStatus: byId("configTomlStatus"),
  copyTomlButton: byId("copyTomlButton"),
  importCcsButton: byId("importCcsButton"),
  ccsApiKeyInput: byId("ccsApiKeyInput"),
  ccsKeyCancel: byId("ccsKeyCancel"),
  ccsKeyConfirm: byId("ccsKeyConfirm"),
  ccsKeyModal: byId("ccsKeyModal"),
  failedTurns: byId("failedTurns"),
  loadingDetail: byId("loadingDetail"),
  loadingOverlay: byId("loadingOverlay"),
  loadingTitle: byId("loadingTitle"),
  logStream: byId("logStream"),
  logCategoryFilter: byId("logCategoryFilter"),
  logLevelFilter: byId("logLevelFilter"),
  logRequestFilter: byId("logRequestFilter"),
  logSearchInput: byId("logSearchInput"),
  logFollowToggle: byId("logFollowToggle"),
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
  troubleshootActions: byId("troubleshootActions"),
  troubleshootButton: byId("troubleshootButton"),
  troubleshootClose: byId("troubleshootClose"),
  troubleshootModal: byId("troubleshootModal"),
  troubleshootRefresh: byId("troubleshootRefresh"),
  troubleshootSummary: byId("troubleshootSummary"),
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
let logRenderedKeys = new Map();
let logWindowStart = null;
let logNextCursor = null;
let logLatestCursor = null;
let logLatestEventRevision = null;
let logFilterTimer = null;
let logFilters = { audience: "safe", category: "all", level: "all", request_id: "", q: "" };
let logAutoFollow = true;
let logRefreshController = null;
let logRefreshSequence = 0;
let logRenderFrame = null;
let logRenderFrameOptions = null;
let latestLogsLoadedOnce = false;
let latestLogsRefreshInFlight = false;
let lastBalanceData = null;
let lastStatusSignature = "";
let lastUsageSignature = "";
let latestUsageRuntime = null;
let usageSessionDomById = new Map();
let usageSessionDetailCache = new Map();
let usageOpenSessionOrder = [];
let usageLatestRevision = null;
let usageNextCursor = null;
let usageHasMore = false;
let usageRefreshInFlight = false;
let usageRefreshQueued = false;
let usageRefreshController = null;
let usageRefreshSequence = 0;
let usageRenderFrame = null;
let usageRenderRuntime = null;
let lastUsageSourceSignature = "";
let lastLogRenderSignature = "";
let latestAdapter = null;
let latestStatus = null;
let latestUpdateCheck = null;
let latestConfigVersion = "";
let externalConfigSyncTimer = null;
let configTomlStatusTimer = null;
let ccsKeyResolve = null;
let uiLanguage = FALLBACK_LANGUAGE;
let contextMenuEl = null;
let contextMenuTarget = null;
let usageTraceTooltipEl = null;
let toolConfigControlCache = new Map();
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
    const [fallbackPack, pack] = await Promise.all([
      languageId === FALLBACK_LANGUAGE ? Promise.resolve(null) : fetchLanguagePack(FALLBACK_LANGUAGE),
      fetchLanguagePack(languageId),
    ]);
    uiLanguage = languageId;
    configuredLanguage = normalizeConfiguredLanguageId(targetLanguage);
    i18n = Object.assign(
      {},
      fallbackPack ? { [FALLBACK_LANGUAGE]: fallbackPack } : {},
      pack ? { [languageId]: pack } : {},
    );
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
  if (els.importCcsButton) els.importCcsButton.addEventListener("click", importConfigToCcs);
  if (els.troubleshootButton) els.troubleshootButton.addEventListener("click", openTroubleshootModal);
  if (els.troubleshootClose) els.troubleshootClose.addEventListener("click", closeTroubleshootModal);
  if (els.troubleshootRefresh) els.troubleshootRefresh.addEventListener("click", refreshTroubleshootModal);
  if (els.ccsKeyCancel) els.ccsKeyCancel.addEventListener("click", () => closeCcsKeyModal(""));
  if (els.ccsKeyConfirm) els.ccsKeyConfirm.addEventListener("click", confirmCcsKeyModal);
  if (els.ccsApiKeyInput) {
    els.ccsApiKeyInput.addEventListener("input", updateCcsKeyConfirmState);
    els.ccsApiKeyInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter" && !els.ccsKeyConfirm.disabled) confirmCcsKeyModal();
      if (event.key === "Escape") closeCcsKeyModal("");
    });
  }
  if (els.logStream) els.logStream.addEventListener("scroll", handleLogScroll);
  if (els.logCategoryFilter) els.logCategoryFilter.addEventListener("change", handleLogFilterChange);
  if (els.logLevelFilter) els.logLevelFilter.addEventListener("change", handleLogFilterChange);
  if (els.logRequestFilter) els.logRequestFilter.addEventListener("input", scheduleLogFilterChange);
  if (els.logSearchInput) els.logSearchInput.addEventListener("input", scheduleLogFilterChange);
  if (els.logFollowToggle) {
    els.logFollowToggle.addEventListener("change", () => {
      logAutoFollow = Boolean(els.logFollowToggle.checked);
      if (logAutoFollow && logRenderPending) {
        logRenderPending = false;
        scheduleRenderLogs({ followTop: true });
      }
    });
  }
  document.addEventListener("contextmenu", handleContextMenu);
  document.addEventListener("click", hideContextMenu);
  document.addEventListener("scroll", () => {
    hideContextMenu();
    hideUsageTraceTooltip();
  }, true);
  window.addEventListener("resize", hideUsageTraceTooltip);
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && els.ccsKeyModal && !els.ccsKeyModal.hidden) closeCcsKeyModal("");
    if (event.key === "Escape" && els.troubleshootModal && !els.troubleshootModal.hidden) closeTroubleshootModal();
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
    if (SENSITIVE_CONFIG_INPUT_IDS.has(input.id)) {
      input.addEventListener("keydown", (event) => {
        if (event.key !== "Enter") return;
        event.preventDefault();
        handleConfigInput({ type: "change", target: input });
        input.blur();
      });
    }
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
    if (!response.ok) {
      const errorDetail = await response.text().catch(() => "");
      const error = new Error(configSaveErrorMessage(response.status, errorDetail));
      error.status = response.status;
      error.detail = errorDetail;
      throw error;
    }
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
    renderConfigSaveState("error", error && error.message ? error.message : "");
  } finally {
    configSaving = false;
    if (saveCompleted && pendingConfig) scheduleConfigSave(CONFIG_AUTOSAVE_RETRY_MS);
  }
}

function configSaveErrorMessage(status, detail) {
  let parsed = null;
  try {
    parsed = detail ? JSON.parse(detail) : null;
  } catch {}
  const code = parsed && (parsed.code || parsed.error);
  if (status === 409 || code === "config_version_conflict") {
    return parsed && parsed.message ? parsed.message : t("configSaveConflict");
  }
  return parsed && (parsed.message || parsed.error)
    ? String(parsed.message || parsed.error)
    : t("configSaveError");
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
    const eventRevision = data.runtime && data.runtime.event_revision !== undefined && data.runtime.event_revision !== null
      ? Number(data.runtime.event_revision)
      : null;
    if (Array.isArray(data.events)) {
      updateLatestLogs(data.events, {
        force: Boolean(options.forceLogs),
        hasMore: data.has_more,
        latestCursor: data.latest_cursor,
        eventRevision,
      });
    } else if (options.forceLogs || currentView === "logs") {
      if (options.forceLogs
        || !latestLogsLoadedOnce
        || eventRevision === null
        || logLatestEventRevision === null
        || eventRevision !== logLatestEventRevision) {
        await refreshLatestLogs({
          force: Boolean(options.forceLogs),
          eventRevision,
        });
      }
    }
    maybeRefreshUsage(data.runtime || {}, options);
  } catch (error) {
    latestStatus = {
      ok: false,
      runtime: {},
      error: error && error.message ? error.message : String(error || ""),
    };
    latestRunning = false;
    latestStarting = false;
    latestRuntimePort = null;
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
  if (document.hidden && !options.force) return;
  const activeRequests = Number(runtime.active_requests || 0);
  const usageRevision = runtime.usage_revision === undefined || runtime.usage_revision === null
    ? null
    : Number(runtime.usage_revision);
  if (!options.force
    && activeRequests <= 0
    && usageRevision !== null
    && usageLatestRevision !== null
    && usageRevision === usageLatestRevision) {
    return;
  }
  const sourceSignature = stableStringify({
    usage_revision: usageRevision,
    active_requests: activeRequests,
    request_count: runtime.request_count || 0,
    billable_request_count: runtime.billable_request_count || 0,
    last_request_at: runtime.last_request_at || "",
    last_activity_at: runtime.last_activity_at || "",
    billing: currentBillingSignature(),
  });
  if (!options.force && activeRequests <= 0 && latestUsageRuntime && sourceSignature === lastUsageSourceSignature) return;
  lastUsageSourceSignature = sourceSignature;
  refreshUsage({ force: Boolean(options.force || activeRequests > 0) }).catch(() => {});
}

async function refreshUsage(options = {}) {
  if (document.hidden && !options.force) return;
  if (usageRefreshInFlight) {
    if (options.force) usageRefreshQueued = true;
    return;
  }
  usageRefreshInFlight = true;
  if (usageRefreshController && typeof usageRefreshController.abort === "function") {
    usageRefreshController.abort();
  }
  usageRefreshController = typeof AbortController === "function" ? new AbortController() : null;
  const sequence = ++usageRefreshSequence;
  const started = performance.now();
  try {
    const data = await apiJson(usageUrl(options), {
      cache: "no-store",
      signal: usageRefreshController ? usageRefreshController.signal : undefined,
    });
    if (sequence !== usageRefreshSequence) return;
    latestUsageRuntime = data.runtime || {};
    if (latestUsageRuntime.unchanged) return;
    usageLatestRevision = latestUsageRuntime.usage_revision === undefined || latestUsageRuntime.usage_revision === null
      ? usageLatestRevision
      : Number(latestUsageRuntime.usage_revision);
    usageNextCursor = latestUsageRuntime.next_cursor || usageNextCursor;
    usageHasMore = Boolean(latestUsageRuntime.has_more);
    scheduleRenderUsage(latestUsageRuntime);
  } catch (error) {
    if (error && (error.name === "AbortError" || String(error.message || "").includes("aborted"))) return;
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
  if (document.hidden && !options.force) return;
  if (currentView !== "logs" && !options.force) return;
  if (!options.force
    && options.eventRevision !== undefined
    && options.eventRevision !== null
    && logLatestEventRevision !== null
    && Number(options.eventRevision) === logLatestEventRevision) {
    return;
  }
  if (logRefreshController && typeof logRefreshController.abort === "function") {
    logRefreshController.abort();
  }
  logRefreshController = typeof AbortController === "function" ? new AbortController() : null;
  const sequence = ++logRefreshSequence;
  latestLogsRefreshInFlight = true;
  try {
    if (options.reset) resetLogState();
    const after = !options.force && !options.reset && logLatestCursor ? logLatestCursor : null;
    const data = await apiJson(logEventsUrl(LOG_INITIAL_PAGE_SIZE, null, { after }), {
      cache: "no-store",
      signal: logRefreshController ? logRefreshController.signal : undefined,
    });
    if (sequence !== logRefreshSequence) return;
    latestLogsLoadedOnce = true;
    if (data.event_revision !== undefined && data.event_revision !== null) {
      logLatestEventRevision = Number(data.event_revision);
    } else if (options.eventRevision !== undefined && options.eventRevision !== null) {
      logLatestEventRevision = Number(options.eventRevision);
    }
    updateLatestLogs(Array.isArray(data.events) ? data.events : [], {
      force: Boolean(options.force),
      hasMore: after ? undefined : data.has_more,
      nextCursor: data.next_cursor,
      latestCursor: data.latest_cursor,
      eventRevision: data.event_revision,
      incremental: Boolean(after),
    });
  } catch (error) {
    if (error && (error.name === "AbortError" || String(error.message || "").includes("aborted"))) return;
    updateLatestLogs([{
      ts: new Date().toISOString(),
      type: "client_error",
      level: "error",
      message: error.message || String(error),
      detail: clientErrorDetail("/api/events", error),
    }], { force: true, nextCursor: null });
  } finally {
    if (sequence === logRefreshSequence) latestLogsRefreshInFlight = false;
  }
}

function usageUrl(options = {}) {
  const params = new URLSearchParams();
  params.set("limit", String(options.limit || 60));
  if (options.cursor) params.set("cursor", options.cursor);
  if (!options.force && usageLatestRevision !== null && !options.cursor) {
    params.set("since_revision", String(usageLatestRevision));
  }
  return "/api/usage?" + params.toString();
}

function logEventsUrl(limit, cursor, options = {}) {
  const params = new URLSearchParams();
  params.set("limit", String(limit || LOG_INITIAL_PAGE_SIZE));
  params.set("audience", logFilters.audience || "safe");
  if (logFilters.category && logFilters.category !== "all") params.set("category", logFilters.category);
  if (logFilters.level && logFilters.level !== "all") params.set("level", logFilters.level);
  if (logFilters.request_id) params.set("request_id", logFilters.request_id);
  if (logFilters.q) params.set("q", logFilters.q);
  if (options.after) params.set("after", options.after);
  else if (cursor) params.set("cursor", cursor);
  return "/api/events?" + params.toString();
}

function readLogFiltersFromUi() {
  return {
    audience: "safe",
    category: els.logCategoryFilter ? els.logCategoryFilter.value || "all" : "all",
    level: els.logLevelFilter ? els.logLevelFilter.value || "all" : "all",
    request_id: els.logRequestFilter ? String(els.logRequestFilter.value || "").trim() : "",
    q: els.logSearchInput ? String(els.logSearchInput.value || "").trim() : "",
  };
}

function scheduleLogFilterChange() {
  if (logFilterTimer) clearTimeout(logFilterTimer);
  logFilterTimer = setTimeout(() => {
    logFilterTimer = null;
    handleLogFilterChange();
  }, 220);
}

function handleLogFilterChange() {
  logFilters = readLogFiltersFromUi();
  refreshLatestLogs({ force: true, reset: true }).catch(() => {});
}

function resetLogState() {
  logEvents = [];
  logDividers = [];
  logHasMore = false;
  logLoadingOlder = false;
  logRenderPending = false;
  logWindowStart = null;
  logNextCursor = null;
  logLatestCursor = null;
  logLatestEventRevision = null;
  logRenderFrameOptions = null;
  lastLogRenderSignature = "";
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
  if (logLoadingOlder) return;
  if (pageLogWindowOlder()) {
    renderLogs({ preserveAnchor: true });
    return;
  }
  if (!logHasMore) return;
  const cursor = oldestLogCursor();
  if (!cursor) return;
  logLoadingOlder = true;
  try {
    const url = logEventsUrl(LOG_OLDER_PAGE_SIZE, cursor);
    const data = await apiJson(url, { cache: "no-store" });
    const older = Array.isArray(data.events) ? data.events : [];
    const existingKeys = new Set(logEvents.map(logEventKey));
    const addedOlder = older.filter((event) => event && event.ts && !existingKeys.has(logEventKey(event)));
    logHasMore = Boolean(data.has_more);
    logNextCursor = data.next_cursor || logNextCursor;
    if (addedOlder.length > 0) {
      const newestLoaded = addedOlder[addedOlder.length - 1];
      logDividers.push({ key: logEventKey(newestLoaded), count: addedOlder.length });
    }
    logEvents = trimLogMemory(mergeEvents(older.concat(logEvents)));
    if (addedOlder.length > 0) logWindowStart = 0;
    pruneLogDividers();
    renderLogs({ preserveAnchor: true });
  } finally {
    logLoadingOlder = false;
  }
}

function renderStatus(data) {
  const runtime = data.runtime || {};
  const runtimeStatus = String(data.runtime_status || runtime.status || "").toLowerCase();
  const isStarting = !data.running && runtimeStatus === RUNTIME_STATUS_STARTING;
  const isStopping = !data.running && runtimeStatus === RUNTIME_STATUS_STOPPING;
  latestStatus = data || null;
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
  if (els.troubleshootModal && !els.troubleshootModal.hidden) renderTroubleshootModal();
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
  renderConfigToml(toml || "-");
  if (els.configTomlStatus) els.configTomlStatus.textContent = codexConfigPathHint();
}

async function copyConfigToml() {
  const text = configTomlCopyText(els.configTomlCode ? els.configTomlCode.textContent : "");
  if (!text || text === "-") {
    setConfigTomlActionStatus(t("codexAdapterMissing"), { warning: true, timeout: 2200 });
    return;
  }
  try {
    await navigator.clipboard.writeText(text);
    setConfigTomlActionStatus(t("copied"));
  } catch {
    setConfigTomlActionStatus(t("copyFailed"), { warning: true, timeout: 2200 });
  }
}

async function importConfigToCcs() {
  const toml = configTomlCopyText(els.configTomlCode ? els.configTomlCode.textContent : "");
  if (!toml || toml === "-") {
    setConfigTomlActionStatus(t("codexAdapterMissing"), { warning: true, timeout: 2200 });
    return;
  }
  const apiKey = await requestCcsApiKey();
  if (!apiKey) return;
  try {
    await openExternalUrl(ccsImportUrl(toml, { apiKey }));
    setConfigTomlActionStatus(t("ccsImportStarted"), { timeout: 2200 });
  } catch {
    setConfigTomlActionStatus(t("ccsImportFailed"), { warning: true, timeout: 2600 });
  }
}

function requestCcsApiKey() {
  if (!els.ccsKeyModal || !els.ccsApiKeyInput) return Promise.resolve("");
  if (ccsKeyResolve) closeCcsKeyModal("");
  els.ccsApiKeyInput.value = "";
  updateCcsKeyConfirmState();
  els.ccsKeyModal.hidden = false;
  window.setTimeout(() => els.ccsApiKeyInput.focus(), 0);
  return new Promise((resolve) => {
    ccsKeyResolve = resolve;
  });
}

function confirmCcsKeyModal() {
  const value = String(els.ccsApiKeyInput ? els.ccsApiKeyInput.value : "").trim();
  if (!value) return;
  closeCcsKeyModal(value);
}

function closeCcsKeyModal(value) {
  if (els.ccsKeyModal) els.ccsKeyModal.hidden = true;
  if (els.ccsApiKeyInput) els.ccsApiKeyInput.value = "";
  updateCcsKeyConfirmState();
  const resolve = ccsKeyResolve;
  ccsKeyResolve = null;
  if (resolve) resolve(String(value || "").trim());
}

function updateCcsKeyConfirmState() {
  if (!els.ccsKeyConfirm || !els.ccsApiKeyInput) return;
  els.ccsKeyConfirm.disabled = !String(els.ccsApiKeyInput.value || "").trim();
}

function openTroubleshootModal() {
  if (!els.troubleshootModal) return;
  renderTroubleshootModal();
  els.troubleshootModal.hidden = false;
  window.setTimeout(() => {
    if (els.troubleshootClose) els.troubleshootClose.focus();
  }, 0);
}

function closeTroubleshootModal() {
  if (els.troubleshootModal) els.troubleshootModal.hidden = true;
}

async function refreshTroubleshootModal() {
  if (els.troubleshootRefresh) els.troubleshootRefresh.disabled = true;
  try {
    const data = await apiJson("/api/status", { cache: "no-store" });
    await syncConfigIfChanged(data.config_version);
    renderStatus(data);
    await Promise.allSettled([
      refreshLatestLogs({ force: true }),
      loadCodexAdapter(),
      currentView === "usage" ? refreshUsage({ force: true }) : Promise.resolve(),
    ]);
  } catch (error) {
    latestStatus = {
      ok: false,
      runtime: {},
      error: error && error.message ? error.message : String(error || ""),
    };
    latestRunning = false;
    latestStarting = false;
    latestRuntimePort = null;
    els.running.textContent = t("unavailable");
    els.statusPill.classList.remove("running");
    els.statusPill.classList.remove("starting");
    renderButtons();
  } finally {
    if (els.troubleshootRefresh) els.troubleshootRefresh.disabled = false;
    renderTroubleshootModal();
  }
}

function renderTroubleshootModal() {
  if (!els.troubleshootSummary || !els.troubleshootActions) return;
  const status = latestStatus || {};
  const runtime = status.runtime || {};
  const hasStatus = Boolean(latestStatus && latestStatus.ok !== undefined);
  const statusOk = hasStatus && status.ok !== false;
  const port = statusOk
    ? (runtime.port || latestRuntimePort || (lastSavedConfig && lastSavedConfig.PROXY_PORT) || "8787")
    : ((lastSavedConfig && lastSavedConfig.PROXY_PORT) || "8787");
  const rows = [
    [t("troubleshootProxyState"), latestRunning ? t("running") : (latestStarting ? t("starting") : t("stopped"))],
    [t("logPort"), String(port)],
    [statusOk ? (status.process_label || t("proxyPid")) : t("proxyPid"), statusOk && status.pid ? String(status.pid) : "-"],
    [t("configTomlTitle"), codexConfigPathHint()],
    [t("troubleshootCatalogPath"), statusOk ? (status.catalog_path || (latestAdapter && latestAdapter.catalog_path) || "-") : "-"],
    [t("troubleshootBaseUrl"), statusOk ? (status.base_url || (latestAdapter && latestAdapter.base_url) || DEFAULT_CCS_ENDPOINT) : "-"],
  ];
  els.troubleshootSummary.replaceChildren(...rows.map(([label, value]) => troubleshootRow(label, value)));

  const actions = troubleshootActions(status, runtime);
  els.troubleshootActions.replaceChildren(...actions.map((text) => {
    const item = document.createElement("div");
    item.className = "troubleshoot-action";
    item.textContent = text;
    return item;
  }));
}

function troubleshootRow(label, value) {
  const row = document.createElement("div");
  row.className = "troubleshoot-row";
  const span = document.createElement("span");
  span.textContent = label;
  const strong = document.createElement("strong");
  strong.textContent = value || "-";
  row.append(span, strong);
  return row;
}

function troubleshootActions(status, runtime) {
  const actions = [];
  const activeRequests = Number(runtime && runtime.active_requests || 0);
  if (latestStarting) actions.push(t("troubleshootActionWaitStartup"));
  if (!latestRunning && !latestStarting) actions.push(t("troubleshootActionStartProxy"));
  if (latestRunning && activeRequests > 0) actions.push(t("troubleshootActionActiveRequests").replace("{count}", formatNumber(activeRequests)));
  if (latestRunning && hasSavedRestartRequiredChanges()) actions.push(t("troubleshootActionRestartNeeded"));
  if (!latestRunning) actions.push(t("troubleshootActionPortConflict"));
  if (!status || !status.ok) actions.push(t("troubleshootActionRefreshStatus"));
  if (actions.length === 0) actions.push(t("troubleshootActionHealthy"));
  return actions;
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

function renderConfigToml(toml) {
  if (!els.configTomlCode) return;
  const text = String(toml || "-");
  if (!text || text === "-") {
    els.configTomlCode.textContent = "-";
    return;
  }
  els.configTomlCode.innerHTML = highlightToml(text);
}

function highlightToml(text) {
  return String(text || "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .split("\n")
    .map(highlightTomlLine)
    .join("\n");
}

function highlightTomlLine(line) {
  const raw = String(line || "");
  const trimmed = raw.trim();
  if (!trimmed) return "";
  if (trimmed.startsWith("#")) return `<span class="toml-comment">${escapeHtml(raw)}</span>`;

  const section = raw.match(/^(\s*)(\[[^\]]+\])(\s*)$/);
  if (section) {
    return `${escapeHtml(section[1])}<span class="toml-section">${escapeHtml(section[2])}</span>${escapeHtml(section[3])}`;
  }

  const keyValue = raw.match(/^(\s*)([A-Za-z0-9_.-]+)(\s*=\s*)(.*)$/);
  if (!keyValue) return escapeHtml(raw);
  return `${escapeHtml(keyValue[1])}<span class="toml-key">${escapeHtml(keyValue[2])}</span>${escapeHtml(keyValue[3])}${highlightTomlValue(keyValue[4])}`;
}

function highlightTomlValue(value) {
  const raw = String(value || "");
  const stringValue = raw.match(/^("(?:\\.|[^"])*")(\s*)$/);
  if (stringValue) {
    return `<span class="toml-string">${escapeHtml(stringValue[1])}</span>${escapeHtml(stringValue[2])}`;
  }
  if (/^(true|false)\s*$/i.test(raw)) return `<span class="toml-bool">${escapeHtml(raw)}</span>`;
  return escapeHtml(raw);
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    "\"": "&quot;",
    "'": "&#39;",
  })[ch]);
}

function ccsImportUrl(toml, options = {}) {
  const apiKey = String(options.apiKey || "").trim();
  const params = new URLSearchParams({
    resource: "provider",
    app: "codex",
    name: appInfo && appInfo.product_name ? appInfo.product_name : "CodeSeeX",
    endpoint: parseTomlStringValue(toml, "base_url") || DEFAULT_CCS_ENDPOINT,
    model: parseTomlStringValue(toml, "model") || DEFAULT_CCS_MODEL,
    config: utf8Base64(toml),
    configFormat: "toml",
  });
  if (apiKey) params.set("apiKey", apiKey);
  return CCS_IMPORT_URL + "?" + params.toString();
}

function parseTomlStringValue(toml, key) {
  const escapedKey = String(key || "").replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = String(toml || "").match(new RegExp("^\\s*" + escapedKey + "\\s*=\\s*\"((?:\\\\.|[^\"])*)\"\\s*$", "m"));
  return match ? unescapeTomlBasicString(match[1]).trim() : "";
}

function unescapeTomlBasicString(value) {
  return String(value || "").replace(/\\([btnfr"\\])/g, (_, ch) => {
    if (ch === "b") return "\b";
    if (ch === "t") return "\t";
    if (ch === "n") return "\n";
    if (ch === "f") return "\f";
    if (ch === "r") return "\r";
    return ch;
  });
}

function utf8Base64(value) {
  const bytes = new TextEncoder().encode(String(value || ""));
  let binary = "";
  const chunkSize = 0x8000;
  for (let index = 0; index < bytes.length; index += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunkSize));
  }
  return btoa(binary);
}

function setConfigTomlActionStatus(message, options = {}) {
  if (!els.configTomlCopyStatus) return;
  if (configTomlStatusTimer) {
    window.clearTimeout(configTomlStatusTimer);
    configTomlStatusTimer = null;
  }
  els.configTomlCopyStatus.textContent = message || "";
  els.configTomlCopyStatus.classList.toggle("warning", Boolean(options.warning));
  const timeout = Number(options.timeout === undefined ? 1800 : options.timeout);
  if (timeout > 0) {
    configTomlStatusTimer = window.setTimeout(() => {
      configTomlStatusTimer = null;
      if (!els.configTomlCopyStatus) return;
      els.configTomlCopyStatus.textContent = "";
      els.configTomlCopyStatus.classList.remove("warning");
    }, timeout);
  }
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
      configured: Boolean(field.configured),
      options: (field.options || []).map((option) => option.value),
    })),
  })));
  currentTools = nextTools;
  if (!els.toolConfigList) return;
  if (signature !== currentToolsSignature) {
    currentToolsSignature = signature;
    els.toolConfigList.replaceChildren(...nextTools.map(renderToolCard));
    rebuildToolConfigControlCache();
  } else if (toolConfigControlCache.size === 0) {
    rebuildToolConfigControlCache();
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
  } else if (field.type === "password") {
    item.appendChild(renderPasswordField(field));
  } else {
    const input = document.createElement("input");
    input.className = "inline-control";
    input.name = field.key;
    input.type = field.type === "number" ? "number" : "text";
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

function renderPasswordField(field) {
  const wrap = document.createElement("div");
  wrap.className = "tool-secret-field";
  const input = document.createElement("input");
  input.className = "inline-control";
  input.name = field.key;
  input.type = "password";
  input.value = "";
  input.placeholder = translateToolText(field.placeholderKey, field.placeholder || "");
  input.autocomplete = "new-password";
  wrap.appendChild(input);
  if (field.configured) {
    const status = document.createElement("small");
    status.className = "muted tool-secret-status";
    status.textContent = t("secretConfigured");
    wrap.appendChild(status);

    const clearLabel = document.createElement("label");
    clearLabel.className = "tool-secret-clear";
    const clear = document.createElement("input");
    clear.type = "checkbox";
    clear.name = field.key + "_CLEAR";
    const clearText = document.createElement("span");
    clearText.textContent = t("clearSavedSecret");
    clearLabel.append(clear, clearText);
    wrap.appendChild(clearLabel);
  }
  return wrap;
}

function rebuildToolConfigControlCache() {
  toolConfigControlCache = new Map();
  if (!els.toolConfigList) return;
  els.toolConfigList.querySelectorAll("[name]").forEach((element) => {
    const name = String(element.name || "").trim();
    if (!name) return;
    if (name === ENABLED_TOOLS_KEY && element.dataset.toolId) {
      toolConfigControlCache.set(`enabled:${normalizeToolId(element.dataset.toolId)}`, element);
    } else if (element.type === "radio") {
      toolConfigControlCache.set(`radio:${name}:${String(element.value || "")}`, element);
    } else if (!toolConfigControlCache.has(`field:${name}`)) {
      toolConfigControlCache.set(`field:${name}`, element);
    }
  });
}

function toolEnabledInput(id) {
  return toolConfigControlCache.get(`enabled:${normalizeToolId(id)}`) || null;
}

function toolFieldInput(key) {
  return toolConfigControlCache.get(`field:${String(key || "")}`) || null;
}

function setToolRadioValue(name, value) {
  const input = toolConfigControlCache.get(`radio:${String(name || "")}:${String(value || "")}`);
  if (input) input.checked = true;
}

function getToolRadioValue(name) {
  const prefix = `radio:${String(name || "")}:`;
  for (const [key, input] of toolConfigControlCache.entries()) {
    if (key.startsWith(prefix) && input.checked) return input.value;
  }
  return "";
}

function applyToolConfigValues(config) {
  const values = config || {};
  const enabledTools = parseEnabledTools(values[ENABLED_TOOLS_KEY], currentTools);
  for (const tool of currentTools) {
    if (isSystemTool(tool)) continue;
    const id = normalizeToolId(tool && tool.id);
    const input = toolEnabledInput(id);
    if (input) input.checked = enabledTools.includes(id);
  }
  for (const field of toolConfigFields()) {
    const value = values[field.key] !== undefined ? String(values[field.key]) : String(field.defaultValue || "");
    if (field.type === "segmented") setToolRadioValue(field.key, value);
    else if (field.type === "boolean") {
      const input = toolFieldInput(field.key);
      if (input) input.checked = isTruthy(value);
    }
    else {
      const input = toolFieldInput(field.key);
      if (input && document.activeElement !== input) input.value = value;
    }
    const clearInput = toolFieldInput(field.key + "_CLEAR");
    if (clearInput) clearInput.checked = false;
  }
}

function collectToolConfigPayload() {
  const payload = {};
  if (!toolsLoaded || currentTools.length === 0) return payload;
  const enabledTools = [];
  for (const tool of currentTools) {
    if (isSystemTool(tool)) continue;
    const id = normalizeToolId(tool && tool.id);
    const input = toolEnabledInput(id);
    if (input && input.checked) enabledTools.push(id);
  }
  payload[ENABLED_TOOLS_KEY] = stringifyEnabledTools(enabledTools);
  for (const field of toolConfigFields()) {
    if (!field.key) continue;
    if (field.type === "segmented") payload[field.key] = getToolRadioValue(field.key) || field.defaultValue || "";
    else if (field.type === "boolean") {
      const input = toolFieldInput(field.key);
      payload[field.key] = input && input.checked ? "true" : "false";
    }
    else {
      const input = toolFieldInput(field.key);
      payload[field.key] = input ? input.value : field.defaultValue || "";
      const clearInput = toolFieldInput(field.key + "_CLEAR");
      if (clearInput && clearInput.checked) payload[field.key + "_CLEAR"] = "true";
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

function scheduleRenderUsage(runtime) {
  usageRenderRuntime = runtime || {};
  if (typeof requestAnimationFrame !== "function") {
    renderUsage(usageRenderRuntime);
    usageRenderRuntime = null;
    return;
  }
  if (usageRenderFrame !== null) return;
  usageRenderFrame = requestAnimationFrame(() => {
    usageRenderFrame = null;
    const nextRuntime = usageRenderRuntime || {};
    usageRenderRuntime = null;
    renderUsage(nextRuntime);
  });
}

function renderUsage(runtime) {
  const started = performance.now();
  const billable = Array.isArray(runtime.billable_history) ? runtime.billable_history : [];
  const fallbackTurns = billable.length ? billable : (Array.isArray(runtime.turn_history) ? runtime.turn_history : []);
  const sessions = Array.isArray(runtime.usage_sessions)
    ? runtime.usage_sessions
    : usageSessionsFromTurns(fallbackTurns);
  const usageSignature = [
    uiLanguage,
    currentBillingSignature(),
    runtime.usage_revision || "",
    runtime.last_activity_at || "",
    runtime.total_cached_input_tokens || 0,
    runtime.total_cache_miss_input_tokens || 0,
    runtime.total_output_tokens || 0,
    sessions.map((session) => usageSessionKey(session) + ":" + (session.session_revision || "")).join(","),
  ].join("|");
  if (usageSignature === lastUsageSignature) return;
  lastUsageSignature = usageSignature;
  const totalTurnsCount = runtime.request_count || fallbackTurns.length;
  const avgMs = runtime.average_ms || average(billable.map((turn) => turn.request_ms || 0).filter((value) => value > 0));
  const totalCached = runtime.total_cached_input_tokens || 0;
  const totalMiss = runtime.total_cache_miss_input_tokens || 0;
  const cacheHitRate = usageCacheHitRate(totalCached, totalMiss);
  const totalCostVal = Array.isArray(runtime.billing_buckets) && runtime.billing_buckets.length
    ? runtime.billing_buckets.reduce((sum, bucket) => sum + costForTokens(bucket), 0)
    : billable.reduce((sum, turn) => sum + costForTokens(turn), 0);

  els.usageTotalTurns.textContent = formatNumber(totalTurnsCount);
  els.usageCacheHitRate.textContent = cacheHitRate;
  els.usageCacheHitRate.className = ["usage-metric-value", "selectable", usageCacheToneClass(totalCached, totalMiss)].filter(Boolean).join(" ");
  els.usageAverageMs.textContent = formatDuration(avgMs);
  els.usageTotalCost.textContent = formatCost(totalCostVal);
  els.usageTotalCost.classList.add("usage-cost-value");
  renderUsageRows(sessions);
  noteSlow("renderUsage", performance.now() - started);
}

function usageSessionsFromTurns(turns) {
  return turns.map((turn) => {
    const kind = usageTurnKind(turn, turn && turn.conversation_turn !== false);
    const row = {
      id: turn.id,
      kind,
      label: usageRecordTitle(turn),
      hint: turn.lifecycle || "",
      model: turn.model,
      requested_model: turn.requested_model,
      reasoning_effort: turn.reasoning_effort || "",
      lifecycle: turn.lifecycle,
      status: turn.lifecycle === "failed_billable" ? "failed" : "completed",
      billable: turn.billable,
      cached_input_tokens: turn.cached_input_tokens || 0,
      cache_miss_input_tokens: turn.cache_miss_input_tokens || 0,
      output_tokens: turn.output_tokens || 0,
      total_tokens: turn.total_tokens || 0,
      request_ms: turn.request_ms || 0,
    };
    return {
      id: turn.id,
      title: usageRecordTitle(turn),
      title_source: "localized",
      completed_at: turn.completed_at,
      conversation_turn: turn.conversation_turn,
      status: row.status,
      cached_input_tokens: row.cached_input_tokens,
      cache_miss_input_tokens: row.cache_miss_input_tokens,
      output_tokens: row.output_tokens,
      total_tokens: row.total_tokens,
      request_ms: row.request_ms,
      rows: [row],
      segments: [{
        ...row,
        tool_name: null,
        iteration: null,
        summary: null,
        completed_at: turn.completed_at,
        rows: [row],
      }],
      technical_details: [
        { label: "request id", value: turn.id || "-" },
        { label: "lifecycle", value: turn.lifecycle || "-" },
      ],
    };
  });
}

function renderUsageRows(sessions) {
  if (sessions.length === 0) {
    syncKeyedChildren(els.usageRows, [{
      key: "empty",
      create: () => {
        const empty = document.createElement("div");
        empty.className = "usage-empty";
        empty.textContent = t("noRows");
        return empty;
      },
      update: (node) => {
        node.textContent = t("noRows");
      },
    }], usageSessionDomById);
    return;
  }
  const anchor = captureScrollAnchor(els.usageRows.closest(".usage-record-wrap") || els.usageRows);
  const rows = sessions.slice(0, 60).map((session) => ({
    key: usageSessionKey(session),
    create: () => usageRecord(session),
    update: (node) => updateUsageRecord(node, session),
  }));
  syncKeyedChildren(els.usageRows, rows, usageSessionDomById);
  restoreScrollAnchor(els.usageRows.closest(".usage-record-wrap") || els.usageRows, anchor);
}

function usageRecord(session) {
  const details = document.createElement("details");
  details.className = "usage-record";
  details.dataset.usageSessionId = usageSessionKey(session);
  details.__usageSession = session;
  const summary = document.createElement("summary");
  summary.className = "usage-grid-spec";
  renderUsageRecordSummary(summary, session);
  details.appendChild(summary);
  details.addEventListener("toggle", () => {
    if (!details.open) return;
    ensureUsageRecordBody(details, details.__usageSession || session);
  });
  return details;
}

function updateUsageRecord(details, session) {
  details.dataset.usageSessionId = usageSessionKey(session);
  details.__usageSession = session;
  const summary = details.querySelector(":scope > summary") || document.createElement("summary");
  summary.className = "usage-grid-spec";
  renderUsageRecordSummary(summary, session);
  if (!summary.parentNode) details.insertBefore(summary, details.firstChild);
  if (details.open) ensureUsageRecordBody(details, session, { force: true });
}

function renderUsageRecordSummary(summary, session) {
  const totalCost = formatCost(costForSession(session));
  const cachedTokens = Number(session.cached_input_tokens || 0);
  const missTokens = Number(session.cache_miss_input_tokens || 0);
  const inputTokens = cachedTokens + missTokens;
  const cacheHitRate = usageCacheHitRate(cachedTokens, missTokens);
  summary.replaceChildren(
    usageTitleCell(usageSessionTitle(session), usageRelativeDateTime(session.completed_at)),
    usageValueCell(formatDuration(session.request_ms), "muted"),
    usageValueCell(formatNumber(inputTokens)),
    usageValueCell(formatNumber(session.output_tokens || 0)),
    usageValueCell(cacheHitRate, usageCacheToneClass(cachedTokens, missTokens)),
    usageValueCell(totalCost),
  );
}

function ensureUsageRecordBody(details, session, options = {}) {
  rememberOpenUsageSession(usageSessionKey(session));
  const detailed = usageDetailedSession(session);
  if (!detailed) {
    let body = details.querySelector(":scope > .usage-trace-pure-container");
    if (!body) {
      body = usageLoadingBody();
      details.appendChild(body);
    }
    fetchUsageSessionDetail(details, session).catch(() => {});
    pruneUsageOpenBodies(details);
    return;
  }
  let body = details.querySelector(":scope > .usage-trace-pure-container");
  if (!body) {
    body = usageRecordBody(detailed);
    details.appendChild(body);
  } else if (options.force || details.dataset.rendered !== "true") {
    updateUsageRecordBody(body, detailed);
  }
  details.dataset.rendered = "true";
  pruneUsageOpenBodies(details);
}

function usageDetailedSession(session) {
  const key = usageSessionKey(session);
  const revision = String(session && session.session_revision || "");
  const cached = usageSessionDetailCache.get(key);
  if (cached && (!revision || cached.sessionRevision === revision)) return cached.session;
  if (Array.isArray(session && session.segments) && session.segments.length) return session;
  if (Array.isArray(session && session.rows) && session.rows.length) return session;
  return null;
}

function usageLoadingBody() {
  const body = document.createElement("div");
  body.className = "usage-trace-pure-container";
  body.dataset.loadingUsageBody = "true";
  const row = document.createElement("div");
  row.className = "usage-grid-spec trace-stripe-row";
  const cell = document.createElement("div");
  cell.className = "trace-cell";
  cell.textContent = t("busyDetail");
  row.append(cell, usageTraceCell("-", true), usageTraceInputCell("-", "-"), usageTraceCell("-", true), usageTraceCell("-", true), usageTraceCell("-", true));
  body.appendChild(row);
  return body;
}

async function fetchUsageSessionDetail(details, session) {
  const key = usageSessionKey(session);
  if (!key || details.dataset.loadingUsageDetail === "true") return;
  details.dataset.loadingUsageDetail = "true";
  try {
    const data = await apiJson("/api/usage/session?id=" + encodeURIComponent(key), { cache: "no-store" });
    const detailed = data && data.session;
    if (!detailed) return;
    usageSessionDetailCache.set(key, {
      session: detailed,
      sessionRevision: String(session && session.session_revision || ""),
      usageRevision: Number(data.usage_revision || 0),
    });
    const body = details.querySelector(":scope > .usage-trace-pure-container") || usageRecordBody(detailed);
    updateUsageRecordBody(body, detailed);
    if (!body.parentNode) details.appendChild(body);
    details.dataset.rendered = "true";
    pruneUsageDetailCache();
  } finally {
    details.dataset.loadingUsageDetail = "false";
  }
}

function rememberOpenUsageSession(key) {
  if (!key) return;
  usageOpenSessionOrder = usageOpenSessionOrder.filter((value) => value !== key);
  usageOpenSessionOrder.push(key);
}

function pruneUsageDetailCache() {
  while (usageOpenSessionOrder.length > 3) {
    const key = usageOpenSessionOrder.shift();
    usageSessionDetailCache.delete(key);
  }
}

function pruneUsageOpenBodies(activeDetails) {
  pruneUsageDetailCache();
  const keep = new Set(usageOpenSessionOrder.slice(-3));
  for (const details of Array.from(els.usageRows.querySelectorAll(".usage-record[open]"))) {
    if (details === activeDetails) continue;
    const key = details.dataset.usageSessionId || "";
    if (keep.has(key)) continue;
    const body = details.querySelector(":scope > .usage-trace-pure-container");
    if (body) body.remove();
    details.dataset.rendered = "false";
  }
}

function usageSessionKey(session) {
  const rows = Array.isArray(session && session.rows) ? session.rows : [];
  const firstRowId = rows.length ? String(rows[0] && rows[0].id || "").trim() : "";
  return firstRowId || String(session && session.id || session && session.completed_at || session && session.title || "usage-session");
}

function usageSessionTitle(session) {
  const title = String(session && session.title || "").trim();
  if (session && session.conversation_turn === false) {
    return usageSessionSemanticTitle(title || "service_request");
  }
  if (title && (session.title_source === "semantic" || session.title_source === "localized")) {
    return usageSessionSemanticTitle(title);
  }
  if (title) return title;
  return session && session.conversation_turn === false ? t("usageIntermediateReply") : t("usageConversationRecord");
}

function usageSessionSemanticTitle(value) {
  const key = String(value || "").trim();
  if (key === "service_request") return t("usageServiceRequestTitle");
  return usageSemanticText(key);
}

function usageRecordTitle(turn) {
  if (turn && turn.lifecycle === "service_ephemeral") return t("usageServiceRequestTitle");
  if (turn && turn.lifecycle === "failed_billable") return t("usageFailedBillable");
  if (turn && turn.conversation_turn === false) return t("usageIntermediateReply");
  return t("usageConversationRecord");
}

function usageTurnKind(turn, isFinal) {
  if (isFinal) return "final_reply";
  if (turn && turn.lifecycle === "service_ephemeral") return "service";
  if (turn && turn.lifecycle === "failed_billable") return "failed_reply";
  return "intermediate_reply";
}

function usageRecordBody(session) {
  const body = document.createElement("div");
  body.className = "usage-trace-pure-container";
  updateUsageRecordBody(body, session);
  return body;
}

function updateUsageRecordBody(body, session) {
  const segments = usageSegmentsForRender(session);
  if (body.dataset.loadingUsageBody === "true") {
    body.replaceChildren();
    delete body.dataset.loadingUsageBody;
    body.__usageSegmentDomById = new Map();
  } else if (!body.__usageSegmentDomById) {
    body.__usageSegmentDomById = new Map();
  }
  syncKeyedChildren(body, segments.map((segment, index) => ({
    key: usageSegmentKey(segment, index),
    signature: stableStringify(["usage-segment", uiLanguage, segment]),
    create: () => usageSegmentRow(segment),
  })), body.__usageSegmentDomById);
}

function usageSegmentKey(segment, index) {
  const id = String(segment && segment.id || "").trim();
  if (id) return "usage-segment|" + id;
  return [
    "usage-segment",
    segment && segment.kind || "",
    segment && segment.completed_at || "",
    segment && segment.tool_name || "",
    segment && segment.iteration || "",
    index,
  ].join("|");
}

function usageTitleCell(title, subtitle) {
  const wrap = document.createElement("div");
  wrap.className = "usage-title-cell";
  const text = document.createElement("span");
  text.className = "usage-record-title-text";
  text.textContent = title || "-";
  const time = document.createElement("span");
  time.className = "usage-record-meta-time";
  time.textContent = subtitle || "-";
  wrap.append(text, time);
  return wrap;
}

function usageValueCell(value, tone) {
  const span = document.createElement("div");
  span.className = ["usage-cell-value", "usage-text-right", tone || ""].filter(Boolean).join(" ");
  span.textContent = value || "-";
  span.title = span.textContent;
  return span;
}

function usageModelLabel(turn) {
  const model = String(turn && turn.model || "").trim();
  const requested = String(turn && turn.requested_model || "").trim();
  return model || requested || "-";
}

function usageCacheHitRate(cachedTokens, missTokens) {
  const cached = Number(cachedTokens || 0);
  const miss = Number(missTokens || 0);
  const total = cached + miss;
  if (total <= 0) return "-";
  const rate = cached / total * 100;
  return (Number.isInteger(rate) ? rate.toFixed(0) : rate.toFixed(1)) + "%";
}

function usageCacheToneClass(cachedTokens, missTokens) {
  const cached = Number(cachedTokens || 0);
  const miss = Number(missTokens || 0);
  const total = cached + miss;
  if (total <= 0) return "";
  const rate = cached / total * 100;
  if (rate >= 85) return "usage-cache-strong";
  if (rate >= 60) return "usage-cache-good";
  if (rate >= 40) return "usage-cache-mid";
  if (rate >= 10) return "usage-cache-low";
  return "usage-cache-none";
}

function usageSegmentsForRender(session) {
  const segments = Array.isArray(session && session.segments) ? session.segments : [];
  if (segments.length) return segments.slice().reverse();
  const rows = Array.isArray(session && session.rows) ? session.rows : [];
  return rows.slice().reverse();
}

function usageSegmentRow(segment) {
  const display = usageSegmentDisplay(segment);
  const row = document.createElement("div");
  row.className = "usage-grid-spec trace-stripe-row";

  const combined = document.createElement("div");
  combined.className = "trace-cell-combined";
  const time = document.createElement("span");
  time.className = "trace-sub-time";
  time.textContent = usageShortTime(segment && segment.completed_at);
  const stage = document.createElement("span");
  stage.className = ["trace-stage", usageStageClass(segment)].filter(Boolean).join(" ");
  stage.textContent = usageStageLabel(segment);
  stage.dataset.tip = usageSegmentTip(segment);
  bindUsageTraceTooltip(stage);
  combined.append(time, stage, usageSplitTag(display.tagCore, display.tagTelemetry));

  row.append(
    combined,
    usageTraceCell(display.elapsed, true),
    usageTraceInputCell(display.inputTotal, display.hit),
    usageTraceCell(display.output, true),
    usageTraceCell(display.cacheHitRate, true),
    usageTraceCell(display.cost, true, display.cost === "-" ? "" : "cost-val"),
  );
  return row;
}

function usageHasTokens(value) {
  if (!value) return false;
  return Number(value.cached_input_tokens || 0) > 0
    || Number(value.cache_miss_input_tokens || 0) > 0
    || Number(value.output_tokens || 0) > 0
    || Number(value.total_tokens || 0) > 0;
}

function usageSegmentDisplay(segment) {
  const hasTokens = usageHasTokens(segment);
  const hasRows = Array.isArray(segment && segment.rows) && segment.rows.length > 0;
  const cached = Number(segment && segment.cached_input_tokens || 0);
  const miss = Number(segment && segment.cache_miss_input_tokens || 0);
  return {
    tagCore: usageTagCore(segment),
    tagTelemetry: usageTagTelemetry(segment),
    elapsed: segment && segment.request_ms ? formatDuration(segment.request_ms) : "-",
    inputTotal: hasTokens ? formatNumber(cached + miss) : "-",
    miss: hasTokens ? formatNumber(segment.cache_miss_input_tokens) : "-",
    hit: hasTokens ? formatNumber(segment.cached_input_tokens) : "-",
    output: hasTokens ? formatNumber(segment.output_tokens) : "-",
    cacheHitRate: hasTokens ? usageCacheHitRate(segment.cached_input_tokens, segment.cache_miss_input_tokens) : "-",
    cost: hasRows || hasTokens ? formatCost(costForTokens(segment)) : "-",
  };
}

function usageTraceInputCell(total, hit) {
  const cell = document.createElement("div");
  cell.className = "trace-cell trace-input-cell usage-text-right";
  cell.append(
    usageTraceInputLine("total", total),
    usageTraceInputLine("hit", hit),
  );
  return cell;
}

function usageTraceInputLine(kind, value) {
  const line = document.createElement("span");
  line.className = "trace-input-line";
  const label = document.createElement("span");
  label.className = "trace-input-label";
  label.textContent = kind === "hit" ? t("usageCacheHitShort") : t("usageInputTotalShort");
  const number = document.createElement("span");
  number.className = "trace-input-number";
  number.textContent = value || "-";
  if (number.textContent === "-") number.classList.add("dash");
  line.append(label, number);
  return line;
}

function usageTraceCell(value, numeric, innerClass) {
  const cell = document.createElement("div");
  cell.className = ["trace-cell", numeric ? "usage-text-right" : ""].filter(Boolean).join(" ");
  const text = value || "-";
  if (text === "-" || innerClass) {
    const inner = document.createElement("span");
    inner.className = text === "-" ? "dash" : innerClass;
    inner.textContent = text;
    cell.appendChild(inner);
  } else {
    cell.textContent = text;
  }
  return cell;
}

function costForSession(session) {
  if (Array.isArray(session && session.billing_buckets) && session.billing_buckets.length) {
    return session.billing_buckets.reduce((sum, bucket) => sum + costForTokens(bucket), 0);
  }
  const rows = Array.isArray(session && session.rows) ? session.rows : [];
  if (rows.length) return rows.reduce((sum, row) => sum + costForTokens(row), 0);
  return costForTokens(session || {});
}

function usageSplitTag(core, telemetry) {
  const pill = document.createElement("div");
  pill.className = "split-tag-pill";
  const coreEl = document.createElement("span");
  coreEl.className = "tag-core";
  coreEl.textContent = core || "-";
  const telemetryEl = document.createElement("span");
  telemetryEl.className = "tag-telemetry";
  telemetryEl.textContent = telemetry || "-";
  pill.append(coreEl, telemetryEl);
  return pill;
}

function usageStageClass(segment) {
  if (!segment) return "";
  if (segment.status === "failed" || segment.kind === "failed_reply") return "failed";
  if (segment.status === "running" || segment.kind === "in_progress_reply" || segment.kind === "tool_call") return "running";
  if (segment.kind === "tool_result") return "tool";
  if (segment.kind === "final_reply") return "final";
  if (segment.kind === "service" || segment.lifecycle === "service_ephemeral") return "service";
  return "reply";
}

function usageStageLabel(segment) {
  if (!segment) return "-";
  if (segment.status === "failed" || segment.kind === "failed_reply") return t("usageFailedBillable");
  if (segment.kind === "tool_result" || segment.kind === "tool_call") return t("usageToolStage");
  if (segment.kind === "final_reply") return t("usageFinalReply");
  if (segment.kind === "service" || segment.lifecycle === "service_ephemeral") return t("usageServiceRequest");
  if (segment.kind === "in_progress_reply" || segment.status === "running") return t("usageInProgressReply");
  return t("usageIntermediateReply");
}

function usageTagCore(segment) {
  if (!segment) return "-";
  if (segment.tool_name) return String(segment.tool_name);
  return usageModelLabel(segment);
}

function usageTagTelemetry(segment) {
  if (!segment) return "-";
  if (segment.status === "failed") return "failed";
  if (segment.status === "running" || segment.kind === "tool_call") return "open";
  if (segment.kind === "tool_result") {
    const summary = String(segment.summary || "").toLowerCase();
    if (summary.includes("opened") || summary.includes("open_page")) return "open";
    if (summary.includes("candidate") || summary.includes("source") || summary.includes("search")) return "search";
    return "done";
  }
  const effort = String(segment.reasoning_effort || "").trim().toLowerCase();
  if (effort) return effort;
  if (segment.lifecycle === "service_ephemeral" || segment.kind === "service") return "none";
  if (segment.kind === "final_reply") return "final";
  if (segment.kind === "client_handoff_model") return "handoff";
  return "model";
}

function usageSegmentTip(segment) {
  if (!segment) return "";
  return [
    usageTipLine("status", segment.status),
    usageTipLine("kind", segment.kind),
    usageTipLine("lifecycle", segment.lifecycle),
    usageTipLine("reasoning", segment.reasoning_effort),
    usageTipLine("hint", usageSemanticText(segment.hint)),
    segment.iteration ? usageTipLine("iteration", formatNumber(segment.iteration)) : "",
    usageTipLine("summary", segment.summary),
  ].filter(Boolean).join("\n");
}

function usageTipLine(label, value) {
  const text = String(value || "").trim();
  return text ? label + ": " + text : "";
}

function bindUsageTraceTooltip(target) {
  target.addEventListener("mouseenter", () => showUsageTraceTooltip(target));
  target.addEventListener("mouseleave", hideUsageTraceTooltip);
}

function ensureUsageTraceTooltip() {
  if (usageTraceTooltipEl) return usageTraceTooltipEl;
  const tooltip = document.createElement("div");
  tooltip.className = "usage-trace-tooltip";
  tooltip.hidden = true;
  document.body.appendChild(tooltip);
  usageTraceTooltipEl = tooltip;
  return tooltip;
}

function showUsageTraceTooltip(target) {
  const text = target && target.dataset ? String(target.dataset.tip || "").trim() : "";
  if (!text) return;
  const tooltip = ensureUsageTraceTooltip();
  tooltip.textContent = text;
  tooltip.hidden = false;
  const targetRect = target.getBoundingClientRect();
  const tooltipRect = tooltip.getBoundingClientRect();
  const gap = 10;
  const margin = 12;
  let left = targetRect.right + gap;
  let top = targetRect.top;
  if (left + tooltipRect.width + margin > window.innerWidth) {
    left = Math.max(margin, targetRect.left - tooltipRect.width - gap);
  }
  const maxTop = Math.max(margin, window.innerHeight - tooltipRect.height - margin);
  top = Math.min(Math.max(margin, top), maxTop);
  tooltip.style.left = left + "px";
  tooltip.style.top = top + "px";
}

function hideUsageTraceTooltip() {
  if (!usageTraceTooltipEl) return;
  usageTraceTooltipEl.hidden = true;
}

function usageRelativeDateTime(value) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "-";
  const now = new Date();
  const label = isSameDate(date, now)
    ? t("usageToday")
    : isSameDate(date, new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1))
      ? t("usageYesterday")
      : String(date.getMonth() + 1).padStart(2, "0") + "-" + String(date.getDate()).padStart(2, "0");
  return label + " " + String(date.getHours()).padStart(2, "0") + ":" + String(date.getMinutes()).padStart(2, "0");
}

function usageShortTime(value) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "--:--:--";
  return [
    String(date.getHours()).padStart(2, "0"),
    String(date.getMinutes()).padStart(2, "0"),
    String(date.getSeconds()).padStart(2, "0"),
  ].join(":");
}

function isSameDate(left, right) {
  return left.getFullYear() === right.getFullYear()
    && left.getMonth() === right.getMonth()
    && left.getDate() === right.getDate();
}

function usageSemanticText(value) {
  const key = String(value || "").trim();
  switch (key) {
    case "conversation":
      return t("usageConversationRecord");
    case "intermediate_reply":
      return t("usageIntermediateReply");
    case "final_reply":
      return t("usageFinalReply");
    case "service_request":
      return t("usageServiceRequest");
    case "failed_billable":
      return t("usageFailedBillable");
    case "intermediate":
      return t("usageIntermediateInfo");
    case "completed_final_response":
      return t("usageCompletedFinalResponse");
    case "background_service_request":
      return t("usageBackgroundServiceRequest");
    case "billable_failed_request":
      return t("usageBillableFailedRequest");
    case "client_tool_handoff":
      return t("usageClientToolHandoff");
    case "billable_model_request":
      return t("usageBillableModelRequest");
    case "usageStatusCompleted":
      return t("usageStatusCompleted");
    case "usage_model_iteration":
      return t("usageModelIteration");
    case "usage_model_iteration_hint":
      return t("usageModelIterationHint");
    case "usage_model_request":
      return t("usageModelRequest");
    case "usage_model_request_hint":
      return t("usageModelRequestHint");
    case "usage_client_handoff_model_stage":
      return t("usageClientHandoffModelStage");
    case "usage_client_handoff_model_stage_hint":
      return t("usageClientHandoffModelStageHint");
    case "usage_web_search_stage":
      return t("usageWebSearchStage");
    case "usage_tool_stage":
      return t("usageToolStage");
    case "usage_tool_completed":
      return t("usageToolCompleted");
    case "usage_tool_failed":
      return t("usageToolFailed");
    case "usage_tool_requested":
      return t("usageToolRequested");
    case "usage_in_progress_reply":
      return t("usageInProgressReply");
    case "usage_in_progress_reply_hint":
      return t("usageInProgressReplyHint");
    default:
      return key;
  }
}

function updateLatestLogs(events, options = {}) {
  const next = Array.isArray(events) ? events : [];
  const hasMore = options.hasMore === undefined ? null : Boolean(options.hasMore);
  if (options.nextCursor !== undefined) logNextCursor = options.nextCursor || logNextCursor;
  if (options.latestCursor !== undefined) {
    logLatestCursor = options.latestCursor || logLatestCursor;
  } else if (next.length > 0) {
    const newest = next[next.length - 1];
    logLatestCursor = newest.cursor || [newest.ts || "", newest.id || ""].join("|") || logLatestCursor;
  }
  if (options.eventRevision !== undefined && options.eventRevision !== null) {
    logLatestEventRevision = Number(options.eventRevision);
  }
  const shouldFollow = options.force || logEvents.length === 0 || (logAutoFollow && isAtLogTop());
  const nextEvents = logEvents.length === 0 ? next.slice(-LOG_INITIAL_PAGE_SIZE) : eventsAfterNewestLog(next);
  if (!options.force && nextEvents.length === 0) {
    logHasMore = hasMore === null ? (next.length >= LOG_INITIAL_PAGE_SIZE || logHasMore) : hasMore;
    if (logRenderPending && shouldFollow) {
      logRenderPending = false;
      scheduleRenderLogs({ followTop: true });
    }
    return;
  }
  if (shouldFollow) {
    logEvents = trimLogMemory(mergeEvents(logEvents.concat(nextEvents)));
    logWindowStart = null;
    logHasMore = hasMore === null ? (next.length >= LOG_INITIAL_PAGE_SIZE || logHasMore) : hasMore;
    pruneLogDividers();
    logRenderPending = false;
    scheduleRenderLogs({ followTop: true });
  } else {
    logEvents = trimLogMemory(mergeEvents(logEvents.concat(nextEvents)));
    logHasMore = hasMore === null ? (next.length >= LOG_INITIAL_PAGE_SIZE || logHasMore) : hasMore;
    pruneLogDividers();
    logRenderPending = true;
  }
}

function renderLogs(options = {}) {
  const started = performance.now();
  const shouldFollow = options.followTop || isAtLogTop();
  const anchor = options.preserveAnchor ? captureScrollAnchor(els.logStream) : null;
  const signature = [
    uiLanguage,
    visibleLogEvents().map(logEventKey).join(","),
    logDividers.map((divider) => `${divider.key}:${divider.count}`).join(","),
  ].join("|");
  if (signature === lastLogRenderSignature) return;
  lastLogRenderSignature = signature;
  if (logEvents.length === 0) {
    syncKeyedChildren(els.logStream, [{
      key: "empty",
      create: () => logEntry({
      time: "--:--:--",
      prefix: "SYS",
      message: t("noLogs"),
      detail: t("noLogsDetail"),
      baseClass: "log-type-system",
      }),
      update: (node) => {
        const next = logEntry({
          time: "--:--:--",
          prefix: "SYS",
          message: t("noLogs"),
          detail: t("noLogsDetail"),
          baseClass: "log-type-system",
        });
        node.replaceWith(next);
        return next;
      },
    }], logRenderedKeys);
    return;
  }
  const nodes = logRenderItems().map((item) => {
    if (item.kind === "divider") {
      return {
        key: item.key,
        signature: `divider|${uiLanguage}|${item.count}`,
        create: () => logDivider(item.count),
      };
    }
    const normalized = normalizeLogEvent(item.event);
    return {
      key: item.key,
      signature: logEventRenderSignature(item.event, normalized),
      create: () => logEntry(normalized),
    };
  });
  syncKeyedChildren(els.logStream, nodes, logRenderedKeys);
  if (anchor) restoreScrollAnchor(els.logStream, anchor);
  else if (shouldFollow) els.logStream.scrollTop = 0;
  noteSlow("renderLogs", performance.now() - started);
}

function scheduleRenderLogs(options = {}) {
  logRenderFrameOptions = Object.assign({}, logRenderFrameOptions || {}, options);
  if (typeof requestAnimationFrame !== "function") {
    const nextOptions = logRenderFrameOptions || {};
    logRenderFrameOptions = null;
    renderLogs(nextOptions);
    return;
  }
  if (logRenderFrame !== null) return;
  logRenderFrame = requestAnimationFrame(() => {
    logRenderFrame = null;
    const nextOptions = logRenderFrameOptions || {};
    logRenderFrameOptions = null;
    renderLogs(nextOptions);
  });
}

function logEventRenderSignature(event, normalized) {
  return [
    "event",
    uiLanguage,
    event && event.ts || "",
    event && event.id || "",
    event && event.type || event && event.event_type || "",
    normalized.level,
    normalized.category,
    normalized.title,
    normalized.summary,
    normalized.requestId,
    normalized.sessionHint,
    normalized.riskFlags.join(","),
  ].join("|");
}

function handleLogScroll() {
  if (isAtLogTop() && logRenderPending) {
    logRenderPending = false;
    scheduleRenderLogs({ followTop: true });
  }
  if (isAtLogBottom()) loadOlderLogs();
}

function logRenderItems() {
  const dividerMap = new Map(logDividers.map((divider) => [divider.key, divider]));
  const items = [];
  for (const event of visibleLogEvents().slice().reverse()) {
    const divider = dividerMap.get(logEventKey(event));
    if (divider) items.push({ kind: "divider", key: "divider|" + divider.key, count: divider.count });
    items.push({ kind: "event", key: logEventKey(event), event });
  }
  return items;
}

function visibleLogEvents() {
  if (logEvents.length <= LOG_RENDER_WINDOW_SIZE) return logEvents;
  const latestStart = Math.max(0, logEvents.length - LOG_RENDER_WINDOW_SIZE);
  const start = logWindowStart === null
    ? latestStart
    : Math.max(0, Math.min(logWindowStart, latestStart));
  const end = Math.min(logEvents.length, start + LOG_RENDER_WINDOW_SIZE);
  return logEvents.slice(start, end);
}

function currentLogWindowRange() {
  if (logEvents.length <= LOG_RENDER_WINDOW_SIZE) {
    return { start: 0, end: logEvents.length };
  }
  const latestStart = Math.max(0, logEvents.length - LOG_RENDER_WINDOW_SIZE);
  const start = logWindowStart === null
    ? latestStart
    : Math.max(0, Math.min(logWindowStart, latestStart));
  return { start, end: Math.min(logEvents.length, start + LOG_RENDER_WINDOW_SIZE) };
}

function pageLogWindowOlder() {
  const range = currentLogWindowRange();
  if (range.start <= 0) return false;
  logWindowStart = Math.max(0, range.start - LOG_RENDER_WINDOW_SIZE);
  return true;
}

function trimLogMemory(events) {
  if (events.length <= LOG_MEMORY_MAX_ITEMS) return events;
  const removed = events.length - LOG_MEMORY_MAX_ITEMS;
  if (logWindowStart !== null) logWindowStart = Math.max(0, logWindowStart - removed);
  return events.slice(removed);
}

function syncKeyedChildren(container, items, cache) {
  if (!container) return;
  const nextKeys = new Set(items.map((item) => item.key));
  for (const [key, node] of Array.from(cache.entries())) {
    if (nextKeys.has(key)) continue;
    if (node && node.parentNode === container) container.removeChild(node);
    cache.delete(key);
  }
  for (const item of items) {
    let node = cache.get(item.key);
    if (!node) {
      node = item.create();
      cache.set(item.key, node);
    } else if (item.signature && node.dataset.renderSignature !== item.signature) {
      const nextNode = item.create();
      if (node.parentNode === container) node.replaceWith(nextNode);
      node = nextNode;
      cache.set(item.key, node);
    } else if (typeof item.update === "function") {
      const nextNode = item.update(node);
      if (nextNode && nextNode !== node) {
        node = nextNode;
        cache.set(item.key, node);
      }
    }
    node.dataset.scrollAnchorKey = item.key;
    node.dataset.renderKey = item.key;
    if (item.signature) node.dataset.renderSignature = item.signature;
    container.appendChild(node);
  }
}

function captureScrollAnchor(scroller) {
  if (!scroller) return null;
  const bounds = scroller.getBoundingClientRect();
  const candidates = Array.from(scroller.querySelectorAll("[data-scroll-anchor-key]"));
  for (const element of candidates) {
    const rect = element.getBoundingClientRect();
    if (rect.bottom < bounds.top || rect.top > bounds.bottom) continue;
    return {
      key: element.dataset.scrollAnchorKey,
      offset: rect.top - bounds.top,
      scrollTop: scroller.scrollTop,
    };
  }
  return { scrollTop: scroller.scrollTop };
}

function restoreScrollAnchor(scroller, anchor) {
  if (!scroller || !anchor) return;
  if (!anchor.key) {
    scroller.scrollTop = anchor.scrollTop || 0;
    return;
  }
  const element = scroller.querySelector(`[data-scroll-anchor-key="${cssEscape(anchor.key)}"]`);
  if (!element) {
    scroller.scrollTop = anchor.scrollTop || 0;
    return;
  }
  const bounds = scroller.getBoundingClientRect();
  const rect = element.getBoundingClientRect();
  scroller.scrollTop += rect.top - bounds.top - anchor.offset;
}

function normalizeLogEvent(event) {
  const type = event.type || event.event_type || "event";
  const level = String(event.severity || event.level || "info").toLowerCase();
  const category = String(event.category || fallbackLogCategory(type, level)).toLowerCase();
  const safeDetail = event.safe_detail || event.detail || null;
  const requestId = event.request_id || (safeDetail && safeDetail.id) || "";
  const riskFlags = Array.isArray(event.risk_flags) ? event.risk_flags.filter(Boolean) : [];
  return {
    time: event.ts ? formatTimeOnly(event.ts) : formatTimeOnly(new Date()),
    level,
    category,
    categoryLabel: logCategoryLabel(category),
    title: event.title || userLogMessage(type, event.message || ""),
    summary: event.summary || event.message || "",
    requestId,
    sessionHint: event.session_hint || "",
    riskFlags,
    metrics: event.metrics || {},
    detailRows: logDetailRows(safeDetail, event.metrics || {}),
    baseClass: `log-category-${category} log-level-${level}`,
  };
}

function fallbackLogCategory(type, level) {
  if (level === "error") return "error";
  if (String(type || "").includes("tool")) return "tool";
  if (String(type || "").includes("request")) return "request";
  return "system";
}

function logCategoryLabel(category) {
  const key = {
    request: "logCategoryRequest",
    tool: "logCategoryTool",
    protocol: "logCategoryProtocol",
    context: "logCategoryContext",
    web: "logCategoryWeb",
    security: "logCategorySecurity",
    system: "logCategorySystem",
    error: "logCategoryError",
  }[category];
  return key ? t(key) : String(category || "system").toUpperCase();
}

function logLevelLabel(level) {
  return String(level || "info").toUpperCase();
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
  const wrap = document.createElement("details");
  wrap.className = `log-entry ${item.baseClass || ""}`;
  if (!item.detailRows.length) wrap.classList.add("log-entry-empty-detail");

  const row = document.createElement("summary");
  row.className = "log-row";
  appendTextSpan(row, "log-time", item.time);
  appendTextSpan(row, "log-level", logLevelLabel(item.level));
  appendTextSpan(row, "log-category", item.categoryLabel);

  const main = document.createElement("span");
  main.className = "log-main";
  const titleLine = document.createElement("span");
  titleLine.className = "log-title-line";
  appendTextSpan(titleLine, "log-title", item.title || t("runtimeEvent"));
  const meta = document.createElement("span");
  meta.className = "log-meta";
  if (item.requestId) appendTextSpan(meta, "log-request-id", compactLogValue(item.requestId, 24));
  if (item.sessionHint) appendTextSpan(meta, "log-session-hint", compactLogValue(item.sessionHint, 28));
  titleLine.appendChild(meta);
  main.appendChild(titleLine);

  const subline = document.createElement("span");
  subline.className = "log-subline";
  appendTextSpan(subline, "log-summary", item.summary || "");
  const riskList = document.createElement("span");
  riskList.className = "log-risk-list";
  for (const flag of item.riskFlags.slice(0, 4)) {
    appendTextSpan(riskList, "log-risk", logRiskLabel(flag));
  }
  subline.appendChild(riskList);
  main.appendChild(subline);
  row.appendChild(main);
  wrap.appendChild(row);

  if (item.detailRows.length) {
    const detail = document.createElement("div");
    detail.className = "log-detail-grid selectable";
    for (const detailRow of item.detailRows) {
      const label = document.createElement("span");
      label.className = "log-detail-key";
      label.textContent = detailRow.key;
      const value = document.createElement("span");
      value.className = "log-detail-value";
      value.textContent = detailRow.value;
      detail.append(label, value);
    }
    wrap.appendChild(detail);
  }
  return wrap;
}

function appendTextSpan(parent, className, text) {
  const span = document.createElement("span");
  span.className = className;
  span.textContent = text || "";
  parent.appendChild(span);
  return span;
}

function logRiskLabel(flag) {
  const value = String(flag || "").trim();
  return value ? value.replace(/_/g, " ") : "";
}

function logDetailRows(detail, metrics) {
  const rows = [];
  appendLogDetailRows(rows, detail, "", 0);
  if (metrics && typeof metrics === "object" && !Array.isArray(metrics)) {
    for (const [key, value] of Object.entries(metrics)) {
      if (value === undefined || value === null || value === "") continue;
      if (logDetailContainsKey(detail, key)) continue;
      addLogDetailRow(rows, "metric." + key, value);
    }
  }
  return rows.slice(0, 48);
}

function logDetailContainsKey(value, targetKey) {
  if (!value || typeof value !== "object" || !targetKey) return false;
  if (Array.isArray(value)) {
    return value.some((item) => logDetailContainsKey(item, targetKey));
  }
  for (const [key, nested] of Object.entries(value)) {
    if (key === targetKey) return true;
    if (nested && typeof nested === "object" && logDetailContainsKey(nested, targetKey)) {
      return true;
    }
  }
  return false;
}

function appendLogDetailRows(rows, value, prefix, depth) {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value)) {
    addLogDetailRow(rows, prefix || "items", value);
    return;
  }
  for (const [key, nested] of Object.entries(value)) {
    if (nested === undefined || nested === null || nested === "") continue;
    const label = prefix ? prefix + "." + key : key;
    if (nested && typeof nested === "object" && !Array.isArray(nested) && depth < 1) {
      appendLogDetailRows(rows, nested, label, depth + 1);
    } else {
      addLogDetailRow(rows, label, nested);
    }
  }
}

function addLogDetailRow(rows, key, value) {
  rows.push({
    key,
    value: compactLogValue(value, 360),
  });
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
  els.appDescription.textContent = t("aboutProductDescription");
  els.appVersion.textContent = "v" + version;
  els.appName.textContent = productName;
  els.aboutVersion.textContent = version;
  if (els.aboutVersionMeta) els.aboutVersionMeta.textContent = version;
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
  if (action === "website") return openOrExplain(urls.website || urls.official, t("websiteUnavailable"));
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
  if (shouldKeepConfigAsDraft(event)) {
    pendingConfig = null;
    clearAutosaveTimer();
    renderConfigSaveState(sameConfigPayload(next, lastSavedConfig) ? "clean" : "draft");
    return;
  }
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

function shouldKeepConfigAsDraft(event) {
  if (!event || event.type !== "input") return false;
  const target = event.target;
  if (!target || !SENSITIVE_CONFIG_INPUT_IDS.has(target.id || target.name)) return false;
  return isTextConfigInput(target);
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
    CONFIG_VERSION: latestConfigVersion || "",
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
    if (key === "CONFIG_VERSION" || key === "config_version") continue;
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

function renderConfigSaveState(state, detail = "") {
  const restartState = state === "savedRestart";
  if (els.restartRequiredBadge) els.restartRequiredBadge.hidden = !(restartRequired || restartState);
  if (!els.configSaveStatus) return;
  const key = {
    draft: "configDraft",
    pending: "configPending",
    saving: "configSaving",
    saved: "configSaved",
    savedRestart: "configSavedRestart",
    error: "configSaveError",
  }[state];
  els.configSaveStatus.hidden = !key;
  if (!key) {
    els.configSaveStatus.textContent = "";
    els.configSaveStatus.dataset.state = "";
    return;
  }
  els.configSaveStatus.textContent = detail ? `${t(key)}: ${detail}` : t(key);
  els.configSaveStatus.dataset.state = state;
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
  document.querySelectorAll("[data-i18n-placeholder]").forEach((element) => {
    element.setAttribute("placeholder", t(element.dataset.i18nPlaceholder));
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
  return (i18n[uiLanguage] && i18n[uiLanguage][key])
    || (i18n[FALLBACK_LANGUAGE] && i18n[FALLBACK_LANGUAGE][key])
    || key;
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
  return output.sort((left, right) => {
    const time = String(left.ts).localeCompare(String(right.ts));
    if (time !== 0) return time;
    return Number(left.id || 0) - Number(right.id || 0);
  });
}

function eventsAfterNewestLog(events) {
  const existingKeys = new Set(logEvents.map(logEventKey));
  return events.filter((event) => event && event.ts && !existingKeys.has(logEventKey(event)));
}

function logEventKey(event) {
  const id = event && event.id !== undefined && event.id !== null ? String(event.id) : "";
  if (id) return [event.ts || "", id].join("|");
  return [event.ts, event.type || "", event.message || "", JSON.stringify(event.detail || null)].join("|");
}

function pruneLogDividers() {
  const eventKeys = new Set(logEvents.map(logEventKey));
  logDividers = logDividers.filter((divider) => eventKeys.has(divider.key));
}

function oldestLogTs() {
  return logEvents.length > 0 ? logEvents[0].ts : null;
}

function oldestLogCursor() {
  if (logEvents.length === 0) return logNextCursor;
  const oldest = logEvents[0];
  return oldest.cursor || [oldest.ts || "", oldest.id || ""].join("|") || logNextCursor;
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
