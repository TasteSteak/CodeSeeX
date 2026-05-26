if (process.argv.includes("--proxy-child")) {
  require("../proxy/server").main();
} else {
  const { app, BrowserWindow, Menu, Tray, dialog, nativeImage, nativeTheme, protocol, shell } = require("electron");
  const fs = require("node:fs");
  const os = require("node:os");
  const path = require("node:path");
  const { readEnvFile } = require("../manager/env-file");
  const { startDesktopManager } = require("../manager/server");
  const { PRODUCT_NAME } = require("../shared/product");

  protocol.registerSchemesAsPrivileged([{
    scheme: "codeseex",
    privileges: {
      bypassCSP: false,
      corsEnabled: true,
      secure: true,
      standard: true,
      stream: true,
      supportFetchAPI: true,
    },
  }]);

  let managerService = null;
  let mainWindow = null;
  let nativeThemeUpdated = null;
  let themePreviewSeq = 0;
  let closeBehavior = "exit";
  let currentConfig = {};
  let tray = null;
  const trayLanguagePackCache = new Map();
  let localProtocolRegistered = false;
  let isQuitting = false;
  let pendingShowWindow = false;
  const singleInstanceLock = app.requestSingleInstanceLock();

  if (!singleInstanceLock) {
    app.quit();
  } else {
    app.on("second-instance", () => {
      pendingShowWindow = true;
      showMainWindow();
    });
  }

  async function createWindow() {
    const startHidden = process.argv.includes("--hidden");
    const rootDir = app.getAppPath();
    const dataDir = resolveDataDir(rootDir);
    const proxyEnv = readEnvFile(path.join(dataDir, "proxy.env"));
    let uiTheme = "system";
    closeBehavior = normalizeCloseBehavior(proxyEnv.UI_CLOSE_BEHAVIOR);
    currentConfig = proxyEnv || {};
    applyLoginItemFromConfig(currentConfig);
    uiTheme = normalizeUiTheme(proxyEnv.UI_THEME);

    if (!managerService) {
      managerService = await startOwnManager({
        dataDir,
        rootDir,
        onConfigChanged(config) {
          currentConfig = config || {};
          uiTheme = normalizeUiTheme(config && config.UI_THEME);
          closeBehavior = normalizeCloseBehavior(config && config.UI_CLOSE_BEHAVIOR);
          applyWindowTheme(mainWindow, uiTheme);
          applyLoginItemFromConfig(config || {});
          refreshTrayMenu();
          notifyRendererConfigChanged();
        },
        onWindowAction(action, payload) {
          if (action === "theme") {
            const seq = Number(payload && payload.seq) || 0;
            if (seq && seq < themePreviewSeq) return;
            if (seq) themePreviewSeq = seq;
            uiTheme = normalizeUiTheme(payload && payload.theme);
            applyWindowTheme(mainWindow, uiTheme);
            return;
          }
          if (action === "login-item") {
            applyLoginItem(Boolean(payload && payload.enabled));
            return;
          }
          handleWindowAction(mainWindow, action);
        },
      });
    }

    mainWindow = new BrowserWindow({
      width: 1280,
      height: 720,
      minWidth: 1100,
      minHeight: 680,
      show: !startHidden,
      title: PRODUCT_NAME,
      icon: appIconPath(rootDir),
      titleBarStyle: "hidden",
      backgroundColor: windowBackgroundColor(uiTheme),
      autoHideMenuBar: true,
      webPreferences: {
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: true,
      },
    });
    applyWindowTheme(mainWindow, uiTheme);
    ensureTray(rootDir);
    installWindowHandlers();
    if (pendingShowWindow) {
      pendingShowWindow = false;
      showMainWindow();
    }

    await mainWindow.loadURL(managerService.url);
    setImmediate(() => {
      if (!isQuitting && managerService && typeof managerService.startInitialProxy === "function") {
        managerService.startInitialProxy();
      }
    });
  }

  function installWindowHandlers() {
    if (!mainWindow || mainWindow.isDestroyed() || mainWindow.__codeseexHandlersInstalled) return;
    mainWindow.__codeseexHandlersInstalled = true;
    if (nativeThemeUpdated) nativeTheme.off("updated", nativeThemeUpdated);
    nativeThemeUpdated = () => applyWindowTheme(mainWindow, currentConfig.UI_THEME || "system");
    nativeTheme.on("updated", nativeThemeUpdated);

    mainWindow.webContents.setWindowOpenHandler(({ url }) => {
      shell.openExternal(url);
      return { action: "deny" };
    });
    mainWindow.webContents.on("context-menu", (event) => {
      event.preventDefault();
    });
    mainWindow.on("close", (event) => {
      if (isQuitting || closeBehavior !== "tray") return;
      event.preventDefault();
      mainWindow.hide();
    });
  }

  if (singleInstanceLock) {
    app.whenReady().then(createWindow).catch((error) => {
      console.error(error);
      dialog.showErrorBox(PRODUCT_NAME + " failed to start", startupErrorMessage(error));
      app.quit();
    });
  }

  app.on("window-all-closed", () => {
    if (process.platform !== "darwin") app.quit();
  });

  app.on("activate", () => {
    if (mainWindow && !mainWindow.isDestroyed()) {
      showMainWindow();
      return;
    }
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });

  app.on("before-quit", () => {
    isQuitting = true;
    if (nativeThemeUpdated) nativeTheme.off("updated", nativeThemeUpdated);
    if (tray) tray.destroy();
    if (managerService) managerService.close();
  });

  async function startOwnManager({ dataDir, rootDir, onConfigChanged, onWindowAction }) {
    const service = await startDesktopManager({
      dataDir,
      rootDir,
      onConfigChanged,
      onWindowAction,
    });
    if (!localProtocolRegistered) {
      protocol.handle(service.protocol, (request) => {
        const activeService = managerService || service;
        return activeService.handleProtocolRequest(request);
      });
      localProtocolRegistered = true;
    }
    return service;
  }

  function resolveDataDir(rootDir) {
    if (process.env.PROXY_DATA_DIR) return path.resolve(process.env.PROXY_DATA_DIR);
    if (process.env.PORTABLE_EXECUTABLE_DIR) return path.resolve(process.env.PORTABLE_EXECUTABLE_DIR);
    return path.join(os.homedir() || app.getPath("home") || rootDir, ".codeseex");
  }

  function applyWindowTheme(window, theme) {
    if (!window || window.isDestroyed()) return;
    const normalizedTheme = normalizeUiTheme(theme);
    window.setBackgroundColor(windowBackgroundColor(normalizedTheme));
  }

  function windowBackgroundColor(theme) {
    return isDarkTheme(theme) ? "#09090b" : "#f4f5f7";
  }

  function handleWindowAction(window, action) {
    if (!window || window.isDestroyed()) return;
    if (action === "minimize") {
      window.minimize();
      return;
    }
    if (action === "maximize") {
      if (window.isMaximized()) window.unmaximize();
      else window.maximize();
      return;
    }
    if (action === "close") window.close();
  }

  function ensureTray(rootDir) {
    if (tray) return tray;
    let image = nativeImage.createFromPath(appIconPath(rootDir));

    if (image.isEmpty()) {
      image = nativeImage.createFromDataURL(fallbackTrayIcon());
    }

    // Keep the tray icon crisp across high-DPI Windows and native macOS menu bars.
    const traySize = process.platform === "darwin" ? 24 : 32;
    image = image.resize({ width: traySize, height: traySize });

    if (process.platform === "darwin") {
      image.setTemplateImage(true);
    }

    tray = new Tray(image);
    tray.setToolTip(PRODUCT_NAME);
    refreshTrayMenu();

    tray.on("click", showMainWindow);
    tray.on("double-click", showMainWindow);

    return tray;
  }

  function refreshTrayMenu() {
    if (!tray) return;
    tray.setContextMenu(Menu.buildFromTemplate(trayMenuTemplate()));
  }

  function trayMenuTemplate() {
    const model = normalizeUpstreamModelOverride(currentConfig.UPSTREAM_MODEL_OVERRIDE);
    const thinking = normalizeThinkingMode(currentConfig.DEEPSEEK_THINKING);
    const temperature = normalizeTemperaturePreset(currentConfig.DEEPSEEK_TEMPERATURE_PRESET);
    return [
      { label: trayText("trayShow", { name: PRODUCT_NAME }), click: showMainWindow },
      { type: "separator" },
      {
        label: trayText("trayModel"),
        submenu: [
          { label: trayText("modelDefault"), type: "radio", checked: model === "default", click: () => updateRuntimeConfig({ UPSTREAM_MODEL_OVERRIDE: "default" }) },
          { label: trayText("modelFlash"), type: "radio", checked: model === "deepseek-v4-flash", click: () => updateRuntimeConfig({ UPSTREAM_MODEL_OVERRIDE: "deepseek-v4-flash" }) },
          { label: trayText("modelPro"), type: "radio", checked: model === "deepseek-v4-pro", click: () => updateRuntimeConfig({ UPSTREAM_MODEL_OVERRIDE: "deepseek-v4-pro" }) },
        ],
      },
      {
        label: trayText("trayThinking"),
        submenu: [
          { label: trayText("thinkingAuto"), type: "radio", checked: thinking === "auto", click: () => updateRuntimeConfig({ DEEPSEEK_THINKING: "auto" }) },
          { label: trayText("thinkingEnabled"), type: "radio", checked: thinking === "enabled", click: () => updateRuntimeConfig({ DEEPSEEK_THINKING: "enabled" }) },
          { label: trayText("thinkingDisabled"), type: "radio", checked: thinking === "disabled", click: () => updateRuntimeConfig({ DEEPSEEK_THINKING: "disabled" }) },
        ],
      },
      {
        label: trayText("trayTemperature"),
        submenu: [
          { label: trayText("temperatureDefault"), type: "radio", checked: temperature === "default", click: () => updateRuntimeConfig({ DEEPSEEK_TEMPERATURE_PRESET: "default" }) },
          { label: trayText("temperatureStrict"), type: "radio", checked: temperature === "strict", click: () => updateRuntimeConfig({ DEEPSEEK_TEMPERATURE_PRESET: "strict" }) },
          { label: trayText("temperatureBalanced"), type: "radio", checked: temperature === "balanced", click: () => updateRuntimeConfig({ DEEPSEEK_TEMPERATURE_PRESET: "balanced" }) },
          { label: trayText("temperatureGeneral"), type: "radio", checked: temperature === "general", click: () => updateRuntimeConfig({ DEEPSEEK_TEMPERATURE_PRESET: "general" }) },
          { label: trayText("temperatureCreative"), type: "radio", checked: temperature === "creative", click: () => updateRuntimeConfig({ DEEPSEEK_TEMPERATURE_PRESET: "creative" }) },
        ],
      },
      { type: "separator" },
      { label: trayText("trayQuit"), click: quitFromTray },
    ];
  }

  async function updateRuntimeConfig(patch) {
    if (!managerService || typeof managerService.handleProtocolRequest !== "function") return;
    currentConfig = Object.assign({}, currentConfig, patch || {});
    refreshTrayMenu();
    try {
      const response = await managerApiRequest("/api/config");
      const config = response.ok ? await response.json() : {};
      const next = Object.assign({}, config || {}, patch || {});
      await managerApiRequest("/api/config", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(next),
      });
    } catch {
      notifyRendererConfigChanged();
    }
  }

  function managerApiRequest(pathname, init = {}) {
    const url = "codeseex://app" + (String(pathname || "/").startsWith("/") ? pathname : "/" + pathname);
    return managerService.handleProtocolRequest(new Request(url, init));
  }

  function notifyRendererConfigChanged() {
    if (!mainWindow || mainWindow.isDestroyed()) return;
    mainWindow.webContents.executeJavaScript(
      "window.dispatchEvent(new CustomEvent('codeseex-config-changed'))",
      true,
    ).catch(() => {});
  }

  function appIconPath(rootDir) {
    return path.join(rootDir, "src", "manager", "static", "assets", "icons", "app.png");
  }

  function showMainWindow() {
    if (!mainWindow || mainWindow.isDestroyed()) return;
    if (mainWindow.isMinimized()) mainWindow.restore();
    mainWindow.show();
    mainWindow.focus();
  }

  function startupErrorMessage(error) {
    const message = error && error.message ? error.message : String(error || "Unknown error");
    const code = error && error.code ? String(error.code) : "";
    if (code === "EACCES" || code === "EPERM") {
      return [
        message,
        "",
        "CodeSeeX could not access a local file or port.",
        "Please close other CodeSeeX instances and check whether security software is blocking the app.",
      ].join("\n");
    }
    if (code === "EADDRINUSE") {
      return [
        message,
        "",
        "The configured CodeSeeX port is already in use.",
        "Please close the other instance or change PROXY_PORT.",
      ].join("\n");
    }
    return message;
  }

  function quitFromTray() {
    isQuitting = true;
    app.quit();
  }

  function normalizeUiTheme(value) {
    return value === "light" || value === "dark" ? value : "system";
  }

  function normalizeCloseBehavior(value) {
    return value === "tray" ? "tray" : "exit";
  }

  function normalizeThinkingMode(value) {
    const normalized = String(value || "auto").trim().toLowerCase();
    return ["auto", "enabled", "disabled"].includes(normalized) ? normalized : "auto";
  }

  function normalizeTemperaturePreset(value) {
    const normalized = String(value || "default").trim().toLowerCase();
    if (normalized === "precise" || normalized === "strict" || normalized === "rigorous") return "strict";
    if (normalized === "balanced" || normalized === "balance") return "balanced";
    if (normalized === "general" || normalized === "chat" || normalized === "translation") return "general";
    if (normalized === "creative" || normalized === "creation") return "creative";
    return "default";
  }

  function normalizeUpstreamModelOverride(value) {
    const normalized = String(value || "default").trim().toLowerCase();
    if (normalized === "flash" || normalized === "deepseek-v4-flash") return "deepseek-v4-flash";
    if (normalized === "pro" || normalized === "deepseek-v4-pro") return "deepseek-v4-pro";
    return "default";
  }

  function trayText(key, vars = {}) {
    const pack = loadTrayLanguagePack(currentConfig);
    const fallback = trayFallbackText(key);
    let text = String((pack && pack[key]) || fallback || key);
    for (const [name, value] of Object.entries(vars || {})) {
      text = text.replace(new RegExp("\\{" + escapeRegExp(name) + "\\}", "g"), String(value));
    }
    return text;
  }

  function trayFallbackText(key) {
    const fallback = {
      modelDefault: "Default",
      modelFlash: "Flash",
      modelPro: "Pro",
      temperatureBalanced: "Balanced",
      temperatureCreative: "Creative",
      temperatureDefault: "Default",
      temperatureGeneral: "General",
      temperatureStrict: "Strict",
      thinkingAuto: "Auto",
      thinkingDisabled: "Force off",
      thinkingEnabled: "Force on",
      trayModel: "Model",
      trayQuit: "Quit",
      trayShow: "Show {name}",
      trayTemperature: "Sampling temperature",
      trayThinking: "Thinking",
    };
    return fallback[key] || key;
  }

  function loadTrayLanguagePack(config) {
    const rootDir = app.getAppPath();
    const id = resolveTrayLanguageId(config && config.UI_LANGUAGE, rootDir);
    return readTrayLanguagePack(rootDir, id) || readTrayLanguagePack(rootDir, "en_us") || {};
  }

  function resolveTrayLanguageId(value, rootDir) {
    const requested = normalizeLanguageId(value || "system");
    if (requested !== "system") return requested;
    const available = availableTrayLanguages(rootDir);
    const preferredLanguages = typeof app.getPreferredSystemLanguages === "function" ? app.getPreferredSystemLanguages() : [];
    for (const locale of preferredLanguages.concat(app.getLocale()).map(normalizeLanguageId)) {
      if (available.includes(locale)) return locale;
      const prefix = locale.split("_")[0];
      const byPrefix = available.find((id) => id === prefix || id.startsWith(prefix + "_"));
      if (byPrefix) return byPrefix;
    }
    return "en_us";
  }

  function availableTrayLanguages(rootDir) {
    const dir = path.join(rootDir, "src", "manager", "static", "lang");
    try {
      return fs.readdirSync(dir)
        .filter((name) => name.endsWith(".json"))
        .map((name) => normalizeLanguageId(name.replace(/\.json$/i, "")))
        .filter(Boolean);
    } catch {
      return ["en_us"];
    }
  }

  function readTrayLanguagePack(rootDir, id) {
    const filePath = path.join(rootDir, "src", "manager", "static", "lang", normalizeLanguageId(id) + ".json");
    const cacheKey = filePath;
    if (trayLanguagePackCache.has(cacheKey)) return trayLanguagePackCache.get(cacheKey);
    try {
      const pack = JSON.parse(fs.readFileSync(filePath, "utf8"));
      trayLanguagePackCache.set(cacheKey, pack);
      return pack;
    } catch {
      trayLanguagePackCache.set(cacheKey, null);
      return null;
    }
  }

  function normalizeLanguageId(value) {
    return String(value || "en_us").trim().replace(/-/g, "_").toLowerCase() || "en_us";
  }

  function escapeRegExp(value) {
    return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  }

  function applyLoginItemFromConfig(config) {
    applyLoginItem(/^(1|true|yes|on|enabled)$/i.test(String(config && config.AUTO_START || "")));
  }

  function applyLoginItem(enabled) {
    try {
      app.setLoginItemSettings({
        openAtLogin: Boolean(enabled),
        openAsHidden: true,
        args: ["--hidden"],
      });
    } catch {}
  }

  function isDarkTheme(theme) {
    return normalizeUiTheme(theme) === "dark" || (normalizeUiTheme(theme) === "system" && nativeTheme.shouldUseDarkColors);
  }

  function fallbackTrayIcon() {
    const svgContent = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" d="M12 2a2 2 0 0 1 2 2c0 .74-.4 1.39-1 1.73V7h4a2 2 0 0 1 2 2v2h2v2h-2v5a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-5H3v-2h2V9a2 2 0 0 1 2-2h4V5.73c-.6-.34-1-.99-1-1.73a2 2 0 0 1 2-2M9 11a2 2 0 0 0-2 2h4a2 2 0 0 0-2-2m6 0a2 2 0 0 0-2 2h4a2 2 0 0 0-2-2z"/></svg>`;
    return "data:image/svg+xml;utf8," + encodeURIComponent(svgContent);
  }
}
