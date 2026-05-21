const { createSequence, emitSse, makeId } = require("../../shared/http");
const { buildDeepSeekPayload, callDeepSeekStream, getAssistantMessage } = require("../deepseek-client");
const { assistantForStorage, buildResponseRecord, normalizeAssistant, responseOutputFromAssistant } = require("../conversation");
const { pushEvent } = require("../runtime");
const { emitAdaptedOutputEvents } = require("../tool-adapters");
const { splitToolCalls } = require("../tools");
const { createDsmlToolBlockStripper } = require("../text-utils");
const { mapUsage, mergeUsage } = require("../usage");

const STREAM_TEXT_CHUNK_SIZE = 80;
const STREAM_TEXT_PACE_MS = 12;
const STREAM_REASONING_CHUNK_SIZE = 80;
const STREAM_TOOL_ARGUMENT_CHUNK_SIZE = 4096;
const THINKING_DISPLAY_MARKER = "codeseex_display_only: thinking_markdown";

async function streamDeepSeekResponseV2(res, options) {
  const {
    id,
    createdAt,
    requestBody,
    messages,
    toolContext,
    config,
    runtime = null,
    authorization,
    toVisibleAssistant,
    hostedToolResultMessages,
    logToolCalls,
    logToolResults,
    flushRuntime,
  } = options;

  const seq = createSequence();
  res.writeHead(200, {
    "Content-Type": "text/event-stream; charset=utf-8",
    "Cache-Control": "no-cache, no-transform",
    Connection: "keep-alive",
    "X-Accel-Buffering": "no",
  });
  if (res.socket && typeof res.socket.setNoDelay === "function") res.socket.setNoDelay(true);
  if (typeof res.flushHeaders === "function") res.flushHeaders();

  const responseMeta = { id, object: "response", created_at: createdAt, model: requestBody.model, status: "in_progress" };
  emitSse(res, "response.created", { type: "response.created", response: responseMeta, sequence_number: seq.next() });
  emitSse(res, "response.in_progress", { type: "response.in_progress", response: responseMeta, sequence_number: seq.next() });

  const emitter = createResponseEmitter(res, seq, id, runtime);
  const workingMessages = messages.slice();
  const storedMessages = [];
  let usage = null;
  let rawAssistant = { role: "assistant", content: "" };

  try {
    while (true) {
      const turnStream = emitter.createTurnStream(config);
      const dsmlStripper = createDsmlToolBlockStripper();
      const payload = buildDeepSeekPayload(requestBody, workingMessages, toolContext, config, { stream: true });
      const completion = await callDeepSeekStream(payload, config, authorization, async (chunk) => {
        if (!chunk || typeof chunk !== "object") return;
        if (chunk.type === "reasoning") {
          if (config.visibleThinkingEnabled) await turnStream.appendReasoning(chunk.delta || "");
          return;
        }
        if (chunk.type === "reasoning_end") {
          await turnStream.closeReasoningWithDisplay();
          return;
        }
        if (chunk.type === "content") {
          await turnStream.closeReasoningWithDisplay();
          await turnStream.appendContent(dsmlStripper.push(chunk.delta || ""));
          return;
        }
        if (chunk.type === "tool_call_delta") {
          await turnStream.closeReasoningWithDisplay();
          await turnStream.closeContent("commentary");
        }
      });
      const turnUsage = mapUsage(completion.usage);
      usage = mergeUsage(usage, turnUsage);

      const currentAssistant = normalizeAssistant(getAssistantMessage(completion));
      rawAssistant = currentAssistant;

      const { external, internal, hosted } = splitToolCalls(currentAssistant.tool_calls, toolContext);
      if (typeof logToolCalls === "function") {
        logToolCalls(runtime, internal, toolContext, "internal");
        logToolCalls(runtime, hosted, toolContext, "hosted");
        logToolCalls(runtime, external, toolContext, "external");
      }
      if (typeof flushRuntime === "function") flushRuntime(config, runtime);

      const hasToolCalls = internal.length > 0 || hosted.length > 0 || external.length > 0;
      const visibleAssistant = toVisibleAssistant(currentAssistant, toolContext, {
        includeInternalPatchCalls: internal.length > 0,
        includeHostedCalls: hosted.length > 0,
      });
      await turnStream.appendContent(dsmlStripper.flush());
      await turnStream.closeReasoningWithDisplay();
      await turnStream.closeContent(hasToolCalls ? "commentary" : "final_answer", currentAssistant.content || "");
      const visibleOutput = turnOutputFromAssistant(visibleAssistant, turnUsage, toolContext, config, {
        phase: hasToolCalls ? "commentary" : "final_answer",
      });
      await emitter.emitItems(filterAlreadyStreamedTurnItems(visibleOutput, turnStream));

      if (hosted.length > 0) {
        const hostedResult = await hostedToolResultMessages(hosted, toolContext, config, workingMessages);
        if (typeof logToolResults === "function") logToolResults(runtime, hostedResult.toolMessages, "hosted");
        if (typeof flushRuntime === "function") flushRuntime(config, runtime);
        storedMessages.push(assistantForStorage(visibleAssistant));
        storedMessages.push(...hostedResult.toolMessages);
        if (external.length > 0 || internal.length > 0) break;
        workingMessages.push(visibleAssistant, ...hostedResult.toolMessages);
        continue;
      }

      if (internal.length > 0) {
        storedMessages.push(assistantForStorage(visibleAssistant));
        break;
      }

      storedMessages.push(assistantForStorage(currentAssistant));
      break;
    }
  } catch (error) {
    logUpstreamStreamError(runtime, error);
    const errorPayload = responseError(error);
    const response = buildResponseRecord({
      id,
      createdAt,
      model: requestBody.model,
      output: emitter.output,
      usage,
      status: "failed",
      error: errorPayload,
    });
    emitter.fail(response);
    res.write("data: [DONE]\n\n");
    res.end();
    return { failed: true, response, usage, storedMessages };
  }

  const response = buildResponseRecord({
    id,
    createdAt,
    model: requestBody.model,
    output: emitter.output,
    usage,
  });
  emitter.complete(response);
  res.write("data: [DONE]\n\n");
  res.end();
  return {
    failed: false,
    response,
    usage,
    storedMessages,
    rawAssistant,
  };
}

function turnOutputFromAssistant(assistant, usage, toolContext, config, options = {}) {
  const output = responseOutputFromAssistant(assistant, usage, toolContext, config, options);
  const reasoning = assistant && assistant.reasoning_content ? assistant.reasoning_content : "";
  if (!reasoning || !config.visibleThinkingEnabled) return output;

  const insertAt = output.findIndex((item) => item && item.type !== "reasoning");
  const thinkingItem = thinkingDisplayMessage(reasoning, config);
  if (insertAt === -1) return output.concat([thinkingItem]);
  return output.slice(0, insertAt).concat([thinkingItem], output.slice(insertAt));
}

function filterAlreadyStreamedTurnItems(items, turnStream) {
  if (!turnStream) return items;
  return (Array.isArray(items) ? items : []).filter((item) => {
    if (!item || typeof item !== "object") return false;
    if (item.type === "reasoning" && turnStream.hasReasoning()) return false;
    if (item.type === "message" && item.codeseex_display_only === "thinking_markdown" && turnStream.hasThinkingDisplay()) return false;
    if (item.type === "message" && turnStream.hasContent()) return false;
    return true;
  });
}

function thinkingDisplayMessage(reasoning, config = {}) {
  const title = config.thinkingTitle || "DeepSeek Thinking";
  const quoted = String(reasoning || "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .trim()
    .split("\n")
    .map((line) => "> " + line)
    .join("\n");
  return Object.assign(messageItem([
    "---",
    "**" + title + "**",
    quoted,
    "---",
  ].filter(Boolean).join("\n"), "commentary"), {
    codeseex_display_only: "thinking_markdown",
    metadata: { codeseex_display_only: true, kind: "thinking_markdown" },
  });
}

function toolUsageMessage(items) {
  const toolItems = (Array.isArray(items) ? items : [items]).filter(Boolean);
  const names = toolItems.map(toolDisplayName).filter(Boolean);
  if (names.length === 0) return null;
  const text = names.length === 1
    ? "\u5df2\u4f7f\u7528\u5de5\u5177 `" + names[0] + "`"
    : toolUsageBatchText(names);
  return Object.assign(messageItem(text, "commentary"), {
    codeseex_display_only: "tool_usage",
    metadata: { codeseex_display_only: true, kind: "tool_usage", tools: names },
  });
}

function toolUsageBatchText(names) {
  const uniqueNames = [];
  for (const name of names) {
    if (!uniqueNames.includes(name)) uniqueNames.push(name);
  }
  const visibleNames = uniqueNames.slice(0, 3);
  const hiddenCount = Math.max(0, uniqueNames.length - visibleNames.length);
  const suffix = hiddenCount > 0 ? " +" + hiddenCount : "";
  return "\u5df2\u4f7f\u7528 " + names.length + " \u4e2a\u5de5\u5177\n" + visibleNames.map((name) => "`" + name + "`").join(" \u00b7 ") + suffix;
}

function shouldEmitToolUsageMessage(item) {
  if (!item || typeof item !== "object") return false;
  return item.type === "proxy_tool_call";
}

function mergeToolUsageItems(items) {
  const output = [];
  const source = Array.isArray(items) ? items : [];
  for (let index = 0; index < source.length; index += 1) {
    const item = source[index];
    if (!shouldEmitToolUsageMessage(item)) {
      output.push(item);
      continue;
    }

    const group = [item];
    while (index + 1 < source.length && shouldEmitToolUsageMessage(source[index + 1])) {
      index += 1;
      group.push(source[index]);
    }

    const usage = toolUsageMessage(group);
    if (usage) output.push(usage);
    output.push(...group);
  }
  return output;
}

function toolDisplayName(item) {
  if (!item || typeof item !== "object") return "";
  if (item.name) return String(item.name);
  if (item.type === "web_search_call") return "web_search";
  if (item.type === "proxy_tool_call") return String(item.name || "tool");
  return "";
}

function createResponseEmitter(res, seq, responseId, runtime) {
  let outputIndex = 0;
  const output = [];
  const lifecycle = createToolLifecycleTracer(runtime, responseId);

  async function emitItems(items) {
    for (const item of mergeToolUsageItems(items)) {
      await emitItem(item);
    }
  }

  async function emitItem(item) {
    if (!item || typeof item !== "object") return;

    const currentIndex = outputIndex;
    outputIndex += 1;

    if (item.type === "reasoning") {
      await emitReasoningItem(item, currentIndex);
    } else if (item.type === "message") {
      await emitMessageItem(item, currentIndex);
    } else if (item.type === "function_call") {
      await emitFunctionCallItem(item, currentIndex);
    } else {
      emitGenericItem(item, currentIndex);
    }

    output.push(item);
  }

  async function emitReasoningItem(item, currentIndex) {
    const summary = Array.isArray(item.summary) ? item.summary : [];
    const text = summary.map((part) => part && (part.text || part.summary_text || "")).filter(Boolean).join("\n\n");
    emitSse(res, "response.output_item.added", {
      type: "response.output_item.added",
      response_id: responseId,
      output_index: currentIndex,
      item: { id: item.id, type: "reasoning", status: "in_progress", summary: [] },
      sequence_number: seq.next(),
    });
    if (text) {
      emitSse(res, "response.reasoning_summary_part.added", {
        type: "response.reasoning_summary_part.added",
        response_id: responseId,
        item_id: item.id,
        output_index: currentIndex,
        summary_index: 0,
        part: { type: "summary_text", text: "" },
        sequence_number: seq.next(),
      });
      for (const delta of splitTextForStreaming(text, STREAM_REASONING_CHUNK_SIZE)) {
        emitSse(res, "response.reasoning_summary_text.delta", {
          type: "response.reasoning_summary_text.delta",
          response_id: responseId,
          item_id: item.id,
          output_index: currentIndex,
          summary_index: 0,
          delta,
          sequence_number: seq.next(),
        });
      }
      emitSse(res, "response.reasoning_summary_text.done", {
        type: "response.reasoning_summary_text.done",
        response_id: responseId,
        item_id: item.id,
        output_index: currentIndex,
        summary_index: 0,
        text,
        sequence_number: seq.next(),
      });
      emitSse(res, "response.reasoning_summary_part.done", {
        type: "response.reasoning_summary_part.done",
        response_id: responseId,
        item_id: item.id,
        output_index: currentIndex,
        summary_index: 0,
        part: { type: "summary_text", text },
        sequence_number: seq.next(),
      });
    }
    const doneItem = Object.assign({}, item, { status: "completed", content: null });
    emitSse(res, "response.output_item.done", {
      type: "response.output_item.done",
      response_id: responseId,
      output_index: currentIndex,
      item: doneItem,
      sequence_number: seq.next(),
    });
  }

  async function emitMessageItem(item, currentIndex) {
    const part = item.content && item.content[0] ? item.content[0] : { type: "output_text", text: "", annotations: [] };
    const addedItem = Object.assign({}, item, { status: "in_progress", content: [] });
    emitSse(res, "response.output_item.added", {
      type: "response.output_item.added",
      response_id: responseId,
      output_index: currentIndex,
      item: addedItem,
      sequence_number: seq.next(),
    });
    emitSse(res, "response.content_part.added", {
      type: "response.content_part.added",
      response_id: responseId,
      item_id: item.id,
      output_index: currentIndex,
      content_index: 0,
      part: Object.assign({}, part, { text: "" }),
      sequence_number: seq.next(),
    });
    await emitTextDeltas(res, seq, responseId, item.id, currentIndex, part.text || "");
    emitSse(res, "response.output_text.done", {
      type: "response.output_text.done",
      response_id: responseId,
      item_id: item.id,
      output_index: currentIndex,
      content_index: 0,
      text: part.text || "",
      sequence_number: seq.next(),
    });
    emitSse(res, "response.content_part.done", {
      type: "response.content_part.done",
      response_id: responseId,
      item_id: item.id,
      output_index: currentIndex,
      content_index: 0,
      part,
      sequence_number: seq.next(),
    });
    emitSse(res, "response.output_item.done", {
      type: "response.output_item.done",
      response_id: responseId,
      output_index: currentIndex,
      item,
      sequence_number: seq.next(),
    });
  }

  async function emitFunctionCallItem(item, currentIndex) {
    const addedItem = Object.assign({}, item, { status: "in_progress", arguments: "" });
    emitSse(res, "response.output_item.added", {
      type: "response.output_item.added",
      response_id: responseId,
      output_index: currentIndex,
      item: addedItem,
      sequence_number: seq.next(),
    });
    lifecycle.record("output_item.added", item, {
      output_index: currentIndex,
      item_id: item.id,
      call_id: item.call_id,
      arguments_bytes: Buffer.byteLength(String(item.arguments || ""), "utf8"),
    });
    await emitFunctionCallArguments(res, seq, responseId, item, currentIndex, {
      onArgumentDelta(delta, index, total) {
        if (index === 0 || index === total - 1) {
          lifecycle.record("arguments_delta", item, {
            output_index: currentIndex,
            item_id: item.id,
            call_id: item.call_id,
            delta_bytes: Buffer.byteLength(String(delta || ""), "utf8"),
            chunk_index: index + 1,
            chunk_count: total,
          });
        }
      },
      onArgumentsDone(args, chunks) {
        lifecycle.record("arguments_done", item, {
          output_index: currentIndex,
          item_id: item.id,
          call_id: item.call_id,
          arguments_bytes: Buffer.byteLength(String(args || ""), "utf8"),
          chunk_count: chunks,
        });
      },
    });
    emitAdaptedOutputEvents(item, (eventName, payload) => {
      emitSse(res, eventName, Object.assign({}, payload, {
        response_id: responseId,
        output_index: currentIndex,
        sequence_number: seq.next(),
      }));
    });
    emitSse(res, "response.output_item.done", {
      type: "response.output_item.done",
      response_id: responseId,
      output_index: currentIndex,
      item,
      sequence_number: seq.next(),
    });
    lifecycle.record("output_item.done", item, {
      output_index: currentIndex,
      item_id: item.id,
      call_id: item.call_id,
      arguments_bytes: Buffer.byteLength(String(item.arguments || ""), "utf8"),
    });
  }

  function emitGenericItem(item, currentIndex) {
    emitSse(res, "response.output_item.added", {
      type: "response.output_item.added",
      response_id: responseId,
      output_index: currentIndex,
      item,
      sequence_number: seq.next(),
    });
    emitAdaptedOutputEvents(item, (eventName, payload) => {
      emitSse(res, eventName, Object.assign({}, payload, {
        response_id: responseId,
        output_index: currentIndex,
        sequence_number: seq.next(),
      }));
    });
    emitSse(res, "response.output_item.done", {
      type: "response.output_item.done",
      response_id: responseId,
      output_index: currentIndex,
      item,
      sequence_number: seq.next(),
    });
  }

  function complete(response) {
    lifecycle.responseCompleted(output.length);
    emitSse(res, "response.completed", {
      type: "response.completed",
      response,
      sequence_number: seq.next(),
    });
  }

  function fail(response) {
    lifecycle.responseCompleted(output.length);
    emitSse(res, "response.failed", {
      type: "response.failed",
      response,
      sequence_number: seq.next(),
    });
  }

  function reserveOutputIndex() {
    const current = outputIndex;
    outputIndex += 1;
    return current;
  }

  function pushOutput(item) {
    if (item) output.push(item);
  }

  function createTurnStream(config = {}) {
    return createTurnStreamEmitter({
      res,
      seq,
      responseId,
      config,
      reserveOutputIndex,
      pushOutput,
    });
  }

  return { emitItems, complete, fail, createTurnStream, output };
}

function responseError(error) {
  return {
    message: error && error.message ? error.message : "Stream failed",
    type: error && error.type ? error.type : "api_error",
    code: error && error.code ? error.code : "stream_failed",
  };
}

function logUpstreamStreamError(runtime, error) {
  if (!runtime) return;
  pushEvent(runtime, {
    type: "upstream_stream_error",
    level: "error",
    message: "DeepSeek upstream stream failed.",
    audience: "diagnostic",
    detail: {
      status: Number(error && error.status) || undefined,
      type: safeLogText(error && error.type),
      code: safeLogText(error && error.code),
      message: safeLogText(error && error.message),
      cause: sanitizeErrorCause(error && error.safe_cause),
    },
  });
}

function sanitizeErrorCause(cause) {
  if (!cause || typeof cause !== "object") return null;
  const output = {};
  for (const [key, value] of Object.entries(cause)) {
    const safe = safeLogText(value);
    if (safe) output[key] = safe;
  }
  return Object.keys(output).length > 0 ? output : null;
}

function safeLogText(value) {
  return String(value || "")
    .replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer [redacted]")
    .replace(/(https?:\/\/)([^:@/\s]+):([^@/\s]+)@/gi, "$1[redacted]@")
    .replace(/[A-Za-z0-9._%+-]+:[A-Za-z0-9._~+/=-]+@/g, "[redacted]@")
    .slice(0, 500);
}

function createTurnStreamEmitter({ res, seq, responseId, config, reserveOutputIndex, pushOutput }) {
  const reasoning = {
    id: makeId("rs"),
    outputIndex: null,
    started: false,
    closed: false,
    text: "",
  };
  const content = {
    id: makeId("msg"),
    outputIndex: null,
    started: false,
    closed: false,
    text: "",
  };
  const thinking = {
    id: makeId("msg"),
    outputIndex: null,
    started: false,
    closed: false,
    text: "",
    atLineStart: true,
  };
  let thinkingDisplayEmitted = false;

  async function appendReasoning(delta) {
    const text = String(delta || "");
    if (!text || reasoning.closed) return;
    ensureReasoningOpen();
    ensureThinkingDisplayOpen();
    reasoning.text += text;
    for (const chunk of splitTextForStreaming(text, STREAM_REASONING_CHUNK_SIZE)) {
      emitSse(res, "response.reasoning_summary_text.delta", {
        type: "response.reasoning_summary_text.delta",
        response_id: responseId,
        item_id: reasoning.id,
        output_index: reasoning.outputIndex,
        summary_index: 0,
        delta: chunk,
        sequence_number: seq.next(),
      });
    }
    await appendThinkingDisplayDelta(text);
  }

  async function closeReasoningWithDisplay() {
    if (!reasoning.started || reasoning.closed) return;
    closeReasoning();
    await closeThinkingDisplay();
  }

  async function appendContent(delta) {
    const text = String(delta || "");
    if (!text || content.closed) return;
    ensureContentOpen("commentary");
    content.text += text;
    await emitTextDeltas(res, seq, responseId, content.id, content.outputIndex, text);
  }

  async function closeContent(phase, finalText) {
    if (!content.started || content.closed) return;
    const text = finalText !== undefined && samePrefix(finalText, content.text) ? String(finalText || "") : content.text;
    const suffix = text.slice(content.text.length);
    if (suffix) {
      content.text = text;
      await emitTextDeltas(res, seq, responseId, content.id, content.outputIndex, suffix);
    }
    const item = messageItem(text, phase || "final_answer");
    item.id = content.id;
    emitSse(res, "response.output_text.done", {
      type: "response.output_text.done",
      response_id: responseId,
      item_id: content.id,
      output_index: content.outputIndex,
      content_index: 0,
      text,
      sequence_number: seq.next(),
    });
    emitSse(res, "response.content_part.done", {
      type: "response.content_part.done",
      response_id: responseId,
      item_id: content.id,
      output_index: content.outputIndex,
      content_index: 0,
      part: item.content[0],
      sequence_number: seq.next(),
    });
    emitSse(res, "response.output_item.done", {
      type: "response.output_item.done",
      response_id: responseId,
      output_index: content.outputIndex,
      item,
      sequence_number: seq.next(),
    });
    content.closed = true;
    pushOutput(item);
  }

  function hasReasoning() {
    return reasoning.started;
  }

  function hasThinkingDisplay() {
    return thinkingDisplayEmitted || thinking.started;
  }

  function hasContent() {
    return content.started;
  }

  function ensureReasoningOpen() {
    if (reasoning.started) return;
    reasoning.started = true;
    reasoning.outputIndex = reserveOutputIndex();
    emitSse(res, "response.output_item.added", {
      type: "response.output_item.added",
      response_id: responseId,
      output_index: reasoning.outputIndex,
      item: { id: reasoning.id, type: "reasoning", status: "in_progress", summary: [] },
      sequence_number: seq.next(),
    });
    emitSse(res, "response.reasoning_summary_part.added", {
      type: "response.reasoning_summary_part.added",
      response_id: responseId,
      item_id: reasoning.id,
      output_index: reasoning.outputIndex,
      summary_index: 0,
      part: { type: "summary_text", text: "" },
      sequence_number: seq.next(),
    });
  }

  function closeReasoning() {
    const text = reasoning.text;
    emitSse(res, "response.reasoning_summary_text.done", {
      type: "response.reasoning_summary_text.done",
      response_id: responseId,
      item_id: reasoning.id,
      output_index: reasoning.outputIndex,
      summary_index: 0,
      text,
      sequence_number: seq.next(),
    });
    emitSse(res, "response.reasoning_summary_part.done", {
      type: "response.reasoning_summary_part.done",
      response_id: responseId,
      item_id: reasoning.id,
      output_index: reasoning.outputIndex,
      summary_index: 0,
      part: { type: "summary_text", text },
      sequence_number: seq.next(),
    });
    const item = {
      id: reasoning.id,
      type: "reasoning",
      status: "completed",
      summary: text ? [{ type: "summary_text", text, title: config.thinkingTitle || "DeepSeek Thinking" }] : [],
      encrypted_content: encodeStreamedReasoningContent(text),
      content: null,
    };
    emitSse(res, "response.output_item.done", {
      type: "response.output_item.done",
      response_id: responseId,
      output_index: reasoning.outputIndex,
      item,
      sequence_number: seq.next(),
    });
    reasoning.closed = true;
    pushOutput(item);
  }

  function ensureContentOpen(phase) {
    if (content.started) return;
    content.started = true;
    content.outputIndex = reserveOutputIndex();
    const item = messageItem("", phase || "commentary");
    item.id = content.id;
    emitSse(res, "response.output_item.added", {
      type: "response.output_item.added",
      response_id: responseId,
      output_index: content.outputIndex,
      item: Object.assign({}, item, { status: "in_progress", content: [] }),
      sequence_number: seq.next(),
    });
    emitSse(res, "response.content_part.added", {
      type: "response.content_part.added",
      response_id: responseId,
      item_id: content.id,
      output_index: content.outputIndex,
      content_index: 0,
      part: { type: "output_text", text: "", annotations: [] },
      sequence_number: seq.next(),
    });
  }

  function ensureThinkingDisplayOpen() {
    if (!config.visibleThinkingEnabled || thinking.started) return;
    thinking.started = true;
    thinkingDisplayEmitted = true;
    thinking.outputIndex = reserveOutputIndex();
    const item = Object.assign(thinkingDisplayMessage("", config), { id: thinking.id });
    emitSse(res, "response.output_item.added", {
      type: "response.output_item.added",
      response_id: responseId,
      output_index: thinking.outputIndex,
      item: Object.assign({}, item, { status: "in_progress", content: [] }),
      sequence_number: seq.next(),
    });
    emitSse(res, "response.content_part.added", {
      type: "response.content_part.added",
      response_id: responseId,
      item_id: thinking.id,
      output_index: thinking.outputIndex,
      content_index: 0,
      part: { type: "output_text", text: "", annotations: [] },
      sequence_number: seq.next(),
    });
    const prefix = "---\n**" + (config.thinkingTitle || "DeepSeek Thinking") + "**\n";
    thinking.text += prefix;
    emitSse(res, "response.output_text.delta", {
      type: "response.output_text.delta",
      response_id: responseId,
      item_id: thinking.id,
      output_index: thinking.outputIndex,
      content_index: 0,
      delta: prefix,
      sequence_number: seq.next(),
    });
  }

  async function appendThinkingDisplayDelta(delta) {
    if (!thinking.started || thinking.closed) return;
    const quoted = quoteThinkingDelta(delta, thinking);
    if (!quoted) return;
    thinking.text += quoted;
    await emitTextDeltas(res, seq, responseId, thinking.id, thinking.outputIndex, quoted, STREAM_TEXT_CHUNK_SIZE, { pace: false });
  }

  async function closeThinkingDisplay() {
    if (!thinking.started || thinking.closed) return;
    let suffix = "";
    if (thinking.text && !thinking.text.endsWith("\n")) suffix += "\n";
    suffix += "---";
    thinking.text += suffix;
    await emitTextDeltas(res, seq, responseId, thinking.id, thinking.outputIndex, suffix, STREAM_TEXT_CHUNK_SIZE, { pace: false });
    const item = Object.assign(thinkingDisplayMessage(reasoning.text, config), {
      id: thinking.id,
      content: [{ type: "output_text", text: thinking.text, annotations: [] }],
    });
    emitSse(res, "response.output_text.done", {
      type: "response.output_text.done",
      response_id: responseId,
      item_id: thinking.id,
      output_index: thinking.outputIndex,
      content_index: 0,
      text: thinking.text,
      sequence_number: seq.next(),
    });
    emitSse(res, "response.content_part.done", {
      type: "response.content_part.done",
      response_id: responseId,
      item_id: thinking.id,
      output_index: thinking.outputIndex,
      content_index: 0,
      part: item.content[0],
      sequence_number: seq.next(),
    });
    emitSse(res, "response.output_item.done", {
      type: "response.output_item.done",
      response_id: responseId,
      output_index: thinking.outputIndex,
      item,
      sequence_number: seq.next(),
    });
    thinking.closed = true;
    pushOutput(item);
  }

  async function emitStreamedMessage(item, options = {}) {
    const currentIndex = reserveOutputIndex();
    const part = item.content && item.content[0] ? item.content[0] : { type: "output_text", text: "", annotations: [] };
    emitSse(res, "response.output_item.added", {
      type: "response.output_item.added",
      response_id: responseId,
      output_index: currentIndex,
      item: Object.assign({}, item, { status: "in_progress", content: [] }),
      sequence_number: seq.next(),
    });
    emitSse(res, "response.content_part.added", {
      type: "response.content_part.added",
      response_id: responseId,
      item_id: item.id,
      output_index: currentIndex,
      content_index: 0,
      part: Object.assign({}, part, { text: "" }),
      sequence_number: seq.next(),
    });
    await emitTextDeltas(res, seq, responseId, item.id, currentIndex, part.text || "", STREAM_TEXT_CHUNK_SIZE, { pace: options.pace !== false });
    emitSse(res, "response.output_text.done", {
      type: "response.output_text.done",
      response_id: responseId,
      item_id: item.id,
      output_index: currentIndex,
      content_index: 0,
      text: part.text || "",
      sequence_number: seq.next(),
    });
    emitSse(res, "response.content_part.done", {
      type: "response.content_part.done",
      response_id: responseId,
      item_id: item.id,
      output_index: currentIndex,
      content_index: 0,
      part,
      sequence_number: seq.next(),
    });
    emitSse(res, "response.output_item.done", {
      type: "response.output_item.done",
      response_id: responseId,
      output_index: currentIndex,
      item,
      sequence_number: seq.next(),
    });
    pushOutput(item);
  }

  return {
    appendReasoning,
    closeReasoningWithDisplay,
    appendContent,
    closeContent,
    hasReasoning,
    hasThinkingDisplay,
    hasContent,
  };
}

async function emitFunctionCallArguments(res, seq, responseId, item, outputIndex, options = {}) {
  const fullArguments = item.arguments || "{}";
  const chunks = splitTextForStreaming(fullArguments, STREAM_TOOL_ARGUMENT_CHUNK_SIZE);
  for (let index = 0; index < chunks.length; index += 1) {
    const delta = chunks[index];
    emitSse(res, "response.function_call_arguments.delta", {
      type: "response.function_call_arguments.delta",
      response_id: responseId,
      item_id: item.id,
      output_index: outputIndex,
      delta,
      sequence_number: seq.next(),
    });
    if (typeof options.onArgumentDelta === "function") options.onArgumentDelta(delta, index, chunks.length);
  }
  emitSse(res, "response.function_call_arguments.done", {
    type: "response.function_call_arguments.done",
    response_id: responseId,
    item_id: item.id,
    output_index: outputIndex,
    name: item.name,
    arguments: fullArguments,
    sequence_number: seq.next(),
  });
  if (typeof options.onArgumentsDone === "function") options.onArgumentsDone(fullArguments, chunks.length);
}

async function emitTextDeltas(res, seq, responseId, itemId, outputIndex, text, size = STREAM_TEXT_CHUNK_SIZE, options = {}) {
  const chunks = splitTextForStreaming(text, size);
  const shouldPace = options.pace !== false;
  for (let index = 0; index < chunks.length; index += 1) {
    const delta = chunks[index];
    emitSse(res, "response.output_text.delta", {
      type: "response.output_text.delta",
      response_id: responseId,
      item_id: itemId,
      output_index: outputIndex,
      content_index: 0,
      delta,
      sequence_number: seq.next(),
    });
    if (shouldPace && STREAM_TEXT_PACE_MS > 0 && index < chunks.length - 1) await delay(STREAM_TEXT_PACE_MS);
  }
}

function encodeStreamedReasoningContent(reasoning) {
  return Buffer.from(String(reasoning || ""), "utf8").toString("base64");
}

function samePrefix(expected, actualPrefix) {
  const full = String(expected || "");
  const prefix = String(actualPrefix || "");
  return full.startsWith(prefix);
}

function quoteThinkingDelta(delta, state) {
  const source = String(delta || "").replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  if (!source) return "";
  let output = "";
  for (const char of source) {
    if (state.atLineStart) {
      output += "> ";
      state.atLineStart = false;
    }
    output += char;
    if (char === "\n") state.atLineStart = true;
  }
  return output;
}

function createToolLifecycleTracer(runtime, responseId) {
  const startedAtByKey = new Map();
  const activeByKey = new Map();
  let tracedTools = 0;

  function record(phase, item, detail = {}) {
    if (!runtime || !isApplyPatchResponseItem(item)) return;
    const key = detail.call_id || (item && item.call_id) || detail.item_id || (item && item.id) || "";
    const now = Date.now();
    if (phase === "output_item.added" && key && !startedAtByKey.has(key)) {
      startedAtByKey.set(key, now);
      activeByKey.set(key, true);
      tracedTools += 1;
    }
    const startedAt = key ? startedAtByKey.get(key) : null;
    if (phase === "output_item.done" && key) activeByKey.delete(key);

    pushEvent(runtime, {
      type: "tool_lifecycle",
      level: "info",
      message: "Tool lifecycle: " + phase,
      audience: "diagnostic",
      detail: compactLifecycleDetail({
        response_id: responseId,
        phase,
        tool: "apply_patch",
        name: (item && item.name) || "shell",
        call_id: detail.call_id || (item && item.call_id) || "",
        item_id: detail.item_id || (item && item.id) || "",
        output_index: detail.output_index,
        arguments_bytes: detail.arguments_bytes,
        delta_bytes: detail.delta_bytes,
        chunk_index: detail.chunk_index,
        chunk_count: detail.chunk_count,
        elapsed_ms: startedAt ? now - startedAt : 0,
      }),
    });
  }

  function responseCompleted(outputCount) {
    if (!runtime || tracedTools === 0) return;
    pushEvent(runtime, {
      type: "tool_lifecycle",
      level: "info",
      message: "Tool lifecycle: response.completed",
      audience: "diagnostic",
      detail: {
        response_id: responseId,
        phase: "response.completed",
        traced_tools: tracedTools,
        active_tools: activeByKey.size,
        output_count: outputCount,
      },
    });
  }

  return { record, responseCompleted };
}

function isApplyPatchResponseItem(item) {
  if (!item || item.type !== "function_call") return false;
  if (item.name === "apply_patch" || item.name === "apply_patch_proxy") return true;
  if (item.name !== "shell") return false;
  const args = parseJsonLoose(item.arguments);
  const command = args && Array.isArray(args.command) ? args.command : null;
  return Boolean(command && command[0] === "apply_patch");
}

function messageItem(text, phase) {
  const item = {
    id: makeId("msg"),
    type: "message",
    status: "completed",
    role: "assistant",
    content: [{ type: "output_text", text: String(text || ""), annotations: [] }],
  };
  if (phase) item.phase = phase;
  return item;
}

function splitTextForStreaming(text, size) {
  const source = String(text || "");
  if (!source) return [];
  const chunkSize = Math.max(1, Number(size) || 80);
  return source.match(new RegExp("[\\s\\S]{1," + chunkSize + "}", "g")) || [source];
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function compactLifecycleDetail(detail) {
  const output = {};
  for (const [key, value] of Object.entries(detail || {})) {
    if (value === undefined || value === null || value === "") continue;
    output[key] = value;
  }
  return output;
}

function parseJsonLoose(value) {
  if (!value) return null;
  if (typeof value === "object" && !Array.isArray(value)) return value;
  if (typeof value !== "string") return null;
  try {
    const parsed = JSON.parse(value);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

module.exports = {
  THINKING_DISPLAY_MARKER,
  streamDeepSeekResponseV2,
  thinkingDisplayMessage,
  turnOutputFromAssistant,
};
