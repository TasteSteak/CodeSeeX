#!/usr/bin/env node

const path = require("node:path");

const { cleanupStaleProxyProcesses } = require("./process-cleanup");
const { readEnvFile } = require("./env-file");

function main() {
  const args = parseArgs(process.argv.slice(2));
  const rootDir = path.resolve(args.root || process.cwd());
  const dataDir = path.resolve(args.data || rootDir);
  const result = cleanupStaleProxyProcesses({
    rootDir,
    dataDir,
    runtimeFile: args.runtime || path.join(dataDir, "runtime.json"),
    ports: cleanupPorts(args, rootDir, dataDir),
    includeManager: Boolean(args["include-manager"]),
    includeDesktop: Boolean(args["include-desktop"]),
  });

  if (!args.quiet) {
    for (const item of result.killed) {
      console.log("[cleanup] stopped stale " + item.type + " process PID " + item.pid);
    }
    if (result.killed.length === 0) console.log("[cleanup] no stale proxy process found");
  }
}

function cleanupPorts(args, rootDir, dataDir) {
  if (args.ports || args.port) return args.ports || args.port;
  const env = Object.assign(
    {},
    readEnvFile(path.join(rootDir, "proxy.env")),
    readEnvFile(path.join(dataDir, "proxy.env")),
    process.env,
  );
  const ports = [env.PROXY_PORT || "8787"];
  if ((args["include-manager"] || args["include-desktop"]) && env.MANAGER_PORT) ports.push(env.MANAGER_PORT);
  return ports.join(",");
}

function parseArgs(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) continue;
    const key = arg.slice(2);
    const next = argv[index + 1];
    if (!next || next.startsWith("--")) {
      result[key] = true;
      continue;
    }
    result[key] = next;
    index += 1;
  }
  return result;
}

if (require.main === module) main();

module.exports = {
  main,
};
