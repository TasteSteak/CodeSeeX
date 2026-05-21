if (process.argv.includes("--proxy-child")) {
  require("../proxy/server").main();
} else {
  const { app, BrowserWindow, Menu, Tray, dialog, nativeImage, nativeTheme, shell } = require("electron");
  const os = require("node:os");
  const path = require("node:path");
  const { readEnvFile } = require("../manager/env-file");
  const { startManager } = require("../manager/server");
  const { PRODUCT_NAME } = require("../shared/product");

  let managerService = null;
  let mainWindow = null;
  let nativeThemeUpdated = null;
  let themePreviewSeq = 0;
  let closeBehavior = "exit";
  let currentConfig = {};
  let tray = null;
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
    const managerHost = process.env.PROXY_HOST || proxyEnv.PROXY_HOST || "127.0.0.1";
    const managerPort = clampPort(process.env.PROXY_PORT || proxyEnv.PROXY_PORT, 8787);
    let uiTheme = "system";
    closeBehavior = normalizeCloseBehavior(proxyEnv.UI_CLOSE_BEHAVIOR);
    currentConfig = proxyEnv || {};
    applyLoginItemFromConfig(currentConfig);
    uiTheme = normalizeUiTheme(proxyEnv.UI_THEME);

    managerService = await startOwnManager({
      dataDir,
      rootDir,
      host: managerHost,
      port: managerPort,
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

  function clampPort(value, fallback) {
    const parsed = Number(value);
    if (!Number.isFinite(parsed)) return fallback;
    return Math.min(65535, Math.max(1, Math.floor(parsed)));
  }

  async function startOwnManager({ dataDir, rootDir, host, port, onConfigChanged, onWindowAction }) {
    return startManager({
      dataDir,
      rootDir,
      host,
      port,
      exitOnClose: false,
      onConfigChanged,
      onWindowAction,
    });
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
    return [
      { label: "Show " + PRODUCT_NAME, click: showMainWindow },
      { type: "separator" },
      {
        label: "Model",
        submenu: [
          { label: "Default", type: "radio", checked: model === "default", click: () => updateRuntimeConfig({ UPSTREAM_MODEL_OVERRIDE: "default" }) },
          { label: "Flash", type: "radio", checked: model === "deepseek-v4-flash", click: () => updateRuntimeConfig({ UPSTREAM_MODEL_OVERRIDE: "deepseek-v4-flash" }) },
          { label: "Pro", type: "radio", checked: model === "deepseek-v4-pro", click: () => updateRuntimeConfig({ UPSTREAM_MODEL_OVERRIDE: "deepseek-v4-pro" }) },
        ],
      },
      {
        label: "Thinking",
        submenu: [
          { label: "Auto", type: "radio", checked: thinking === "auto", click: () => updateRuntimeConfig({ DEEPSEEK_THINKING: "auto" }) },
          { label: "Force on", type: "radio", checked: thinking === "enabled", click: () => updateRuntimeConfig({ DEEPSEEK_THINKING: "enabled" }) },
          { label: "Force off", type: "radio", checked: thinking === "disabled", click: () => updateRuntimeConfig({ DEEPSEEK_THINKING: "disabled" }) },
        ],
      },
      { type: "separator" },
      { label: "Quit", click: quitFromTray },
    ];
  }

  async function updateRuntimeConfig(patch) {
    if (!managerService || !managerService.url) return;
    currentConfig = Object.assign({}, currentConfig, patch || {});
    refreshTrayMenu();
    try {
      const response = await fetch(managerService.url + "/api/config", { cache: "no-store" });
      const config = response.ok ? await response.json() : {};
      const next = Object.assign({}, config || {}, patch || {});
      await fetch(managerService.url + "/api/config", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(next),
      });
    } catch {
      notifyRendererConfigChanged();
    }
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

  function normalizeUpstreamModelOverride(value) {
    const normalized = String(value || "default").trim().toLowerCase();
    if (normalized === "flash" || normalized === "deepseek-v4-flash") return "deepseek-v4-flash";
    if (normalized === "pro" || normalized === "deepseek-v4-pro") return "deepseek-v4-pro";
    return "default";
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
