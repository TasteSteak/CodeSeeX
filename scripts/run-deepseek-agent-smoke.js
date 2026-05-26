"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");

const ROOT_DIR = path.resolve(__dirname, "..");
const DEFAULT_CONFIG_PATH = path.join(__dirname, "deepseek-agent-smoke.config.json");
const EXAMPLE_CONFIG_PATH = path.join(__dirname, "deepseek-agent-smoke.config.example.json");

main().catch((error) => {
  console.error("[smoke] failed:", error && error.stack ? error.stack : String(error));
  process.exitCode = 1;
});

async function main() {
  const configPath = resolveConfigPath();
  const config = loadConfig(configPath);
  const runId = timestampId();
  const reportDir = path.resolve(ROOT_DIR, "debug", "deepseek-agent-smoke", runId);
  fs.mkdirSync(reportDir, { recursive: true });

  const proxyBaseUrl = normalizeProxyBaseUrl(config.proxyBaseUrl || "http://127.0.0.1:8787/v1");
  const proxyOrigin = proxyBaseUrl.replace(/\/v1\/?$/, "");
  const model = String(config.model || "deepseek-v4-pro").trim();
  const workspaceDir = path.resolve(config.workspaceDir || fs.mkdtempSync(path.join(os.tmpdir(), "codeseex-smoke-workspace-")));
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "codeseex-smoke-"));
  const codexHome = path.join(tempRoot, "codex-home");
  const codeseexDataDir = path.resolve(config.codeseexDataDir || path.join(os.homedir(), ".codeseex"));
  fs.mkdirSync(codexHome, { recursive: true });
  fs.mkdirSync(codeseexDataDir, { recursive: true });
  fs.mkdirSync(workspaceDir, { recursive: true });

  const catalogPath = await resolveCatalogPath(config, proxyOrigin, reportDir);
  writeCodexConfig({ codexHome, proxyBaseUrl, model, catalogPath, extraConfigToml: config.extraConfigToml });
  writeAuthIfNeeded(codexHome, config.apiKey);

  console.log("[smoke] config:", configPath);
  console.log("[smoke] report:", reportDir);
  console.log("[smoke] proxy:", proxyBaseUrl);
  console.log("[smoke] model:", model);
  console.log("[smoke] catalog:", catalogPath);
  console.log("[smoke] CODEX_HOME:", codexHome);

  await assertProxyHealth(proxyOrigin);

  const outputJsonl = path.join(reportDir, "codex-events.jsonl");
  const lastMessageFile = path.join(reportDir, "last-message.txt");
  const latestRequestPath = path.join(codeseexDataDir, "debug", "latest-request.json");
  const latestDiagnosticPath = path.join(codeseexDataDir, "debug", "latest-context-diagnostic.json");
  const scenarios = normalizeScenarios(config);
  const promptFile = path.join(reportDir, "prompt.txt");
  fs.writeFileSync(promptFile, scenarios.map((scenario) => "## " + scenario.name + "\n" + scenario.prompt).join("\n\n"), "utf8");

  const env = Object.assign({}, process.env, {
    CODEX_HOME: codexHome,
    CODESEEX_SMOKE_OK: "CODESEEX_SMOKE_OK",
    NO_COLOR: "1",
  });
  if (String(config.apiKey || "").trim()) env.OPENAI_API_KEY = String(config.apiKey || "").trim();

  const scenarioResults = [];
  let previousSessionId = "";
  let result = { exitCode: 0, signal: null, timedOut: false };
  for (let index = 0; index < scenarios.length; index += 1) {
    const scenario = scenarios[index];
    const scenarioDir = path.join(reportDir, String(index + 1).padStart(2, "0") + "-" + sanitizeFileSegment(scenario.name));
    fs.mkdirSync(scenarioDir, { recursive: true });
    const scenarioLastMessageFile = path.join(scenarioDir, "last-message.txt");
    const scenarioOutputJsonl = path.join(scenarioDir, "codex-events.jsonl");
    const scenarioStderr = path.join(scenarioDir, "codex-stderr.txt");
    fs.writeFileSync(path.join(scenarioDir, "prompt.txt"), scenario.prompt, "utf8");
    const args = buildCodexArgs({
      resumeSessionId: scenario.resume === true ? previousSessionId : "",
      workspaceDir,
      lastMessageFile: scenarioLastMessageFile,
    });
    const scenarioResult = await runProcess(String(config.codexBin || "codex"), args, {
      cwd: workspaceDir,
      env,
      timeoutMs: numberOrDefault(scenario.timeoutMs || config.timeoutMs, 180000),
      stdoutFile: scenarioOutputJsonl,
      stderrFile: scenarioStderr,
      stdinText: scenario.prompt,
    });
    const scenarioLatestRequest = readJsonIfExists(latestRequestPath);
    const scenarioLatestDiagnostic = readJsonIfExists(latestDiagnosticPath);
    const scenarioLatestRequestFile = path.join(scenarioDir, "latest-request.json");
    const scenarioLatestDiagnosticFile = path.join(scenarioDir, "latest-context-diagnostic.json");
    if (scenarioLatestRequest) fs.writeFileSync(scenarioLatestRequestFile, JSON.stringify(scenarioLatestRequest, null, 2), "utf8");
    if (scenarioLatestDiagnostic) fs.writeFileSync(scenarioLatestDiagnosticFile, JSON.stringify(scenarioLatestDiagnostic, null, 2), "utf8");
    const scenarioCompiler = contextCompilerFromSnapshots(scenarioLatestRequest, scenarioLatestDiagnostic);
    const sessionId = extractSessionId(scenarioOutputJsonl) || previousSessionId;
    if (sessionId) previousSessionId = sessionId;
    const lastMessage = readTextIfExists(scenarioLastMessageFile);
    scenarioResults.push({
      name: scenario.name,
      resume: Boolean(scenario.resume),
      session_id: sessionId,
      exit_code: scenarioResult.exitCode,
      timed_out: Boolean(scenarioResult.timedOut),
      last_message: lastMessage.trim(),
      last_message_file: scenarioLastMessageFile,
      events_jsonl: scenarioOutputJsonl,
      stderr_file: scenarioStderr,
      latest_request_file: scenarioLatestRequest ? scenarioLatestRequestFile : "",
      latest_context_diagnostic_file: scenarioLatestDiagnostic ? scenarioLatestDiagnosticFile : "",
      context_compiler: scenarioCompiler,
      expected: scenario.expected || {},
      passed_expectations: evaluateScenarioExpectations(lastMessage, scenario.expected || {}),
    });
    result = scenarioResult;
    fs.appendFileSync(outputJsonl, readTextIfExists(scenarioOutputJsonl), "utf8");
    fs.appendFileSync(path.join(reportDir, "codex-stderr.txt"), readTextIfExists(scenarioStderr), "utf8");
    fs.writeFileSync(lastMessageFile, lastMessage, "utf8");
    if (scenarioResult.exitCode !== 0) break;
  }

  const latestRequest = readJsonIfExists(latestRequestPath);
  const latestDiagnostic = readJsonIfExists(latestDiagnosticPath);
  const summary = buildSummary({
    result,
    configPath,
    proxyBaseUrl,
    model,
    catalogPath,
    codexHome,
    workspaceDir,
    reportDir,
    codeseexDataDir,
    latestRequestPath,
    latestDiagnosticPath,
    lastMessageFile,
    outputJsonl,
    latestRequest,
    latestDiagnostic,
    scenarioResults,
  });
  fs.writeFileSync(path.join(reportDir, "summary.json"), JSON.stringify(summary, null, 2), "utf8");
  if (latestRequest) fs.writeFileSync(path.join(reportDir, "latest-request.json"), JSON.stringify(latestRequest, null, 2), "utf8");
  if (latestDiagnostic) fs.writeFileSync(path.join(reportDir, "latest-context-diagnostic.json"), JSON.stringify(latestDiagnostic, null, 2), "utf8");

  console.log("[smoke] exitCode:", result.exitCode);
  console.log("[smoke] upstream model:", summary.upstream_model || "(unknown)");
  console.log("[smoke] context compiler:", summary.context_compiler_mode || "(missing)");
  console.log("[smoke] tool facts:", summary.tool_fact_count);
  console.log("[smoke] last message:", summary.last_message_excerpt || "(empty)");
  console.log("[smoke] summary:", path.join(reportDir, "summary.json"));
  if (summary.scenarios && summary.scenarios.length > 0) {
    for (const scenario of summary.scenarios) {
      console.log("[smoke] scenario:", scenario.name, "ok=" + scenario.ok, "exit=" + scenario.exit_code);
    }
  }

  if (result.exitCode !== 0) process.exitCode = result.exitCode || 1;
  if (config.keepTemp !== true) {
    try {
      fs.rmSync(tempRoot, { recursive: true, force: true });
    } catch {}
  } else {
    console.log("[smoke] temp kept:", tempRoot);
  }

  process.exitCode = result.exitCode || 0;
}

function normalizeScenarios(config) {
  if (Array.isArray(config.scenarios) && config.scenarios.length > 0) {
    return config.scenarios.map((scenario, index) => ({
      name: String(scenario.name || "scenario-" + (index + 1)),
      prompt: String(scenario.prompt || ""),
      resume: Boolean(scenario.resume),
      timeoutMs: scenario.timeoutMs,
      expected: scenario.expected || {},
    })).filter((scenario) => scenario.prompt);
  }
  return [{
    name: "default",
    prompt: String(config.prompt || ""),
    resume: false,
    timeoutMs: config.timeoutMs,
    expected: config.expected || {},
  }];
}

function buildCodexArgs({ resumeSessionId, workspaceDir, lastMessageFile }) {
  const startCommon = [
    "--json",
    "--skip-git-repo-check",
    "--sandbox",
    "danger-full-access",
    "--cd",
    workspaceDir,
    "--output-last-message",
    lastMessageFile,
  ];
  if (resumeSessionId) {
    const resumeCommon = [
      "--json",
      "--skip-git-repo-check",
      "--output-last-message",
      lastMessageFile,
    ];
    return ["exec", "resume"].concat(resumeCommon, [resumeSessionId, "-"]);
  }
  return ["exec"].concat(startCommon, ["-"]);
}

function resolveConfigPath() {
  const arg = process.argv.find((item) => item.startsWith("--config="));
  const explicit = arg ? arg.slice("--config=".length) : process.argv[2];
  if (explicit && !explicit.startsWith("--")) return path.resolve(explicit);
  return DEFAULT_CONFIG_PATH;
}

function loadConfig(configPath) {
  if (!fs.existsSync(configPath)) {
    throw new Error("Config file not found: " + configPath + "\nCreate it from: " + EXAMPLE_CONFIG_PATH);
  }
  return JSON.parse(fs.readFileSync(configPath, "utf8"));
}

async function resolveCatalogPath(config, proxyOrigin, reportDir) {
  if (config.catalogPath) return path.resolve(String(config.catalogPath));
  const adapter = await fetchJson(proxyOrigin + "/api/codex-adapter").catch(() => null);
  if (adapter && adapter.catalog_path) return String(adapter.catalog_path);
  const generated = await fetchJson(proxyOrigin + "/api/codex-adapter/generate", { method: "POST" }).catch((error) => {
    fs.writeFileSync(path.join(reportDir, "catalog-generate-error.txt"), String(error && error.stack || error), "utf8");
    return null;
  });
  if (generated && generated.catalog_path) return String(generated.catalog_path);
  throw new Error("Could not resolve catalogPath. Fill catalogPath in the config file or start CodeSeeX manager.");
}

function writeCodexConfig({ codexHome, proxyBaseUrl, model, catalogPath, extraConfigToml }) {
  const toml = [
    'model_provider = "custom"',
    "model = " + tomlString(model),
    "disable_response_storage = true",
    'model_reasoning_effort = "xhigh"',
    "model_catalog_json = " + tomlString(catalogPath),
    "",
    "[model_providers.custom]",
    'name = "DeepSeek"',
    'wire_api = "responses"',
    "requires_openai_auth = true",
    "base_url = " + tomlString(proxyBaseUrl),
    "",
    "[windows]",
    'sandbox = "elevated"',
    "",
    "[features]",
    "js_repl = false",
    "",
    String(extraConfigToml || "").trim(),
    "",
  ].join("\n");
  fs.writeFileSync(path.join(codexHome, "config.toml"), toml, "utf8");
}

function writeAuthIfNeeded(codexHome, apiKey) {
  const key = String(apiKey || "").trim();
  if (!key) return;
  fs.writeFileSync(path.join(codexHome, "auth.json"), JSON.stringify({ OPENAI_API_KEY: key }, null, 2), "utf8");
}

async function assertProxyHealth(proxyOrigin) {
  const health = await fetchJson(proxyOrigin + "/healthz");
  if (!health || health.ok !== true) throw new Error("CodeSeeX proxy health check failed.");
}

function runProcess(command, args, options) {
  return new Promise((resolve) => {
    const stdout = fs.createWriteStream(options.stdoutFile, { flags: "w" });
    const stderr = fs.createWriteStream(options.stderrFile, { flags: "w" });
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env,
      windowsHide: true,
      shell: false,
      stdio: ["pipe", "pipe", "pipe"],
    });
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      try {
        child.kill("SIGTERM");
      } catch {}
    }, options.timeoutMs);
    child.stdout.pipe(stdout);
    child.stderr.pipe(stderr);
    child.stdin.end(String(options.stdinText || ""), "utf8");
    child.on("error", (error) => {
      clearTimeout(timer);
      stdout.end();
      stderr.end();
      fs.appendFileSync(options.stderrFile, "\n[spawn error] " + error.message + "\n", "utf8");
      resolve({ exitCode: 1, signal: null, timedOut, error: error.message });
    });
    child.on("close", (exitCode, signal) => {
      clearTimeout(timer);
      stdout.end();
      stderr.end();
      resolve({ exitCode, signal, timedOut });
    });
  });
}

function buildSummary(details) {
  const lastMessage = readTextIfExists(details.lastMessageFile);
  const request = details.latestRequest || {};
  const diagnostic = details.latestDiagnostic || {};
  const compiler = request.context_compiler || diagnostic.context_compiler || {};
  return {
    ok: details.result.exitCode === 0,
    exit_code: details.result.exitCode,
    timed_out: Boolean(details.result.timedOut),
    config_path: details.configPath,
    report_dir: details.reportDir,
    proxy_base_url: details.proxyBaseUrl,
    model: details.model,
    upstream_model: request.request_model || "",
    requested_model: request.requested_model || "",
    catalog_path: details.catalogPath,
    codex_home: details.codexHome,
    codeseex_data_dir: details.codeseexDataDir,
    workspace_dir: details.workspaceDir,
    context_compiler_mode: compiler.mode || "",
    context_compiler_compressed: Boolean(compiler.compressed),
    context_compiler_estimated_tokens: Number(compiler.estimated_tokens || 0),
    tool_fact_count: Array.isArray(request.tool_facts) ? request.tool_facts.length : Number(diagnostic.tool_fact_count || 0),
    context_conflict_count: Array.isArray(request.context_conflicts) ? request.context_conflicts.length : Number(diagnostic.context_conflict_count || 0),
    last_message_excerpt: lastMessage.trim().slice(0, 500),
    scenarios: (details.scenarioResults || []).map((scenario) => ({
      name: scenario.name,
      ok: scenario.exit_code === 0 && scenario.passed_expectations.ok,
      resume: scenario.resume,
      session_id: scenario.session_id,
      exit_code: scenario.exit_code,
      timed_out: scenario.timed_out,
      expected: scenario.expected,
      passed_expectations: scenario.passed_expectations,
      context_compiler: scenario.context_compiler,
      last_message_excerpt: String(scenario.last_message || "").slice(0, 500),
      files: {
        events_jsonl: scenario.events_jsonl,
        last_message: scenario.last_message_file,
        stderr: scenario.stderr_file,
        latest_request: scenario.latest_request_file,
        latest_context_diagnostic: scenario.latest_context_diagnostic_file,
      },
    })),
    files: {
      events_jsonl: details.outputJsonl,
      last_message: details.lastMessageFile,
      latest_request: path.join(details.reportDir, "latest-request.json"),
      latest_context_diagnostic: path.join(details.reportDir, "latest-context-diagnostic.json"),
      source_latest_request: details.latestRequestPath,
      source_latest_context_diagnostic: details.latestDiagnosticPath,
    },
  };
}

function contextCompilerFromSnapshots(request, diagnostic) {
  const compiler = request && request.context_compiler || diagnostic && diagnostic.context_compiler || {};
  return {
    mode: compiler.mode || "",
    compile_ms: Number(compiler.compile_ms || 0),
    compressed: Boolean(compiler.compressed),
    dropped_blocks: Number(compiler.dropped_blocks || 0),
    compacted_messages: Number(compiler.compacted_messages || 0),
    estimated_tokens: Number(compiler.estimated_tokens || 0),
    tool_fact_count: Array.isArray(request && request.tool_facts)
      ? request.tool_facts.length
      : Number(diagnostic && diagnostic.tool_fact_count || compiler.tool_fact_count || 0),
    conflict_count: Array.isArray(request && request.context_conflicts)
      ? request.context_conflicts.length
      : Number(diagnostic && diagnostic.context_conflict_count || compiler.conflict_count || 0),
    max_tokens: Number(compiler.budget && compiler.budget.max_tokens || diagnostic && diagnostic.context_budget && diagnostic.context_budget.max_tokens || 0),
    max_bytes: Number(compiler.budget && compiler.budget.max_bytes || diagnostic && diagnostic.context_budget && diagnostic.context_budget.max_bytes || 0),
  };
}

function evaluateScenarioExpectations(lastMessage, expected) {
  const text = String(lastMessage || "");
  const missingIncludes = [];
  for (const value of Array.isArray(expected.includes) ? expected.includes : []) {
    if (!text.includes(String(value))) missingIncludes.push(String(value));
  }
  const presentExcludes = [];
  for (const value of Array.isArray(expected.excludes) ? expected.excludes : []) {
    if (text.includes(String(value))) presentExcludes.push(String(value));
  }
  let jsonOk = true;
  if (expected.json === true) {
    try {
      JSON.parse(text);
    } catch {
      jsonOk = false;
    }
  }
  return {
    ok: missingIncludes.length === 0 && presentExcludes.length === 0 && jsonOk,
    missing_includes: missingIncludes,
    present_excludes: presentExcludes,
    json_ok: jsonOk,
  };
}

function extractSessionId(filePath) {
  const lines = readTextIfExists(filePath).split(/\r?\n/).filter(Boolean);
  for (const line of lines) {
    try {
      const event = JSON.parse(line);
      if (event && event.type === "thread.started" && event.thread_id) return String(event.thread_id);
    } catch {}
  }
  return "";
}

function sanitizeFileSegment(value) {
  return String(value || "scenario").replace(/[^a-z0-9_-]+/gi, "-").replace(/^-+|-+$/g, "").slice(0, 60) || "scenario";
}

async function fetchJson(url, options = {}) {
  const response = await fetch(url, options);
  const text = await response.text();
  if (!response.ok) throw new Error("HTTP " + response.status + " from " + url + ": " + text.slice(0, 500));
  return text ? JSON.parse(text) : null;
}

function normalizeProxyBaseUrl(value) {
  const raw = String(value || "").replace(/\/+$/, "");
  return raw.endsWith("/v1") ? raw : raw + "/v1";
}

function tomlString(value) {
  return JSON.stringify(String(value || ""));
}

function numberOrDefault(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function timestampId() {
  return new Date().toISOString().replace(/[-:]/g, "").replace(/\..+$/, "").replace("T", "-");
}

function readTextIfExists(filePath) {
  try {
    return fs.readFileSync(filePath, "utf8");
  } catch {
    return "";
  }
}

function readJsonIfExists(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, "utf8"));
  } catch {
    return null;
  }
}
