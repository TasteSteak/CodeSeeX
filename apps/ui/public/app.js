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
const DEFAULT_CCS_CONTEXT_WINDOW = 1000000;
const CODEX_CONFIG_PATH_UNIX = "~/.codex/config.toml";
const CODEX_CONFIG_PATH_WINDOWS = "%USERPROFILE%\\.codex\\config.toml";
const REFRESH_RUNNING_MS = 2000;
const REFRESH_IDLE_MS = 5000;
const REFRESH_HIDDEN_MS = 10000;
const SLOW_RENDER_MS = 80;
const CONFIG_CHANGED_EVENT = "codeseex-config-changed";
const UPDATE_PROGRESS_EVENT = "codeseex-update-progress";
const RUNTIME_STATUS_STARTING = "starting";
const RUNTIME_STATUS_STOPPING = "stopping";
const ENABLED_TOOLS_KEY = "ENABLED_TOOLS";
const DEFAULT_TEMPERATURE_PRESET = "default";
const DEFAULT_REASONING_SUMMARY_MODE = "smart";
const REASONING_SUMMARY_MODES = ["none", "smart", "fixed", "full"];
const FALLBACK_PEAK_VALLEY = Object.freeze({
  enabled: true,
  timezone: "Asia/Shanghai",
  utcOffsetMinutes: 480,
  multiplier: 2,
  windows: Object.freeze([
    Object.freeze({ from: 9 * 60, to: 12 * 60 }),
    Object.freeze({ from: 14 * 60, to: 18 * 60 }),
  ]),
});
const RESTART_REQUIRED_KEYS = new Set([
  "NETWORK_PROXY_MODE",
  "PROXY_PORT",
]);
// Populated by the backend, never sent back: these describe the resolved
// catalog and pricing rather than a user setting.
const READ_ONLY_CONFIG_KEYS = new Set([
  "CATALOG",
  "CATALOG_MODELS",
  "CATALOG_STATUS",
  "UPSTREAM_MODEL_CHOICES",
]);
const catalogState = {
  revision: "",
  source: "builtin",
  providerName: "",
  defaultModel: "",
  currency: "CNY",
  unit: "per_1m_tokens",
  models: [],
  pricing: null,
  status: {},
};
let currentBillingRatesSignature = "";
const VISIBLE_MODEL_CARDS = 4;
const MODEL_CARD_GAP = 8;
const CATALOG_LABEL_FLASH_MS = 1600;
let selectedCatalogModel = "";
let modelListHeightTimer = null;
const catalogLabelTimers = new Map();
const SYSTEM_LANGUAGE = "system";
const FALLBACK_LANGUAGE = "en_us";
const DEFAULT_LANGUAGE = SYSTEM_LANGUAGE;

const els = {
  aboutStatConnection: byId("aboutStatConnection"),
  aboutStatModel: byId("aboutStatModel"),
  aboutStatus: byId("aboutStatus"),
  aboutUpdateDot: byId("aboutUpdateDot"),
  activeRequests: byId("activeRequests"),
  appDescription: byId("appDescription"),
  appLicense: byId("appLicense"),
  appVersion: byId("appVersion"),
  aboutVersion: byId("aboutVersion"),
  balanceGranted: byId("balanceGranted"),
  balanceStatus: byId("balanceStatus"),
  balanceToppedUp: byId("balanceToppedUp"),
  balanceTotal: byId("balanceTotal"),
  billingCardPanel: byId("billingCardPanel"),
  billingModelList: byId("billingModelList"),
  catalogRefreshButton: byId("catalogRefreshButton"),
  catalogRefreshLabel: byId("catalogRefreshLabel"),
  completedTurns: byId("completedTurns"),
  autoStart: byId("AUTO_START"),
  catalogNotice: byId("catalogNotice"),
  configTomlCode: byId("configTomlCode"),
  configSaveStatus: byId("configSaveStatus"),
  configTomlCopyStatus: byId("configTomlCopyStatus"),
  configTomlStatus: byId("configTomlStatus"),
  copyTomlButton: byId("copyTomlButton"),
  launchCodexButton: byId("launchCodexButton"),
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
  deepseekBaseUrl: byId("DEEPSEEK_BASE_URL"),
  proxyPort: byId("PROXY_PORT"),
  rechargeBalanceButton: byId("rechargeBalanceButton"),
  refreshBalanceButton: byId("refreshBalanceButton"),
  releaseNotesBody: byId("releaseNotesBody"),
  releaseNotesClose: byId("releaseNotesClose"),
  releaseNotesModal: byId("releaseNotesModal"),
  releaseNotesNotice: byId("releaseNotesNotice"),
  releaseNotesSource: byId("releaseNotesSource"),
  releaseNotesSubtitle: byId("releaseNotesSubtitle"),
  restartRequiredBadge: byId("restartRequiredBadge"),
  running: byId("running"),
  startButton: byId("startButton"),
  startButtonIcon: byId("startButtonIcon"),
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
  troubleshootVerifyRuntime: byId("troubleshootVerifyRuntime"),
  troubleshootSummary: byId("troubleshootSummary"),
  uiLanguage: byId("UI_LANGUAGE"),
  codexAppModelListInjection: byId("CODEX_APP_MODEL_LIST_INJECTION"),
  usageAverageMs: byId("usageAverageMs"),
  usageCacheHitRate: byId("usageCacheHitRate"),
  usageRows: byId("usageRows"),
  usageTotalCost: byId("usageTotalCost"),
  usageTotalTurns: byId("usageTotalTurns"),
  updateButton: byId("updateButton"),
  updateButtonDot: byId("updateButtonDot"),
  updateBackgroundBar: byId("updateBackgroundBar"),
  updateBackgroundOpen: byId("updateBackgroundOpen"),
  updateBackgroundPercent: byId("updateBackgroundPercent"),
  updateBackgroundStatus: byId("updateBackgroundStatus"),
  updateBackgroundTask: byId("updateBackgroundTask"),
  updateCancel: byId("updateCancel"),
  updateModal: byId("updateModal"),
  updateModalBackground: byId("updateModalBackground"),
  updateModalSubtitle: byId("updateModalSubtitle"),
  updateModalTitle: byId("updateModalTitle"),
  updatePercentText: byId("updatePercentText"),
  updateProgressBar: byId("updateProgressBar"),
  updateSizeText: byId("updateSizeText"),
  updateTaskLabel: byId("updateTaskLabel"),
  updateTaskState: byId("updateTaskState"),
  workspace: byId("workspace"),
};

let appInfo = null;
let latestReleaseNotes = null;
let releaseNotesLoad = null;
let releaseNotesLoadFailed = false;
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
// Number of consecutive 409 (config_version_conflict) save retries already spent.
let configSaveConflictRetries = 0;
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
let latestUpstreamModelOverride = null;
let latestUpstreamModelChoices = [];
let latestWebSearchBackend = "local";
let latestCatalogRuntimeDiagnostic = null;
let codexRuntimeVerificationInFlight = false;
let troubleshootTechnicalOpen = false;
let latestStatus = null;
let latestUpdateCheck = null;
let updateInstallInProgress = false;
let updateProgressState = {
  active: false,
  background: false,
  visible: false,
  stage: "idle",
  version: "",
  downloaded: 0,
  contentLength: null,
  percent: null,
  error: "",
};
let updateNoticeSeenVersion = "";
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
      ? normalizeLanguageManifest(loadedLanguages)
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
  els.startButton.addEventListener("click", () => (latestRunning
    ? actionPost("/api/restart", t("restartingTitle"), t("restartingDetail"))
    : actionPost("/api/start", t("startingTitle"), t("startingDetail"))));
  els.stopButton.addEventListener("click", () => actionPost("/api/stop", t("stoppingTitle"), t("stoppingDetail")));
  if (els.catalogRefreshButton) els.catalogRefreshButton.addEventListener("click", refreshCatalogDocument);
  if (els.billingModelList) els.billingModelList.addEventListener("click", selectBillingModel);
  if (els.refreshBalanceButton) els.refreshBalanceButton.addEventListener("click", refreshBalance);
  if (els.rechargeBalanceButton) els.rechargeBalanceButton.addEventListener("click", openRechargePage);
  if (els.copyTomlButton) els.copyTomlButton.addEventListener("click", copyConfigToml);
  if (els.launchCodexButton) els.launchCodexButton.addEventListener("click", launchCodexApp);
  if (els.importCcsButton) els.importCcsButton.addEventListener("click", importConfigToCcs);
  if (els.troubleshootButton) els.troubleshootButton.addEventListener("click", openTroubleshootModal);
  if (els.troubleshootClose) els.troubleshootClose.addEventListener("click", closeTroubleshootModal);
  if (els.troubleshootRefresh) els.troubleshootRefresh.addEventListener("click", refreshTroubleshootModal);
  if (els.troubleshootVerifyRuntime) els.troubleshootVerifyRuntime.addEventListener("click", verifyCodexRuntime);
  if (els.ccsKeyCancel) els.ccsKeyCancel.addEventListener("click", () => closeCcsKeyModal(""));
  if (els.ccsKeyConfirm) els.ccsKeyConfirm.addEventListener("click", confirmCcsKeyModal);
  if (els.updateModalBackground) els.updateModalBackground.addEventListener("click", hideUpdateModalToBackground);
  if (els.updateBackgroundOpen) els.updateBackgroundOpen.addEventListener("click", showUpdateModal);
  if (els.updateCancel) els.updateCancel.addEventListener("click", cancelDesktopUpdate);
  if (els.releaseNotesClose) els.releaseNotesClose.addEventListener("click", closeReleaseNotesModal);
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
  window.addEventListener("resize", scheduleModelListHeightSync);
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && els.ccsKeyModal && !els.ccsKeyModal.hidden) closeCcsKeyModal("");
    if (event.key === "Escape" && els.troubleshootModal && !els.troubleshootModal.hidden) closeTroubleshootModal();
    if (event.key === "Escape" && els.updateModal && !els.updateModal.hidden) hideUpdateModalToBackground();
    if (event.key === "Escape" && els.releaseNotesModal && !els.releaseNotesModal.hidden) closeReleaseNotesModal();
    if (event.key === "Escape") hideContextMenu();
  });
  document.addEventListener("visibilitychange", () => scheduleNextRefresh(0));
  window.addEventListener(CONFIG_CHANGED_EVENT, () => scheduleExternalConfigSync());
  if (els.toolConfigList) {
    els.toolConfigList.addEventListener("input", handleConfigInput);
    els.toolConfigList.addEventListener("change", handleConfigInput);
    els.toolConfigList.addEventListener("focusout", handleConfigInput);
  }
  [
    els.autoStart,
    els.uiLanguage,
    els.deepseekBaseUrl,
    els.proxyPort,
    ...billingInputs(),
  ].forEach((input) => {
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
  onRadioChange("DEEPSEEK_TEMPERATURE_PRESET", handleConfigInput);
  onRadioChange("DEEPSEEK_THINKING", handleConfigInput);
  onRadioChange("DEEPSEEK_TRANSPORT", handleConfigInput);
  onRadioChange("NETWORK_PROXY_MODE", handleConfigInput);
  onRadioChange("LOG_RETENTION_DAYS", handleConfigInput);
  onRadioChange("UI_CLOSE_BEHAVIOR", handleConfigInput);
  onRadioChange("EXPERIMENT_REASONING_SUMMARY_MODE", handleConfigInput);
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
  if (els.aboutStatus) {
    els.aboutStatus.addEventListener("click", handleAboutStatusClick);
  }

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
  const target = contextSelectionRoot(contextMenuTarget);
  if (!target) return;
  const range = document.createRange();
  range.selectNodeContents(target);
  const selection = window.getSelection();
  selection.removeAllRanges();
  selection.addRange(range);
}

function contextSelectionRoot(target) {
  const pageSelector = ".dashboard-panel, .usage-page, .log-page, .config-page, .about-page";
  const clickedPage = target && target.closest ? target.closest(pageSelector) : null;
  if (clickedPage && isVisibleElement(clickedPage)) return clickedPage;
  const activePage = Array.from(document.querySelectorAll(`.workspace ${pageSelector}`)).find(isVisibleElement);
  if (activePage && isVisibleElement(activePage)) return activePage;
  return els.workspace || document.querySelector(".workspace");
}

function isVisibleElement(element) {
  if (!(element instanceof Element)) return false;
  const style = window.getComputedStyle(element);
  return style.display !== "none" && style.visibility !== "hidden" && element.getClientRects().length > 0;
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

async function refreshCatalogDocument() {
  const button = els.catalogRefreshButton;
  if (!button || button.disabled) return;
  const knownSlugs = new Set(catalogModels().map((model) => model.slug));
  button.disabled = true;
  if (els.catalogRefreshLabel) els.catalogRefreshLabel.textContent = t("catalogFetching");
  try {
    const data = await apiJson("/api/catalog/refresh", { method: "POST", cache: "no-store" });
    if (data && data.ok === false) throw new Error(String(data.error || "catalog_refresh_failed"));
    const config = await loadConfig({ render: false }).catch(() => null);
    if (config) applyCatalogPayload(config.CATALOG, config.CATALOG_STATUS);
    const added = catalogModels().filter((model) => !knownSlugs.has(model.slug)).length;
    currentBillingRatesSignature = "";
    renderBillingCatalog();
    flashCatalogLabel(els.catalogRefreshLabel, "catalogFetchModels", added > 0 ? `${t("catalogFetchAdded")} +${added}` : t("catalogFetchUpToDate"));
  } catch (error) {
    flashCatalogLabel(els.catalogRefreshLabel, "catalogFetchModels", t("catalogFetchFailed"));
  } finally {
    button.disabled = false;
  }
}

/// Briefly replaces a button label, then restores its localized text.
function flashCatalogLabel(label, key, text) {
  if (!label) return;
  const pending = catalogLabelTimers.get(label);
  if (pending) clearTimeout(pending);
  label.textContent = text;
  catalogLabelTimers.set(label, setTimeout(() => {
    catalogLabelTimers.delete(label);
    label.textContent = t(key);
  }, CATALOG_LABEL_FLASH_MS));
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
    configSaveConflictRetries = 0;
    await syncDesktopConfig(payload, previousConfig).catch(() => {});
    await loadConfig();
    await loadCodexAdapter().catch(() => {});
    if (toolsLoaded) await loadTools();
    await refresh({ forceLogs: true, force: true });
    if (currentView === "usage") await refreshUsage({ force: true });
  } catch (error) {
    // A save can lose a race with any other writer of config.toml (the app own
    // tray actions, another instance, or a manual edit). The server rejects
    // a stale revision with 409 instead of overwriting, so refresh the revision and
    // resend the same payload exactly once before surfacing the failure.
    if (error && error.status === 409 && configSaveConflictRetries < 1) {
      configSaveConflictRetries += 1;
      renderConfigSaveState("pending");
      try {
        await loadConfig();
        pendingConfig = payload;
        scheduleConfigSave(CONFIG_AUTOSAVE_RETRY_MS);
        return;
      } catch (reloadError) {
        // Fall through to the error state below.
      }
    }
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
  try {
    await listen(UPDATE_PROGRESS_EVENT, (event) => {
      handleUpdateProgressEvent(event && event.payload ? event.payload : {});
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

function resolveApiBaseUrl() {
  if (apiBaseUrl === null) apiBaseUrl = defaultApiBaseUrl();
  return apiBaseUrl;
}

function apiUrl(url) {
  const value = String(url || "");
  if (!isApiRequestUrl(value) || /^https?:\/\//i.test(value)) return value;
  const base = resolveApiBaseUrl();
  return base ? base + value : value;
}

async function apiFetch(url, options = {}) {
  if (isTauriRuntime() && isApiRequestUrl(url)) {
    return desktopManagerFetch(url, options);
  }
  const target = apiUrl(url);
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
    const jsonBody = parseJsonOrNull(body);
    const preview = body ? " " + body.slice(0, 180).replace(/\s+/g, " ") : "";
    const error = new Error(`${url} failed: HTTP ${response.status}${preview}`);
    error.endpoint = String(url || "");
    error.targetUrl = response.codeseexTargetUrl || "";
    error.status = response.status;
    error.responseBody = jsonBody;
    error.serverMessage = jsonBody && typeof jsonBody.message === "string"
      ? jsonBody.message
      : jsonBody && typeof jsonBody.error === "string"
        ? jsonBody.error
        : "";
    throw error;
  }
  return response.json();
}

function parseJsonOrNull(value) {
  try {
    return JSON.parse(String(value || ""));
  } catch {
    return null;
  }
}

function actionErrorMessage(fallback, error) {
  const detail = error && error.serverMessage
    ? error.serverMessage
    : error && error.message
      ? error.message
      : "";
  if (!detail) return fallback;
  const compact = String(detail).replace(/\s+/g, " ").trim();
  const suffix = compact.length > 220 ? compact.slice(0, 217) + "..." : compact;
  return `${fallback} ${suffix}`;
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
  let desktopError = null;
  if (isTauriRuntime()) {
    try {
      latestUpdateCheck = await desktopInvoke("desktop_check_update");
      renderUpdateState({ silent: Boolean(options.silent) });
      return latestUpdateCheck;
    } catch (error) {
      desktopError = error && error.message ? error.message : String(error);
    }
  }
  try {
    latestUpdateCheck = await apiJson("/api/update-check", { cache: "no-store" });
    if (desktopError && latestUpdateCheck && typeof latestUpdateCheck === "object") {
      latestUpdateCheck.desktop_updater_error = desktopError;
      latestUpdateCheck.installable = false;
      if (!latestUpdateCheck.has_update) {
        latestUpdateCheck.ok = false;
        latestUpdateCheck.error = desktopError;
      }
    }
  } catch (error) {
    latestUpdateCheck = {
      ok: false,
      has_update: false,
      installable: false,
      error: desktopError || error.message || String(error),
    };
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
      ? normalizeLanguageManifest(manifest.languages)
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
  els.startButton.disabled = busy || latestStarting;
  els.stopButton.disabled = busy || (!latestRunning && !latestStarting);
  if (els.launchCodexButton) els.launchCodexButton.disabled = busy || !latestRunning;
  if (els.startButtonIcon) {
    els.startButtonIcon.classList.toggle("btn-icon-play", !latestRunning);
    els.startButtonIcon.classList.toggle("btn-icon-refresh", latestRunning);
  }
  setIconButtonLabel(els.startButton, latestRunning ? t("restart") : t("start"));
  setIconButtonLabel(els.stopButton, t("stop"));
}

/// 纯图标按钮没有可见文字，用 title / aria-label 承载可访问名称。
function setIconButtonLabel(button, label) {
  if (!button) return;
  button.setAttribute("aria-label", label);
  button.setAttribute("title", label);
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
  setRadioValue("DEEPSEEK_TEMPERATURE_PRESET", normalizeTemperaturePreset(config.DEEPSEEK_TEMPERATURE_PRESET));
  setRadioValue("DEEPSEEK_TRANSPORT", normalizeUpstreamTransport(config.DEEPSEEK_TRANSPORT));
  latestWebSearchBackend = normalizeWebSearchBackend(config.WEB_SEARCH_BACKEND);
  setRadioValue("WEB_SEARCH_BACKEND", latestWebSearchBackend);
  // The upstream model picker is gone; the value is carried through unchanged so
  // a save never rewrites a model the user configured elsewhere.
  // TODO(upstream-model-picker): re-attach a control for this once designed.
  latestUpstreamModelOverride = config.UPSTREAM_MODEL_OVERRIDE == null
    ? null
    : String(config.UPSTREAM_MODEL_OVERRIDE);
  latestUpstreamModelChoices = Array.isArray(config.UPSTREAM_MODEL_CHOICES)
    ? config.UPSTREAM_MODEL_CHOICES.map((value) => String(value))
    : [];
  setRadioValue("NETWORK_PROXY_MODE", normalizeNetworkProxyMode(config.NETWORK_PROXY_MODE || config.WEB_SEARCH_PROXY_MODE));
  setRadioValue("LOG_RETENTION_DAYS", normalizeRetentionDays(config.LOG_RETENTION_DAYS));
  setRadioValue("UI_CLOSE_BEHAVIOR", normalizeCloseBehavior(config.UI_CLOSE_BEHAVIOR));
  const nextTheme = config.UI_THEME || "system";
  setRadioValue("UI_THEME", nextTheme);
  if (els.autoStart) els.autoStart.checked = isTruthy(config.AUTO_START || "false");
  if (els.codexAppModelListInjection) els.codexAppModelListInjection.checked = config.CODEX_APP_MODEL_LIST_INJECTION !== "false";
  setRadioValue(
    "EXPERIMENT_REASONING_SUMMARY_MODE",
    normalizeReasoningSummaryMode(config.EXPERIMENT_REASONING_SUMMARY_MODE),
  );
  if (els.deepseekBaseUrl && document.activeElement !== els.deepseekBaseUrl) els.deepseekBaseUrl.value = normalizeDeepSeekBaseUrl(config.DEEPSEEK_BASE_URL || "");
  if (document.activeElement !== els.proxyPort) els.proxyPort.value = normalizePort(config.PROXY_PORT || "8787");
  const nextLanguage = normalizeConfiguredLanguageId(config.UI_LANGUAGE || DEFAULT_LANGUAGE);
  if (document.activeElement !== els.uiLanguage) els.uiLanguage.value = nextLanguage;
  applyCatalogPayload(config.CATALOG, config.CATALOG_STATUS);
  setBillingInputValues(config);
  currentAdapterSignature = "";
  applyTheme(nextTheme);
  if (resolveLanguageId(nextLanguage) !== uiLanguage || nextLanguage !== configuredLanguage) applyLanguage(nextLanguage);
  lastSavedConfig = normalizeConfigPayload(config);
  lastUsageSignature = "";
  if (!restartRequired) renderConfigSaveState("clean");
  renderCodexAdapter(latestAdapter || {});
  renderAboutStats();
}

function renderCodexAdapter(adapter) {
  latestAdapter = adapter || {};
  const signature = stableStringify({
    adapter: latestAdapter,
    model: latestUpstreamModelOverride,
  });
  if (signature === currentAdapterSignature) return;
  currentAdapterSignature = signature;
  const toml = String(latestAdapter.toml_snippet || "");
  renderConfigToml(toml || "-");
  renderCatalogNotice(latestAdapter.catalog_diagnostic || null);
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
    setConfigTomlActionStatus(t("ccsImportStartedCatalogWarning"), { warning: true, timeout: 5200 });
  } catch {
    setConfigTomlActionStatus(t("ccsImportFailed"), { warning: true, timeout: 2600 });
  }
}

async function launchCodexApp() {
  if (busy) return;
  const inject = Boolean(els.codexAppModelListInjection && els.codexAppModelListInjection.checked);
  setBusy(true, t("codexLaunchTitle"), t(inject ? "codexLaunchExperimentalDetail" : "codexLaunchDetail"));
  try {
    const result = await apiJson("/api/codex-app/launch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ inject }),
    });
    const injection = result && result.injection;
    const injectionEnabled = Boolean(injection && injection.enabled);
    const injectionOk = Boolean(injection && injection.ok);
    const statusKey = injectionEnabled && !injectionOk
      ? "codexLaunchStartedWithInjectionWarning"
      : injectionEnabled
        ? "codexLaunchStartedWithInjection"
        : "codexLaunchStarted";
    setConfigTomlActionStatus(t(statusKey), {
      warning: injectionEnabled && !injectionOk,
      timeout: injectionEnabled && !injectionOk ? 5200 : 2600,
    });
    await refresh({ forceLogs: true, force: true });
  } catch (error) {
    setConfigTomlActionStatus(actionErrorMessage(t("codexLaunchFailed"), error), { warning: true, timeout: 9000 });
    await refresh({ forceLogs: true, force: true }).catch(() => {});
  } finally {
    setBusy(false);
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

async function verifyCodexRuntime() {
  if (codexRuntimeVerificationInFlight) return;
  codexRuntimeVerificationInFlight = true;
  latestCatalogRuntimeDiagnostic = { status: "checking", issue: "checking" };
  renderTroubleshootModal();
  renderCatalogNotice(latestAdapter && latestAdapter.catalog_diagnostic ? latestAdapter.catalog_diagnostic : null);
  if (els.troubleshootVerifyRuntime) els.troubleshootVerifyRuntime.disabled = true;
  try {
    const data = await apiJson("/api/codex-adapter/runtime", { cache: "no-store" });
    latestCatalogRuntimeDiagnostic = data && data.runtime_diagnostic
      ? data.runtime_diagnostic
      : { status: "unavailable", issue: "empty_runtime_diagnostic" };
    await refreshLatestLogs({ force: true }).catch(() => {});
  } catch (error) {
    latestCatalogRuntimeDiagnostic = {
      status: "unavailable",
      issue: "runtime_probe_failed",
      message: error && error.message ? error.message : String(error || ""),
    };
  } finally {
    codexRuntimeVerificationInFlight = false;
    if (els.troubleshootVerifyRuntime) els.troubleshootVerifyRuntime.disabled = false;
    renderTroubleshootModal();
    renderCatalogNotice(latestAdapter && latestAdapter.catalog_diagnostic ? latestAdapter.catalog_diagnostic : null);
  }
}

function renderTroubleshootModal() {
  if (!els.troubleshootSummary || !els.troubleshootActions) return;
  const status = latestStatus || {};
  const runtime = status.runtime || {};
  const catalog = latestAdapter && latestAdapter.catalog_diagnostic ? latestAdapter.catalog_diagnostic : null;
  const runtimeCatalog = latestCatalogRuntimeDiagnostic;
  const hasStatus = Boolean(latestStatus && latestStatus.ok !== undefined);
  const statusOk = hasStatus && status.ok !== false;
  const port = statusOk
    ? (runtime.port || latestRuntimePort || (lastSavedConfig && lastSavedConfig.PROXY_PORT) || "8787")
    : ((lastSavedConfig && lastSavedConfig.PROXY_PORT) || "8787");
  const baseUrl = statusOk
    ? (status.base_url || (latestAdapter && latestAdapter.base_url) || DEFAULT_CCS_ENDPOINT)
    : DEFAULT_CCS_ENDPOINT;
  const processLabel = statusOk ? (status.process_label || t("proxyPid")) : t("proxyPid");
  const proxyState = latestRunning ? "ok" : (latestStarting ? "checking" : "error");
  const catalogState = catalogStatusClass(catalog);
  const runtimeState = catalogRuntimeStatusClass(runtimeCatalog);
  els.troubleshootSummary.replaceChildren(
    troubleshootOverview({
      state: proxyState,
      title: latestRunning ? t("troubleshootProxyRunning") : (latestStarting ? t("troubleshootProxyStarting") : t("troubleshootProxyStopped")),
      meta: `${t("logPort")} ${port}${statusOk && status.pid ? ` · ${processLabel} ${status.pid}` : ""}`,
    }),
    troubleshootDiagnosticGrid([
      {
        title: t("troubleshootProxyService"),
        value: latestRunning ? t("running") : (latestStarting ? t("starting") : t("stopped")),
        detail: `${t("troubleshootBaseUrl")} ${baseUrl}`,
        state: proxyState,
      },
      {
        title: t("troubleshootCodexConfig"),
        value: catalogStatusLabel(catalog),
        detail: catalogSummaryDetail(catalog),
        state: catalogState,
      },
      {
        title: t("troubleshootRuntimeCatalog"),
        value: catalogRuntimeStatusLabel(runtimeCatalog),
        detail: catalogRuntimeSummaryDetail(runtimeCatalog),
        state: runtimeState,
      },
    ]),
    troubleshootTechnicalDetails([
      [t("configTomlTitle"), codexConfigPathHint()],
      [t("troubleshootCatalogPath"), catalog ? (catalog.path || "-") : (statusOk ? (status.catalog_path || (latestAdapter && latestAdapter.catalog_path) || "-") : "-")],
      [t("troubleshootCatalogModels"), catalogModelsLabel(catalog)],
      [t("troubleshootCatalogToml"), catalogTomlLabel(catalog)],
      [t("troubleshootRuntimeCatalog"), catalogRuntimeTechnicalLabel(runtimeCatalog)],
      [t("troubleshootBaseUrl"), baseUrl],
    ]),
  );

  const actions = troubleshootActions(status, runtime, catalog, runtimeCatalog);
  els.troubleshootActions.replaceChildren(...actions.map((text) => {
    const item = document.createElement("div");
    item.className = "troubleshoot-action";
    item.textContent = text;
    return item;
  }));
}

function troubleshootOverview(options) {
  const wrap = document.createElement("div");
  wrap.className = ["troubleshoot-overview", options.state ? `is-${options.state}` : ""].filter(Boolean).join(" ");
  const dot = document.createElement("span");
  dot.className = "troubleshoot-overview-dot";
  dot.setAttribute("aria-hidden", "true");
  const text = document.createElement("div");
  text.className = "troubleshoot-overview-text";
  const title = document.createElement("strong");
  title.textContent = options.title || "-";
  const meta = document.createElement("span");
  meta.textContent = options.meta || "";
  text.append(title, meta);
  wrap.append(dot, text);
  return wrap;
}

function troubleshootDiagnosticGrid(items) {
  const grid = document.createElement("div");
  grid.className = "troubleshoot-diagnostic-grid";
  for (const item of items) grid.append(troubleshootDiagnosticItem(item));
  return grid;
}

function troubleshootDiagnosticItem(item) {
  const row = document.createElement("section");
  row.className = ["troubleshoot-diagnostic-item", item.state ? `is-${item.state}` : ""].filter(Boolean).join(" ");
  const header = document.createElement("div");
  header.className = "troubleshoot-diagnostic-header";
  const title = document.createElement("span");
  title.textContent = item.title || "-";
  const badge = document.createElement("strong");
  badge.textContent = item.value || "-";
  header.append(title, badge);
  const detail = document.createElement("p");
  detail.textContent = item.detail || "";
  row.append(header, detail);
  return row;
}

function troubleshootTechnicalDetails(rows) {
  const details = document.createElement("details");
  details.className = "troubleshoot-technical";
  details.open = troubleshootTechnicalOpen;
  details.addEventListener("toggle", () => {
    troubleshootTechnicalOpen = details.open;
  });
  const summary = document.createElement("summary");
  summary.textContent = t("troubleshootTechnicalDetails");
  const body = document.createElement("div");
  body.className = "troubleshoot-technical-body";
  for (const [label, value] of rows) body.append(troubleshootTechnicalRow(label, value));
  details.append(summary, body);
  return details;
}

function troubleshootTechnicalRow(label, value) {
  const row = document.createElement("div");
  row.className = "troubleshoot-technical-row";
  const key = document.createElement("span");
  key.textContent = label;
  const val = document.createElement("strong");
  val.textContent = value || "-";
  row.append(key, val);
  return row;
}

function troubleshootActions(status, runtime, catalog, runtimeCatalog) {
  const actions = [];
  const activeRequests = Number(runtime && runtime.active_requests || 0);
  const catalogState = catalogStatusClass(catalog);
  const runtimeState = catalogRuntimeStatusClass(runtimeCatalog);
  if (catalogState === "error" || catalogState === "warning") actions.push(t("troubleshootActionCatalogProblem"));
  if (catalogState === "repaired") actions.push(t("troubleshootActionCatalogRepaired"));
  if (runtimeState === "error") actions.push(t("troubleshootActionRuntimeCatalogProblem"));
  if (runtimeState === "warning") actions.push(t("troubleshootActionRuntimeCatalogUnavailable"));
  if (latestStarting) actions.push(t("troubleshootActionWaitStartup"));
  if (!latestRunning && !latestStarting) actions.push(t("troubleshootActionStartProxy"));
  if (latestRunning && activeRequests > 0) actions.push(t("troubleshootActionActiveRequests").replace("{count}", formatNumber(activeRequests)));
  if (latestRunning && hasSavedRestartRequiredChanges()) actions.push(t("troubleshootActionRestartNeeded"));
  if (!latestRunning) actions.push(t("troubleshootActionPortConflict"));
  if (!status || !status.ok) actions.push(t("troubleshootActionRefreshStatus"));
  if (actions.length === 0) actions.push(t("troubleshootActionHealthy"));
  return actions;
}

function renderCatalogNotice(diagnostic) {
  if (!els.catalogNotice) return;
  const state = catalogStatusClass(diagnostic);
  const runtimeState = catalogRuntimeStatusClass(latestCatalogRuntimeDiagnostic);
  if (runtimeState === "error") {
    els.catalogNotice.textContent = t("catalogNoticeRuntimeError");
    els.catalogNotice.className = "catalog-notice is-error";
    els.catalogNotice.hidden = false;
    return;
  }
  if (!diagnostic || state === "unknown" || state === "ok") {
    els.catalogNotice.hidden = true;
    els.catalogNotice.textContent = "";
    els.catalogNotice.className = "catalog-notice";
    return;
  }
  const key = state === "repaired"
    ? "catalogNoticeRepaired"
    : state === "error"
      ? "catalogNoticeError"
      : "catalogNoticeWarning";
  els.catalogNotice.textContent = t(key);
  els.catalogNotice.className = `catalog-notice is-${state}`;
  els.catalogNotice.hidden = false;
}

function catalogRuntimeStatusClass(diagnostic) {
  const status = String(diagnostic && diagnostic.status || "").toLowerCase();
  if (!diagnostic) return "unknown";
  if (status === "checking") return "checking";
  if (status === "ok") return "ok";
  if (status === "error") return "error";
  if (status === "unavailable") return "warning";
  if (status === "warning") return "warning";
  return "warning";
}

function catalogRuntimeStatusLabel(diagnostic) {
  if (!diagnostic) return t("catalogRuntimeNotChecked");
  const status = String(diagnostic.status || "").toLowerCase();
  if (status === "checking") return t("catalogRuntimeStatusChecking");
  if (status === "ok") return t("catalogRuntimeStatusOk");
  if (status === "error") return t("catalogRuntimeStatusError");
  if (status === "unavailable") return t("catalogRuntimeStatusUnavailable");
  return t("catalogRuntimeStatusWarning");
}

function catalogRuntimeSummaryDetail(diagnostic) {
  if (!diagnostic) return t("catalogRuntimeHint");
  const status = String(diagnostic.status || "").toLowerCase();
  if (status === "checking") return t("catalogRuntimeCheckingHint");
  if (status === "ok") return t("catalogRuntimeVerifiedHint");
  if (status === "error") return t("catalogRuntimeErrorHint");
  if (status === "unavailable") return t("catalogRuntimeUnavailableHint");
  return t("catalogRuntimeWarningHint");
}

function catalogRuntimeTechnicalLabel(diagnostic) {
  if (!diagnostic) return t("catalogRuntimeNotChecked");
  const issue = String(diagnostic.issue || "none");
  const pathNote = String(diagnostic.path_note || "").trim();
  const duration = diagnostic.duration_ms !== undefined ? ` · ${formatNumber(diagnostic.duration_ms)} ms` : "";
  return `${catalogRuntimeStatusLabel(diagnostic)} · ${issue}${pathNote ? ` · ${pathNote}` : ""}${duration}`;
}

function catalogSummaryDetail(diagnostic) {
  if (!diagnostic) return t("catalogNotCheckedHint");
  const parts = [];
  parts.push(catalogTomlLabel(diagnostic));
  const count = Number(diagnostic.model_count || 0);
  if (count > 0) parts.push(t("catalogModelCount").replace("{count}", formatNumber(count)));
  return parts.join(" · ");
}

function catalogStatusClass(diagnostic) {
  const status = String(diagnostic && diagnostic.status || "").toLowerCase();
  if (!diagnostic) return "unknown";
  if (status === "ok") return "ok";
  if (status === "repaired") return "repaired";
  if (status === "error") return "error";
  if (status === "warning") return "warning";
  return "warning";
}

function catalogStatusLabel(diagnostic) {
  if (!diagnostic) return "-";
  const state = catalogStatusClass(diagnostic);
  if (state === "ok") return t("catalogStatusOk");
  if (state === "repaired") return t("catalogStatusRepaired");
  if (state === "error") return t("catalogStatusError");
  return t("catalogStatusWarning");
}

function catalogModelsLabel(diagnostic) {
  if (!diagnostic) return "-";
  const count = Number(diagnostic.model_count || 0);
  const models = Array.isArray(diagnostic.models) ? diagnostic.models.filter(Boolean) : [];
  if (models.length > 0) return `${formatNumber(count)} - ${models.join(", ")}`;
  return formatNumber(count);
}

function catalogTomlOk(diagnostic) {
  return Boolean(diagnostic && diagnostic.toml_has_model_catalog_json && diagnostic.toml_catalog_path_matches);
}

function catalogTomlLabel(diagnostic) {
  if (!diagnostic) return "-";
  if (!diagnostic.toml_has_model_catalog_json) return t("catalogTomlMissing");
  if (!diagnostic.toml_catalog_path_matches) return t("catalogTomlMismatch");
  return t("catalogTomlOk");
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
  const endpoint = parseTomlStringValue(toml, "base_url") || DEFAULT_CCS_ENDPOINT;
  const model = parseTomlStringValue(toml, "model") || catalogState.defaultModel || DEFAULT_CCS_MODEL;
  const config = {
    auth: { OPENAI_API_KEY: apiKey },
    config: String(toml || ""),
    modelCatalog: { models: ccsModelCatalogModels() },
  };
  const params = new URLSearchParams({
    resource: "provider",
    app: "codex",
    name: appInfo && appInfo.product_name ? appInfo.product_name : "CodeSeeX",
    endpoint,
    model,
    config: utf8Base64(JSON.stringify(config)),
    configFormat: "json",
  });
  if (apiKey) params.set("apiKey", apiKey);
  return CCS_IMPORT_URL + "?" + params.toString();
}

function ccsModelCatalogModels() {
  const known = new Map();
  for (const entry of catalogModels()) {
    const model = String((entry && entry.slug) || "").trim();
    if (!model || known.has(model)) continue;
    known.set(model, {
      model,
      displayName: String((entry && entry.display_name) || model),
      contextWindow: Number(entry && entry.context_window) || DEFAULT_CCS_CONTEXT_WINDOW,
    });
  }
  const adapterModels = Array.isArray(latestAdapter && latestAdapter.models) ? latestAdapter.models : [];
  for (const slug of adapterModels) {
    const model = String(slug || "").trim();
    if (!model || known.has(model)) continue;
    known.set(model, { model, displayName: model, contextWindow: DEFAULT_CCS_CONTEXT_WINDOW });
  }
  return Array.from(known.values());
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
  updateUpdateButtonState(hasUpdate);
  if (!els.aboutStatus || !latestUpdateCheck || options.silent) return;

  if (updateInstallInProgress) {
    setAboutStatus(t("installingUpdate"), false);
  } else if (hasUpdate) {
    setAboutStatus(renderUpdateAvailableMessage(latestUpdateCheck), false, { html: true });
  } else if (latestUpdateCheck.ok) {
    setAboutStatus(updateMessage("updateCurrent", latestUpdateCheck), false);
  } else {
    setAboutStatus(updateMessage("updateCheckFailed", latestUpdateCheck), true);
  }
}

function updateUpdateButtonState(hasUpdate = Boolean(latestUpdateCheck && latestUpdateCheck.has_update)) {
  if (!els.updateButton) return;
  const label = els.updateButton.querySelector("[data-update-button-label]");
  const installable = Boolean(latestUpdateCheck && latestUpdateCheck.installable);
  const key = updateInstallInProgress ? "installingUpdate" : (hasUpdate && installable ? "installUpdate" : "checkUpdate");
  if (label) label.textContent = t(key);
  els.updateButton.disabled = updateInstallInProgress;
}

function updateNoticeVersion(data = latestUpdateCheck) {
  return String(data && (data.latest_version || data.current_version) || "").trim();
}

function isUpdateNoticeSeen(data = latestUpdateCheck) {
  const version = updateNoticeVersion(data);
  return Boolean(version && updateNoticeSeenVersion === version);
}

function markUpdateNoticeSeen() {
  const version = updateNoticeVersion();
  if (!version) return;
  updateNoticeSeenVersion = version;
  renderUpdateState({ silent: true });
}

function renderUpdateAvailableMessage(data = {}) {
  const url = data.url || (appInfo && appInfo.urls && appInfo.urls.releases) || "";
  const version = data.latest_version || data.current_version || "-";
  const prefix = t("updateAvailablePrefix");
  if (data.installable) return updateMessage("updateAvailableInstallable", data);
  if (!url) return updateMessage("updateAvailable", data);
  return `${escapeHtml(prefix)} <a href="${escapeHtml(url)}" data-update-link="true" target="_blank" rel="noopener">${escapeHtml(version)}</a>`;
}

function updateMessage(key, data = {}) {
  return t(key)
    .replace("{version}", data.latest_version || data.current_version || "-")
    .replace("{current}", data.current_version || "-")
    .replace("{error}", data.error || t("unknownError"));
}

function showUpdateModal() {
  updateProgressState.visible = true;
  updateProgressState.background = false;
  renderUpdateProgress();
}

function hideUpdateModalToBackground() {
  if (!updateProgressState.active) {
    if (els.updateModal) els.updateModal.hidden = true;
    return;
  }
  updateProgressState.visible = false;
  updateProgressState.background = true;
  renderUpdateProgress();
}

function handleUpdateProgressEvent(payload = {}) {
  const stage = String(payload.stage || "downloading").trim() || "downloading";
  const terminal = stage === "failed" || stage === "canceled" || stage === "restarting";
  updateProgressState = {
    active: stage !== "canceled",
    background: updateProgressState.background && stage !== "failed" && stage !== "restarting",
    visible: (updateProgressState.visible || !updateProgressState.background) && stage !== "canceled" && stage !== "restarting",
    stage,
    version: String(payload.version || updateProgressState.version || updateNoticeVersion() || ""),
    downloaded: Number(payload.downloaded || 0),
    contentLength: payload.content_length === undefined || payload.content_length === null ? updateProgressState.contentLength : Number(payload.content_length),
    percent: payload.percent === undefined || payload.percent === null ? null : Number(payload.percent),
    error: String(payload.error || ""),
  };
  updateInstallInProgress = !terminal && updateProgressState.active;
  if (stage === "failed") {
    updateProgressState.visible = !updateProgressState.background;
    setAboutStatus(updateMessage("updateInstallFailed", { error: updateProgressState.error || t("unknownError") }), true);
  } else if (stage === "canceled") {
    setAboutStatus(t("updateCanceledTask"), false);
  } else if (stage === "restarting") {
    setAboutStatus(t("updateInstalledRestarting"), false);
  } else {
    setAboutStatus(updateStageLabel(stage), false);
  }
  renderUpdateState({ silent: true });
  renderUpdateProgress();
}

function renderUpdateProgress() {
  const state = updateProgressState || {};
  const stage = state.stage || "idle";
  const percent = updatePercentValue(state);
  const percentText = Number.isFinite(percent) ? `${Math.round(percent)}%` : "-";
  const task = updateStageLabel(stage);
  const status = updateStageState(stage);
  const canBackground = Boolean(state.active && !["installing", "failed", "canceled", "restarting"].includes(stage));
  const canCancel = Boolean(state.active && !["installing", "failed", "canceled", "restarting"].includes(stage));
  const isFailed = stage === "failed";

  if (els.updateModal) els.updateModal.hidden = !state.visible;
  if (els.updateModalTitle) els.updateModalTitle.textContent = t(isFailed ? "updateFailedTitle" : "updateDownloadingTitle");
  if (els.updateModalSubtitle) els.updateModalSubtitle.textContent = t(isFailed ? "updateFailedSubtitle" : "updateDownloadSubtitle");
  if (els.updateSizeText) els.updateSizeText.textContent = updateSizeText(state);
  if (els.updatePercentText) els.updatePercentText.textContent = percentText;
  if (els.updateProgressBar) {
    els.updateProgressBar.style.width = Number.isFinite(percent) ? `${percent}%` : "0%";
    els.updateProgressBar.classList.toggle("failed", isFailed);
  }
  if (els.updateTaskLabel) els.updateTaskLabel.textContent = task;
  if (els.updateTaskState) {
    els.updateTaskState.textContent = status.label;
    els.updateTaskState.className = `step-state ${status.className}`;
  }
  if (els.updateModalBackground) els.updateModalBackground.hidden = !canBackground;
  if (els.updateCancel) {
    els.updateCancel.disabled = !canCancel && !isFailed;
    els.updateCancel.textContent = t(isFailed ? "close" : "cancel");
  }

  const showBackground = Boolean(state.active && state.background && stage !== "canceled" && stage !== "restarting");
  if (els.updateBackgroundStatus) els.updateBackgroundStatus.hidden = !showBackground;
  if (els.updateBackgroundTask) els.updateBackgroundTask.textContent = task;
  if (els.updateBackgroundPercent) els.updateBackgroundPercent.textContent = percentText;
  if (els.updateBackgroundBar) {
    els.updateBackgroundBar.style.width = Number.isFinite(percent) ? `${percent}%` : "0%";
    els.updateBackgroundBar.classList.toggle("failed", isFailed);
  }
}

async function cancelDesktopUpdate() {
  if (updateProgressState.stage === "failed") {
    updateProgressState.active = false;
    updateProgressState.visible = false;
    updateProgressState.background = false;
    renderUpdateProgress();
    renderUpdateState({ silent: true });
    return;
  }
  if (!updateProgressState.active) {
    if (els.updateModal) els.updateModal.hidden = true;
    return;
  }
  try {
    await desktopInvoke("desktop_cancel_update");
  } catch (error) {
    handleUpdateProgressEvent({
      stage: "failed",
      version: updateProgressState.version,
      downloaded: updateProgressState.downloaded,
      content_length: updateProgressState.contentLength,
      error: error && error.message ? error.message : String(error),
    });
  }
}

function updateStageLabel(stage) {
  const key = {
    starting: "updateStartingTask",
    downloading: "updateDownloadTask",
    verifying: "updateVerifyingTask",
    installing: "updateInstallingTask",
    failed: "updateFailedTask",
    restarting: "updateRestartingTask",
    canceled: "updateCanceledTask",
  }[stage] || "updateDownloadTask";
  return t(key);
}

function updateStageState(stage) {
  if (stage === "failed") return { label: t("updateStateFailed"), className: "failed" };
  if (stage === "canceled") return { label: t("updateStateDone"), className: "done" };
  if (stage === "restarting" || stage === "installing" || stage === "verifying") {
    return { label: t("updateStateRunning"), className: "active" };
  }
  return { label: t("updateStateRunning"), className: "active" };
}

function updateSizeText(state) {
  const downloaded = Number(state.downloaded || 0);
  const total = Number(state.contentLength || 0);
  if (total > 0) return `${formatBytes(downloaded)} / ${formatBytes(total)}`;
  if (downloaded > 0) return formatBytes(downloaded);
  return t("updateStateWaiting");
}

function updatePercentValue(state) {
  const stage = state.stage || "idle";
  if (stage === "failed") return Number.isFinite(Number(state.percent)) ? Number(state.percent) : 0;
  if (stage === "verifying" || stage === "installing" || stage === "restarting") return 100;
  const explicit = Number(state.percent);
  if (Number.isFinite(explicit)) return Math.max(0, Math.min(100, explicit));
  const downloaded = Number(state.downloaded || 0);
  const total = Number(state.contentLength || 0);
  if (total > 0) return Math.max(0, Math.min(100, (downloaded / total) * 100));
  return stage === "starting" ? 0 : null;
}

function formatBytes(value) {
  const bytes = Number(value || 0);
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let size = bytes;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const digits = unit === 0 || size >= 10 ? 0 : 1;
  return `${size.toFixed(digits)} ${units[unit]}`;
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
      width: field.width,
      valueKey: field.valueKey,
      visibleWhen: field.visibleWhen,
      options: (field.options || []).map((option) => option.value),
    })),
  })));
  currentTools = nextTools;
  if (!els.toolConfigList) return;
  if (signature !== currentToolsSignature) {
    currentToolsSignature = signature;
    els.toolConfigList.replaceChildren(...orderToolCards(nextTools).map(renderToolCard));
    rebuildToolConfigControlCache();
    applyToolFieldVisibility();
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
  const extraFields = toolCardExtraFields(tool);
  fields.forEach((field, index) => {
    if (index > 0) {
      const divider = settingDivider();
      divider.dataset.toolFieldDivider = "true";
      body.appendChild(divider);
    }
    body.appendChild(renderToolField(field));
  });
  extraFields.forEach((field, index) => {
    if (fields.length > 0 || index > 0) {
      const divider = settingDivider();
      divider.dataset.toolFieldDivider = "true";
      body.appendChild(divider);
    }
    body.appendChild(field);
  });
  if (fields.length > 0 || extraFields.length > 0) card.appendChild(body);
  return card;
}

/// Cards CodeSeeX owns that are not part of the tool's own schema but still
/// belong on the card rather than on a separate settings page.
function toolCardExtraFields(tool) {
  if (normalizeToolId(tool && tool.id) === "web_search") return [renderWebSearchBackendField()];
  return [];
}

function renderWebSearchBackendField() {
  const item = document.createElement("div");
  item.className = "setting-item";
  const labelWrap = document.createElement("span");
  const label = document.createElement("label");
  label.dataset.i18n = "webSearchBackend";
  label.textContent = t("webSearchBackend");
  const hint = document.createElement("small");
  hint.className = "muted";
  hint.dataset.i18n = "webSearchBackendHint";
  hint.textContent = t("webSearchBackendHint");
  labelWrap.append(label, hint);
  const control = document.createElement("div");
  control.className = "segmented-control compact-segmented-control";
  control.append(
    webSearchBackendOption("official", "webSearchBackend_official", "DeepSeek official"),
    webSearchBackendOption("local", "webSearchBackend_local", "CodeSeeX local"),
  );
  item.append(labelWrap, control);
  return item;
}

function webSearchBackendOption(value, labelKey, labelText) {
  const input = document.createElement("input");
  input.type = "radio";
  input.name = "WEB_SEARCH_BACKEND";
  input.id = `web_search_backend_${value}`;
  input.value = value;
  input.checked = value === latestWebSearchBackend;
  const label = document.createElement("label");
  label.htmlFor = input.id;
  label.dataset.i18n = labelKey;
  label.textContent = labelText;
  const fragment = document.createDocumentFragment();
  fragment.append(input, label);
  return fragment;
}

/// The three workspace tools are the least specific ones, so they are demoted
/// below everything else on the tools page without touching their definition.
const TOOL_CARD_TRAILING_IDS = ["list_directory", "read_file_range", "workspace_search"];

function orderToolCards(tools) {
  const leading = [];
  const trailing = [];
  for (const tool of Array.isArray(tools) ? tools : []) {
    const id = normalizeToolId(tool && tool.id);
    (TOOL_CARD_TRAILING_IDS.includes(id) ? trailing : leading).push(tool);
  }
  return [...leading, ...trailing];
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
  const source = (Array.isArray(labels) ? labels : []).filter(
    (label) => label && typeof label === "object",
  );
  // A card that already carries the System label does not need the Built-in one.
  const hasSystemLabel = source.some(
    (label) => String(label.id || "").trim() === "system",
  );
  const seen = new Set();
  const output = [];
  for (const label of source) {
    const id = String(label.id || label.label || "").trim();
    if (!id || seen.has(id)) continue;
    if (hasSystemLabel && id === "built_in") continue;
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
  if (field && field.visibleWhen && field.visibleWhen.key) {
    item.dataset.visibleWhenKey = String(field.visibleWhen.key);
    item.dataset.visibleWhenValue = String(field.visibleWhen.value || "");
  }

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
  } else if (field.type === "readonly") {
    item.appendChild(renderReadonlyField(field));
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
    input.className = `inline-control ${toolFieldWidthClass(field)}`.trim();
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
  select.className = `inline-control ${toolFieldWidthClass(field)}`.trim();
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
  textarea.className = `inline-control ${toolFieldWidthClass(field)}`.trim();
  textarea.name = field.key;
  textarea.rows = 3;
  textarea.value = field.value || field.defaultValue || "";
  textarea.placeholder = translateToolText(field.placeholderKey, field.placeholder || "");
  return textarea;
}

function renderReadonlyField(field) {
  const output = document.createElement("output");
  output.className = `inline-control tool-readonly-field ${toolFieldWidthClass(field)}`.trim();
  output.textContent = translateToolText(field.valueKey, field.value || field.defaultValue || "-");
  return output;
}

function renderPasswordField(field) {
  const wrap = document.createElement("div");
  wrap.className = `tool-secret-field ${toolFieldWidthClass(field)}`.trim();
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

function toolFieldWidthClass(field) {
  const width = String(field && field.width || "").trim().toLowerCase();
  if (width === "wide" || width === "compact") return `tool-field-${width}`;
  const key = String(field && field.key || "").toLowerCase();
  const type = String(field && field.type || "").toLowerCase();
  if (type === "password" || /(api[_-]?key|token|secret|password|credential)/.test(key)) return "tool-field-wide";
  if (/(url|uri|endpoint|base[_-]?url|host|proxy|path)/.test(key)) return "tool-field-wide";
  if (/(^|[_-])model($|[_-])/.test(key)) return "tool-field-compact";
  return "";
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
    if (field.type === "readonly") continue;
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
  applyToolFieldVisibility();
}

function applyToolFieldVisibility() {
  if (!els.toolConfigList) return;
  els.toolConfigList.querySelectorAll(".tool-card-body").forEach((body) => {
    const rows = Array.from(body.querySelectorAll(":scope > .setting-item"));
    for (const row of rows) {
      const key = row.dataset.visibleWhenKey;
      if (!key) {
        row.hidden = false;
        continue;
      }
      const expected = row.dataset.visibleWhenValue || "";
      const actual = getToolRadioValue(key) || String(toolFieldInput(key) && toolFieldInput(key).value || "");
      row.hidden = actual !== expected;
    }
    let hasVisibleRow = false;
    for (const child of Array.from(body.children)) {
      if (child.matches(".setting-item")) {
        if (!child.hidden) hasVisibleRow = true;
        continue;
      }
      if (!child.matches("[data-tool-field-divider]")) continue;
      let next = child.nextElementSibling;
      while (next && !next.matches(".setting-item")) next = next.nextElementSibling;
      child.hidden = !hasVisibleRow || !next || next.hidden;
    }
  });
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
    if (field.type === "readonly") continue;
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
  /* 模型列表的行高只在可见时才能测量 */
  if (currentConfigTab === "proxy") requestAnimationFrame(updateBillingModelListHeight);
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
    ? sumCosts(runtime.billing_buckets)
    : sumCosts(billable);

  els.usageTotalTurns.textContent = formatNumber(totalTurnsCount);
  els.usageCacheHitRate.textContent = cacheHitRate;
  els.usageCacheHitRate.className = ["usage-metric-value", "selectable", usageCacheToneClass(totalCached, totalMiss)].filter(Boolean).join(" ");
  els.usageAverageMs.textContent = formatDuration(avgMs);
  els.usageTotalCost.textContent = formatCostOrUnpriced(totalCostVal);
  els.usageTotalCost.className = ["usage-metric-value", "selectable", "usage-cost-value", usageCostToneClass({
    billing_buckets: runtime.billing_buckets,
    rows: billable,
  })].filter(Boolean).join(" ");
  els.usageTotalCost.title = usageCostTitle({
    billing_buckets: runtime.billing_buckets,
    rows: billable,
  });
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
  const totalCost = formatCostOrUnpriced(costForSession(session));
  const costTone = usageCostToneClass(session);
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
    usageValueCell(totalCost, costTone, usageCostTitle(session)),
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

function usageValueCell(value, tone, title) {
  const span = document.createElement("div");
  span.className = ["usage-cell-value", "usage-text-right", tone || ""].filter(Boolean).join(" ");
  span.textContent = value || "-";
  span.title = title || span.textContent;
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
    usageTraceInputCell(display.inputTotal, display.miss),
    usageTraceCell(display.output, true),
    usageTraceCell(display.cacheHitRate, true),
    usageTraceCell(
      display.cost,
      true,
      display.cost === "-" ? "" : ["cost-val", usageCostToneClass(segment)].filter(Boolean).join(" "),
      display.cost === "-" ? "" : usageCostTitle(segment),
    ),
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
    output: hasTokens ? formatNumber(segment.output_tokens) : "-",
    cacheHitRate: hasTokens ? usageCacheHitRate(segment.cached_input_tokens, segment.cache_miss_input_tokens) : "-",
    cost: hasRows || hasTokens ? formatCostOrUnpriced(costForTokens(segment)) : "-",
  };
}

function usageTraceInputCell(total, miss) {
  const cell = document.createElement("div");
  cell.className = "trace-cell trace-input-cell usage-text-right";
  cell.append(
    usageTraceInputLine("total", total),
    usageTraceInputLine("miss", miss),
  );
  return cell;
}

function usageTraceInputLine(kind, value) {
  const line = document.createElement("span");
  line.className = "trace-input-line";
  const label = document.createElement("span");
  label.className = "trace-input-label";
  label.textContent = kind === "miss" ? t("usageCacheMissShort") : t("usageInputTotalShort");
  const number = document.createElement("span");
  number.className = "trace-input-number";
  number.textContent = value || "-";
  if (number.textContent === "-") number.classList.add("dash");
  line.append(label, number);
  return line;
}

function usageTraceCell(value, numeric, innerClass, title) {
  const cell = document.createElement("div");
  cell.className = ["trace-cell", numeric ? "usage-text-right" : ""].filter(Boolean).join(" ");
  const text = value || "-";
  if (text === "-" || innerClass) {
    const inner = document.createElement("span");
    inner.className = text === "-" ? "dash" : innerClass;
    inner.textContent = text;
    if (title) inner.title = title;
    cell.appendChild(inner);
  } else {
    cell.textContent = text;
    if (title) cell.title = title;
  }
  return cell;
}

function costForSession(session) {
  if (Array.isArray(session && session.billing_buckets) && session.billing_buckets.length) {
    return sumCosts(session.billing_buckets);
  }
  const rows = Array.isArray(session && session.rows) ? session.rows : [];
  if (rows.length) return sumCosts(rows);
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
  if (segment.kind === "tool_result" || segment.kind === "vision") return "tool";
  if (segment.kind === "final_reply") return "final";
  if (segment.kind === "service" || segment.lifecycle === "service_ephemeral") return "service";
  return "reply";
}

function usageStageLabel(segment) {
  if (!segment) return "-";
  if (segment.status === "failed" || segment.kind === "failed_reply") return t("usageFailedBillable");
  if (segment.kind === "vision") return t("usageVisionStage");
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
  if (segment.kind === "vision") return "vision";
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
    case "usage_vision_stage":
      return t("usageVisionStage");
    case "usage_vision_completed":
      return t("usageVisionCompleted");
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
      create: () => logEntry(emptyLogEntry()),
      update: (node) => {
        const next = logEntry(emptyLogEntry());
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

function emptyLogEntry() {
  return {
    time: "--:--:--",
    level: "info",
    category: "system",
    categoryLabel: t("logCategorySystem"),
    title: t("noLogs"),
    summary: t("noLogsDetail"),
    requestId: "",
    sessionHint: "",
    riskFlags: [],
    metrics: {},
    detailRows: [],
    baseClass: "log-category-system log-level-info",
  };
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
  const detailRows = Array.isArray(item.detailRows) ? item.detailRows : [];
  const wrap = document.createElement("details");
  wrap.className = `log-entry ${item.baseClass || ""}`;
  if (!detailRows.length) wrap.classList.add("log-entry-empty-detail");

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

  if (detailRows.length) {
    const detail = document.createElement("div");
    detail.className = "log-detail-grid selectable";
    for (const detailRow of detailRows) {
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
  els.appDescription.textContent = t("aboutProductDescription");
  els.appVersion.textContent = "v" + version;
  els.aboutVersion.textContent = version;
  els.appLicense.textContent = info.license || t("notDeclared");
  renderAboutStats();
}

/// About 页的运行摘要只展示真实数据：目录里的默认模型与本机端点，不再写死模型名和端口。
function renderAboutStats() {
  if (els.aboutStatModel) els.aboutStatModel.textContent = catalogState.defaultModel || "-";
  if (els.aboutStatConnection) {
    const port = (lastSavedConfig && lastSavedConfig.PROXY_PORT) || "8787";
    els.aboutStatConnection.textContent = "127.0.0.1:" + port + "/v1";
  }
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
  if (view === "config") requestAnimationFrame(updateBillingModelListHeight);
}

function handleAboutAction(action) {
  if (!appInfo) return setAboutStatus(t("appInfoLoading"), true);
  const urls = appInfo.urls || {};
  if (action === "release-notes") return openReleaseNotesModal();
  if (action === "website") return openOrExplain(urls.website, t("websiteUnavailable"));
  if (action === "feedback") return openOrExplain(urls.feedback, t("feedbackUnavailable"));
  if (action === "source") return openOrExplain(urls.source, t("sourceUnavailable"));
  if (action === "license") return openOrExplain(urls.license, t("licenseUnavailable"));
  if (action === "update") return handleUpdateCheck();
}

async function openReleaseNotesModal() {
  if (!els.releaseNotesModal) return;
  els.releaseNotesModal.hidden = false;
  renderReleaseNotes();
  if (latestReleaseNotes) return;
  if (!releaseNotesLoad) {
    releaseNotesLoadFailed = false;
    releaseNotesLoad = apiJson("/api/release-notes", { cache: "no-store" })
      .then((data) => {
        if (data && typeof data === "object" && data.ok && data.release_notes) {
          latestReleaseNotes = data;
          return;
        }
        releaseNotesLoadFailed = true;
      })
      .catch(() => {
        releaseNotesLoadFailed = true;
      })
      .finally(() => {
        releaseNotesLoad = null;
      });
  }
  await releaseNotesLoad;
  if (els.releaseNotesModal && !els.releaseNotesModal.hidden) renderReleaseNotes();
}

function closeReleaseNotesModal() {
  if (els.releaseNotesModal) els.releaseNotesModal.hidden = true;
}

function releaseNotesLanguageCandidates() {
  const locale = normalizeLanguageId(uiLanguage);
  const candidates = [locale];
  candidates.push(FALLBACK_LANGUAGE);
  return Array.from(new Set(candidates));
}

function releaseNotesLocaleEntry(notes) {
  if (!notes || typeof notes !== "object") return null;
  for (const locale of releaseNotesLanguageCandidates()) {
    const entry = notes[locale];
    if (entry && typeof entry === "object") return { entry, locale };
  }
  return null;
}

function releaseNotesSourceLabel(source) {
  if (source === "github") return t("releaseNotesSourceGitHub");
  if (source === "cache") return t("releaseNotesSourceCache");
  return t("releaseNotesSourceBundled");
}

function renderReleaseNotes() {
  if (!els.releaseNotesBody) return;
  const data = latestReleaseNotes;
  els.releaseNotesBody.replaceChildren();
  if (els.releaseNotesSource) els.releaseNotesSource.hidden = true;
  if (els.releaseNotesNotice) els.releaseNotesNotice.hidden = true;

  if (!data) {
    const text = t(releaseNotesLoadFailed ? "releaseNotesUnavailable" : "releaseNotesLoading");
    if (els.releaseNotesSubtitle) els.releaseNotesSubtitle.textContent = text;
    els.releaseNotesBody.append(releaseNotesEmpty(text));
    return;
  }

  const version = String(data.current_version || "").trim();
  if (els.releaseNotesSubtitle) {
    els.releaseNotesSubtitle.textContent = version
      ? t("releaseNotesCurrentVersion").replace("{version}", version)
      : t("releaseNotesTitle");
  }
  if (!data.ok || !data.release_notes || typeof data.release_notes !== "object") {
    els.releaseNotesBody.append(releaseNotesEmpty(t("releaseNotesUnavailable")));
    return;
  }

  if (els.releaseNotesSource) {
    els.releaseNotesSource.textContent = releaseNotesSourceLabel(data.source);
    els.releaseNotesSource.hidden = false;
  }

  const releases = Array.isArray(data.release_notes.releases) ? data.release_notes.releases : [];
  let rendered = 0;
  let usedEnglishFallback = false;
  for (const release of releases) {
    const localized = releaseNotesLocaleEntry(release && release.notes);
    if (!localized) continue;
    usedEnglishFallback ||= localized.locale === FALLBACK_LANGUAGE && uiLanguage !== FALLBACK_LANGUAGE;
    els.releaseNotesBody.append(renderReleaseNotesEntry(release, localized.entry));
    rendered += 1;
  }
  if (rendered === 0) els.releaseNotesBody.append(releaseNotesEmpty(t("releaseNotesNoEntries")));

  const notices = [];
  if (data.stale) notices.push(t("releaseNotesOfflineFallback"));
  if (usedEnglishFallback) notices.push(t("releaseNotesLanguageFallback"));
  if (els.releaseNotesNotice && notices.length > 0) {
    els.releaseNotesNotice.textContent = notices.join(" ");
    els.releaseNotesNotice.hidden = false;
  }
}

function renderReleaseNotesEntry(release, note) {
  const entry = document.createElement("article");
  entry.className = "release-notes-entry";
  const heading = document.createElement("div");
  heading.className = "release-notes-entry-head";
  const version = document.createElement("strong");
  version.className = "release-notes-version tabular-nums";
  version.textContent = String(release && release.version || "-");
  const date = document.createElement("time");
  date.className = "release-notes-date tabular-nums";
  date.textContent = String(release && release.released_at || "");
  heading.append(version, date);
  entry.appendChild(heading);

  const summary = String(note && note.summary || "").trim();
  if (summary) {
    const summaryElement = document.createElement("p");
    summaryElement.className = "release-notes-summary";
    summaryElement.textContent = summary;
    entry.appendChild(summaryElement);
  }
  for (const section of Array.isArray(note && note.sections) ? note.sections : []) {
    const items = Array.isArray(section && section.items)
      ? section.items.map((item) => String(item || "").trim()).filter(Boolean)
      : [];
    if (items.length === 0) continue;
    const title = String(section && section.title || "").trim();
    if (title) {
      const titleElement = document.createElement("h3");
      titleElement.className = "release-notes-section";
      titleElement.textContent = title;
      entry.appendChild(titleElement);
    }
    const list = document.createElement("ul");
    list.className = "release-notes-list";
    for (const item of items) {
      const row = document.createElement("li");
      row.textContent = item;
      list.appendChild(row);
    }
    entry.appendChild(list);
  }
  return entry;
}

function releaseNotesEmpty(text) {
  const empty = document.createElement("p");
  empty.className = "release-notes-empty";
  empty.textContent = text;
  return empty;
}

async function handleUpdateCheck() {
  markUpdateNoticeSeen();
  if (latestUpdateCheck && latestUpdateCheck.has_update && latestUpdateCheck.installable) {
    return installDesktopUpdate();
  }
  setAboutStatus(t("checkingUpdate"), false);
  const update = await checkForUpdates();
  renderUpdateState();
  return update;
}

async function installDesktopUpdate() {
  if (!isTauriRuntime()) {
    const url = latestUpdateCheck && (latestUpdateCheck.url || (appInfo && appInfo.urls && appInfo.urls.releases));
    return openOrExplain(url, updateMessage("updateCheckFailed", latestUpdateCheck || {}));
  }
  updateInstallInProgress = true;
  updateProgressState = {
    active: true,
    background: false,
    visible: true,
    stage: "starting",
    version: updateNoticeVersion() || "",
    downloaded: 0,
    contentLength: null,
    percent: 0,
    error: "",
  };
  renderUpdateState();
  renderUpdateProgress();
  try {
    await desktopInvoke("desktop_install_update");
  } catch (error) {
    handleUpdateProgressEvent({
      stage: "failed",
      version: updateProgressState.version,
      error: error && error.message ? error.message : String(error),
    });
    updateInstallInProgress = false;
    renderUpdateState({ silent: true });
  }
}

function handleAboutStatusClick(event) {
  const link = event.target && event.target.closest ? event.target.closest("[data-update-link]") : null;
  if (!link) return;
  event.preventDefault();
  openOrExplain(link.href, updateMessage("updateCheckFailed", latestUpdateCheck || {})).catch((error) => {
    setAboutStatus(error && error.message ? error.message : String(error), true);
  });
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
  applyToolFieldVisibility();
  refreshUsageForBillingConfigInput(event);
  const nextPayload = buildConfigPayload();
  const next = normalizeConfigPayload(nextPayload);
  if (shouldKeepConfigAsDraft(event)) {
    pendingConfig = null;
    clearAutosaveTimer();
    renderConfigSaveState(sameConfigPayload(next, lastSavedConfig) ? "clean" : "draft");
    return;
  }
  if (sameConfigPayload(next, lastSavedConfig) && !secretConfigPayloadChanged(nextPayload)) {
    pendingConfig = null;
    clearAutosaveTimer();
    renderConfigSaveState(restartRequired ? "savedRestart" : "clean");
    return;
  }
  pendingConfig = nextPayload;
  renderConfigSaveState("pending");
  scheduleConfigSave(configAutosaveDelayForEvent(event));
}

function refreshUsageForBillingConfigInput(event) {
  // `handleConfigInput` is also called without an event (theme, model lock), so
  // this must not assume one.
  const id = String((event && event.target && event.target.id) || "");
  if (!id.startsWith("BILLING_")) return;
  lastUsageSignature = "";
  if (latestUsageRuntime) renderUsage(latestUsageRuntime);
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
  if (!target) return false;
  if (isSecretConfigKey(target.id || target.name)) return true;
  if (!SENSITIVE_CONFIG_INPUT_IDS.has(target.id || target.name)) return false;
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
  const payload = {
    ...collectToolConfigPayload(),
    CONFIG_VERSION: latestConfigVersion || "",
    DEEPSEEK_THINKING: getRadioValue("DEEPSEEK_THINKING") || "auto",
    DEEPSEEK_TEMPERATURE_PRESET: normalizeTemperaturePreset(getRadioValue("DEEPSEEK_TEMPERATURE_PRESET")),
    DEEPSEEK_TRANSPORT: selectedUpstreamTransportForSave(),
    WEB_SEARCH_BACKEND: normalizeWebSearchBackend(getRadioValue("WEB_SEARCH_BACKEND") || latestWebSearchBackend),
    NETWORK_PROXY_MODE: normalizeNetworkProxyMode(getRadioValue("NETWORK_PROXY_MODE")),
    CODEX_APP_MODEL_LIST_INJECTION: els.codexAppModelListInjection && els.codexAppModelListInjection.checked ? "true" : "false",
    // The feature is always on now; the mode below decides how much the chain shows.
    EXPERIMENT_REASONING_SUMMARY: "true",
    EXPERIMENT_REASONING_SUMMARY_MODE: normalizeReasoningSummaryMode(getRadioValue("EXPERIMENT_REASONING_SUMMARY_MODE")),
    AUTO_START: els.autoStart && els.autoStart.checked ? "true" : "false",
    COMMUNITY_TOOL_CODE_ENABLED: "false",
    UI_THEME: getRadioValue("UI_THEME") || "system",
    UI_CLOSE_BEHAVIOR: normalizeCloseBehavior(getRadioValue("UI_CLOSE_BEHAVIOR")),
    UI_LANGUAGE: els.uiLanguage ? normalizeConfiguredLanguageId(els.uiLanguage.value) : DEFAULT_LANGUAGE,
    DEEPSEEK_BASE_URL: normalizeDeepSeekBaseUrl(els.deepseekBaseUrl ? els.deepseekBaseUrl.value : ""),
    PROXY_PORT: normalizePort(els.proxyPort ? els.proxyPort.value : "", 8787),
    LOG_RETENTION_DAYS: getRadioValue("LOG_RETENTION_DAYS") || "7",
    CATALOG_PRICING: catalogPeakPricingPayload(),
  };
  if (latestUpstreamModelOverride) payload.UPSTREAM_MODEL_OVERRIDE = latestUpstreamModelOverride;
  return payload;
}

function normalizeConfigPayload(payload) {
  const output = {};
  for (const [key, value] of Object.entries(payload || {})) {
    if (key === "CONFIG_VERSION" || key === "config_version") continue;
    if (READ_ONLY_CONFIG_KEYS.has(key)) continue;
    if (isSecretConfigKey(key)) continue;
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

function isSecretConfigKey(key) {
  const normalized = String(key || "").trim().toUpperCase();
  return normalized === "VISION_API_KEY"
    || normalized === "VISION_ANALYZE_API_KEY"
    || normalized === "VISION_GENERATE_API_KEY"
    || normalized.endsWith("_API_KEY_CLEAR")
    || normalized.endsWith("_SECRET_CLEAR");
}

function secretConfigPayloadChanged(payload) {
  return Object.entries(payload || {}).some(([key, value]) => {
    if (!isSecretConfigKey(key)) return false;
    const text = String(value || "").trim().toLowerCase();
    return text === "true"
      || text === "1"
      || (text !== "" && !key.toUpperCase().endsWith("_CONFIGURED"));
  });
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
  renderUpdateProgress();
  if (els.releaseNotesModal && !els.releaseNotesModal.hidden) renderReleaseNotes();
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

function normalizeLanguageManifest(items) {
  const byId = new Map();
  for (const item of Array.isArray(items) ? items : []) {
    const id = normalizeLanguageId(item && item.id);
    if (!id || byId.has(id)) continue;
    byId.set(id, {
      id,
      name: (item && item.name) || id,
      sortKey: String((item && (item.sort_key || item.sortKey)) || id),
      url: (item && item.url) || "",
    });
  }
  return Array.from(byId.values()).sort((left, right) => {
    const bySortKey = left.sortKey.localeCompare(right.sortKey);
    return bySortKey || left.id.localeCompare(right.id);
  });
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
  return billingRateInputs();
}

function billingRateInputs() {
  if (!els.billingCardPanel) return [];
  return Array.from(els.billingCardPanel.querySelectorAll("input[data-model][data-rate]"));
}

function setBillingInputValues() {
  renderBillingCatalog();
}

/// Renders the fetched model list next to the billing card of the selected
/// model. Prices come from the catalog document; a model without a rate stays
/// unpriced instead of silently inheriting another model's price.
function renderBillingCatalog() {
  const panel = els.billingCardPanel;
  if (!panel) return;
  const models = catalogModels();
  if (!models.some((model) => model.slug === selectedCatalogModel)) {
    const preferred = models.find((model) => model.slug === catalogState.defaultModel) || models[0];
    selectedCatalogModel = preferred ? preferred.slug : "";
  }
  const signature = stableStringify({
    models: models.map((model) => [model.slug, model.display_name, model.short_display_name, model.description]),
    selected: selectedCatalogModel,
    upstream: latestUpstreamModelOverride,
    revision: catalogState.revision,
    currency: catalogState.currency,
  });
  if (signature === currentBillingRatesSignature && panel.childElementCount) return;
  currentBillingRatesSignature = signature;
  renderBillingModelList(models);
  renderBillingCard(models.find((model) => model.slug === selectedCatalogModel) || null);
  requestAnimationFrame(updateBillingModelListHeight);
}

function renderBillingModelList(models) {
  const list = els.billingModelList;
  if (!list) return;
  list.textContent = "";
  if (models.length === 0) {
    const empty = document.createElement("div");
    empty.className = "billing-model-empty";
    const title = document.createElement("span");
    title.setAttribute("data-i18n", "catalogEmpty");
    title.textContent = t("catalogEmpty");
    const hint = document.createElement("small");
    hint.setAttribute("data-i18n", "catalogEmptyHint");
    hint.textContent = t("catalogEmptyHint");
    empty.append(title, hint);
    list.append(empty);
    return;
  }
  for (const model of models) {
    const selected = model.slug === selectedCatalogModel;
    const item = document.createElement("div");
    item.className = "billing-model-item";
    const card = document.createElement("button");
    card.type = "button";
    card.className = selected ? "billing-model-card is-selected" : "billing-model-card";
    card.dataset.model = model.slug;
    card.setAttribute("aria-pressed", selected ? "true" : "false");
    const head = document.createElement("span");
    head.className = "billing-model-head";
    const name = document.createElement("span");
    name.className = "billing-model-name";
    name.textContent = model.display_name || model.slug;
    head.append(name);
    const badge = catalogModelBadge(model);
    if (badge) {
      const badgeEl = document.createElement("span");
      badgeEl.className = "billing-model-badge";
      badgeEl.textContent = badge;
      head.append(badgeEl);
    }
    card.append(head);
    const slug = String(model.slug || "").trim();
    if (slug) {
      const desc = document.createElement("span");
      desc.className = "billing-model-desc";
      desc.textContent = slug;
      card.append(desc);
    }
    item.append(card, renderModelLock(model));
    list.append(item);
  }
}

/// The lock pins this model as the upstream one. It stays hidden until the row
/// is hovered, and stays visible in red once it is the pinned model.
function renderModelLock(model) {
  const slug = String(model && model.slug ? model.slug : "").trim();
  const lockable = upstreamModelChoices().includes(slug);
  const locked = lockable && latestUpstreamModelOverride === slug;
  const button = document.createElement("button");
  button.type = "button";
  button.className = locked ? "model-lock is-locked" : "model-lock";
  button.dataset.lockModel = slug;
  button.setAttribute("aria-pressed", locked ? "true" : "false");
  button.disabled = !lockable;
  const label = t(locked ? "modelLockClear" : "modelLockPin");
  button.title = label;
  button.setAttribute("aria-label", label);
  button.innerHTML = MODEL_LOCK_ICON;
  return button;
}

/// MingCute lock-fill (MIT); inline so the icon follows the button colour.
const MODEL_LOCK_ICON =
  '<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path fill="currentColor" d="M12 2a6 6 0 0 1 6 6h1a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V10a2 2 0 0 1 2-2h1a6 6 0 0 1 6-6m-.107 10.005A1.998 1.998 0 0 0 11 15.729V17a1 1 0 1 0 2 0v-1.27a1.997 1.997 0 0 0-.894-3.725 1 1 0 0 0-.213 0M12 4a4 4 0 0 0-4 4h8a4 4 0 0 0-4-4"/></svg>';

/// Slugs the backend can actually pin; anything else would save nothing.
function upstreamModelChoices() {
  return Array.isArray(latestUpstreamModelChoices) ? latestUpstreamModelChoices : [];
}

function toggleUpstreamModel(slug) {
  const target = String(slug || "").trim();
  if (!target || !upstreamModelChoices().includes(target)) return;
  latestUpstreamModelOverride = latestUpstreamModelOverride === target ? "default" : target;
  renderBillingCatalog();
  handleConfigInput();
}

function renderBillingCard(model) {
  const panel = els.billingCardPanel;
  if (!panel) return;
  panel.textContent = "";
  if (!model) return;
  const rate = catalogRateFor(model.slug);
  const card = document.createElement("div");
  card.className = "billing-rate-card";
  card.dataset.model = model.slug;
  const header = document.createElement("div");
  header.className = "billing-card-header";
  const meta = document.createElement("div");
  meta.className = "billing-model-meta";
  const title = document.createElement("strong");
  title.textContent = model.display_name || model.slug;
  meta.append(title);
  const unit = String(t("billingUnit") || "").trim();
  if (unit) {
    const hint = document.createElement("small");
    hint.className = "muted";
    hint.setAttribute("data-i18n", "billingUnit");
    hint.textContent = unit;
    meta.append(hint);
  }
  header.append(meta);
  const badges = document.createElement("span");
  badges.className = "billing-card-badges";
  const modelBadge = catalogModelBadge(model);
  if (modelBadge) {
    const badgeEl = document.createElement("span");
    badgeEl.className = "billing-model-badge";
    badgeEl.textContent = modelBadge;
    badges.append(badgeEl);
  }
  const pricingBadge = catalogPricingBadge(rate);
  if (pricingBadge) {
    const badgeEl = document.createElement("span");
    badgeEl.className = pricingBadge.warn ? "billing-model-badge is-warn" : "billing-model-badge";
    badgeEl.textContent = pricingBadge.text;
    badges.append(badgeEl);
  }
  if (badges.childElementCount > 0) header.append(badges);
  card.append(header);
  const row = document.createElement("div");
  row.className = "billing-row";
  for (const [key, labelKey] of [["cached_input", "billingCachedInput"], ["cache_miss_input", "billingCacheMissInput"], ["output", "billingOutput"]]) {
    const field = document.createElement("div");
    field.className = "billing-field";
    const label = document.createElement("span");
    label.className = "billing-prefix";
    label.setAttribute("data-i18n", labelKey);
    label.textContent = t(labelKey);
    const input = document.createElement("input");
    input.type = "number";
    input.step = "0.001";
    input.min = "0";
    input.dataset.model = model.slug;
    input.dataset.rate = key;
    input.value = rate ? String(rate[key]) : "";
    input.setAttribute("data-i18n-placeholder", "billingUnpriced");
    input.placeholder = t("billingUnpriced");
    input.addEventListener("input", handleConfigInput);
    input.addEventListener("change", handleConfigInput);
    input.addEventListener("focusout", handleConfigInput);
    const suffix = document.createElement("span");
    suffix.className = "billing-suffix";
    suffix.textContent = catalogState.currency || "CNY";
    field.append(label, input, suffix);
    row.append(field);
  }
  card.append(row);
  panel.append(card);
}

function catalogModelBadge(model) {
  return String((model && model.short_display_name) || "").trim();
}

/// Marks group-priced and unpriced models instead of letting them look like a
/// regular catalog rate.
function catalogPricingBadge(rate) {
  if (!rate) return { text: t("billingUnpriced"), warn: true };
  if (rate.source === "group") return { text: t("billingGroupPriced"), warn: true };
  return null;
}

function selectBillingModel(event) {
  const lock = event.target && event.target.closest ? event.target.closest(".model-lock") : null;
  if (lock) {
    toggleUpstreamModel(lock.dataset ? lock.dataset.lockModel : "");
    return;
  }
  const card = event.target && event.target.closest ? event.target.closest(".billing-model-card") : null;
  if (!card) return;
  const slug = card.dataset ? card.dataset.model : "";
  if (!slug || slug === selectedCatalogModel) return;
  selectedCatalogModel = slug;
  renderBillingCatalog();
}

/// Keeps exactly VISIBLE_MODEL_CARDS model card rows in view; the rest scrolls.
function updateBillingModelListHeight() {
  const list = els.billingModelList;
  if (!list) return;
  const cards = list.querySelectorAll(".billing-model-card");
  if (cards.length === 0) {
    list.style.maxHeight = "";
    return;
  }
  if (cards.length <= VISIBLE_MODEL_CARDS) {
    list.style.maxHeight = "none";
    return;
  }
  const cardHeight = cards[0].offsetHeight;
  if (cardHeight <= 0) return;
  list.style.maxHeight = `${cardHeight * VISIBLE_MODEL_CARDS + MODEL_CARD_GAP * (VISIBLE_MODEL_CARDS - 1)}px`;
}

function scheduleModelListHeightSync() {
  if (modelListHeightTimer) clearTimeout(modelListHeightTimer);
  modelListHeightTimer = setTimeout(() => {
    modelListHeightTimer = null;
    updateBillingModelListHeight();
  }, 120);
}

function catalogRateOverrides() {
  const rates = {};
  for (const input of billingRateInputs()) {
    const slug = input.dataset ? input.dataset.model : "";
    const key = input.dataset ? input.dataset.rate : "";
    if (!slug || !key) continue;
    const parsed = Number(input.value);
    if (!Number.isFinite(parsed) || parsed < 0) continue;
    const base = catalogRateFor(slug) || {};
    rates[slug] = rates[slug] || {
      cached_input: Number(base.cached_input || 0),
      cache_miss_input: Number(base.cache_miss_input || 0),
      output: Number(base.output || 0),
    };
    rates[slug][key] = parsed;
  }
  return rates;
}

function catalogPeakPricingPayload() {
  const payload = {};
  const rates = catalogRateOverrides();
  if (Object.keys(rates).length) payload.rates = rates;
  payload.currency = catalogState.currency || "CNY";
  payload.unit = catalogState.unit || "per_1m_tokens";
  return payload;
}

function currentBillingSignature() {
  return stableStringify({
    peakValley: catalogPeakValley(),
    rates: catalogRateOverrides(),
    revision: catalogState.revision,
  });
}

function currentPeakValleyBillingEnabled() {
  return catalogPeakValley().enabled;
}

/// Parses the catalog document injected by the backend. Everything the
/// settings and usage views price with comes from here.
function applyCatalogPayload(catalog, status = {}) {
  catalogState.revision = String((catalog && catalog.revision) || status.revision || "");
  catalogState.source = String(status.source_label || status.source || "builtin");
  catalogState.providerName = String((catalog && catalog.provider_name) || "");
  catalogState.defaultModel = String((catalog && catalog.default_model) || "");
  catalogState.models = Array.isArray(catalog && catalog.models) ? catalog.models : [];
  catalogState.pricing = (catalog && catalog.pricing) || null;
  catalogState.currency = String((catalog && catalog.pricing && catalog.pricing.currency) || "CNY");
  catalogState.unit = String((catalog && catalog.pricing && catalog.pricing.unit) || "per_1m_tokens");
  catalogState.status = status || {};
  renderAboutStats();
}

function catalogModels() {
  return Array.isArray(catalogState.models) ? catalogState.models : [];
}

function catalogRateFor(model) {
  const slug = String(model || "").trim();
  const pricing = catalogState.pricing;
  if (!slug) return null;
  const rates = (pricing && pricing.rates && pricing.rates[slug]) || null;
  if (rates) return { ...normalizeRates(rates), source: "model" };
  const entry = catalogModels().find((item) => item.slug === slug);
  const group = entry && entry.pricing_group;
  const grouped = group && pricing && pricing.groups ? pricing.groups[group] : null;
  if (grouped) return { ...normalizeRates(grouped), source: "group", group };
  return null;
}

function normalizeRates(rates) {
  return {
    cached_input: normalizeRateInput(rates && rates.cached_input, 0),
    cache_miss_input: normalizeRateInput(rates && rates.cache_miss_input, 0),
    output: normalizeRateInput(rates && rates.output, 0),
  };
}

function catalogPeakValley() {
  const peak = catalogState.pricing && catalogState.pricing.peak_valley;
  if (!peak) return FALLBACK_PEAK_VALLEY;
  const windows = Array.isArray(peak.windows)
    ? peak.windows.map((window) => {
        const from = hhmmToMinute(window && window.from);
        const to = hhmmToMinute(window && window.to);
        return from === null || to === null || from >= to ? null : { from, to };
      }).filter(Boolean)
    : [];
  const multiplier = Number(peak.multiplier);
  return {
    enabled: peak.enabled !== false,
    timezone: String(peak.timezone || FALLBACK_PEAK_VALLEY.timezone),
    multiplier: Number.isFinite(multiplier) && multiplier >= 1 ? multiplier : FALLBACK_PEAK_VALLEY.multiplier,
    windows: windows.length ? windows : FALLBACK_PEAK_VALLEY.windows,
  };
}

function hhmmToMinute(value) {
  const match = /^([01][0-9]|2[0-3]):([0-5][0-9])$/.exec(String(value || "").trim());
  return match ? Number(match[1]) * 60 + Number(match[2]) : null;
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

function normalizeReasoningSummaryMode(value) {
  const normalized = String(value || "").trim().toLowerCase();
  return REASONING_SUMMARY_MODES.includes(normalized) ? normalized : DEFAULT_REASONING_SUMMARY_MODE;
}

function normalizeTemperaturePreset(value) {
  const normalized = String(value || DEFAULT_TEMPERATURE_PRESET).trim().toLowerCase();
  if (normalized === "precise" || normalized === "strict" || normalized === "rigorous") return "strict";
  if (normalized === "balanced" || normalized === "balance") return "balanced";
  if (normalized === "general" || normalized === "chat" || normalized === "translation") return "general";
  if (normalized === "creative" || normalized === "creation") return "creative";
  return DEFAULT_TEMPERATURE_PRESET;
}

function normalizeUpstreamTransport(value) {
  const normalized = String(value || "native_responses").trim().toLowerCase();
  if (normalized === "chat" || normalized === "chat_compat" || normalized === "compat") {
    return "chat_compat";
  }
  return "native_responses";
}

function selectedUpstreamTransportForSave() {
  return normalizeUpstreamTransport(getRadioValue("DEEPSEEK_TRANSPORT"));
}

function normalizeWebSearchBackend(value) {
  const normalized = String(value || "local").trim().toLowerCase();
  return normalized === "official" || normalized === "deepseek" ? "official" : "local";
}

function normalizeNetworkProxyMode(value) {
  const normalized = String(value || "system").trim().toLowerCase();
  return normalized === "none" || normalized === "no_proxy" || normalized === "direct" ? "none" : "system";
}

function normalizeCloseBehavior(value) {
  return String(value || "exit") === "tray" ? "tray" : "exit";
}

/// `null` means the model is unpriced: the caller must render "unpriced"
/// rather than inventing a number.
function costForTokens(tokens) {
  const rates = ratesForTokens(tokens);
  if (!rates) return null;
  const cached = Number(tokens.cached_input_tokens || tokens.cachedInputTokens || 0);
  const cacheMiss = Number(tokens.cache_miss_input_tokens || tokens.cacheMissInputTokens || 0);
  const output = Number(tokens.output_tokens || tokens.outputTokens || 0);
  const multiplier = currentPeakValleyBillingEnabled() ? billingMultiplierForTokens(tokens) : 1;
  return ((cached * rates.cached_input + cacheMiss * rates.cache_miss_input + output * rates.output) / 1000000) * multiplier;
}

function ratesForTokens(tokens) {
  if (!tokens) return null;
  const model = tokens.model || tokens.requested_model || tokens.requestedModel;
  return catalogRateFor(model);
}

function formatCostOrUnpriced(value) {
  return value === null || value === undefined ? t("billingUnpriced") : formatCost(value);
}

function sumCosts(items) {
  let total = 0;
  let unpriced = false;
  for (const item of Array.isArray(items) ? items : []) {
    const cost = costForTokens(item);
    if (cost === null) unpriced = true;
    else total += cost;
  }
  return unpriced ? null : total;
}

function billingMultiplierForTokens(tokens) {
  const explicit = Number(tokens && (tokens.billing_multiplier || tokens.billingMultiplier));
  if (Number.isFinite(explicit) && explicit > 0) return explicit;
  const peak = catalogPeakValley();
  if (!peak.enabled) return 1;
  return isPeakBillingTime(tokens && (tokens.completed_at || tokens.completedAt), peak) ? peak.multiplier : 1;
}

function usageCostToneClass(source) {
  if (!source || !currentPeakValleyBillingEnabled()) return "usage-cost-normal";
  return usageHasPeakBilling(source) ? "usage-cost-peak" : "usage-cost-normal";
}

function usageCostTitle(source) {
  if (!source || !currentPeakValleyBillingEnabled()) return "";
  return usageHasPeakBilling(source) ? t("usagePeakBillingCost") : t("usageOffPeakBillingCost");
}

function usageHasPeakBilling(source) {
  if (!source || typeof source !== "object") return false;
  const buckets = Array.isArray(source.billing_buckets || source.billingBuckets)
    ? source.billing_buckets || source.billingBuckets
    : [];
  if (buckets.some((bucket) => usageHasPeakBilling(bucket))) return true;
  const rows = Array.isArray(source.rows) ? source.rows : [];
  if (rows.some((row) => usageHasPeakBilling(row))) return true;
  const segments = Array.isArray(source.segments) ? source.segments : [];
  if (segments.some((segment) => usageHasPeakBilling(segment))) return true;

  const period = String(source.billing_period || source.billingPeriod || "").toLowerCase();
  const multiplier = Number(source.billing_multiplier || source.billingMultiplier || 0);
  if ((period === "peak" || multiplier > 1) && usageHasTokens(source)) return true;

  return usageHasTokens(source) && isPeakBillingTime(source.completed_at || source.completedAt);
}

/// Window boundaries come from the catalog pricing document: `[from, to)`.
function isPeakBillingTime(timestamp, peak = catalogPeakValley()) {
  if (!timestamp || !peak || !peak.enabled) return false;
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return false;
  const offsetMinutes = Number.isFinite(Number(peak.utcOffsetMinutes))
    ? Number(peak.utcOffsetMinutes)
    : FALLBACK_PEAK_VALLEY.utcOffsetMinutes;
  const local = new Date(date.getTime() + offsetMinutes * 60000);
  const minute = local.getUTCHours() * 60 + local.getUTCMinutes();
  return peak.windows.some((window) => minute >= window.from && minute < window.to);
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

function oldestLogCursor() {
  if (logEvents.length === 0) return logNextCursor;
  const oldest = logEvents[0];
  return oldest.cursor || [oldest.ts || "", oldest.id || ""].join("|") || logNextCursor;
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
