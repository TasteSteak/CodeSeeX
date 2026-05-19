const http = require("node:http");
const path = require("node:path");

const { loadProxyConfig } = require("../shared/config");
const { appendJsonl, readJson, writeJson } = require("../shared/json-store");
const { addCorsHeaders, enforceLocalAccess, handleHttpError, httpError, makeId, nowSeconds, readJsonBody, sendJson } = require("../shared/http");
const { buildDeepSeekPayload, callDeepSeekJson, getAssistantMessage } = require("./deepseek-client");
const { beginRequest, createRuntime, finishRequest, pushEvent, writeRuntime } = require("./runtime");
const { assistantForStorage, buildConversation, buildResponseRecord, buildStoredRecord, estimateTokensForInput, inputToMessages, normalizeAssistant, normalizeInput, sanitizeToolContent } = require("./conversation");
const { createToolContext, splitToolCalls } = require("./tools");
const { streamDeepSeekResponseV2, turnOutputFromAssistant } = require("./streaming");
const { mapUsage, mergeUsage } = require("./usage");
const { executeProxyWebSearch } = require("./web-search-executor");
const { executeListDirectory, executeReadFileRange, executeWorkspaceSearch } = require("./workspace-tools");

const DEBUG_MAX_BYTES = 256 * 1024;

function main() {
  const config = loadProxyConfig();
  const service = createProxyService(config);
  service.start();
}

function createProxyContext(config) {
  const state = readJson(config.stateFile, { responses: {} });
  if (!state.responses || typeof state.responses !== "object") state.responses = {};

  const runtime = createRuntime(config);
  writeRuntime(config, runtime);

  return { config, state, runtime };
}

function createProxyService(config, options = {}) {
  const context = createProxyContext(config);
  const { runtime } = context;

  const server = http.createServer((req, res) => handleRequest(req, res, context));
  let closed = false;
  let exitTimer = null;
  const parentMonitor = config.parentPid ? setInterval(() => {
    if (!isPidAlive(config.parentPid)) shutdown();
  }, 2000) : null;
  if (parentMonitor) parentMonitor.unref();
  const processHandlers = {
    sigint: () => shutdown(),
    sigterm: () => shutdown(),
    uncaughtException: (error) => fatal(config, runtime, error),
    unhandledRejection: (error) => fatal(config, runtime, error),
  };
  process.on("SIGINT", processHandlers.sigint);
  process.on("SIGTERM", processHandlers.sigterm);
  process.on("uncaughtException", processHandlers.uncaughtException);
  process.on("unhandledRejection", processHandlers.unhandledRejection);
  server.once("close", cleanupProcessHandlers);

  server.on("error", (error) => {
    runtime.status = "error";
    runtime.stopped_at = new Date().toISOString();
    runtime.error = { message: error.message || "Server failed to start.", code: error.code || null };
    writeRuntime(config, runtime);
    console.error(error);
    if (typeof options.onError === "function") options.onError(error);
    if (options.exitOnError !== false) process.exit(1);
  });

  function start() {
    server.listen(config.port, config.host, () => {
      const address = server.address();
      const actualPort = typeof address === "object" && address ? address.port : config.port;
      markProxyRunning(context, { host: config.host, port: actualPort, message: "Proxy process started." });
      console.log("[proxy] Listening on " + runtime.base_url);
      console.log("[proxy] DeepSeek upstream: " + config.deepseekBaseUrl + "/chat/completions");
    });
  }

  function shutdown() {
    markStopped();
    closeServer(() => process.exit(0));
    exitTimer = setTimeout(() => process.exit(0), 500);
    exitTimer.unref();
  }

  function close() {
    markStopped();
    return new Promise((resolve) => closeServer(resolve));
  }

  function markStopped() {
    if (closed) return;
    closed = true;
    markProxyStopped(context, { message: "Proxy process stopped." });
  }

  function closeServer(callback) {
    if (!server.listening) {
      cleanupProcessHandlers();
      callback();
      return;
    }
    server.close(callback);
  }

  function cleanupProcessHandlers() {
    process.off("SIGINT", processHandlers.sigint);
    process.off("SIGTERM", processHandlers.sigterm);
    process.off("uncaughtException", processHandlers.uncaughtException);
    process.off("unhandledRejection", processHandlers.unhandledRejection);
    if (parentMonitor) clearInterval(parentMonitor);
    if (exitTimer) clearTimeout(exitTimer);
    exitTimer = null;
  }

  return {
    close,
    start,
    shutdown,
    server,
    context,
    state: context.state,
    runtime: context.runtime,
    handleRequest(req, res) {
      return handleRequest(req, res, context);
    },
  };
}

function markProxyRunning(context, options = {}) {
  const { config, runtime } = context;
  const host = options.host || config.host;
  const actualPort = Number(options.port || config.port);
  runtime.status = "running";
  runtime.port = actualPort;
  runtime.base_url = "http://" + host + ":" + actualPort + "/v1";
  runtime.error = null;
  runtime.stopped_at = null;
  pushEvent(runtime, {
    type: "proxy_started",
    level: "success",
    message: options.message || "Proxy service started.",
    audience: "user",
    detail: {
      base_url: runtime.base_url,
      pid: runtime.pid,
    },
  });
  writeRuntime(config, runtime);
}

function markProxyStopped(context, options = {}) {
  const { config, runtime } = context;
  runtime.status = "stopped";
  runtime.stopped_at = new Date().toISOString();
  pushEvent(runtime, {
    type: "proxy_stopped",
    level: "info",
    message: options.message || "Proxy service stopped.",
    audience: "user",
    detail: { pid: runtime.pid },
  });
  writeRuntime(config, runtime);
}

async function handleRequest(req, res, context) {
  const { config } = context;
  try {
    const url = new URL(req.url, "http://" + (req.headers.host || "localhost"));
    const route = matchRoute(req.method, url.pathname);
    const preflightRoute = req.method === "OPTIONS" ? matchRoute("GET", url.pathname) || matchRoute("POST", url.pathname) || matchRoute("DELETE", url.pathname) : null;
    const accessRoute = route || preflightRoute;
    if (!accessRoute) throw httpError(404, "Route not found.", "invalid_request_error", "not_found");
    const accessOptions = accessRoute.name === "models" ? { allowDesktopAppOrigins: true } : {};
    if (!enforceLocalAccess(req, res, accessOptions)) return;
    if (accessRoute.name === "models") logModelsRequest(context.runtime, req, config);
    if (req.method === "OPTIONS") {
      addCorsHeaders(req, res, accessOptions);
      res.writeHead(204);
      res.end();
      return;
    }
    if (!route) throw httpError(404, "Route not found.", "invalid_request_error", "not_found");

    if (route.name === "health") {
      sendJson(res, 200, {
        ok: true,
        service: "codex-deepseek-responses-proxy",
        upstream: config.deepseekBaseUrl,
        stored_responses: Object.keys(context.state.responses).length,
      });
      return;
    }

    if (route.name === "models") {
      sendJson(res, 200, modelList(config));
      return;
    }

    if (route.name === "response_retrieve") {
      const record = context.state.responses[route.params.responseId];
      if (!record) throw httpError(404, "Response " + route.params.responseId + " was not found.", "invalid_request_error", "response_not_found");
      sendJson(res, 200, record.response);
      return;
    }

    if (route.name === "response_delete") {
      const existed = Boolean(context.state.responses[route.params.responseId]);
      if (existed) {
        delete context.state.responses[route.params.responseId];
        saveState(context);
      }
      sendJson(res, 200, { id: route.params.responseId, object: "response.deleted", deleted: existed });
      return;
    }

    if (route.name === "response_input_items") {
      const record = context.state.responses[route.params.responseId];
      if (!record) throw httpError(404, "Response " + route.params.responseId + " was not found.", "invalid_request_error", "response_not_found");
      sendJson(res, 200, { object: "list", data: record.input_items || [], has_more: false });
      return;
    }

    const body = await readJsonBody(req, config.requestBodyLimitBytes);
    if (route.name === "input_tokens") {
      sendJson(res, 200, { object: "response.input_tokens", input_tokens: estimateTokensForInput(body.input) });
      return;
    }

    if (route.name === "responses_create") {
      await handleResponsesCreate(req, res, body, context);
      return;
    }
  } catch (error) {
    handleHttpError(res, error);
  }
}

async function handleResponsesCreate(req, res, requestBody, context) {
  const started = Date.now();
  const { config, runtime } = context;
  const modelAlias = applyCodexModelAlias(requestBody, config, runtime);
  requestBody = modelAlias.requestBody;

  const id = makeId("resp");
  const createdAt = nowSeconds();
  const startedAt = new Date(started).toISOString();
  beginRequest(runtime, { id, model: requestBody.model, requestedModel: modelAlias.requestedModel, stream: Boolean(requestBody.stream), startedAt });
  writeRuntime(config, runtime);
  let requestFinished = false;

  try {
    const previousRecord = resolvePreviousRecord(requestBody.previous_response_id, context);
    const normalizedInput = normalizeInput(requestBody.input);
    logIncomingToolItems(runtime, normalizedInput);
    const workspaceScope = resolveWorkspaceScope(requestBody, normalizedInput, config);
    const currentMessages = inputToMessages(normalizedInput);
    const conversationMessages = buildConversation(requestBody, previousRecord, currentMessages);
    const toolContext = createToolContext(requestBody.tools || [], { rootDir: workspaceScope.rootDir, extensionDir: config.extensionDir });
    const contextDiagnostic = captureContextDiagnostic(context, {
      requestBody,
      previousRecord,
      normalizedInput,
      currentMessages,
      conversationMessages,
      workspaceScope,
    });

    writeDebug(config, "latest-request.json", {
      at: new Date().toISOString(),
      mode: requestBody.stream ? "stream" : "sync",
      request_model: requestBody.model,
      requested_model: modelAlias.requestedModel,
      previous_response_id: requestBody.previous_response_id || null,
      normalized_input: normalizedInput,
      upstream_messages: conversationMessages,
      tools: toolContext.upstreamTools,
      response_tools: requestBody.tools || [],
      workspace_scope: workspaceScope,
    });

    if (requestBody.stream) {
      const streamResult = await streamDeepSeekResponseV2(res, {
        id,
        createdAt,
        requestBody,
        messages: conversationMessages,
        toolContext,
        config: Object.assign({}, config, { workspaceScope }),
        runtime,
        authorization: req.headers.authorization || "",
        toVisibleAssistant,
        hostedToolResultMessages,
        logToolCalls,
        logToolResults,
        flushRuntime,
      });
      finishRequest(runtime, {
        id,
        model: requestBody.model,
        requestedModel: modelAlias.requestedModel,
        stream: true,
        status: streamResult && streamResult.failed ? "failed" : "completed",
        usage: streamResult && streamResult.usage,
        requestMs: Date.now() - started,
        startedAt,
        config,
        error: streamResult && streamResult.response && streamResult.response.error ? streamResult.response.error.message : "",
      });
      requestFinished = true;
      writeRuntime(config, runtime);

      if (streamResult && streamResult.response) {
        if (!streamResult.failed) {
          const record = buildStoredRecord({
            id,
            createdAt,
            response: streamResult.response,
            requestBody,
            previousRecord,
            normalizedInput,
            currentMessages,
            storedMessages: streamResult.storedMessages,
            rawAssistant: streamResult.rawAssistant,
            conversationMessages,
          });
          persistRecord(record, context);
        }

        writeDebug(config, "latest-response.json", {
          at: new Date().toISOString(),
          mode: "stream",
          response: streamResult.response,
          upstream_usage: streamResult.usage,
          stored_messages: streamResult.storedMessages || [],
        });
        appendDebug(config, "history.jsonl", { at: new Date().toISOString(), request: requestBody, response: streamResult.response });
        captureContextResponseDiagnostic(context, {
          requestId: id,
          response: streamResult.response,
          usage: streamResult.usage,
          failed: Boolean(streamResult.failed),
          contextDiagnosticId: contextDiagnostic.id,
        });
      }
      return;
    }

    const result = await runDeepSeekTurn({
      requestBody,
      messages: conversationMessages,
      toolContext,
      config: Object.assign({}, config, { workspaceScope }),
      runtime,
      authorization: req.headers.authorization || "",
    });

    finishRequest(runtime, {
      id,
      model: requestBody.model,
      requestedModel: modelAlias.requestedModel,
      stream: false,
      status: "completed",
      usage: result.usage,
      requestMs: Date.now() - started,
      startedAt,
      config,
    });
    requestFinished = true;
    writeRuntime(config, runtime);

    const response = buildResponseRecord({
      id,
      createdAt,
      model: requestBody.model,
      output: result.output,
      usage: result.usage,
    });

    const record = buildStoredRecord({
      id,
      createdAt,
      response,
      requestBody,
      previousRecord,
      normalizedInput,
      currentMessages,
      storedMessages: result.storedMessages,
      rawAssistant: result.rawAssistant,
      conversationMessages,
    });
    persistRecord(record, context);

    writeDebug(config, "latest-response.json", {
      at: new Date().toISOString(),
      mode: requestBody.stream ? "stream" : "sync",
      response,
      upstream_usage: result.usage,
      stored_messages: result.storedMessages,
    });
    appendDebug(config, "history.jsonl", { at: new Date().toISOString(), request: requestBody, response });
    captureContextResponseDiagnostic(context, {
      requestId: id,
      response,
      usage: result.usage,
      failed: false,
      contextDiagnosticId: contextDiagnostic.id,
    });

    sendJson(res, 200, response);
  } catch (error) {
    if (!requestFinished) {
      finishRequest(runtime, {
        id,
        model: requestBody.model,
        requestedModel: modelAlias.requestedModel,
        stream: Boolean(requestBody.stream),
        status: "failed",
        requestMs: Date.now() - started,
        startedAt,
        config,
        error: error && error.message ? error.message : String(error),
      });
      writeRuntime(config, runtime);
    }
    throw error;
  }
}

function applyCodexModelAlias(requestBody, config, runtime) {
  const requestedModel = String(requestBody && requestBody.model || "");
  const mappedModel = resolveCodexModelAlias(requestedModel, config);
  if (!mappedModel || mappedModel === requestedModel) {
    return { requestBody, requestedModel, model: requestedModel, aliased: false };
  }
  pushEvent(runtime, {
    type: "model_alias_applied",
    level: "info",
    message: "Codex official model request mapped to CodeSeeX model.",
    audience: "user",
    detail: {
      requested_model: requestedModel,
      model: mappedModel,
    },
  });
  return {
    requestBody: Object.assign({}, requestBody, { model: mappedModel }),
    requestedModel,
    model: mappedModel,
    aliased: true,
  };
}

function resolveWorkspaceScope(requestBody, normalizedInput, config = {}) {
  const roots = [];
  for (const candidate of workspaceRootCandidates(requestBody, normalizedInput, config)) {
    const normalized = normalizeExistingDirectory(candidate);
    if (!normalized || hasPath(roots, normalized)) continue;
    roots.push(normalized);
  }
  const fallbackRoot = normalizeExistingDirectory(config.rootDir) || process.cwd();
  if (roots.length === 0) roots.push(fallbackRoot);

  return {
    rootDir: roots[0],
    roots,
    allowOutsideWorkspace: workspaceToolsAllowOutside(requestBody, normalizedInput, config),
    fileAccess: normalizeWorkspaceFileAccess(config.workspaceToolFileAccess),
  };
}

function workspaceRootCandidates(requestBody, normalizedInput, config = {}) {
  const candidates = [];
  collectPathLikeValues(requestBody, candidates);
  collectPathLikeValues(normalizedInput, candidates);
  for (const root of Array.isArray(config.workspaceRoots) ? config.workspaceRoots : []) candidates.push(root);
  if (config.rootDir) candidates.push(config.rootDir);
  return candidates.filter((candidate) => looksLikeUsefulWorkspacePath(candidate, config));
}

function collectPathLikeValues(value, output, key = "") {
  if (output.length > 80 || value === undefined || value === null) return;
  if (typeof value === "string") {
    collectPathLikeText(value, output, key);
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value.slice(0, 200)) collectPathLikeValues(item, output, key);
    return;
  }
  if (typeof value !== "object") return;
  for (const [childKey, child] of Object.entries(value)) {
    const nextKey = key ? key + "." + childKey : childKey;
    if (isLikelyPathKey(childKey)) collectPathLikeValues(child, output, nextKey);
    else if (childKey === "content" || childKey === "text" || childKey === "input") collectPathLikeValues(child, output, nextKey);
  }
}

function collectPathLikeText(text, output, key = "") {
  const value = String(text || "");
  if (!value) return;
  if (isLikelyPathKey(key)) {
    addPathCandidate(output, value);
    return;
  }

  for (const pattern of [
    /<cwd>\s*([^<\r\n]+)\s*<\/cwd>/gi,
    /<[^>]*(?:cwd|workdir|workspace|project)[^>]*>\s*([^<\r\n]+)\s*<\/[^>]+>/gi,
    /(?:^|\n)\s*(?:cwd|workdir|workspace|workspace_root|project_root|current_dir|current_directory)\s*[:=]\s*["']?([^"'\r\n]+)["']?/gi,
    /(?:cwd|workdir|workspace|project)\s+is\s+["']?([^"'\r\n]+)["']?/gi,
  ]) {
    let match;
    while ((match = pattern.exec(value)) !== null) addPathCandidate(output, match[1]);
  }
}

function addPathCandidate(output, value) {
  const text = stripPathCandidate(value);
  if (!text || !looksLikeAbsolutePath(text)) return;
  output.push(text);
}

function stripPathCandidate(value) {
  return String(value || "")
    .trim()
    .replace(/^`+|`+$/g, "")
    .replace(/^["']|["']$/g, "")
    .replace(/[),.;\]]+$/g, "");
}

function isLikelyPathKey(key) {
  return /(?:^|[_.-])(?:cwd|workdir|workspace|workspace_root|project_root|current_dir|current_directory|root_dir|root|path|dir|directory)(?:$|[_.-])/i.test(String(key || ""));
}

function looksLikeUsefulWorkspacePath(candidate, config = {}) {
  const value = stripPathCandidate(candidate);
  if (!looksLikeAbsolutePath(value)) return false;
  const resolved = normalizeExistingDirectory(value);
  if (!resolved) return false;
  const ignored = [
    config.dataDir,
    config.debugDir,
    config.extensionDir,
  ].map(normalizeExistingDirectory).filter(Boolean);
  return !ignored.some((dir) => samePathOrInside(resolved, dir));
}

function looksLikeAbsolutePath(value) {
  const text = String(value || "").trim();
  if (!text) return false;
  return /^[a-zA-Z]:[\\/]/.test(text) || /^\\\\[^\\]/.test(text) || text.startsWith("/");
}

function normalizeExistingDirectory(value) {
  const raw = stripPathCandidate(value);
  if (!raw) return "";
  try {
    const resolved = path.resolve(raw);
    const stat = safeStatLocal(resolved);
    if (stat && stat.isDirectory()) return canonicalPath(resolved);
    if (stat && stat.isFile()) return canonicalPath(path.dirname(resolved));
    const parent = safeStatLocal(path.dirname(resolved));
    if (parent && parent.isDirectory()) return canonicalPath(path.dirname(resolved));
  } catch {}
  return "";
}

function safeStatLocal(filePath) {
  try {
    return filePath ? require("node:fs").statSync(filePath) : null;
  } catch {
    return null;
  }
}

function canonicalPath(filePath) {
  try {
    return require("node:fs").realpathSync.native(path.resolve(filePath));
  } catch {
    return path.resolve(filePath);
  }
}

function workspaceToolsAllowOutside(requestBody, normalizedInput, config = {}) {
  const mode = normalizeWorkspaceFileAccess(config.workspaceToolFileAccess);
  if (mode === "all") return true;
  if (mode === "workspace") return false;
  return requestIndicatesFullFileAccess(requestBody) || requestIndicatesFullFileAccess(normalizedInput);
}

function normalizeWorkspaceFileAccess(value) {
  const mode = String(value || "auto").trim().toLowerCase();
  if (["all", "full", "danger-full-access", "unrestricted"].includes(mode)) return "all";
  if (["workspace", "restricted", "safe"].includes(mode)) return "workspace";
  return "auto";
}

function requestIndicatesFullFileAccess(value) {
  const text = JSON.stringify(value || "").toLowerCase();
  return text.includes("danger-full-access")
    || text.includes("sandbox_mode\":\"danger")
    || text.includes("sandbox\":\"elevated")
    || text.includes("windows.sandbox\":\"elevated")
    || text.includes("approval_policy\":\"never");
}

function hasPath(paths, candidate) {
  return paths.some((item) => samePath(item, candidate));
}

function samePath(left, right) {
  const a = normalizeComparablePath(left);
  const b = normalizeComparablePath(right);
  return Boolean(a && b && a === b);
}

function samePathOrInside(target, root) {
  if (samePath(target, root)) return true;
  const relative = path.relative(root, target);
  return Boolean(relative && !relative.startsWith("..") && !path.isAbsolute(relative));
}

function normalizeComparablePath(value) {
  const resolved = path.resolve(String(value || ""));
  return process.platform === "win32" ? resolved.toLowerCase() : resolved;
}

function resolveCodexModelAlias(model, config = {}) {
  const requested = String(model || "");
  const preferred = CODEX_OFFICIAL_MODEL_ALIASES[requested];
  if (!preferred) return requested;
  return firstAvailableModel(config, preferred);
}

const CODEX_OFFICIAL_MODEL_ALIASES = Object.freeze({
  "gpt-5.4": ["deepseek-v4-pro", "deepseek-v4-flash"],
  "gpt-5.4-mini": ["deepseek-v4-flash", "deepseek-v4-pro"],
});

function firstAvailableModel(config, preferredModels) {
  const availableModels = (Array.isArray(config.availableModels) ? config.availableModels : [])
    .map((model) => String(model || ""))
    .filter(Boolean);
  const available = new Set(availableModels);
  for (const model of preferredModels) {
    if (available.has(model)) return model;
  }
  return availableModels.find((model) => /^deepseek/i.test(model)) || preferredModels[0];
}

async function runDeepSeekTurn({ requestBody, messages, toolContext, config, runtime = null, authorization, callJson = callDeepSeekJson }) {
  const workingMessages = messages.slice();
  let usage = null;
  const storedMessages = [];
  let rawAssistant = { role: "assistant", content: "" };

  while (true) {
    const payload = buildDeepSeekPayload(requestBody, workingMessages, toolContext, config, { stream: false });
    const completion = await callJson(payload, config, authorization);
    usage = mergeUsage(usage, mapUsage(completion.usage));

    const currentAssistant = normalizeAssistant(getAssistantMessage(completion));
    rawAssistant = currentAssistant;
    const { external, internal, hosted } = splitToolCalls(currentAssistant.tool_calls, toolContext);
    logToolCalls(runtime, internal, toolContext, "internal");
    logToolCalls(runtime, hosted, toolContext, "hosted");
    logToolCalls(runtime, external, toolContext, "external");
    flushRuntime(config, runtime);

    if (hosted.length > 0) {
      const visibleAssistant = toVisibleAssistant(currentAssistant, toolContext, {
        includeHostedCalls: true,
        includeInternalPatchCalls: internal.length > 0,
      });
      const hostedResult = await hostedToolResultMessages(hosted, toolContext, config, workingMessages);
      logToolResults(runtime, hostedResult.toolMessages, "hosted");
      flushRuntime(config, runtime);
      storedMessages.push(assistantForStorage(visibleAssistant));
      storedMessages.push(...hostedResult.toolMessages);
      if (external.length > 0 || internal.length > 0) {
        return {
          rawAssistant: visibleAssistant,
          output: turnOutputFromAssistant(visibleAssistant, usage, toolContext, config, { phase: "commentary" }),
          usage,
          storedMessages,
        };
      }
      workingMessages.push(visibleAssistant, ...hostedResult.toolMessages);
      continue;
    }

    if (internal.length > 0) {
      const visibleAssistant = toVisibleAssistant(currentAssistant, toolContext, { includeInternalPatchCalls: true });
      storedMessages.push(assistantForStorage(visibleAssistant));
      return {
        rawAssistant: visibleAssistant,
        output: turnOutputFromAssistant(visibleAssistant, usage, toolContext, config, { phase: "commentary" }),
        usage,
        storedMessages,
      };
    }

    const visibleAssistant = toVisibleAssistant(currentAssistant, toolContext);
    storedMessages.push(assistantForStorage(currentAssistant));
    return {
      rawAssistant,
        output: turnOutputFromAssistant(visibleAssistant, usage, toolContext, config, { phase: "final_answer" }),
      usage,
      storedMessages,
    };
  }

}

function captureContextDiagnostic(context, details) {
  const { config, runtime } = context;
  const summary = buildContextDiagnosticSummary(details);
  pushEvent(runtime, {
    type: "context_diagnostic",
    level: summary.compaction_input_items > 0 || summary.context_management_count > 0 ? "warn" : "info",
    message: "Context diagnostic captured.",
    audience: "diagnostic",
    detail: summary,
  });
  writeRuntime(config, runtime);
  writeDebug(config, "latest-context-diagnostic.json", summary);
  appendDebug(config, "context-diagnostics.jsonl", summary);
  return summary;
}

function captureContextResponseDiagnostic(context, details) {
  const { config, runtime } = context;
  const summary = buildContextResponseDiagnosticSummary(details);
  pushEvent(runtime, {
    type: "context_response_diagnostic",
    level: summary.compaction_output_items > 0 ? "warn" : "info",
    message: "Context response diagnostic captured.",
    audience: "diagnostic",
    detail: summary,
  });
  writeRuntime(config, runtime);
  writeDebug(config, "latest-context-response-diagnostic.json", summary);
  appendDebug(config, "context-response-diagnostics.jsonl", summary);
  return summary;
}

function buildContextDiagnosticSummary({ requestBody, previousRecord, normalizedInput, currentMessages, conversationMessages }) {
  const inputItems = Array.isArray(requestBody && requestBody.input) ? requestBody.input : requestBody && requestBody.input !== undefined ? [requestBody.input] : [];
  const inputTypeCounts = countInputItemTypes(normalizedInput);
  const conversationRoleCounts = countMessageRoles(conversationMessages);
  const toolCounts = countChatToolProtocol(conversationMessages);
  const previousMessages = previousRecord && Array.isArray(previousRecord.upstream_messages) ? previousRecord.upstream_messages : [];
  const previousOutput = previousRecord && previousRecord.response && Array.isArray(previousRecord.response.output) ? previousRecord.response.output : [];
  const previousInput = previousRecord && Array.isArray(previousRecord.input_items) ? previousRecord.input_items : [];
  const contextManagement = requestBody ? requestBody.context_management : undefined;
  const conversationBytes = safeJsonByteLength(conversationMessages);
  const previousBytes = safeJsonByteLength(previousMessages);
  return {
    id: makeId("ctxdiag"),
    at: new Date().toISOString(),
    model: requestBody && requestBody.model || "",
    stream: Boolean(requestBody && requestBody.stream),
    previous_response_id_present: Boolean(requestBody && requestBody.previous_response_id),
    previous_record_found: Boolean(previousRecord),
    context_management_count: countContextManagementEntries(contextManagement),
    context_management_present: contextManagement !== undefined,
    context_management_shape: summarizeContextManagementShape(contextManagement),
    raw_input_item_count: inputItems.length,
    normalized_input_item_count: Array.isArray(normalizedInput) ? normalizedInput.length : 0,
    input_item_counts: inputTypeCounts,
    compaction_input_items: Number(inputTypeCounts.compaction || 0),
    reasoning_input_items: Number(inputTypeCounts.reasoning || 0),
    estimated_input_tokens: estimateTokensForInput(requestBody && requestBody.input),
    current_message_count: Array.isArray(currentMessages) ? currentMessages.length : 0,
    current_messages_json_bytes: safeJsonByteLength(currentMessages),
    conversation_message_count: Array.isArray(conversationMessages) ? conversationMessages.length : 0,
    conversation_role_counts: conversationRoleCounts,
    conversation_json_bytes: conversationBytes,
    estimated_conversation_tokens: estimateTokensFromBytes(conversationBytes),
    assistant_tool_call_count: toolCounts.assistant_tool_call_count,
    tool_message_count: toolCounts.tool_message_count,
    previous_input_item_count: previousInput.length,
    previous_output_item_count: previousOutput.length,
    previous_output_item_counts: countInputItemTypes(previousOutput),
    previous_compaction_output_items: countItemsByType(previousOutput, "compaction"),
    previous_upstream_message_count: previousMessages.length,
    previous_upstream_json_bytes: previousBytes,
    estimated_previous_upstream_tokens: estimateTokensFromBytes(previousBytes),
  };
}

function buildContextResponseDiagnosticSummary({ requestId, response, usage, failed, contextDiagnosticId }) {
  const output = response && Array.isArray(response.output) ? response.output : [];
  const outputTypeCounts = countInputItemTypes(output);
  const responseBytes = safeJsonByteLength(summarizeResponseShape(response));
  return {
    id: makeId("ctxresdiag"),
    at: new Date().toISOString(),
    request_id: requestId || "",
    context_diagnostic_id: contextDiagnosticId || "",
    response_id: response && response.id || "",
    status: response && response.status || (failed ? "failed" : "completed"),
    failed: Boolean(failed),
    output_item_count: output.length,
    output_item_counts: outputTypeCounts,
    compaction_output_items: Number(outputTypeCounts.compaction || 0),
    reasoning_output_items: Number(outputTypeCounts.reasoning || 0),
    message_output_items: Number(outputTypeCounts.message || 0),
    function_call_output_items: Number(outputTypeCounts.function_call || 0),
    response_shape_json_bytes: responseBytes,
    usage: summarizeUsageShape(usage || response && response.usage),
  };
}

function countInputItemTypes(items) {
  const counts = {};
  for (const item of Array.isArray(items) ? items : []) {
    const type = inputItemType(item);
    counts[type] = Number(counts[type] || 0) + 1;
  }
  return counts;
}

function inputItemType(item) {
  if (typeof item === "string") return "string";
  if (!item || typeof item !== "object") return "unknown";
  if (item.type) return String(item.type);
  if (item.role) return "message";
  return "object";
}

function countItemsByType(items, type) {
  return (Array.isArray(items) ? items : []).filter((item) => inputItemType(item) === type).length;
}

function countMessageRoles(messages) {
  const counts = {};
  for (const message of Array.isArray(messages) ? messages : []) {
    const role = message && message.role ? String(message.role) : "unknown";
    counts[role] = Number(counts[role] || 0) + 1;
  }
  return counts;
}

function countChatToolProtocol(messages) {
  let assistantToolCalls = 0;
  let toolMessages = 0;
  for (const message of Array.isArray(messages) ? messages : []) {
    if (!message || typeof message !== "object") continue;
    if (message.role === "assistant" && Array.isArray(message.tool_calls)) assistantToolCalls += message.tool_calls.length;
    if (message.role === "tool") toolMessages += 1;
  }
  return {
    assistant_tool_call_count: assistantToolCalls,
    tool_message_count: toolMessages,
  };
}

function countContextManagementEntries(value) {
  if (value === undefined) return 0;
  if (Array.isArray(value)) return value.length;
  return 1;
}

function summarizeContextManagementShape(value) {
  if (value === undefined) return null;
  if (Array.isArray(value)) {
    return {
      kind: "array",
      length: value.length,
      item_kinds: value.slice(0, 8).map((item) => item === null ? "null" : Array.isArray(item) ? "array" : typeof item),
    };
  }
  if (!value || typeof value !== "object") return { kind: typeof value };
  return {
    kind: "object",
    keys: Object.keys(value).slice(0, 20),
  };
}

function summarizeResponseShape(response) {
  if (!response || typeof response !== "object") return null;
  return {
    id: response.id || "",
    status: response.status || "",
    output: Array.isArray(response.output) ? response.output.map((item) => ({ type: inputItemType(item), status: item && item.status || undefined })) : [],
    usage: summarizeUsageShape(response.usage),
  };
}

function summarizeUsageShape(usage) {
  if (!usage || typeof usage !== "object") return null;
  return {
    input_tokens: Number(usage.input_tokens || usage.prompt_tokens || 0),
    cached_input_tokens: Number(usage.cached_input_tokens || usage.prompt_cache_hit_tokens || ((usage.input_tokens_details || usage.prompt_tokens_details || {}).cached_tokens) || 0),
    cache_miss_input_tokens: Number(usage.cache_miss_input_tokens || 0),
    output_tokens: Number(usage.output_tokens || usage.completion_tokens || 0),
    reasoning_output_tokens: Number(usage.reasoning_output_tokens || ((usage.output_tokens_details || usage.completion_tokens_details || {}).reasoning_tokens) || 0),
    total_tokens: Number(usage.total_tokens || 0),
  };
}

function safeJsonByteLength(value) {
  try {
    return Buffer.byteLength(JSON.stringify(value || null), "utf8");
  } catch {
    return 0;
  }
}

function estimateTokensFromBytes(bytes) {
  return Math.max(0, Math.ceil((Number(bytes) || 0) / 4));
}

function toVisibleAssistant(rawAssistant, toolContext, options = {}) {
  const visible = Object.assign({}, rawAssistant || {});
  const { external, internal, hosted } = splitToolCalls(visible.tool_calls, toolContext);
  const allowedIds = new Set(external.map((call) => call && call.id).filter(Boolean));
  if (options.includeInternalPatchCalls) {
    for (const call of internal) if (call && call.id) allowedIds.add(call.id);
  }
  if (options.includeHostedCalls) {
    for (const call of hosted) if (call && call.id) allowedIds.add(call.id);
  }
  const toolCalls = (Array.isArray(visible.tool_calls) ? visible.tool_calls : [])
    .filter((call) => call && call.id && allowedIds.has(call.id));
  if (toolCalls.length > 0) visible.tool_calls = toolCalls;
  else delete visible.tool_calls;
  return visible;
}

async function hostedToolResultMessages(toolCalls, toolContext, config = {}, messages = []) {
  const toolMessages = [];
  const visibleItems = [];
  const calls = Array.isArray(toolCalls) ? toolCalls : [];
  let executedCount = 0;
  for (const toolCall of calls) {
    const item = toolContext.responseToolItemFromChat(toolCall);
    visibleItems.push(item);
    executedCount += 1;
    toolMessages.push({
      role: "tool",
      tool_call_id: toolCall.id || makeId("call"),
      content: sanitizeToolContent(JSON.stringify(await proxyHostedToolContent(item, config, messages))),
    });
  }
  return { toolMessages, visibleItems, executedCount };
}

function logToolCalls(runtime, toolCalls, toolContext, scope) {
  for (const toolCall of Array.isArray(toolCalls) ? toolCalls : []) {
    const name = toolCall && toolCall.function ? toolCall.function.name || "tool" : "tool";
    pushEvent(runtime, {
      type: "tool_call",
      level: scope === "external" ? "warn" : "info",
      message: toolScopeLabel(scope) + "tool call: " + name,
      audience: "user",
      detail: {
        scope,
        name,
        call_id: toolCall.id || "",
      },
    });
  }
}

function logToolResults(runtime, toolMessages, scope) {
  for (const message of Array.isArray(toolMessages) ? toolMessages : []) {
    pushEvent(runtime, {
      type: "tool_result",
      level: "success",
      message: toolScopeLabel(scope) + "tool result injected.",
      audience: "user",
      detail: {
        scope,
        call_id: message.tool_call_id || "",
        bytes: typeof message.content === "string" ? Buffer.byteLength(message.content) : 0,
      },
    });
  }
}

function logIncomingToolItems(runtime, items) {
  for (const item of Array.isArray(items) ? items : []) {
    if (!item || typeof item !== "object") continue;
    if (isIncomingToolCallItem(item)) {
      pushEvent(runtime, {
        type: "tool_call",
        level: "info",
        message: "Client tool call: " + incomingToolName(item),
        audience: "user",
        detail: {
          scope: "response",
          name: incomingToolName(item),
          call_id: item.call_id || item.id || "",
        },
      });
      continue;
    }
    if (isIncomingToolOutputItem(item)) {
      pushEvent(runtime, {
        type: "tool_result",
        level: "success",
        message: "Client tool result returned.",
        audience: "user",
        detail: {
          scope: "response",
          name: incomingToolName(item),
          call_id: item.call_id || item.tool_call_id || item.id || "",
        },
      });
    }
  }
}

function toolScopeLabel(scope) {
  if (scope === "hosted") return "Proxy ";
  if (scope === "internal") return "Internal ";
  if (scope === "external") return "External ";
  return "";
}

function isIncomingToolCallItem(item) {
  return item.type === "function_call"
    || item.type === "custom_tool_call"
    || item.type === "apply_patch_call"
    || item.type === "web_search_call";
}

function isIncomingToolOutputItem(item) {
  return item.type === "function_call_output"
    || item.type === "custom_tool_call_output"
    || item.type === "apply_patch_call_output"
    || item.type === "web_search_call_output";
}

function incomingToolName(item) {
  if (item.name) return String(item.name);
  if (item.type === "apply_patch_call" || item.type === "apply_patch_call_output") return "apply_patch";
  if (item.type === "web_search_call" || item.type === "web_search_call_output") return "web_search";
  if (item.type === "custom_tool_call" || item.type === "custom_tool_call_output") return "custom_tool";
  return "tool";
}

function flushRuntime(config, runtime) {
  if (!runtime) return;
  writeRuntime(config, runtime);
}

async function proxyHostedToolContent(item, config, messages = []) {
  if (item && (item.type === "function_call" || item.type === "proxy_tool_call") && item.name === "workspace_search") {
    return executeWorkspaceSearch(item.arguments, config);
  }
  if (item && (item.type === "function_call" || item.type === "proxy_tool_call") && item.name === "read_file_range") {
    return executeReadFileRange(item.arguments, config);
  }
  if (item && (item.type === "function_call" || item.type === "proxy_tool_call") && item.name === "list_directory") {
    return executeListDirectory(item.arguments, config);
  }
  return proxyWebSearchToolContent(item, config, messages);
}

async function proxyWebSearchToolContent(item, config, messages = []) {
  const action = item && item.action ? item.action : {};
  const result = await executeProxyWebSearch(action, config, { messages });
  if (!result.ok) {
    return {
      error: "proxy_web_search_failed",
      message: "Proxy web search was enabled, but no usable search results were returned. Do not invent search results.",
      query: action.query || action.queries || action.open_urls || action.open_ids || "",
      details: result,
    };
  }
  return {
    query: result.query,
    queries: result.queries,
    grouped_results: result.grouped_results,
    source: result.source,
    quality: result.quality,
    low_confidence: result.low_confidence,
    results: result.results,
    opened_results: result.opened_results,
    opened_count: result.opened_count,
    stage: result.stage,
    mode: result.mode,
    next_action: result.next_action,
  };
}

function modelList(config) {
  return {
    object: "list",
    data: config.availableModels.map((model) => ({
      id: model,
      object: "model",
      created: 0,
      owned_by: "deepseek",
    })),
  };
}

function logModelsRequest(runtime, req, config) {
  pushEvent(runtime, {
    type: "models_requested",
    level: "info",
    message: "Model list requested.",
    audience: "diagnostic",
    detail: {
      origin: String((req.headers && req.headers.origin) || ""),
      method: String(req.method || ""),
      user_agent: String((req.headers && req.headers["user-agent"]) || "").slice(0, 200),
      models: config.availableModels.slice(),
    },
  });
}

function resolvePreviousRecord(id, context) {
  if (!id) return null;
  const record = context.state.responses[id];
  if (!record) throw httpError(404, "Response " + id + " was not found.", "invalid_request_error", "response_not_found");
  return record;
}

function persistRecord(record, context) {
  context.state.responses[record.id] = record;
  trimState(context);
  saveState(context);
}

function trimState(context) {
  const records = Object.values(context.state.responses).sort((a, b) => a.created_at - b.created_at);
  while (records.length > context.config.maxStoredResponses) {
    const oldest = records.shift();
    if (oldest) delete context.state.responses[oldest.id];
  }
}

function saveState(context) {
  writeJson(context.config.stateFile, context.state);
}

function writeDebug(config, filename, value) {
  if (!config.debugEnabled) return;
  try {
    writeJson(path.join(config.debugDir, filename), sanitizeDebugValue(value));
  } catch {}
}

function appendDebug(config, filename, value) {
  if (!config.debugEnabled) return;
  try {
    appendJsonl(path.join(config.debugDir, filename), sanitizeDebugValue(value), {
      retentionDays: config.logRetentionDays,
      maxLines: 500,
      maxBytes: DEBUG_MAX_BYTES,
    });
  } catch {}
}

function sanitizeDebugValue(value) {
  const seen = new WeakSet();
  return sanitize(value, 0);

  function sanitize(current, depth) {
    if (depth > 8) return "[truncated: max debug depth]";
    if (typeof current === "string") return truncateDebugString(current);
    if (!current || typeof current !== "object") return current;
    if (seen.has(current)) return "[circular]";
    if (!Array.isArray(current) && current.type === "compaction") return sanitizeCompactionDebugItem(current);
    seen.add(current);
    if (Array.isArray(current)) return current.slice(0, 80).map((item) => sanitize(item, depth + 1));
    const output = {};
    for (const [key, child] of Object.entries(current)) {
      if (key === "context_management") {
        output[key] = summarizeContextManagementShape(child);
        continue;
      }
      if (isSensitiveDebugKey(key)) {
        output[key] = child ? "********" : child;
        continue;
      }
      if (key === "reasoning_content" || key === "encrypted_content") {
        output[key] = "[redacted]";
        continue;
      }
      output[key] = sanitize(child, depth + 1);
    }
    return output;
  }
}

function sanitizeCompactionDebugItem(item) {
  return {
    type: "compaction",
    id: item.id || undefined,
    status: item.status || undefined,
    keys: Object.keys(item).filter((key) => key !== "summary" && key !== "content" && key !== "encrypted_content").slice(0, 20),
  };
}

function truncateDebugString(value) {
  const text = String(value || "");
  if (Buffer.byteLength(text, "utf8") <= DEBUG_MAX_BYTES) return text;
  return text.slice(0, DEBUG_MAX_BYTES) + "\n[truncated: debug value exceeded " + DEBUG_MAX_BYTES + " bytes]";
}

function isSensitiveDebugKey(key) {
  return /(?:authorization|api[_-]?key|token|secret|password|http_proxy|https_proxy|all_proxy|proxy)$/i.test(String(key || ""));
}

function matchRoute(method, pathname) {
  const routes = [
    { method: "GET", regex: /^\/(?:v1\/)?healthz$/, name: "health" },
    { method: "GET", regex: /^\/(?:v1\/)?models$/, name: "models" },
    { method: "POST", regex: /^\/(?:v1\/)?responses$/, name: "responses_create" },
    { method: "POST", regex: /^\/(?:v1\/)?responses\/input_tokens$/, name: "input_tokens" },
    { method: "GET", regex: /^\/(?:v1\/)?responses\/([^/]+)$/, name: "response_retrieve", params: ["responseId"] },
    { method: "DELETE", regex: /^\/(?:v1\/)?responses\/([^/]+)$/, name: "response_delete", params: ["responseId"] },
    { method: "GET", regex: /^\/(?:v1\/)?responses\/([^/]+)\/input_items$/, name: "response_input_items", params: ["responseId"] },
  ];

  for (const route of routes) {
    if (route.method !== method) continue;
    const match = pathname.match(route.regex);
    if (!match) continue;
    const params = {};
    (route.params || []).forEach((key, index) => {
      params[key] = match[index + 1];
    });
    return { name: route.name, params };
  }
  return null;
}

function isPidAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function fatal(config, runtime, error) {
  runtime.status = "error";
  runtime.error = { message: error && error.message ? error.message : String(error), code: "fatal" };
  runtime.stopped_at = new Date().toISOString();
  writeRuntime(config, runtime);
  console.error(error);
  process.exit(1);
}

module.exports = {
  buildContextDiagnosticSummary,
  buildContextResponseDiagnosticSummary,
  createProxyContext,
  createProxyService,
  handleRequest,
  main,
  markProxyRunning,
  markProxyStopped,
  runDeepSeekTurn,
};
