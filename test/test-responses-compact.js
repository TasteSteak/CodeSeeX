"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { createProxyService } = require("../src/proxy/server");

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});

async function main() {
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "codeseex-compact-"));
  const port = await getFreePort();
  const upstream = await createFakeDeepSeekServer();
  const config = {
    host: "127.0.0.1",
    port,
    stateFile: path.join(dataDir, "proxy-state.json"),
    runtimeFile: path.join(dataDir, "runtime.json"),
    debugDir: path.join(dataDir, "debug"),
    debugEnabled: true,
    logRetentionDays: 7,
    requestBodyLimitBytes: 2 * 1024 * 1024,
    maxStoredResponses: 20,
    availableModels: ["deepseek-v4-pro"],
    deepseekBaseUrl: upstream.baseUrl,
    requestTimeoutMs: 5000,
    contextWindow: 1000000,
    effectiveContextWindowPercent: 90,
    compactionSecret: "route-test-compaction-secret-value",
  };
  const service = createProxyService(config, { exitOnError: false, logErrors: false });
  service.start();
  await waitForListening(service);
  try {
    const response = await fetch("http://127.0.0.1:" + port + "/v1/responses/compact", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model: "deepseek-v4-pro",
        input: [
          { type: "message", role: "user", content: "Use a tool and compact it." },
          { type: "function_call", id: "fc_1", call_id: "call_1", name: "shell_command", arguments: "{\"command\":\"echo COMPACT_ROUTE\"}" },
          { type: "function_call_output", id: "out_1", call_id: "call_1", output: "COMPACT_ROUTE\n" },
        ],
      }),
    });
    assert.equal(response.status, 200);
    const body = await response.json();
    assert.equal(body.object, "response");
    assert.equal(body.output[0].type, "compaction");
    assert.ok(body.output[0].encrypted_content.startsWith("codeseex-compaction-v1:"));
    assert.ok(!JSON.stringify(body.output[0]).includes("COMPACT_ROUTE"));
    assert.ok(body.output.length > 1, "standalone compact should return a compacted context window, not only the compaction item");
    assert.ok(JSON.stringify(body.output.slice(1)).includes("COMPACT_ROUTE"), "recent retained input items should remain visible in the compacted window");
    assertUserCompactEvent(config.runtimeFile, {
      mode: "manual",
      responseId: body.id,
      minReturnedWindowItems: 2,
      minRetainedInputItems: 1,
    });

    const continuation = await fetch("http://127.0.0.1:" + port + "/v1/responses/compact", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model: "deepseek-v4-pro",
        input: [
          body.output[0],
          { type: "message", role: "user", content: "Compact again." },
        ],
      }),
    });
    assert.equal(continuation.status, 200);
    const continuedBody = await continuation.json();
    assert.equal(continuedBody.output[0].type, "compaction");
    assertUserCompactEvent(config.runtimeFile, {
      mode: "manual",
      responseId: continuedBody.id,
    });

    const autoResponse = await fetch("http://127.0.0.1:" + port + "/v1/responses", {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: "Bearer test-key" },
      body: JSON.stringify({
        model: "deepseek-v4-pro",
        input: [{ type: "message", role: "user", content: "Trigger automatic compact AUTO_COMPACT_ROUTE." }],
        context_management: { compact_threshold: 1 },
      }),
    });
    assert.equal(autoResponse.status, 200);
    const autoBody = await autoResponse.json();
    assert.equal(autoBody.output[autoBody.output.length - 1].type, "compaction");
    assertUserCompactEvent(config.runtimeFile, {
      mode: "automatic",
      responseId: autoBody.id,
    });

    const checkpointResponse = await fetch("http://127.0.0.1:" + port + "/v1/responses", {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: "Bearer test-key" },
      body: JSON.stringify({
        model: "deepseek-v4-pro",
        input: [
          { type: "message", role: "user", content: "Earlier conversation content." },
          {
            type: "message",
            role: "user",
            content: "You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.",
          },
        ],
      }),
    });
    assert.equal(checkpointResponse.status, 200);
    const checkpointBody = await checkpointResponse.json();
    assertCheckpointCompactionEvents(config.runtimeFile, checkpointBody.id);

  } finally {
    await service.close();
    await upstream.close();
    fs.rmSync(dataDir, { recursive: true, force: true });
  }
  console.log("Responses compact route test passed.");
}

function assertUserCompactEvent(runtimeFile, options = {}) {
  const runtime = JSON.parse(fs.readFileSync(runtimeFile, "utf8"));
  const events = Array.isArray(runtime.events) ? runtime.events : [];
  const event = events.find((item) => {
    const detail = item && item.detail || {};
    return item
      && item.type === "context_compacted"
      && item.audience === "user"
      && detail.mode === options.mode
      && (!options.responseId || detail.response_id === options.responseId);
  });
  assert.ok(event, "compact requests should create a user-visible context_compacted event");
  assert.equal(event.level, "info");
  if (options.expectedMessageCount !== undefined) assert.equal(event.detail.message_count, options.expectedMessageCount);
  if (options.expectedToolFacts !== undefined) assert.equal(event.detail.tool_fact_count, options.expectedToolFacts);
  if (options.minReturnedWindowItems !== undefined) assert.ok(event.detail.returned_window_items >= options.minReturnedWindowItems);
  if (options.minRetainedInputItems !== undefined) assert.ok(event.detail.retained_input_items >= options.minRetainedInputItems);
}

function assertCheckpointCompactionEvents(runtimeFile, responseId) {
  const runtime = JSON.parse(fs.readFileSync(runtimeFile, "utf8"));
  const events = Array.isArray(runtime.events) ? runtime.events : [];
  const related = events.filter((item) => item && item.detail && item.detail.model === "deepseek-v4-pro");
  const checkpointStart = events.find((item) => item && item.type === "context_compaction_started" && item.audience === "user");
  const checkpointDone = events.find((item) => item && item.type === "context_compaction_completed" && item.audience === "user" && item.detail && item.detail.duration_ms !== undefined);
  assert.ok(checkpointStart, "Codex checkpoint compaction should log a context_compaction_started event");
  assert.ok(checkpointDone, "Codex checkpoint compaction should log a context_compaction_completed event");
  const checkpointIndex = events.findIndex((item) => item === checkpointStart);
  const completedIndex = events.findIndex((item) => item === checkpointDone);
  assert.ok(checkpointIndex !== -1 && completedIndex > checkpointIndex, "checkpoint compaction logs should be ordered start -> completed");
  const ordinaryBetween = events.slice(checkpointIndex, completedIndex + 1).filter((item) => item && (item.type === "request_started" || item.type === "request_completed"));
  assert.equal(ordinaryBetween.length, 0, "checkpoint compaction should not be shown as an ordinary conversation request");
}

async function getFreePort() {
  const server = require("node:http").createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function createFakeDeepSeekServer() {
  const http = require("node:http");
  const server = http.createServer((req, res) => {
    if (req.method !== "POST" || !req.url.endsWith("/chat/completions")) {
      res.writeHead(404, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: { message: "Not found" } }));
      return;
    }
    let raw = "";
    req.on("data", (chunk) => { raw += chunk; });
    req.on("end", () => {
      const body = raw ? JSON.parse(raw) : {};
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({
        id: "chatcmpl_compact_test",
        object: "chat.completion",
        created: Math.floor(Date.now() / 1000),
        model: body.model || "deepseek-v4-pro",
        choices: [{
          index: 0,
          message: { role: "assistant", content: "Automatic compact test response." },
          finish_reason: "stop",
        }],
        usage: { input_tokens: 16, output_tokens: 6, total_tokens: 22 },
      }));
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const port = server.address().port;
  return {
    baseUrl: "http://127.0.0.1:" + port,
    close() {
      return new Promise((resolve) => server.close(resolve));
    },
  };
}

async function waitForListening(service) {
  const started = Date.now();
  while (Date.now() - started < 2000) {
    if (service.server.listening) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error("Proxy test server did not start.");
}
