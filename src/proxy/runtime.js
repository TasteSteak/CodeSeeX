const { appendJsonl, writeJson } = require("../shared/json-store");
const { appendEventLog } = require("../shared/event-log");

function createRuntime(config) {
  return {
    status: "starting",
    pid: process.pid,
    host: config.host,
    requested_port: config.port,
    deepseek_base_url: config.deepseekBaseUrl,
    base_url: null,
    port: null,
    started_at: new Date().toISOString(),
    stopped_at: null,
    active_requests: 0,
    request_count: 0,
    failed_request_count: 0,
    last_started_at: null,
    last_request_at: null,
    last_request_ms: null,
    total_input_tokens: 0,
    total_cached_input_tokens: 0,
    total_cache_miss_input_tokens: 0,
    total_output_tokens: 0,
    total_reasoning_output_tokens: 0,
    total_tokens: 0,
    last_input_tokens: 0,
    last_cached_input_tokens: 0,
    last_cache_miss_input_tokens: 0,
    last_output_tokens: 0,
    last_reasoning_output_tokens: 0,
    last_total_tokens: 0,
    last_turn: null,
    turn_history: [],
    metrics_samples: [],
    events: [],
    event_log_file: config.eventLogFile,
    root_dir: config.rootDir,
    data_dir: config.dataDir,
    log_retention_days: config.logRetentionDays || 7,
    parent_pid: config.parentPid,
    error: null,
  };
}

function writeRuntime(config, runtime) {
  try {
    writeJson(config.runtimeFile, runtime);
  } catch {}
}

function beginRequest(runtime, meta = {}) {
  runtime.active_requests = Math.max(0, Number(runtime.active_requests || 0)) + 1;
  runtime.last_started_at = meta.startedAt || new Date().toISOString();
  const detail = {
    model: meta.model || "",
    stream: Boolean(meta.stream),
  };
  if (meta.requestedModel && meta.requestedModel !== meta.model) detail.requested_model = meta.requestedModel;
  pushEvent(runtime, {
    type: "request_started",
    level: "info",
    message: "Conversation request received.",
    audience: "user",
    detail,
  });
}

function finishRequest(runtime, meta = {}) {
  runtime.active_requests = Math.max(0, Number(runtime.active_requests || 0) - 1);
  runtime.last_request_at = meta.completedAt || new Date().toISOString();
  runtime.last_request_ms = Number(meta.requestMs || 0);

  if (meta.status === "failed") {
    runtime.failed_request_count = Number(runtime.failed_request_count || 0) + 1;
    const detail = {
      model: meta.model || "",
      stream: Boolean(meta.stream),
      duration_ms: runtime.last_request_ms,
      error: meta.error || "",
    };
    if (meta.requestedModel && meta.requestedModel !== meta.model) detail.requested_model = meta.requestedModel;
    pushEvent(runtime, {
      type: "request_failed",
      level: "error",
      message: "Conversation request failed.",
      audience: "user",
      detail,
    });
    return;
  }

  runtime.request_count = Number(runtime.request_count || 0) + 1;
  const turn = applyUsage(runtime, meta.usage);
  turn.id = meta.id || "";
  turn.model = meta.model || "";
  if (meta.requestedModel && meta.requestedModel !== meta.model) turn.requested_model = meta.requestedModel;
  turn.stream = Boolean(meta.stream);
  turn.status = "completed";
  turn.started_at = meta.startedAt || runtime.last_started_at || null;
  turn.completed_at = runtime.last_request_at;
  turn.request_ms = runtime.last_request_ms;
  runtime.last_turn = turn;
  pushTurn(runtime, turn);
  pushEvent(runtime, {
    type: "request_completed",
    level: "success",
    message: "Conversation completed.",
    audience: "user",
    detail: requestCompletedDetail(turn, meta),
  });
}

function requestCompletedDetail(turn, meta = {}) {
  const detail = { duration_ms: turn.request_ms };
  const cost = estimateCostCny(turn, meta.config || {});
  if (Number.isFinite(cost)) detail.cost_cny = cost;
  return detail;
}

function estimateCostCny(turn, config = {}) {
  const cached = Number(turn.cached_input_tokens || 0);
  const cacheMiss = Number(turn.cache_miss_input_tokens || 0);
  const output = Number(turn.output_tokens || 0);
  const cachedRate = numberOrDefault(config.billingCachedInputCny, 0.025);
  const missRate = numberOrDefault(config.billingCacheMissInputCny, 3);
  const outputRate = numberOrDefault(config.billingOutputCny, 6);
  return (cached * cachedRate + cacheMiss * missRate + output * outputRate) / 1000000;
}

function numberOrDefault(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function applyUsage(runtime, usage) {
  const input = usage ? usage.input_tokens || 0 : 0;
  const cached = usage ? usage.cached_input_tokens || ((usage.input_tokens_details || {}).cached_tokens || 0) : 0;
  const cacheMiss = usage ? usage.cache_miss_input_tokens || Math.max(0, input - cached) : 0;
  const output = usage ? usage.output_tokens || 0 : 0;
  const reasoning = usage ? usage.reasoning_output_tokens || ((usage.output_tokens_details || {}).reasoning_tokens || 0) : 0;
  const total = usage ? usage.total_tokens || input + output : 0;
  runtime.last_input_tokens = input;
  runtime.last_cached_input_tokens = cached;
  runtime.last_cache_miss_input_tokens = cacheMiss;
  runtime.last_output_tokens = output;
  runtime.last_reasoning_output_tokens = reasoning;
  runtime.last_total_tokens = total;
  runtime.total_input_tokens += input;
  runtime.total_cached_input_tokens = Number(runtime.total_cached_input_tokens || 0) + cached;
  runtime.total_cache_miss_input_tokens = Number(runtime.total_cache_miss_input_tokens || 0) + cacheMiss;
  runtime.total_output_tokens += output;
  runtime.total_reasoning_output_tokens = Number(runtime.total_reasoning_output_tokens || 0) + reasoning;
  runtime.total_tokens += total;
  const sample = {
    active: runtime.active_requests || 0,
    inputTokens: input,
    cachedInputTokens: cached,
    cacheMissInputTokens: cacheMiss,
    outputTokens: output,
    reasoningOutputTokens: reasoning,
    totalTokens: total,
    requestMs: runtime.last_request_ms || 0,
  };
  pushSample(runtime, sample);
  return {
    input_tokens: input,
    cached_input_tokens: cached,
    cache_miss_input_tokens: cacheMiss,
    output_tokens: output,
    reasoning_output_tokens: reasoning,
    total_tokens: total,
  };
}

function pushSample(runtime, sample) {
  runtime.metrics_samples.push({
    ts: Date.now(),
    active: sample.active || 0,
    inputTokens: sample.inputTokens || 0,
    cachedInputTokens: sample.cachedInputTokens || 0,
    cacheMissInputTokens: sample.cacheMissInputTokens || 0,
    outputTokens: sample.outputTokens || 0,
    reasoningOutputTokens: sample.reasoningOutputTokens || 0,
    totalTokens: sample.totalTokens || 0,
    requestMs: sample.requestMs || 0,
  });
  if (runtime.metrics_samples.length > 180) runtime.metrics_samples.splice(0, runtime.metrics_samples.length - 180);
}

function pushTurn(runtime, turn) {
  if (!Array.isArray(runtime.turn_history)) runtime.turn_history = [];
  runtime.turn_history.push(Object.assign({}, turn));
  if (runtime.turn_history.length > 80) runtime.turn_history.splice(0, runtime.turn_history.length - 80);
}

function pushEvent(runtime, event) {
  if (!runtime) return;
  if (!Array.isArray(runtime.events)) runtime.events = [];
  const item = {
    ts: new Date().toISOString(),
    type: event.type || "event",
    level: event.level || "info",
    message: event.message || "",
    audience: normalizeEventAudience(event),
    detail: event.detail || null,
  };
  runtime.events.push(item);
  if (runtime.events.length > 240) runtime.events.splice(0, runtime.events.length - 240);
  appendRuntimeEvent(runtime, item);
}

function normalizeEventAudience(event = {}) {
  if (event.audience === "diagnostic") return "diagnostic";
  if (event.audience === "user") return "user";
  return isDiagnosticEventType(event.type) ? "diagnostic" : "user";
}

function isDiagnosticEventType(type) {
  return type === "context_diagnostic"
    || type === "context_response_diagnostic"
    || type === "tool_lifecycle";
}

function appendRuntimeEvent(runtime, event) {
  if (!runtime.event_log_file) return;
  try {
    if (runtime.root_dir && runtime.data_dir) {
      appendEventLog(runtime.root_dir, runtime.data_dir, event, { retentionDays: runtime.log_retention_days });
      return;
    }
    appendJsonl(runtime.event_log_file, event, { retentionDays: runtime.log_retention_days });
  } catch {}
}

module.exports = {
  applyUsage,
  beginRequest,
  createRuntime,
  finishRequest,
  pushSample,
  pushEvent,
  writeRuntime,
};
