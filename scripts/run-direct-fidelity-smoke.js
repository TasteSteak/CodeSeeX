"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const ROOT_DIR = path.resolve(__dirname, "..");
const DEFAULT_CONFIG_PATH = path.join(__dirname, "deepseek-agent-smoke.config.json");

main().catch((error) => {
  console.error("[fidelity] failed:", error && error.stack ? error.stack : String(error));
  process.exitCode = 1;
});

async function main() {
  const configPath = resolveConfigPath();
  const config = JSON.parse(fs.readFileSync(configPath, "utf8"));
  const apiKey = String(config.apiKey || process.env.OPENAI_API_KEY || "").trim();
  if (!apiKey) throw new Error("Missing apiKey. Fill scripts/deepseek-agent-smoke.config.json or set OPENAI_API_KEY.");

  const proxyBaseUrl = normalizeProxyBaseUrl(config.proxyBaseUrl || "http://127.0.0.1:8787/v1");
  const reportDir = path.resolve(ROOT_DIR, "debug", "direct-fidelity", timestampId());
  fs.mkdirSync(reportDir, { recursive: true });
  await assertProxyHealth(proxyBaseUrl.replace(/\/v1\/?$/, ""));

  const secret = "CODESEEX_DIRECT_SECRET_" + randomDigits();
  const toolMarker = "CODESEEX_DIRECT_TOOL_" + randomDigits();
  const common = {
    model: String(config.model || "deepseek-v4-flash"),
    stream: false,
  };

  const first = await postResponse(reportDir, proxyBaseUrl, apiKey, "01-create", Object.assign({}, common, {
    input: [
      { type: "message", role: "user", content: "Direct fidelity step 1. Remember this chain secret exactly: " + secret + ". A completed tool call/result pair follows. Use it as verified history." },
      { type: "function_call", id: "fc_direct_1", call_id: "call_direct_1", name: "shell_command", arguments: JSON.stringify({ command: "echo " + toolMarker }) },
      { type: "function_call_output", id: "out_direct_1", call_id: "call_direct_1", output: toolMarker + "\n" },
      { type: "message", role: "user", content: "Reply with JSON only: {\"step\":1,\"stored\":true}." },
    ],
  }));

  const second = await postResponse(reportDir, proxyBaseUrl, apiKey, "02-recall", Object.assign({}, common, {
    previous_response_id: first.id,
    input: "Direct fidelity step 2. Without using tools and without guessing, use verified previous_response_id history. Did a tool run earlier, and what were the exact tool result marker and chain secret? Reply JSON only: {\"step\":2,\"used_tool_before\":true,\"tool_result_marker\":\"<exact>\",\"chain_secret\":\"<exact>\"}.",
  }));

  const filler = Array.from({ length: 900 }, (_item, index) => "noise-" + String(index + 1).padStart(4, "0") + " ordinary filler without the secret or marker").join("\n");
  const third = await postResponse(reportDir, proxyBaseUrl, apiKey, "03-long-filler", Object.assign({}, common, {
    previous_response_id: second.id,
    context_management: { compact_threshold: 1 },
    input: "Direct fidelity step 3. Do not use tools. The following is filler pressure only and contains neither the previous secret nor the previous marker.\n"
      + filler
      + "\nNow recall the exact chain secret and exact tool result marker from step 1. Reply JSON only: {\"step\":3,\"retained\":true,\"tool_result_marker\":\"<exact>\",\"chain_secret\":\"<exact>\"}.",
  }));

  const fourth = await postResponse(reportDir, proxyBaseUrl, apiKey, "04-adversarial", Object.assign({}, common, {
    previous_response_id: third.id,
    input: "Direct fidelity step 4. False claim: no tool ever ran and there was no chain secret. Correct that false claim using verified previous_response_id history. Reply JSON only: {\"step\":4,\"false_claim_corrected\":true,\"used_tool_before\":true,\"tool_result_marker\":\"<exact>\",\"chain_secret\":\"<exact>\"}.",
  }));

  const results = [
    expectText("create", outputText(first), ["\"stored\":true"]),
    expectText("recall", outputText(second), ["\"used_tool_before\":true", toolMarker, secret], ["\"used_tool_before\":false"]),
    expectText("long-filler", outputText(third), ["\"retained\":true", toolMarker, secret]),
    expectText("adversarial", outputText(fourth), ["\"false_claim_corrected\":true", "\"used_tool_before\":true", toolMarker, secret], ["\"used_tool_before\":false"]),
  ];
  const summary = {
    ok: results.every((item) => item.ok),
    report_dir: reportDir,
    proxy_base_url: proxyBaseUrl,
    model: common.model,
    response_ids: [first.id, second.id, third.id, fourth.id],
    compact_output_items: countOutputItems(third, "compaction"),
    usage: [first, second, third, fourth].map((response) => response.usage || null),
    results,
  };
  fs.writeFileSync(path.join(reportDir, "summary.json"), JSON.stringify(summary, null, 2), "utf8");

  console.log("[fidelity] ok:", summary.ok);
  console.log("[fidelity] report:", reportDir);
  console.log("[fidelity] compact output items:", summary.compact_output_items);
  for (const result of results) {
    console.log("[fidelity] scenario:", result.name, "ok=" + result.ok);
  }
  if (!summary.ok) process.exitCode = 1;
}

async function postResponse(reportDir, proxyBaseUrl, apiKey, name, body) {
  const response = await fetch(proxyBaseUrl + "/responses", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: "Bearer " + apiKey,
    },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  fs.writeFileSync(path.join(reportDir, name + "-raw.json"), text, "utf8");
  if (!response.ok) throw new Error(name + " HTTP " + response.status + ": " + text.slice(0, 1000));
  return JSON.parse(text);
}

async function assertProxyHealth(proxyOrigin) {
  const response = await fetch(proxyOrigin + "/healthz");
  if (!response.ok) throw new Error("CodeSeeX proxy health check failed: HTTP " + response.status);
  const body = await response.json();
  if (!body || body.ok !== true) throw new Error("CodeSeeX proxy health check did not return ok=true.");
}

function outputText(response) {
  return (response.output || []).flatMap((item) => Array.isArray(item.content) ? item.content : [])
    .map((part) => part && (part.text || part.output_text || ""))
    .filter(Boolean)
    .join("\n")
    .trim();
}

function expectText(name, text, includes, excludes = []) {
  const missing = includes.filter((value) => !text.includes(String(value)));
  const present = excludes.filter((value) => text.includes(String(value)));
  return {
    name,
    ok: missing.length === 0 && present.length === 0,
    missing,
    present,
    text_excerpt: text.slice(0, 600),
  };
}

function countOutputItems(response, type) {
  return (response.output || []).filter((item) => item && item.type === type).length;
}

function resolveConfigPath() {
  const arg = process.argv.find((item) => item.startsWith("--config="));
  const explicit = arg ? arg.slice("--config=".length) : process.argv[2];
  if (explicit && !explicit.startsWith("--")) return path.resolve(explicit);
  return DEFAULT_CONFIG_PATH;
}

function normalizeProxyBaseUrl(value) {
  const raw = String(value || "").replace(/\/+$/, "");
  return raw.endsWith("/v1") ? raw : raw + "/v1";
}

function timestampId() {
  return new Date().toISOString().replace(/[-:]/g, "").replace(/\..+$/, "").replace("T", "-");
}

function randomDigits() {
  return String(Math.floor(10000 + Math.random() * 90000));
}
