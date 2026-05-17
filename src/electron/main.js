if (process.argv.includes("--proxy-child")) {
  require("../proxy/server").main();
} else {
  const { app, BrowserWindow, Menu, Tray, dialog, nativeImage, nativeTheme, shell } = require("electron");
  const path = require("node:path");
  const { readEnvFile } = require("../manager/env-file");
  const { startManager } = require("../manager/server");
  const { PRODUCT_NAME } = require("../shared/product");

  let managerService = null;
  let mainWindow = null;
  let nativeThemeUpdated = null;
  let themePreviewSeq = 0;
  let closeBehavior = "exit";
  let tray = null;
  let isQuitting = false;

  async function createWindow() {
    const rootDir = app.getAppPath();
    const dataDir = resolveDataDir(rootDir);
    const proxyEnv = readEnvFile(path.join(dataDir, "proxy.env"));
    const managerHost = process.env.PROXY_HOST || proxyEnv.PROXY_HOST || "127.0.0.1";
    const managerPort = clampPort(process.env.PROXY_PORT || proxyEnv.PROXY_PORT, 8787);
    let uiTheme = "system";
    managerService = await startOwnManager({
      dataDir,
      rootDir,
      host: managerHost,
      port: managerPort,
      onConfigChanged(config) {
        uiTheme = normalizeUiTheme(config && config.UI_THEME);
        closeBehavior = normalizeCloseBehavior(config && config.UI_CLOSE_BEHAVIOR);
        applyWindowTheme(mainWindow, uiTheme);
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
        handleWindowAction(mainWindow, action);
      },
    });

    mainWindow = new BrowserWindow({
      width: 1280,
      height: 720,
      minWidth: 1100,
      minHeight: 680,
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

    if (nativeThemeUpdated) nativeTheme.off("updated", nativeThemeUpdated);
    nativeThemeUpdated = () => applyWindowTheme(mainWindow, uiTheme);
    nativeTheme.on("updated", nativeThemeUpdated);

    mainWindow.webContents.setWindowOpenHandler(({ url }) => {
      shell.openExternal(url);
      return { action: "deny" };
    });
    mainWindow.on("close", (event) => {
      if (isQuitting || closeBehavior !== "tray") return;
      event.preventDefault();
      mainWindow.hide();
    });

    await mainWindow.loadURL(managerService.url);
  }

  app.whenReady().then(createWindow).catch((error) => {
    console.error(error);
    dialog.showErrorBox(PRODUCT_NAME + " failed to start", error && error.message ? error.message : String(error));
    app.quit();
  });

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
    if (app.isPackaged) return path.dirname(app.getPath("exe"));
    return rootDir;
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
    tray.setContextMenu(Menu.buildFromTemplate([
      { label: "Show " + PRODUCT_NAME, click: showMainWindow },
      { type: "separator" },
      { label: "Quit", click: quitFromTray },
    ]));

    tray.on("click", showMainWindow);
    tray.on("double-click", showMainWindow);

    return tray;
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

  function isDarkTheme(theme) {
    return normalizeUiTheme(theme) === "dark" || (normalizeUiTheme(theme) === "system" && nativeTheme.shouldUseDarkColors);
  }

  function fallbackTrayIcon() {
    const svgContent = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" d="M12 2a2 2 0 0 1 2 2c0 .74-.4 1.39-1 1.73V7h4a2 2 0 0 1 2 2v2h2v2h-2v5a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-5H3v-2h2V9a2 2 0 0 1 2-2h4V5.73c-.6-.34-1-.99-1-1.73a2 2 0 0 1 2-2M9 11a2 2 0 0 0-2 2h4a2 2 0 0 0-2-2m6 0a2 2 0 0 0-2 2h4a2 2 0 0 0-2-2z"/></svg>`;
    return "data:image/svg+xml;utf8," + encodeURIComponent(svgContent);
  }
}
