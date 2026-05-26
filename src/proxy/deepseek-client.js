const { readCodexAuthApiKey, rememberAuthorizationHeader } = require("../shared/codex-auth");
const { temperatureForPreset } = require("../shared/config");
const { httpError, makeId, parseJsonResponse } = require("../shared/http");
const { resolveDispatcher } = require("./network-dispatcher");

function buildDeepSeekPayload(requestBody, messages, toolContext, config, overrides = {}) {
  const payload = {
    model: requestBody.model,
    messages,
    stream: Boolean(overrides.stream !== undefined ? overrides.stream : requestBody.stream),
  };

  if (payload.stream) payload.stream_options = { include_usage: true };
  const configuredTemperature = temperatureForPreset(config && config.temperaturePreset);
  if (typeof configuredTemperature === "number") payload.temperature = configuredTemperature;
  else if (typeof requestBody.temperature === "number") payload.temperature = requestBody.temperature;
  if (typeof requestBody.top_p === "number") payload.top_p = requestBody.top_p;
  if (typeof requestBody.max_output_tokens === "number") payload.max_tokens = requestBody.max_output_tokens;
  if (typeof requestBody.max_completion_tokens === "number") payload.max_tokens = requestBody.max_completion_tokens;
  if (!overrides.omitTools && toolContext.upstreamTools.length > 0) payload.tools = toolContext.upstreamTools;

  const toolChoice = overrides.omitTools ? undefined : (overrides.toolChoice !== undefined ? overrides.toolChoice : toolContext.normalizeToolChoice(requestBody.tool_choice));
  if (toolChoice !== undefined) payload.tool_choice = toolChoice;

  const format = mapResponseFormat(requestBody.text);
  if (format) payload.response_format = format;

  const thinking = mapThinking(requestBody.reasoning, config);
  if (thinking) payload.thinking = thinking;

  return stripUndefined(payload);
}

async function callDeepSeekJson(payload, config, authorization) {
  const response = await fetchDeepSeek(Object.assign({}, payload, { stream: false }), config, authorization);
  return parseDeepSeekJson(response);
}

async function callDeepSeekStream(payload, config, authorization, onChunk) {
  const body = await fetchDeepSeekStream(payload, config, authorization);
  if (!body) {
    const completion = await callDeepSeekJson(payload, config, authorization);
    if (onChunk) await onChunk({ type: "content", delta: getAssistantMessage(completion).content || "" });
    return completion;
  }

  const reader = body.getReader();
  const decoder = new TextDecoder("utf-8");
  let buffer = "";
  let accumulatedReasoning = "";
  let accumulatedContent = "";
  let usage = null;
  const toolCallStates = new Map();
  let lastToolIndex = 0;
  let reasoningClosed = false;

  function toolStateForIndex(index) {
    const normalizedIndex = normalizeToolCallIndex(index);
    if (!toolCallStates.has(normalizedIndex)) {
      toolCallStates.set(normalizedIndex, { id: "", name: "", arguments: "" });
    }
    return { index: normalizedIndex, state: toolCallStates.get(normalizedIndex) };
  }

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });

      const frames = [];
      let idx = buffer.indexOf("\n\n");
      while (idx !== -1) {
        frames.push(buffer.slice(0, idx));
        buffer = buffer.slice(idx + 2);
        idx = buffer.indexOf("\n\n");
      }

      for (const frame of frames) {
        const parsed = parseDeepSeekFrame(frame);
        if (!parsed || parsed === "[DONE]") continue;

        if (parsed.usage) usage = parsed.usage;

        const delta = parsed.choices && parsed.choices[0] ? parsed.choices[0].delta : null;
        if (!delta) continue;

        if (delta.reasoning_content && !reasoningClosed) {
          accumulatedReasoning += delta.reasoning_content;
          if (onChunk) {
            await onChunk({ type: "reasoning", delta: delta.reasoning_content, accumulated: accumulatedReasoning });
          }
        }

        if (delta.content) {
          if (accumulatedReasoning && !accumulatedContent && !reasoningClosed && onChunk) {
            reasoningClosed = true;
            await onChunk({ type: "reasoning_end" });
          }
          reasoningClosed = true;
          accumulatedContent += delta.content;
          if (onChunk) {
            await onChunk({ type: "content", delta: delta.content, accumulated: accumulatedContent });
          }
        }

        if (delta.tool_calls) {
          if (accumulatedReasoning && !accumulatedContent && !reasoningClosed && onChunk) {
            reasoningClosed = true;
            await onChunk({ type: "reasoning_end" });
          }
          reasoningClosed = true;
          for (const tc of delta.tool_calls) {
            const nextToolIndex = tc.index !== undefined ? tc.index : lastToolIndex;
            const toolState = toolStateForIndex(nextToolIndex);
            lastToolIndex = toolState.index;
            const currentTool = toolState.state;
            if (tc.id) currentTool.id = tc.id;
            const idDelta = tc.id || "";
            let nameDelta = "";
            let argumentsDelta = "";
            if (tc.function) {
              if (tc.function.name) {
                nameDelta = tc.function.name;
                currentTool.name += tc.function.name;
              }
              if (tc.function.arguments) {
                argumentsDelta = tc.function.arguments;
                currentTool.arguments += tc.function.arguments;
              }
            }
            if (onChunk && (idDelta || nameDelta || argumentsDelta)) {
              await onChunk({
                type: "tool_call_delta",
                index: toolState.index,
                id_delta: idDelta,
                name_delta: nameDelta,
                arguments_delta: argumentsDelta,
              });
            }
          }
        }
      }
    }
  } catch (error) {
    throw toUpstreamStreamError(error);
  } finally {
    reader.releaseLock();
  }

  const toolCalls = Array.from(toolCallStates.entries())
    .sort((left, right) => left[0] - right[0])
    .map((entry) => entry[1])
    .filter((tc) => tc && tc.name);

  return {
    choices: [{
      message: {
        role: "assistant",
        content: accumulatedContent,
        reasoning_content: accumulatedReasoning || undefined,
        tool_calls: toolCalls.length > 0 ? toolCalls.map((tc, i) => ({
          id: tc.id || makeId("call"),
          type: "function",
          function: { name: tc.name, arguments: tc.arguments },
        })) : undefined,
      },
    }],
    usage: usage || { input_tokens: 0, output_tokens: 0, total_tokens: 0 },
  };
}

function normalizeToolCallIndex(value) {
  const index = Number(value);
  if (!Number.isFinite(index) || index < 0) return 0;
  return Math.floor(index);
}

async function fetchDeepSeek(payload, config, authorization) {
  const url = resolveChatCompletionsUrl(config.deepseekBaseUrl, { officialV1Compat: config.deepseekOfficialV1Compat });
  const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
  const timeout = setTimeout(() => controller && controller.abort(), config.requestTimeoutMs);
  try {
    const headers = {
      "Content-Type": "application/json",
      Accept: payload.stream ? "text/event-stream" : "application/json",
    };
    const authHeader = resolveAuthorizationHeader(Object.assign({}, config, { authorization }));
    if (authHeader) headers.Authorization = authHeader;

    return await fetch(url, {
      method: "POST",
      headers,
      body: JSON.stringify(payload),
      signal: controller ? controller.signal : undefined,
      dispatcher: resolveDispatcher(config),
    });
  } catch (error) {
    throw toUpstreamConnectionError(error);
  } finally {
    clearTimeout(timeout);
  }
}

async function parseDeepSeekJson(response) {
  const body = await parseJsonResponse(response);
  if (!response.ok) throw upstreamError(response.status, body);
  return body;
}

function getAssistantMessage(completion) {
  const choice = completion && Array.isArray(completion.choices) ? completion.choices[0] : null;
  return choice && choice.message ? choice.message : { role: "assistant", content: "" };
}

function upstreamError(status, body) {
  const message = (body && body.error ? body.error.message : null) || (body ? body.message : null) || (body ? body.raw : null) || "Upstream DeepSeek request failed with status " + status + ".";
  return httpError(status, message, "api_error", (body && body.error ? body.error.code : null) || "upstream_error");
}

function toUpstreamConnectionError(error) {
  if (isHttpError(error)) return error;
  if (error && error.name === "AbortError") return httpError(504, "Upstream DeepSeek request timed out.", "api_error", "upstream_timeout");
  const wrapped = httpError(502, "Failed to connect to DeepSeek upstream: " + safeErrorSummary(error), "api_error", "upstream_connection_failed");
  attachSafeCause(wrapped, error);
  return wrapped;
}

function toUpstreamStreamError(error) {
  if (isHttpError(error)) return error;
  const wrapped = httpError(502, "DeepSeek upstream stream failed: " + safeErrorSummary(error), "api_error", "upstream_stream_failed");
  attachSafeCause(wrapped, error);
  return wrapped;
}

function isHttpError(error) {
  return Boolean(error && Number.isFinite(Number(error.status)) && error.type && error.code);
}

function attachSafeCause(target, source) {
  const cause = source && source.cause ? source.cause : null;
  target.safe_cause = {
    name: sanitizeErrorText(source && source.name),
    code: sanitizeErrorText((source && source.code) || (cause && cause.code)),
    cause_code: sanitizeErrorText(cause && cause.code),
    cause_name: sanitizeErrorText(cause && cause.name),
    syscall: sanitizeErrorText(cause && cause.syscall),
  };
}

function safeErrorSummary(error) {
  const parts = [];
  const message = sanitizeErrorText(error && error.message ? error.message : String(error || "unknown_error"));
  if (message) parts.push(message);
  const code = sanitizeErrorText((error && error.code) || (error && error.cause && error.cause.code));
  if (code && !parts.some((part) => part.includes(code))) parts.push("code=" + code);
  const causeMessage = sanitizeErrorText(error && error.cause && error.cause.message);
  if (causeMessage && causeMessage !== message) parts.push("cause=" + causeMessage);
  return parts.filter(Boolean).join("; ") || "unknown_error";
}

function sanitizeErrorText(value) {
  return String(value || "")
    .replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer [redacted]")
    .replace(/(https?:\/\/)([^:@/\s]+):([^@/\s]+)@/gi, "$1[redacted]@")
    .replace(/[A-Za-z0-9._%+-]+:[A-Za-z0-9._~+/=-]+@/g, "[redacted]@")
    .slice(0, 500);
}

function parseDeepSeekFrame(frame) {
  const lines = frame.split("\n").map((line) => line.trim()).filter(Boolean);
  const data = lines.filter((line) => line.startsWith("data:")).map((line) => line.slice(5).trim()).join("\n");
  if (!data) return null;
  if (data === "[DONE]") return "[DONE]";
  return JSON.parse(data);
}

function mapThinking(reasoning, config) {
  const forced = String(config.thinkingMode || "auto").toLowerCase();
  if (forced === "enabled") return { type: "enabled" };
  if (forced === "disabled") return { type: "disabled" };
  const effort = reasoning ? reasoning.effort : null;
  if (effort === "none") return { type: "disabled" };
  if (effort) return { type: "enabled" };
  return undefined;
}

function mapResponseFormat(textConfig) {
  const format = textConfig ? textConfig.format : null;
  if (!format || !format.type || format.type === "text") return undefined;
  if (format.type === "json_object" || format.type === "json_schema") return { type: "json_object" };
  return undefined;
}

function resolveChatCompletionsUrl(baseUrl, options = {}) {
  const url = new URL(baseUrl || "https://api.deepseek.com/");
  const pathname = String(url.pathname || "/").replace(/\/+$/, "");
  if (/\/chat\/completions$/i.test(pathname)) {
    url.pathname = pathname || "/";
    url.search = "";
    url.hash = "";
    return url.toString();
  }
  const officialRoot = officialDeepSeekOrigin(url) && (pathname === "" || pathname === "/");
  const officialV1Compat = options.officialV1Compat !== false;
  url.pathname = officialRoot && officialV1Compat
    ? "/v1/chat/completions"
    : (pathname || "") + "/chat/completions";
  url.search = "";
  url.hash = "";
  return url.toString();
}

function officialDeepSeekOrigin(url) {
  return Boolean(url && url.protocol === "https:" && url.hostname.toLowerCase() === "api.deepseek.com");
}

function stripUndefined(value) {
  if (Array.isArray(value)) return value.map(stripUndefined);
  if (!value || typeof value !== "object") return value;
  const result = {};
  for (const [key, current] of Object.entries(value)) {
    if (current !== undefined) result[key] = stripUndefined(current);
  }
  return result;
}

function resolveAuthorizationHeader(config) {
  const requestAuth = rememberAuthorizationHeader(config && config.authorization);
  if (requestAuth) return requestAuth;
  const codexApiKey = String(readCodexAuthApiKey(config) || "").trim();
  if (codexApiKey) return formatBearer(codexApiKey);
  return "";
}

function formatBearer(value) {
  return /^Bearer\s+/i.test(value) ? value : "Bearer " + value;
}

module.exports = {
  buildDeepSeekPayload,
  callDeepSeekJson,
  callDeepSeekStream,
  getAssistantMessage,
  resolveChatCompletionsUrl,
};
async function fetchDeepSeekStream(payload, config, authorization) {
  const url = resolveChatCompletionsUrl(config.deepseekBaseUrl, { officialV1Compat: config.deepseekOfficialV1Compat });
  const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
  const timeout = setTimeout(() => controller && controller.abort(), config.requestTimeoutMs);
  try {
    const headers = {
      "Content-Type": "application/json",
      Accept: "text/event-stream",
    };
    const authHeader = resolveAuthorizationHeader(Object.assign({}, config, { authorization }));
    if (authHeader) headers.Authorization = authHeader;

    const resp = await fetch(url, {
      method: "POST",
      headers,
      body: JSON.stringify(Object.assign({}, payload, { stream: true })),
      signal: controller ? controller.signal : undefined,
      dispatcher: resolveDispatcher(config),
    });
    clearTimeout(timeout);
    if (!resp.ok) {
      const text = await resp.text().catch(() => "");
      throw upstreamError(resp.status, parseUpstreamErrorText(text));
    }
    return resp.body;
  } catch (error) {
    clearTimeout(timeout);
    throw toUpstreamConnectionError(error);
  }
}

function parseUpstreamErrorText(text) {
  const value = String(text || "").trim();
  if (!value) return null;
  try {
    return JSON.parse(value);
  } catch {
    return { error: { message: value } };
  }
}
