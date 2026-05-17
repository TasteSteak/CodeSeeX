const path = require("node:path");

const { loadProxyConfig } = require("../shared/config");
const { readJson } = require("../shared/json-store");
const { repairMojibakeText } = require("../shared/text-encoding");
const { createProxyService } = require("../proxy/server");
const { cleanupStaleProxyProcesses } = require("./process-cleanup");

function createInlineProxyController(options) {
  let service = null;
  let closingPromise = null;
  let lastError = null;
  const stdout = [];
  const stderr = [];
  const rootDir = options.rootDir;
  const dataDir = options.dataDir || rootDir;
  const runtimeFile = options.runtimeFile || path.join(dataDir, "runtime.json");

  function start(extraEnv = {}) {
    if (service) return status();
    if (closingPromise) {
      lastError = "Inline proxy is still stopping.";
      return status();
    }
    const cleanup = cleanupStaleProxyProcesses({
      rootDir,
      dataDir,
      runtimeFile,
      ports: [Number(extraEnv.PROXY_PORT || process.env.PROXY_PORT || 8787)],
      includeProxy: true,
      excludePids: [process.pid],
    });
    for (const item of cleanup.killed) pushLine(stdout, "[cleanup] stopped stale " + item.type + " process PID " + item.pid);

    const previousEnv = applyEnv(rootDir, dataDir, runtimeFile, extraEnv);
    try {
      service = createProxyService(loadProxyConfig(process.env), {
        exitOnError: false,
        onError(error) {
          lastError = error.message || String(error);
          pushLine(stderr, "[proxy] " + lastError);
          service = null;
        },
      });
      service.start();
      lastError = null;
      pushLine(stdout, "[proxy] Starting inline proxy service");
    } catch (error) {
      service = null;
      lastError = error.message || String(error);
      pushLine(stderr, "[proxy] " + lastError);
    } finally {
      restoreEnv(previousEnv);
    }
    return status();
  }

  function stop() {
    if (!service) return status();
    const closing = service;
    service = null;
    closingPromise = closing.close().catch((error) => {
      lastError = error.message || String(error);
      pushLine(stderr, "[proxy] " + lastError);
    }).finally(() => {
      closingPromise = null;
    });
    pushLine(stdout, "[proxy] Inline proxy service stopped");
    return status();
  }

  async function restart(extraEnv = {}) {
    stop();
    if (closingPromise) await closingPromise;
    return start(extraEnv);
  }

  function status() {
    const runtime = service ? service.runtime : readJson(runtimeFile, null);
    return {
      mode: "inline",
      running: Boolean(service && runtime && runtime.status === "running"),
      pid: service ? process.pid : null,
      last_error: lastError,
      stdout: stdout.slice(-80),
      stderr: stderr.slice(-80),
      runtime,
    };
  }

  return { dataDir, rootDir, restart, start, status, stop };
}

function applyEnv(rootDir, dataDir, runtimeFile, extraEnv) {
  const next = Object.assign({}, extraEnv, {
    PROXY_ROOT_DIR: rootDir,
    PROXY_DATA_DIR: dataDir,
    PROXY_RUNTIME_FILE: extraEnv.PROXY_RUNTIME_FILE || runtimeFile,
    PROXY_STATE_FILE: extraEnv.PROXY_STATE_FILE || path.join(dataDir, "proxy-state.json"),
    PROXY_DEBUG_DIR: extraEnv.PROXY_DEBUG_DIR || path.join(dataDir, "debug"),
    PROXY_PARENT_PID: "0",
  });
  const previous = {};
  for (const [key, value] of Object.entries(next)) {
    previous[key] = process.env[key];
    process.env[key] = String(value);
  }
  return previous;
}

function restoreEnv(previous) {
  for (const [key, value] of Object.entries(previous)) {
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
}

function pushLine(target, line) {
  target.push({ at: new Date().toISOString(), line: repairMojibakeText(line) });
  if (target.length > 200) target.splice(0, target.length - 200);
}

module.exports = {
  createInlineProxyController,
};
