const { execFileSync } = require("node:child_process");
const path = require("node:path");

const { readJson, writeJson } = require("../shared/json-store");

function cleanupStaleProxyProcesses(options = {}) {
  const rootDir = path.resolve(options.rootDir || process.cwd());
  const dataDir = path.resolve(options.dataDir || rootDir);
  const runtimeFile = options.runtimeFile || path.join(dataDir, "runtime.json");
  const ports = normalizePorts(options.ports);
  const includeProxy = options.includeProxy !== false;
  const includeManager = options.includeManager === true;
  const includeDesktop = options.includeDesktop === true;
  const excludePids = new Set([process.pid, process.ppid].concat(options.excludePids || []).filter(Boolean).map(Number));
  const processes = listProcesses();
  const byPid = new Map(processes.map((item) => [Number(item.pid), item]));
  const portOwners = listPortOwners(ports);
  const targets = new Map();
  const runtime = readJson(runtimeFile, null);

  if (runtime && runtime.pid) {
    addTargetIfConfirmed(targets, byPid.get(Number(runtime.pid)), {
      rootDir,
      includeProxy,
      includeManager,
      includeDesktop,
      reason: "runtime",
    });
  }

  for (const pid of portOwners) {
    addTargetIfConfirmed(targets, byPid.get(Number(pid)), {
      rootDir,
      includeProxy,
      includeManager,
      includeDesktop,
      trustPortOwners: options.trustPortOwners === true,
      reason: "port",
    });
  }

  for (const proc of processes) {
    addTargetIfConfirmed(targets, proc, {
      rootDir,
      includeProxy,
      includeManager,
      includeDesktop,
      reason: "process",
    });
  }

  const killed = [];
  for (const target of Array.from(targets.values()).sort((left, right) => right.pid - left.pid)) {
    if (!target.pid || excludePids.has(target.pid)) continue;
    try {
      killProcessTree(target.pid);
      killed.push(target);
    } catch (error) {
      target.error = error.message || String(error);
    }
  }

  if (killed.length > 0) markRuntimeStopped(runtimeFile, killed.map((item) => item.pid));
  return { killed, targets: Array.from(targets.values()) };
}

function addTargetIfConfirmed(targets, proc, options) {
  if (!proc || !proc.pid) return;
  const type = classifyProxyProcess(proc, options.rootDir, {
    trustedSignal: options.reason === "runtime" || (options.reason === "port" && options.trustPortOwners === true),
  });
  if (!type) return;
  if (type === "proxy" && !options.includeProxy) return;
  if (type === "manager" && !options.includeManager) return;
  if (type === "desktop" && !options.includeDesktop) return;
  if (!targets.has(proc.pid)) {
    targets.set(proc.pid, {
      pid: proc.pid,
      name: proc.name || "",
      commandLine: proc.commandLine || "",
      type,
      reason: options.reason,
    });
  }
}

function classifyProxyProcess(proc, rootDir, options = {}) {
  if (isShellProcess(proc)) return null;
  const root = normalizePath(rootDir);
  const commandLine = normalizeText(proc.commandLine || "");
  const executablePath = normalizePath(proc.executablePath || "");
  const name = String(proc.name || "").toLowerCase();
  const inProject = commandLine.includes(root) || executablePath.includes(root);
  const trustedSignal = options.trustedSignal === true;
  if (!inProject && !trustedSignal) return null;

  if (isProxyCommand(commandLine)) {
    return "proxy";
  }

  if (isManagerCommand(commandLine)) {
    return "manager";
  }

  if (isDesktopCommand(name, commandLine, executablePath, trustedSignal)) {
    return "desktop";
  }

  return null;
}

function isShellProcess(proc) {
  const name = path.basename(String(proc.name || proc.executablePath || "")).toLowerCase();
  return name === "sh"
    || name === "bash"
    || name === "zsh"
    || name === "dash"
    || name === "fish"
    || name === "cmd"
    || name === "cmd.exe"
    || name === "powershell"
    || name === "powershell.exe"
    || name === "pwsh"
    || name === "pwsh.exe";
}

function isProxyCommand(commandLine) {
  return /(^|[\\/\s"'.])proxy\.js([\\/\s"']|$)/i.test(commandLine)
    || commandLine.includes("--proxy-child")
    || /src[\\/]proxy[\\/]server\.js/i.test(commandLine);
}

function isManagerCommand(commandLine) {
  return /(^|[\\/\s"'.])manager\.js([\\/\s"']|$)/i.test(commandLine)
    || /src[\\/]manager[\\/]server\.js/i.test(commandLine);
}

function isDesktopCommand(name, commandLine, executablePath, trustedSignal) {
  if (name !== "electron.exe" && name !== "electron") return false;
  if (commandLine.includes("electron") || executablePath.includes("electron")) return true;
  return trustedSignal;
}

function listProcesses() {
  if (process.platform === "win32") return listWindowsProcesses();
  return listPosixProcesses();
}

function listWindowsProcesses() {
  const script = [
    "$items = Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine",
    "$items | ConvertTo-Json -Compress -Depth 3",
  ].join("; ");
  const output = runPowerShell(script);
  const parsed = parseJson(output, []);
  const items = Array.isArray(parsed) ? parsed : (parsed ? [parsed] : []);
  return items.map((item) => ({
    pid: Number(item.ProcessId),
    parentPid: Number(item.ParentProcessId),
    name: item.Name || "",
    executablePath: item.ExecutablePath || "",
    commandLine: item.CommandLine || "",
  })).filter((item) => item.pid);
}

function listPortOwners(ports) {
  if (ports.length === 0) return [];
  if (process.platform !== "win32") return listPosixPortOwners(ports);
  const portList = ports.map((port) => Number(port)).filter(Boolean).join(",");
  if (!portList) return [];
  const script = [
    "$ports = @(" + portList + ")",
    "$items = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object { $ports -contains $_.LocalPort } | Select-Object -ExpandProperty OwningProcess -Unique",
    "@($items) | ConvertTo-Json -Compress",
  ].join("; ");
  const parsed = parseJson(runPowerShell(script), []);
  return (Array.isArray(parsed) ? parsed : [parsed]).map(Number).filter(Boolean);
}

function listPosixProcesses() {
  try {
    const output = execFileSync("ps", ["-axo", "pid=,ppid=,comm=,args="], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    return output.split(/\r?\n/)
      .map((line) => {
        const match = line.match(/^\s*(\d+)\s+(\d+)\s+(\S+)(?:\s+([\s\S]*))?$/);
        if (!match) return null;
        return {
          pid: Number(match[1]),
          parentPid: Number(match[2]),
          name: match[3] || "",
          executablePath: match[3] || "",
          commandLine: match[4] || match[3] || "",
        };
      })
      .filter((item) => item && item.pid);
  } catch {
    return [];
  }
}

function listPosixPortOwners(ports) {
  const normalizedPorts = ports.map(Number).filter(Boolean);
  if (normalizedPorts.length === 0) return [];
  const owners = new Set();
  for (const pid of listPosixPortOwnersWithLsof(normalizedPorts)) owners.add(pid);
  if (owners.size === 0) {
    for (const pid of listPosixPortOwnersWithSs(normalizedPorts)) owners.add(pid);
  }
  return Array.from(owners);
}

function listPosixPortOwnersWithLsof(ports) {
  const owners = new Set();
  for (const port of ports) {
    try {
      const output = execFileSync("lsof", ["-nP", "-iTCP:" + port, "-sTCP:LISTEN", "-t"], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      });
      for (const line of output.split(/\r?\n/)) {
        const pid = Number(line.trim());
        if (pid) owners.add(pid);
      }
    } catch {}
  }
  return Array.from(owners);
}

function listPosixPortOwnersWithSs(ports) {
  try {
    const output = execFileSync("ss", ["-ltnp"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    const wanted = new Set(ports.map(Number));
    const owners = new Set();
    for (const line of output.split(/\r?\n/)) {
      const port = parseListenPort(line);
      if (!wanted.has(port)) continue;
      for (const match of line.matchAll(/pid=(\d+)/g)) owners.add(Number(match[1]));
    }
    return Array.from(owners);
  } catch {
    return [];
  }
}

function parseListenPort(line) {
  const match = String(line || "").match(/(?:^|\s)(?:\[[^\]]+\]|[^\s:]+|\*):(\d+)\s/);
  return match ? Number(match[1]) : 0;
}

function runPowerShell(script) {
  try {
    return execFileSync("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script], {
      encoding: "utf8",
      windowsHide: true,
      stdio: ["ignore", "pipe", "ignore"],
    });
  } catch {
    return "";
  }
}

function killProcessTree(pid) {
  if (process.platform === "win32") {
    execFileSync("taskkill.exe", ["/PID", String(pid), "/T", "/F"], {
      encoding: "utf8",
      windowsHide: true,
      stdio: ["ignore", "pipe", "ignore"],
    });
    return;
  }
  let killedAny = false;
  let lastError = null;
  for (const targetPid of posixProcessTreePids(pid)) {
    try {
      process.kill(targetPid, "SIGTERM");
      killedAny = true;
    } catch (error) {
      lastError = error;
    }
  }
  if (!killedAny && lastError) throw lastError;
}

function posixProcessTreePids(pid) {
  const rootPid = Number(pid);
  if (!rootPid) return [];
  const childrenByParent = new Map();
  for (const proc of listProcesses()) {
    const parentPid = Number(proc.parentPid);
    if (!parentPid) continue;
    if (!childrenByParent.has(parentPid)) childrenByParent.set(parentPid, []);
    childrenByParent.get(parentPid).push(Number(proc.pid));
  }
  const seen = new Set();
  const result = [];
  function visit(currentPid) {
    if (!currentPid || seen.has(currentPid)) return;
    seen.add(currentPid);
    for (const childPid of childrenByParent.get(currentPid) || []) visit(childPid);
    result.push(currentPid);
  }
  visit(rootPid);
  return result;
}

function markRuntimeStopped(runtimeFile, killedPids) {
  const runtime = readJson(runtimeFile, null);
  if (!runtime || !runtime.pid || !killedPids.includes(Number(runtime.pid))) return;
  const next = Object.assign({}, runtime, {
    status: "stopped",
    stopped_at: new Date().toISOString(),
    error: { message: "Stopped stale proxy process before startup.", code: "stale_proxy_stopped" },
  });
  try {
    writeJson(runtimeFile, next);
  } catch {}
}

function normalizePorts(value) {
  if (Array.isArray(value)) return value.map(Number).filter((port) => Number.isFinite(port) && port > 0);
  if (value === undefined || value === null || value === "") return [];
  return String(value).split(",").map((part) => Number(part.trim())).filter((port) => Number.isFinite(port) && port > 0);
}

function normalizePath(value) {
  const resolved = path.resolve(String(value || ""));
  return process.platform === "win32" ? resolved.replace(/\//g, "\\").toLowerCase() : resolved;
}

function normalizeText(value) {
  const text = String(value || "");
  return process.platform === "win32" ? text.replace(/\//g, "\\").toLowerCase() : text;
}

function parseJson(value, fallback) {
  if (!String(value || "").trim()) return fallback;
  try {
    return JSON.parse(value);
  } catch {
    return fallback;
  }
}

module.exports = {
  cleanupStaleProxyProcesses,
  classifyProxyProcess,
};
