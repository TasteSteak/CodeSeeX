const assert = require("node:assert");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");

const { startDesktopManager } = require("../src/manager/server");

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});

async function main() {
  const rootDir = path.resolve(__dirname, "..");
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "codeseex-desktop-manager-"));
  const occupiedPort = await occupyRandomPort();
  fs.writeFileSync(path.join(dataDir, "proxy.env"), [
    "PROXY_HOST=127.0.0.1",
    "PROXY_PORT=" + occupiedPort.port,
    "UI_LANGUAGE=en_us",
    "",
  ].join("\n"), "utf8");

  const manager = await startDesktopManager({ dataDir, rootDir });
  try {
    const indexResponse = await manager.handleProtocolRequest(new Request("codeseex://app/"));
    assert.equal(indexResponse.status, 200);
    assert.match(await indexResponse.text(), /CodeSeeX/);

    const configResponse = await manager.handleProtocolRequest(new Request("codeseex://app/api/config"));
    assert.equal(configResponse.status, 200);
    const config = await configResponse.json();
    assert.equal(String(config.PROXY_PORT), String(occupiedPort.port));
    assert.equal(config.DEEPSEEK_OFFICIAL_V1_COMPAT, "true");
    assert.equal(config.BILLING_FLASH_CACHED_INPUT_CNY, "0.02");
    assert.equal(config.BILLING_FLASH_CACHE_MISS_INPUT_CNY, "1");
    assert.equal(config.BILLING_FLASH_OUTPUT_CNY, "2");
    assert.equal(config.BILLING_PRO_CACHED_INPUT_CNY, "0.025");
    assert.equal(config.BILLING_PRO_CACHE_MISS_INPUT_CNY, "3");
    assert.equal(config.BILLING_PRO_OUTPUT_CNY, "6");

    const compatResponse = await manager.handleProtocolRequest(new Request("codeseex://app/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ DEEPSEEK_OFFICIAL_V1_COMPAT: "false", BILLING_FLASH_OUTPUT_CNY: "2.5" }),
    }));
    assert.equal(compatResponse.status, 200);
    const compatConfigResponse = await manager.handleProtocolRequest(new Request("codeseex://app/api/config"));
    const compatConfig = await compatConfigResponse.json();
    assert.equal(compatConfig.DEEPSEEK_OFFICIAL_V1_COMPAT, "false");
    assert.equal(compatConfig.BILLING_FLASH_OUTPUT_CNY, "2.5");

    manager.startInitialProxy();
    await waitFor(() => {
      const status = manager.controller.status();
      return status.runtime && status.runtime.status === "error";
    }, 2000);
    const status = manager.controller.status();
    assert.equal(status.running, false);
    assert.equal(status.runtime.error.code, "EADDRINUSE");

    const availablePort = await occupyRandomPort();
    const recoveryPort = availablePort.port;
    await availablePort.close();
    const saveResponse = await manager.handleProtocolRequest(new Request("codeseex://app/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ PROXY_PORT: String(recoveryPort) }),
    }));
    assert.equal(saveResponse.status, 200);
    const startResponse = await manager.handleProtocolRequest(new Request("codeseex://app/api/start", { method: "POST" }));
    assert.equal(startResponse.status, 200);
    await waitFor(() => {
      const recovered = manager.controller.status();
      return recovered.runtime && recovered.runtime.status === "running";
    }, 2000);
    const recovered = manager.controller.status();
    assert.equal(recovered.running, true);
    assert.equal(recovered.runtime.port, recoveryPort);
  } finally {
    await Promise.resolve(manager.close());
    await occupiedPort.close();
    fs.rmSync(dataDir, { recursive: true, force: true });
  }

  console.log("Desktop manager port isolation test passed.");
}

async function occupyRandomPort() {
  const server = http.createServer((_req, res) => res.end("occupied"));
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  return {
    port: address.port,
    close() {
      return new Promise((resolve) => server.close(resolve));
    },
  };
}

async function waitFor(predicate, timeoutMs) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error("Timed out waiting for expected desktop manager status.");
}
