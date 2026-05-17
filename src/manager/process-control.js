const path = require("node:path");
const { spawn } = require("node:child_process");
const { readJson, writeJson } = require("../shared/json-store");
const { repairMojibakeText } = require("../shared/text-encoding");
const { cleanupStaleProxyProcesses } = require("./process-cleanup");

function createProxyController(options) {
  let child = null;
  let lastError = null;
  const stdout = [];
  const stderr = [];
  const rootDir = options.rootDir;
  const dataDir = options.dataDir || rootDir;
  const runtimeFile = options.runtimeFile || path.join(dataDir, "runtime.json");
  const stateFile = options.stateFile || path.join(dataDir, "proxy-state.json");
  const debugDir = options.debugDir || path.join(dataDir, "debug");
  const proxyCommand = options.proxyCommand || process.execPath;
  const proxyArgs = Array.isArray(options.proxyArgs) ? options.proxyArgs.slice() : [options.proxyScript];
  const proxyCwd = options.proxyCwd || rootDir;

  function start(extraEnv = {}) {
    if (child && !child.killed) return status();
    const cleanup = cleanupStaleProxyProcesses({
      rootDir,
      dataDir,
      runtimeFile,
      ports: [Number(extraEnv.PROXY_PORT || process.env.PROXY_PORT || 8787)],
      includeProxy: true,
      excludePids: [child && child.pid],
    });
    for (const item of cleanup.killed) pushLine(stdout, "[cleanup] stopped stale " + item.type + " process PID " + item.pid);
    const existing = readJson(runtimeFile, null);
    if (isRuntimeAlive(existing)) return status();

    const env = Object.assign({}, process.env, extraEnv, {
      PROXY_ROOT_DIR: rootDir,
      PROXY_DATA_DIR: dataDir,
      PROXY_STATE_FILE: extraEnv.PROXY_STATE_FILE || stateFile,
      PROXY_RUNTIME_FILE: extraEnv.PROXY_RUNTIME_FILE || runtimeFile,
      PROXY_DEBUG_DIR: extraEnv.PROXY_DEBUG_DIR || debugDir,
      PROXY_PARENT_PID: String(process.pid),
    });

    child = spawn(proxyCommand, proxyArgs, {
      cwd: proxyCwd,
      env,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    });

    lastError = null;
    child.stdout.on("data", (chunk) => pushLine(stdout, chunk));
    child.stderr.on("data", (chunk) => pushLine(stderr, chunk));
    child.on("error", (error) => {
      lastError = error.message || String(error);
      writeStoppedRuntime(env.PROXY_RUNTIME_FILE, child ? child.pid : null, lastError);
    });
    child.on("exit", (code, signal) => {
      if (code && code !== 0) lastError = "Proxy exited with code " + code;
      if (signal) lastError = "Proxy exited by signal " + signal;
      writeStoppedRuntime(env.PROXY_RUNTIME_FILE, child ? child.pid : null, lastError);
      child = null;
    });

    waitForRuntimeRunning(env.PROXY_RUNTIME_FILE, child.pid, 2000);
    return status();
  }

  function stop() {
    const runtime = readJson(runtimeFile, null);
    const runtimePid = runtime && runtime.pid ? runtime.pid : null;
    try {
      writeStoppedRuntime(runtimeFile, child ? child.pid : runtimePid, "Proxy stopped by manager.");
      if (child && !child.killed) child.kill();
      else if (runtimePid && runtimePid !== process.pid && isPidAlive(runtimePid)) process.kill(runtimePid);
    } catch (error) {
      lastError = error.message || String(error);
    }
    child = null;
    return status();
  }

  function restart(extraEnv = {}) {
    stop();
    return start(extraEnv);
  }

  function status() {
    const runtime = readJson(runtimeFile, null);
    const runtimeRunning = isRuntimeAlive(runtime);
    return {
      mode: "child",
      running: Boolean((child && !child.killed) || runtimeRunning),
      pid: child ? child.pid : (runtimeRunning ? runtime.pid : null),
      last_error: lastError,
      stdout: stdout.slice(-80),
      stderr: stderr.slice(-80),
      runtime: runtimeRunning ? runtime : clearStaleRuntime(runtimeFile, runtime),
    };
  }

  return { dataDir, rootDir, restart, start, status, stop };
}

function pushLine(target, chunk) {
  const text = repairMojibakeText(Buffer.from(chunk).toString("utf8"));
  for (const line of text.split(/\r?\n/)) {
    if (!line) continue;
    target.push({ at: new Date().toISOString(), line });
  }
  if (target.length > 200) target.splice(0, target.length - 200);
}

module.exports = {
  createProxyController,
};

function isRuntimeAlive(runtime) {
  if (!runtime || runtime.status !== "running" || !runtime.pid) return false;
  return isPidAlive(runtime.pid);
}

function isPidAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function clearStaleRuntime(runtimeFile, runtime) {
  if (!runtime || runtime.status !== "running") return runtime;
  const next = Object.assign({}, runtime, {
    status: "stopped",
    stopped_at: new Date().toISOString(),
    error: runtime.error || { message: "Detected stale runtime state.", code: "stale_runtime" },
  });
  try {
    writeJson(runtimeFile, next);
  } catch {}
  return next;
}

function writeStoppedRuntime(runtimeFile, pid, message) {
  const runtime = readJson(runtimeFile, null);
  if (!runtime) return;
  if (pid && runtime.pid && runtime.pid !== pid) return;
  const next = Object.assign({}, runtime, {
    status: "stopped",
    stopped_at: new Date().toISOString(),
    error: message ? { message, code: "proxy_stopped" } : runtime.error,
  });
  try {
    writeJson(runtimeFile, next);
  } catch {}
}

function waitForRuntimeRunning(runtimeFile, pid, timeoutMs) {
  const started = Date.now();
  const poll = () => {
    const runtime = readJson(runtimeFile, null);
    if (runtime && runtime.status === "running" && (!pid || runtime.pid === pid)) return;
    if (pid && !isPidAlive(pid)) return;
    if (Date.now() - started >= timeoutMs) return;
    setTimeout(poll, 50).unref();
  };
  setTimeout(poll, 0).unref();
}
