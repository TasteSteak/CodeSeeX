const http = require("node:http");
const path = require("node:path");

const { loadProxyConfig } = require("../shared/config");
const { appendJsonl, readJsonStrict, writeJson, writeJsonCompact } = require("../shared/json-store");
const { addCorsHeaders, enforceLocalAccess, handleHttpError, httpError, makeId, nowSeconds, readJsonBody, sendJson } = require("../shared/http");
const { buildDeepSeekPayload, callDeepSeekJson, getAssistantMessage, resolveChatCompletionsUrl } = require("./deepseek-client");
const { resolvePreviousContext } = require("./conversation-state");
const { beginRequest, createRuntime, finishRequest, pushEvent, writeRuntime } = require("./runtime");
const { assistantForStorage, buildResponseRecord, buildStoredRecord, estimateTokensForInput, normalizeAssistant, normalizeInput, sanitizeToolContent } = require("./conversation");
const { buildCodeseexCompactionItem, buildCodeseexCompactionWindow, buildTurnStorageMessages, compileContext, mergeTurnToolFactsForStorage } = require("./context-compiler");
const { createToolContext, splitToolCalls } = require("./tools");
const { streamDeepSeekResponseV2, turnOutputFromAssistant } = require("./streaming");
const { toolOutputValueToText } = require("./text-utils");
const { mapUsage, mergeUsage } = require("./usage");
const { executeListDirectory, executeReadFileRange, executeWorkspaceSearch } = require("./workspace-tools");

const DEBUG_MAX_BYTES = 256 * 1024;

function main() {
  const config = loadProxyConfig();
  const service = createProxyService(config);
  service.start();
}

function createProxyContext(config) {
  const runtime = createRuntime(config);
  let state;
  try {
    state = readJsonStrict(config.stateFile, { responses: {} });
    if (!state.responses || typeof state.responses !== "object" || Array.isArray(state.responses)) state.responses = {};
    const recovery = recoverInterruptedResponses(state);
    if (recovery.count > 0) {
      try {
        writeJsonCompact(config.stateFile, state);
      } catch (error) {
        runtime.status = "error";
        runtime.stopped_at = new Date().toISOString();
        runtime.error = {
          code: "STATE_FILE_WRITE_FAILED",
          message: error && error.message ? error.message : "Proxy state file could not be updated.",
          path: config.stateFile,
        };
        pushEvent(runtime, {
          type: "state_persist_failed",
          level: "error",
          message: "Interrupted request recovery could not be saved.",
          audience: "diagnostic",
          detail: runtime.error,
        });
        writeRuntime(config, runtime);
        throw error;
      }
      pushEvent(runtime, {
        type: "state_recovered_interrupted",
        level: "warn",
        message: "Recovered interrupted in-progress response checkpoints.",
        audience: "diagnostic",
        detail: {
          interrupted_count: recovery.count,
          response_ids: recovery.ids.slice(0, 20),
        },
      });
    }
  } catch (error) {
    if (runtime.error && runtime.error.code === "STATE_FILE_WRITE_FAILED") throw error;
    runtime.status = "error";
    runtime.stopped_at = new Date().toISOString();
    runtime.error = {
      code: "STATE_FILE_INVALID",
      message: error && error.message ? error.message : "Proxy state file is invalid.",
      path: config.stateFile,
    };
    pushEvent(runtime, {
      type: "state_load_failed",
      level: "error",
      message: "Proxy state file is invalid. CodeSeeX will not overwrite it.",
      audience: "diagnostic",
      detail: runtime.error,
    });
    writeRuntime(config, runtime);
    throw error;
  }
  writeRuntime(config, runtime);

  return { config, state, runtime };
}

function recoverInterruptedResponses(state) {
  const responses = state && state.responses && typeof state.responses === "object" ? state.responses : {};
  const interruptedAt = new Date().toISOString();
  const ids = [];
  for (const [id, record] of Object.entries(responses)) {
    if (!record || typeof record !== "object") continue;
    if (normalizeLifecycleStatus(record.status) !== "in_progress") continue;
    ids.push(id);
    record.status = "interrupted";
    record.interrupted_at = interruptedAt;
    record.updated_at = interruptedAt;
    record.interruption_reason = "proxy_started_with_in_progress_checkpoint";
    if (record.response && typeof record.response === "object") {
      record.response.status = "interrupted";
      record.response.error = record.response.error || {
        message: "Request was interrupted before completion.",
        type: "api_error",
        code: "request_interrupted",
      };
    }
    if (record.context_diagnostic && typeof record.context_diagnostic === "object") {
      record.context_diagnostic.lifecycle = Object.assign({}, record.context_diagnostic.lifecycle || {}, {
        status: "interrupted",
        interrupted_at: interruptedAt,
        interruption_reason: "proxy_started_with_in_progress_checkpoint",
      });
    }
  }
  return { count: ids.length, ids };
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
    if (options.logErrors !== false) console.error(error);
    if (typeof options.onError === "function") options.onError(error);
    if (options.exitOnError !== false) process.exit(1);
  });

  function start() {
    server.listen(config.port, config.host, () => {
      const address = server.address();
      const actualPort = typeof address === "object" && address ? address.port : config.port;
      markProxyRunning(context, { host: config.host, port: actualPort, message: "Proxy process started." });
      console.log("[proxy] Listening on " + runtime.base_url);
      console.log("[proxy] DeepSeek upstream: " + resolveChatCompletionsUrl(config.deepseekBaseUrl, { officialV1Compat: config.deepseekOfficialV1Compat }));
    });
  }

  function shutdown() {
    markStopped();
    closeServer(() => process.exit(0));
    exitTimer = setTimeout(() => process.exit(0), 500);
    exitTimer.unref();
  }

  function close(options = {}) {
    if (!options.preserveRuntime) markStopped();
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

    if (route.name === "responses_compact") {
      await handleResponsesCompact(req, res, body, context);
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

async function handleResponsesCompact(req, res, requestBody, context) {
  const { config, runtime } = context;
  const id = makeId("resp");
  const createdAt = nowSeconds();
  const previousRecord = resolvePreviousContext(requestBody.previous_response_id, context);
  const normalizedInput = normalizeInput(requestBody.input);
  const compiledContext = compileContext({ requestBody, previousRecord, normalizedInput, config });
  const compact = buildCodeseexCompactionWindow({ requestBody, previousRecord, normalizedInput, config });
  const output = compact.output;
  const response = buildResponseRecord({
    id,
    createdAt,
    model: requestBody.model || config.upstreamModelOverride || config.availableModels[0] || "deepseek-v4-pro",
    output,
    usage: {
      input_tokens: estimateTokensForInput(requestBody.input),
      output_tokens: Math.max(1, Math.ceil(compact.summary.length / 4)),
      total_tokens: estimateTokensForInput(requestBody.input) + Math.max(1, Math.ceil(compact.summary.length / 4)),
    },
  });

  const record = buildStoredRecord({
    id,
    createdAt,
    startedAt: new Date(createdAt * 1000).toISOString(),
    status: "completed",
    response,
    requestBody,
    previousRecord,
    normalizedInput,
    currentMessages: compiledContext.currentMessages,
    storedMessages: [],
    rawAssistant: { role: "assistant", content: "" },
    turnMessages: buildTurnStorageMessages(compiledContext, [], config, { requestInstructions: requestBody.instructions }),
    toolFacts: mergeTurnToolFactsForStorage(normalizedInput, response, []),
    compactions: compiledContext.compactions,
    contextDiagnostic: compiledContext.diagnostic,
  });
  persistRecord(record, context);
  emitContextCompactedEvent(runtime, {
    mode: "manual",
    responseId: id,
    compact,
    outputItemCount: output.length,
    retainedInputItems: compact.retainedItems.length,
  });
  pushEvent(runtime, {
    type: "responses_compacted",
    level: "info",
    message: "Response context compacted.",
    audience: "diagnostic",
    detail: {
      response_id: id,
      compaction_id: output[0] && output[0].id,
      message_count: compact.payload.message_count,
      retained_message_count: compact.payload.retained_message_count,
      tool_fact_count: compact.payload.tool_fact_count,
      returned_window_items: output.length,
      retained_input_items: compact.retainedItems.length,
    },
  });
  writeRuntime(config, runtime);
  writeDebug(config, "latest-compact-response.json", {
    at: new Date().toISOString(),
    request: requestBody,
    response,
    compact: {
      id: compact.payload.id,
      message_count: compact.payload.message_count,
      retained_message_count: compact.payload.retained_message_count,
      tool_fact_count: compact.payload.tool_fact_count,
      returned_window_items: output.length,
      retained_input_items: compact.retainedItems.length,
    },
    context_compiler: compiledContext.diagnostic,
  });
  sendJson(res, 200, response);
}

async function handleResponsesCreate(req, res, requestBody, context) {
  const started = Date.now();
  const { config, runtime } = context;
  const modelAlias = applyCodexModelAlias(requestBody, config, runtime);
  requestBody = modelAlias.requestBody;

  const id = makeId("resp");
  const createdAt = nowSeconds();
  const startedAt = new Date(started).toISOString();
  let normalizedInput = normalizeInput(requestBody.input);
  const checkpointCompaction = detectCheckpointCompactionRequest(normalizedInput);
  const requestKind = checkpointCompaction ? "context_compaction" : "conversation";
  beginRequest(runtime, { id, model: requestBody.model, requestedModel: modelAlias.requestedModel, stream: Boolean(requestBody.stream), startedAt, kind: requestKind });
  writeRuntime(config, runtime);
  let requestFinished = false;
  let previousRecord = null;
  let compiledContext = null;
  let currentMessages = [];
  let conversationMessages = [];
  let contextDiagnostic = null;
  let lastCheckpoint = {
    storedMessages: [],
    rawAssistant: { role: "assistant", content: "" },
    usage: null,
  };

  const checkpoint = (status, updates = {}) => {
    if (Array.isArray(updates.storedMessages)) lastCheckpoint.storedMessages = updates.storedMessages.slice();
    if (updates.rawAssistant) lastCheckpoint.rawAssistant = updates.rawAssistant;
    if (updates.usage !== undefined) lastCheckpoint.usage = updates.usage;
    const record = persistResponseCheckpoint(context, {
      id,
      createdAt,
      startedAt,
      status,
      requestBody,
      previousRecord,
      normalizedInput,
      compiledContext,
      currentMessages,
      conversationMessages,
      storedMessages: lastCheckpoint.storedMessages,
      rawAssistant: updates.rawAssistant || lastCheckpoint.rawAssistant,
      response: updates.response,
      usage: updates.usage !== undefined ? updates.usage : lastCheckpoint.usage,
      error: updates.error,
      contextDiagnostic,
      reason: updates.reason || "",
    });
    return record;
  };

  try {
    previousRecord = resolvePreviousContext(requestBody.previous_response_id, context);
    logIncomingToolItems(runtime, normalizedInput);
    const workspaceScope = resolveWorkspaceScope(requestBody, normalizedInput, config);
    compiledContext = compileContext({ requestBody, previousRecord, normalizedInput, config });
    currentMessages = compiledContext.currentMessages;
    conversationMessages = compiledContext.messages;
    const toolContext = createToolContext(requestBody.tools || [], {
      rootDir: workspaceScope.rootDir,
      extensionDir: config.extensionDir,
      communityToolCodeEnabled: config.communityToolCodeEnabled,
      toolConfig: config,
    });
    contextDiagnostic = captureContextDiagnostic(context, {
      requestBody,
      previousRecord,
      normalizedInput,
      currentMessages,
      conversationMessages,
      workspaceScope,
      compiledContext,
    });
    const autoCompaction = maybeBuildAutomaticCompaction({
      requestBody,
      previousRecord,
      normalizedInput,
      compiledContext,
      conversationMessages,
      config,
    });
    checkpoint("in_progress", { reason: "request_compiled" });

    writeDebug(config, "latest-request.json", {
      at: new Date().toISOString(),
      mode: requestBody.stream ? "stream" : "sync",
      request_model: requestBody.model,
      requested_model: modelAlias.requestedModel,
      previous_response_id: requestBody.previous_response_id || null,
      normalized_input: normalizedInput,
      upstream_messages: conversationMessages,
      context_compiler: compiledContext.diagnostic,
      tool_facts: compiledContext.toolFacts,
      compactions: compiledContext.compactions,
      context_conflicts: compiledContext.conflicts,
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
        onCheckpoint: (payload) => checkpoint("in_progress", payload),
        autoCompactionItem: autoCompaction && autoCompaction.item,
      });
      if (autoCompaction && streamResult && !streamResult.failed) {
        emitContextCompactedEvent(runtime, {
          mode: "automatic",
          responseId: id,
          compact: autoCompaction.compact,
          threshold: resolveCompactThreshold(requestBody.context_management),
          estimatedTokens: compiledContext && compiledContext.diagnostic && compiledContext.diagnostic.estimated_tokens,
        });
      }
      finishRequest(runtime, {
        id,
        model: requestBody.model,
        requestedModel: modelAlias.requestedModel,
        stream: true,
        kind: requestKind,
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
        checkpoint(streamResult.failed ? "failed" : "completed", {
          response: streamResult.response,
          storedMessages: streamResult.storedMessages || [],
          rawAssistant: streamResult.rawAssistant,
          usage: streamResult.usage,
          error: streamResult.response && streamResult.response.error,
          reason: streamResult.failed ? "stream_failed" : "stream_completed",
        });

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
      onCheckpoint: (payload) => checkpoint("in_progress", payload),
    });

    finishRequest(runtime, {
      id,
      model: requestBody.model,
      requestedModel: modelAlias.requestedModel,
      stream: false,
      kind: requestKind,
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
      output: result.output.concat(autoCompaction ? [autoCompaction.item] : []),
      usage: result.usage,
    });
    if (autoCompaction) {
      emitContextCompactedEvent(runtime, {
        mode: "automatic",
        responseId: id,
        compact: autoCompaction.compact,
        threshold: resolveCompactThreshold(requestBody.context_management),
        estimatedTokens: compiledContext && compiledContext.diagnostic && compiledContext.diagnostic.estimated_tokens,
      });
    }

    checkpoint("completed", {
      response,
      storedMessages: result.storedMessages,
      rawAssistant: result.rawAssistant,
      usage: result.usage,
      reason: "sync_completed",
    });

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
        kind: requestKind,
        status: "failed",
        requestMs: Date.now() - started,
        startedAt,
        config,
        error: error && error.message ? error.message : String(error),
      });
      writeRuntime(config, runtime);
    }
    if (compiledContext) {
      try {
        checkpoint("failed", {
          response: buildResponseRecord({
            id,
            createdAt,
            model: requestBody.model,
            output: [],
            usage: lastCheckpoint.usage,
            status: "failed",
            error: responseErrorFromException(error),
          }),
          storedMessages: lastCheckpoint.storedMessages,
          rawAssistant: lastCheckpoint.rawAssistant,
          usage: lastCheckpoint.usage,
          error,
          reason: "request_failed",
        });
      } catch {}
    }
    throw error;
  }
}

function persistResponseCheckpoint(context, details = {}) {
  const status = normalizeLifecycleStatus(details.status);
  const requestBody = details.requestBody || {};
  const config = context.config || {};
  const compiledContext = details.compiledContext || null;
  const storedMessages = Array.isArray(details.storedMessages) ? details.storedMessages : [];
  const response = details.response || buildResponseRecord({
    id: details.id,
    createdAt: details.createdAt,
    model: requestBody.model || config.upstreamModelOverride || "deepseek-v4-pro",
    output: [],
    usage: details.usage || null,
    status,
    error: status === "failed" || status === "interrupted" ? responseErrorFromException(details.error) : null,
  });
  const turnMessages = compiledContext
    ? buildTurnStorageMessages(compiledContext, storedMessages, config, { requestInstructions: requestBody.instructions })
    : undefined;
  const record = buildStoredRecord({
    id: details.id,
    createdAt: details.createdAt,
    startedAt: details.startedAt,
    status,
    response,
    requestBody,
    previousRecord: details.previousRecord,
    normalizedInput: details.normalizedInput || [],
    currentMessages: details.currentMessages || (compiledContext && compiledContext.currentMessages) || [],
    storedMessages,
    rawAssistant: assistantForLifecycle(status, details.rawAssistant),
    conversationMessages: details.conversationMessages || (compiledContext && compiledContext.messages) || [],
    turnMessages,
    toolFacts: mergeTurnToolFactsForStorage(details.normalizedInput || [], response, storedMessages),
    compactions: compiledContext && Array.isArray(compiledContext.compactions) ? compiledContext.compactions : [],
    contextDiagnostic: checkpointDiagnostic(details, status, response, storedMessages),
  });
  persistRecord(record, context);
  return record;
}

function normalizeLifecycleStatus(status) {
  const value = String(status || "completed").trim().toLowerCase();
  if (value === "in_progress" || value === "completed" || value === "failed" || value === "interrupted") return value;
  return "completed";
}

function assistantForLifecycle(status, rawAssistant) {
  if (status === "completed") return rawAssistant;
  const assistant = normalizeAssistant(rawAssistant);
  if (Array.isArray(assistant.tool_calls) && assistant.tool_calls.length > 0) return assistant;
  return { role: "assistant", content: "" };
}

function checkpointDiagnostic(details, status, response, storedMessages) {
  const compiledDiagnostic = details.compiledContext && details.compiledContext.diagnostic
    ? details.compiledContext.diagnostic
    : null;
  const diagnostic = compiledDiagnostic && typeof compiledDiagnostic === "object"
    ? Object.assign({}, compiledDiagnostic)
    : {};
  diagnostic.lifecycle = {
    status,
    checkpoint_reason: details.reason || "",
    checkpoint_at: new Date().toISOString(),
    stored_message_count: Array.isArray(storedMessages) ? storedMessages.length : 0,
    response_status: response && response.status || status,
  };
  if (details.contextDiagnostic && details.contextDiagnostic.id) diagnostic.lifecycle.context_diagnostic_id = details.contextDiagnostic.id;
  if (details.error) diagnostic.lifecycle.error = responseErrorFromException(details.error);
  return diagnostic;
}

function responseErrorFromException(error) {
  if (!error) return null;
  if (error && typeof error === "object" && error.message && error.type && error.code) {
    return {
      message: String(error.message || "Request failed."),
      type: String(error.type || "api_error"),
      code: String(error.code || "request_failed"),
    };
  }
  if (error && typeof error === "object" && error.message) {
    return {
      message: String(error.message),
      type: String(error.type || "api_error"),
      code: String(error.code || "request_failed"),
    };
  }
  if (error && typeof error === "object" && error.error) return responseErrorFromException(error.error);
  return {
    message: String(error || "Request failed."),
    type: "api_error",
    code: "request_failed",
  };
}

function applyCodexModelAlias(requestBody, config, runtime) {
  const requestedModel = String(requestBody && requestBody.model || "");
  const overrideModel = resolveUpstreamModelOverride(config);
  const mappedModel = overrideModel || resolveCodexModelAlias(requestedModel, config);
  if (!mappedModel || mappedModel === requestedModel) {
    return { requestBody, requestedModel, model: requestedModel, aliased: false };
  }
  pushEvent(runtime, {
    type: "model_alias_applied",
    level: "info",
    message: overrideModel ? "CodeSeeX upstream model override applied." : "Codex official model request mapped to CodeSeeX model.",
    audience: "user",
    detail: {
      requested_model: requestedModel,
      model: mappedModel,
      source: overrideModel ? "override" : "alias",
    },
  });
  return {
    requestBody: Object.assign({}, requestBody, { model: mappedModel }),
    requestedModel,
    model: mappedModel,
    aliased: true,
  };
}

function resolveUpstreamModelOverride(config = {}) {
  const value = String(config.upstreamModelOverride || config.UPSTREAM_MODEL_OVERRIDE || "default").trim().toLowerCase();
  if (value === "flash" || value === "deepseek-v4-flash") return "deepseek-v4-flash";
  if (value === "pro" || value === "deepseek-v4-pro") return "deepseek-v4-pro";
  return "";
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

async function runDeepSeekTurn({ requestBody, messages, toolContext, config, runtime = null, authorization, callJson = callDeepSeekJson, onCheckpoint = null }) {
  const workingMessages = messages.slice();
  let usage = null;
  const storedMessages = [];
  const outputItems = [];
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
      outputItems.push(...turnOutputFromAssistant(visibleAssistant, usage, toolContext, config, { phase: "commentary" }));
      storedMessages.push(assistantForStorage(visibleAssistant));
      storedMessages.push(...hostedResult.toolMessages);
      await maybeCheckpoint(onCheckpoint, {
        storedMessages,
        rawAssistant: visibleAssistant,
        usage,
        reason: "hosted_tool_result",
      });
      if (external.length > 0 || internal.length > 0) {
        return {
          rawAssistant: visibleAssistant,
          output: outputItems,
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
      await maybeCheckpoint(onCheckpoint, {
        storedMessages,
        rawAssistant: visibleAssistant,
        usage,
        reason: "internal_tool_call",
      });
      return {
        rawAssistant: visibleAssistant,
        output: turnOutputFromAssistant(visibleAssistant, usage, toolContext, config, { phase: "commentary", includeThinkingDisplay: false }),
        usage,
        storedMessages,
      };
    }

    const visibleAssistant = toVisibleAssistant(currentAssistant, toolContext);
    storedMessages.push(assistantForStorage(currentAssistant));
    return {
      rawAssistant,
      output: outputItems.concat(turnOutputFromAssistant(visibleAssistant, usage, toolContext, config, { phase: "final_answer", includeThinkingDisplay: false })),
      usage,
      storedMessages,
    };
  }

}

async function maybeCheckpoint(onCheckpoint, payload) {
  if (typeof onCheckpoint !== "function") return;
  await onCheckpoint(Object.assign({}, payload, {
    storedMessages: Array.isArray(payload && payload.storedMessages) ? payload.storedMessages.slice() : [],
  }));
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

function emitContextCompactedEvent(runtime, details = {}) {
  const compact = details.compact || {};
  const payload = compact.payload || {};
  pushEvent(runtime, {
    type: "context_compacted",
    level: "info",
    message: contextCompactedMessage(details.mode),
    audience: "user",
    detail: compactEventDetail({
      mode: details.mode || "manual",
      response_id: details.responseId || "",
      compaction_id: payload.id || compact.id || "",
      message_count: details.messageCount !== undefined ? details.messageCount : payload.message_count,
      retained_message_count: details.retainedMessageCount !== undefined ? details.retainedMessageCount : payload.retained_message_count,
      tool_fact_count: details.toolFactCount !== undefined ? details.toolFactCount : payload.tool_fact_count,
      returned_window_items: details.outputItemCount,
      retained_input_items: details.retainedInputItems,
      input_item_count: details.inputItemCount,
      threshold_tokens: details.threshold,
      estimated_tokens: details.estimatedTokens,
    }),
  });
}

function contextCompactedMessage(mode) {
  if (mode === "automatic") return "Context compacted automatically.";
  if (mode === "checkpoint") return "Context checkpoint compaction requested.";
  return "Context compacted.";
}

function compactEventDetail(detail) {
  const output = {};
  for (const [key, value] of Object.entries(detail || {})) {
    if (value === undefined || value === null || value === "") continue;
    if (typeof value === "number" && !Number.isFinite(value)) continue;
    output[key] = value;
  }
  return output;
}

function detectCheckpointCompactionRequest(items) {
  const lastUser = lastUserMessageText(items);
  if (!lastUser) return false;
  return /\bCONTEXT\s+CHECKPOINT\s+COMPACTION\b/i.test(lastUser)
    && /\bhandoff\s+summary\b/i.test(lastUser)
    && /\banother\s+LLM\b/i.test(lastUser);
}

function lastUserMessageText(items) {
  for (let index = (Array.isArray(items) ? items.length : 0) - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (!item || typeof item !== "object") continue;
    if (item.type !== "message" && !item.role) continue;
    if (String(item.role || "user").toLowerCase() !== "user") continue;
    return messageItemText(item);
  }
  return "";
}

function messageItemText(item) {
  const content = item && item.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return String(content || "");
  return content.map((part) => {
    if (typeof part === "string") return part;
    if (!part || typeof part !== "object") return "";
    if (typeof part.text === "string") return part.text;
    if (typeof part.input_text === "string") return part.input_text;
    if (typeof part.output_text === "string") return part.output_text;
    return "";
  }).join("\n");
}

function buildContextDiagnosticSummary({ requestBody, previousRecord, normalizedInput, currentMessages, conversationMessages, compiledContext }) {
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
    previous_chain: previousRecord && previousRecord.chain_diagnostic ? previousRecord.chain_diagnostic : null,
    estimated_previous_upstream_tokens: estimateTokensFromBytes(previousBytes),
    context_compiler: compiledContext && compiledContext.diagnostic ? compiledContext.diagnostic : null,
    tool_fact_count: compiledContext && Array.isArray(compiledContext.toolFacts) ? compiledContext.toolFacts.length : 0,
    compaction_summary_count: compiledContext && Array.isArray(compiledContext.compactions) ? compiledContext.compactions.length : 0,
    context_conflict_count: compiledContext && Array.isArray(compiledContext.conflicts) ? compiledContext.conflicts.length : 0,
    context_budget: compiledContext && compiledContext.budget ? {
      mode: compiledContext.budget.mode,
      context_window: compiledContext.budget.contextWindow,
      effective_context_window_percent: compiledContext.budget.effectivePercent,
      max_tokens: compiledContext.budget.maxTokens,
      max_bytes: compiledContext.budget.maxBytes,
      max_tool_output_bytes: compiledContext.budget.maxToolOutputBytes,
    } : null,
  };
}

function maybeBuildAutomaticCompaction({ requestBody, previousRecord, normalizedInput, compiledContext, conversationMessages, config }) {
  const contextManagement = requestBody && requestBody.context_management;
  const threshold = resolveCompactThreshold(contextManagement);
  if (!Number.isFinite(threshold) || threshold <= 0) return null;
  const estimatedTokens = compiledContext && compiledContext.diagnostic
    ? Number(compiledContext.diagnostic.estimated_tokens || 0)
    : estimateTokensFromBytes(safeJsonByteLength(conversationMessages));
  if (estimatedTokens < threshold) return null;
  return buildCodeseexCompactionItem({
    requestBody,
    previousRecord,
    normalizedInput,
    compiledContext,
    config,
  });
}

function resolveCompactThreshold(value) {
  if (value === undefined || value === null || value === false) return null;
  if (typeof value === "number") return value;
  if (Array.isArray(value)) {
    for (const item of value) {
      const threshold = resolveCompactThreshold(item);
      if (Number.isFinite(threshold) && threshold > 0) return threshold;
    }
    return null;
  }
  if (typeof value !== "object") return null;
  const candidates = [
    value.compact_threshold,
    value.threshold,
    value.token_threshold,
    value.max_tokens,
  ];
  if (value.compaction && typeof value.compaction === "object") {
    candidates.push(value.compaction.compact_threshold, value.compaction.threshold);
  }
  for (const candidate of candidates) {
    const parsed = Number(candidate);
    if (Number.isFinite(parsed) && parsed > 0) return parsed;
  }
  return null;
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
      content: sanitizeToolContent(toolOutputValueToText(await proxyHostedToolContent(item, config, messages, toolContext))),
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
    || item.type === "web_search_call";
}

function isIncomingToolOutputItem(item) {
  return item.type === "function_call_output"
    || item.type === "custom_tool_call_output"
    || item.type === "web_search_call_output";
}

function incomingToolName(item) {
  if (item.name) return String(item.name);
  if (item.type === "web_search_call" || item.type === "web_search_call_output") return "web_search";
  if (item.type === "custom_tool_call" || item.type === "custom_tool_call_output") return "custom_tool";
  return "tool";
}

function flushRuntime(config, runtime) {
  if (!runtime) return;
  writeRuntime(config, runtime);
}

async function proxyHostedToolContent(item, config, messages = [], toolContext = null) {
  const registered = await executeRegisteredHostedTool(item, config, messages, toolContext);
  if (registered.handled) return registered.result;

  if (isHostedExecutionItem(item, "workspace_search")) {
    return executeWorkspaceSearch(item.arguments, config);
  }
  if (isHostedExecutionItem(item, "read_file_range")) {
    return executeReadFileRange(item.arguments, config);
  }
  if (isHostedExecutionItem(item, "list_directory")) {
    return executeListDirectory(item.arguments, config);
  }
  if (!isWebSearchExecutionItem(item)) {
    return {
      ok: false,
      error: "proxy_hosted_tool_not_implemented",
      message: "The proxy-hosted tool is registered but does not provide an executeProxyTool() handler.",
      tool: item && item.name || "",
      type: item && item.type || "",
    };
  }
  return proxyWebSearchToolContent(item, config, messages);
}

async function executeRegisteredHostedTool(item, config, messages, toolContext) {
  const name = item && item.name ? String(item.name) : "";
  const entry = name && toolContext && toolContext.byName ? toolContext.byName.get(name) : null;
  const nativeTool = entry && entry.nativeTool && typeof entry.nativeTool === "object" ? entry.nativeTool : null;
  if (!nativeTool || typeof nativeTool.executeProxyTool !== "function") return { handled: false, result: null };
  try {
    return {
      handled: true,
      result: await nativeTool.executeProxyTool({
        item,
        arguments: item && item.arguments,
        config,
        messages,
        toolContext,
      }),
    };
  } catch (error) {
    return {
      handled: true,
      result: {
        ok: false,
        error: "proxy_hosted_tool_failed",
        message: error && error.message ? error.message : String(error),
        tool: name,
      },
    };
  }
}

function isHostedExecutionItem(item, name) {
  return Boolean(item && (
    item.type === "function_call"
    || item.type === "proxy_tool_call"
  ) && item.name === name);
}

function isWebSearchExecutionItem(item) {
  if (!item || typeof item !== "object") return false;
  return item.type === "web_search_call"
    || item.name === "web_search"
    || item.name === "web_search_preview";
}

async function proxyWebSearchToolContent(item, config, messages = []) {
  const action = item && item.action ? item.action : {};
  const { executeProxyWebSearch } = require("./web-search-executor");
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

function persistRecord(record, context) {
  context.state.responses[record.id] = record;
  trimState(context);
  try {
    saveState(context);
  } catch (error) {
    reportStatePersistFailure(context, error, record);
    throw error;
  }
}

function trimState(context) {
  const responses = context.state.responses && typeof context.state.responses === "object" ? context.state.responses : {};
  const responseCount = Object.keys(responses).length;
  const softLimit = Number(context.config.maxStoredResponses) || 0;
  context.state.retention = {
    policy: "preserve_all_response_chains",
    soft_response_limit: softLimit,
    response_count: responseCount,
    approx_json_bytes: safeJsonByteLength(context.state),
    soft_limit_exceeded: Boolean(softLimit > 0 && responseCount > softLimit),
    updated_at: new Date().toISOString(),
  };
}

function saveState(context) {
  writeJsonCompact(context.config.stateFile, context.state);
}

function reportStatePersistFailure(context, error, record) {
  const { config, runtime } = context;
  pushEvent(runtime, {
    type: "state_persist_failed",
    level: "error",
    message: "Proxy state checkpoint could not be saved.",
    audience: "diagnostic",
    detail: {
      response_id: record && record.id || "",
      status: record && record.status || "",
      state_file: config.stateFile,
      error: error && error.message ? error.message : String(error),
      code: error && error.code || null,
    },
  });
  writeRuntime(config, runtime);
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
    if (typeof current === "string") return truncateDebugString(sanitizeSensitiveDebugString(current));
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

function sanitizeSensitiveDebugString(value) {
  return String(value || "")
    .replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer ********")
    .replace(/sk-[A-Za-z0-9]{12,}/g, "sk-********")
    .replace(/(["']?(?:api[_-]?key|authorization|token|secret|password)["']?\s*[:=]\s*["']?)[^"',\s}]+/gi, "$1********");
}

function isSensitiveDebugKey(key) {
  return /(?:authorization|api[_-]?key|token|secret|password|http_proxy|https_proxy|all_proxy|proxy)$/i.test(String(key || ""));
}

function matchRoute(method, pathname) {
  const routes = [
    { method: "GET", regex: /^\/(?:v1\/)?healthz$/, name: "health" },
    { method: "GET", regex: /^\/(?:v1\/)?models$/, name: "models" },
    { method: "POST", regex: /^\/(?:v1\/)?responses$/, name: "responses_create" },
    { method: "POST", regex: /^\/(?:v1\/)?responses\/compact$/, name: "responses_compact" },
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
  maybeBuildAutomaticCompaction,
  resolveCompactThreshold,
  resolvePreviousContext,
  runDeepSeekTurn,
  sanitizeDebugValue,
};
