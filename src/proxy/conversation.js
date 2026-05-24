const crypto = require("node:crypto");

const { makeId } = require("../shared/http");
const { cloneJson } = require("../shared/json-store");
const { repairMojibakeText } = require("../shared/text-encoding");
const { chatToolCallFromResponseItem } = require("./tools");
const { extractTaggedThinking, extractText, normalizeFileLinks, normalizeReasoningText, sanitizeToolXmlInText, stripDsmlToolBlocks } = require("./text-utils");

const HIDDEN_REASONING_PREFIX = "codeseex-reasoning-v1:";
const THINKING_DISPLAY_ONLY_PATTERN = /codeseex_display_only:\s*thinking_markdown/i;
const MAX_HISTORY_MESSAGES = 60;
const MAX_HISTORY_BYTES = 120000;
const MAX_TOOL_OUTPUT_BYTES = 12000;
const MAX_MESSAGE_CONTENT_BYTES = 24000;
const MAX_STORED_INPUT_ITEMS = 20;

function normalizeInput(input) {
  if (input === undefined || input === null) return [];
  if (typeof input === "string") return [messageItem("user", input)];
  if (!Array.isArray(input)) {
    if (typeof input === "object" && input.role) return [normalizeMessage(input)];
    throw new Error("input must be a string or array.");
  }

  const items = [];
  for (const raw of input) {
    if (typeof raw === "string") {
      items.push(messageItem("user", raw));
      continue;
    }
    if (!raw || typeof raw !== "object") continue;
    if (isDisplayOnlyItem(raw)) continue;
    if (raw.type === "message" || raw.role) {
      const text = extractText(raw.content);
      if (isDisplayOnlyText(text)) continue;
      items.push(normalizeMessage(raw));
      continue;
    }
    if (raw.type === "function_call" || raw.type === "custom_tool_call" || raw.type === "web_search_call") {
      items.push(Object.assign({ id: raw.id || makeId("tc"), status: raw.status || "completed" }, raw));
      continue;
    }
    if (raw.type === "function_call_output" || raw.type === "custom_tool_call_output" || raw.type === "web_search_call_output") {
      if (!raw.call_id) continue;
      items.push(Object.assign({ id: raw.id || makeId("tco"), status: raw.status || "completed" }, raw));
      continue;
    }
    if (raw.type === "reasoning" || raw.type === "compaction") items.push(raw);
  }
  return items;
}

function inputToMessages(items) {
  const messages = [];
  const resolvedToolCallIds = collectResolvedToolCallIds(items);
  const toolNamesByCallId = collectToolCallNamesById(items);
  let pendingAssistant = null;
  let pendingReasoning = "";

  for (const item of items) {
    if (!item) continue;
    if (item.type === "reasoning") {
      const reasoning = extractReasoningItem(item);
      if (reasoning) {
        if (pendingAssistant && (pendingAssistant.content || pendingAssistant.tool_calls.length > 0)) {
          flushPending();
          pendingReasoning = joinReasoning(pendingReasoning, reasoning);
        } else if (pendingAssistant) {
          pendingAssistant.reasoning_content = joinReasoning(pendingAssistant.reasoning_content, reasoning);
        } else {
          pendingReasoning = joinReasoning(pendingReasoning, reasoning);
        }
      }
      continue;
    }
    if (item.type === "message" || item.role) {
      if (isDisplayOnlyItem(item) || isDisplayOnlyText(extractText(item.content))) continue;
      const role = normalizeRole(item.role);
      if (role === "assistant") {
        const parsed = parseAssistantDisplay(extractText(item.content));
        const reasoning = joinReasoning(pendingReasoning, parsed.reasoning_content);
        pendingReasoning = "";
        if (!parsed.content && !reasoning) continue;
        if (!parsed.content && reasoning) {
          pendingReasoning = joinReasoning(pendingReasoning, reasoning);
          continue;
        }
        flushPending();
        pendingAssistant = {
          role: "assistant",
          content: parsed.content || "",
          reasoning_content: reasoning,
          tool_calls: [],
        };
        continue;
      }

      flushPending();
      pendingReasoning = "";
      const text = stripAssistantDecorations(extractText(item.content));
      if (!text) continue;
      messages.push({ role, content: text });
      continue;
    }
    if (item.type === "function_call" || item.type === "custom_tool_call" || item.type === "web_search_call") {
      const callId = resolveToolCallId(item);
      if (!callId || !resolvedToolCallIds.has(callId)) continue;
      if (!pendingAssistant) pendingAssistant = { role: "assistant", content: "", reasoning_content: "", tool_calls: [] };
      if (pendingReasoning) {
        pendingAssistant.reasoning_content = joinReasoning(pendingAssistant.reasoning_content, pendingReasoning);
        pendingReasoning = "";
      }
      if (item.type === "web_search_call") {
        pendingAssistant.tool_calls.push(chatToolCallFromResponseItem(item));
        continue;
      }
      pendingAssistant.tool_calls.push(chatToolCallFromResponseItem(item));
      continue;
    }
    if (item.type === "function_call_output" || item.type === "custom_tool_call_output" || item.type === "web_search_call_output") {
      flushPending();
      const callId = resolveToolCallId(item);
      if (!callId) continue;
      messages.push({ role: "tool", tool_call_id: callId, content: sanitizeToolContent(normalizeToolOutputText(item, { toolName: toolNamesByCallId.get(callId) })) });
    }
  }

  flushPending();
  return messages;

  function flushPending() {
    if (!pendingAssistant) return;
    const message = { role: "assistant", content: pendingAssistant.content || "" };
    if (pendingAssistant.reasoning_content) message.reasoning_content = pendingAssistant.reasoning_content;
    if (pendingAssistant.tool_calls.length > 0) message.tool_calls = pendingAssistant.tool_calls.map(cleanToolCallForChat);
    if (message.content || message.reasoning_content || message.tool_calls) messages.push(message);
    pendingAssistant = null;
  }
}

function joinToolOutputs(existing, next) {
  const left = String(existing || "").trim();
  const right = String(next || "").trim();
  if (!left) return right;
  if (!right) return left;
  return left + "\n" + right;
}

function joinReasoning(existing, next) {
  const left = String(existing || "").trim();
  const right = String(next || "").trim();
  if (!left) return right;
  if (!right || left === right) return left;
  return left + "\n\n" + right;
}

function cleanToolCallForChat(toolCall) {
  if (!toolCall || typeof toolCall !== "object") return toolCall;
  const cleaned = Object.assign({}, toolCall);
  delete cleaned.patchBodies;
  return cleaned;
}

function enforceChatToolProtocol(messages, options = {}) {
  const result = [];
  const list = Array.isArray(messages) ? messages : [];
  let pending = null;

  for (let index = 0; index < list.length; index += 1) {
    const message = list[index];
    if (!message || typeof message !== "object") continue;

    if (message.role === "tool") {
      if (!pending) continue;
      pending.toolMessages.push(message);
      if (pendingHasAllToolOutputs(pending)) flushPending(false);
      continue;
    }

    if (message.role === "assistant") {
      if (pending) {
        mergePendingAssistant(pending, message);
        if (pendingHasAllToolOutputs(pending)) flushPending(false);
        continue;
      }

      if (Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
        pending = {
          assistant: cloneAssistantForProtocol(message),
          toolMessages: [],
        };
        continue;
      }

      result.push(message);
      continue;
    }

    flushPending(false);
    result.push(message);
  }

  flushPending(Boolean(options && options.preservePendingTail));
  return result;

  function flushPending(preservePendingTail) {
    if (!pending) return;
    const expectedIds = pending.assistant.tool_calls.map((call) => call && call.id).filter(Boolean);
    const toolMessages = orderedToolMessagesForIds(pending.toolMessages, expectedIds);
    const complete = expectedIds.length > 0 && toolMessages.length === expectedIds.length;
    if (complete) {
      result.push(pending.assistant, ...toolMessages);
    } else if (preservePendingTail) {
      result.push(pending.assistant, ...pending.toolMessages);
    } else {
      const downgraded = Object.assign({}, pending.assistant);
      delete downgraded.tool_calls;
      if (downgraded.content || downgraded.reasoning_content) result.push(downgraded);
    }
    pending = null;
  }
}

function pendingHasAllToolOutputs(pending) {
  if (!pending || !pending.assistant) return false;
  const expectedIds = pending.assistant.tool_calls.map((call) => call && call.id).filter(Boolean);
  return expectedIds.length > 0 && orderedToolMessagesForIds(pending.toolMessages, expectedIds).length === expectedIds.length;
}

function orderedToolMessagesForIds(toolMessages, expectedIds) {
  const byId = new Map();
  for (const toolMessage of Array.isArray(toolMessages) ? toolMessages : []) {
    if (!toolMessage || toolMessage.role !== "tool" || !toolMessage.tool_call_id) continue;
    if (!byId.has(toolMessage.tool_call_id)) byId.set(toolMessage.tool_call_id, toolMessage);
  }
  const output = [];
  for (const id of expectedIds || []) {
    if (byId.has(id)) output.push(byId.get(id));
  }
  return output;
}

function cloneAssistantForProtocol(message) {
  const cloned = Object.assign({}, message);
  cloned.tool_calls = Array.isArray(message.tool_calls) ? message.tool_calls.map(cleanToolCallForChat) : [];
  return cloned;
}

function mergePendingAssistant(pending, nextAssistant) {
  if (!pending || !pending.assistant || !nextAssistant) return;
  const next = cloneAssistantForProtocol(nextAssistant);
  pending.assistant.content = [pending.assistant.content, next.content].filter(Boolean).join("\n\n");
  pending.assistant.reasoning_content = [pending.assistant.reasoning_content, next.reasoning_content].filter(Boolean).join("\n\n");
  if (Array.isArray(next.tool_calls) && next.tool_calls.length > 0) pending.assistant.tool_calls.push(...next.tool_calls);
}

function collectResolvedToolCallIds(items) {
  const resolved = new Set();
  for (const item of items || []) {
    if (!item || (item.type !== "function_call_output" && item.type !== "custom_tool_call_output" && item.type !== "web_search_call_output")) continue;
    const callId = resolveToolCallId(item);
    if (callId) resolved.add(callId);
  }
  return resolved;
}

function collectToolCallNamesById(items) {
  const byId = new Map();
  for (const item of items || []) {
    if (!item || (item.type !== "function_call" && item.type !== "custom_tool_call" && item.type !== "web_search_call")) continue;
    const callId = resolveToolCallId(item);
    if (!callId) continue;
    const name = toolCallNameFromResponseItem(item);
    if (name) byId.set(callId, name);
  }
  return byId;
}

function toolCallNameFromResponseItem(item) {
  if (!item || typeof item !== "object") return "";
  if (item.name) return String(item.name);
  if (item.type === "web_search_call") return "web_search";
  if (item.function && item.function.name) return String(item.function.name);
  return "";
}

function buildConversation(requestBody, previousRecord, currentMessages) {
  let messages = [];
  if (requestBody.instructions) messages.push({ role: "system", content: requestBody.instructions });
  const previousMessages = previousRecord && Array.isArray(previousRecord.upstream_messages)
    ? budgetHistoryMessages(previousRecord.upstream_messages, { enforceProtocol: false })
    : [];
  messages = messages.concat(dedupeCurrentMessagesAlreadyInHistory(previousMessages, currentMessages));
  return enforceChatToolProtocol(messages);
}

function dedupeCurrentMessagesAlreadyInHistory(previousMessages, currentMessages) {
  const previous = Array.isArray(previousMessages) ? previousMessages : [];
  const current = Array.isArray(currentMessages) ? currentMessages : [];
  if (previous.length === 0 || current.length === 0) return previous.concat(current);

  const previousKeys = new Set(previous.map(messageSignature));
  let index = 0;
  while (index < current.length && previousKeys.has(messageSignature(current[index]))) index += 1;
  return previous.concat(current.slice(index));
}

function messageSignature(message) {
  if (!message || typeof message !== "object") return "";
  const role = normalizeRole(message.role);
  if (role === "assistant" && Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
    return role + "|calls|" + message.tool_calls.map((call) => [
      call && call.id,
      call && call.function ? call.function.name : "",
      call && call.function ? call.function.arguments : "",
    ].join(":")).join("|");
  }
  if (role === "tool") return role + "|" + (message.tool_call_id || "") + "|" + (message.content || "");
  return role + "|" + (message.content || "");
}

function normalizeAssistant(message) {
  if (!message || typeof message !== "object") return { role: "assistant", content: "" };
  const content = typeof message.content === "string" ? message.content : "";
  const extracted = extractTaggedThinking(content);
  const hasRealToolCalls = Array.isArray(message.tool_calls) && message.tool_calls.length > 0;
  const cleanedContent = sanitizeToolXmlInText(stripDsmlToolBlocks(extracted.content || ""), hasRealToolCalls);
  const result = { role: "assistant", content: cleanedContent };
  if (typeof message.reasoning_content === "string" && message.reasoning_content) result.reasoning_content = message.reasoning_content;
  else if (extracted.reasoning) result.reasoning_content = extracted.reasoning;
  if (hasRealToolCalls) result.tool_calls = message.tool_calls.map(normalizeChatToolCall);
  return result;
}

function normalizeChatToolCall(toolCall) {
  const fn = toolCall.function || {};
  return {
    id: toolCall.id || toolCall.call_id || makeId("call"),
    type: "function",
    function: {
      name: fn.name || toolCall.name || "",
      arguments: typeof fn.arguments === "string" ? fn.arguments : JSON.stringify(fn.arguments || toolCall.arguments || {}),
    },
  };
}

function responseOutputFromAssistant(rawAssistant, usage, toolContext, config, options = {}) {
  const output = [];
  const reasoning = rawAssistant.reasoning_content || "";
  let body = rawAssistant.content || "";
  const messagePhase = resolveAssistantMessagePhase(options);

  if (reasoning && config.visibleThinkingEnabled) {
    output.push(visibleReasoningItem(reasoning, config.thinkingTitle));
  }
  else if (reasoning) output.push(hiddenReasoningItem(reasoning));

  body = normalizeFileLinks(stripDsmlToolBlocks(body));
  if (body) {
    output.push(messageOutputItem(body, messagePhase));
  }

  for (const toolCall of rawAssistant.tool_calls || []) {
    const itemOrItems = toolContext.responseToolItemFromChat(toolCall);
    const items = Array.isArray(itemOrItems) ? itemOrItems : [itemOrItems];
    for (const item of items) {
      output.push(item);
    }
  }
  return output;
}

function visibleReasoningItem(reasoning, title) {
  const text = normalizeReasoningText(reasoning);
  return {
    id: makeId("rs"),
    type: "reasoning",
    status: "completed",
    summary: text ? [{ type: "summary_text", text, title: title || "DeepSeek Thinking" }] : [],
    encrypted_content: encodeHiddenReasoning(reasoning),
  };
}

function hiddenReasoningItem(reasoning) {
  return {
    id: makeId("rs"),
    type: "reasoning",
    status: "completed",
    summary: [],
    encrypted_content: encodeHiddenReasoning(reasoning),
  };
}

function messageOutputItem(text, phase) {
  const item = {
    id: makeId("msg"),
    type: "message",
    status: "completed",
    role: "assistant",
    content: [{ type: "output_text", text, annotations: [] }],
  };
  if (phase) item.phase = phase;
  return item;
}

function resolveAssistantMessagePhase(options = {}) {
  if (options.phase === "commentary" || options.phase === "final_answer") return options.phase;
  if (options.final === true) return "final_answer";
  if (options.final === false) return "commentary";
  return undefined;
}

function buildResponseRecord({ id, createdAt, model, output, usage, status = "completed", error = null }) {
  return {
    id,
    object: "response",
    created_at: createdAt,
    status,
    model,
    output,
    usage,
    error,
    incomplete_details: null,
    parallel_tool_calls: true,
  };
}

function buildStoredRecord({ id, createdAt, response, requestBody, previousRecord, normalizedInput, currentMessages, storedMessages, rawAssistant, conversationMessages }) {
  const previous = previousRecord && Array.isArray(previousRecord.upstream_messages) ? previousRecord.upstream_messages : [];
  const nextMessages = budgetHistoryMessages(previous.concat(currentMessages).concat(storedMessages || []), { preservePendingTail: true });
  return {
    id,
    created_at: createdAt,
    response: sanitizeResponseForStorage(response),
    request_model: requestBody.model || null,
    upstream_model: requestBody.model || null,
    previous_response_id: requestBody.previous_response_id || null,
    input_items: sanitizeInputItems(normalizedInput),
    upstream_messages: nextMessages,
    assistant_message: assistantForStorage(rawAssistant),
  };
}

function assistantForStorage(rawAssistant) {
  const normalized = normalizeAssistant(rawAssistant);
  const stored = { role: "assistant", content: normalized.content || "" };
  if (Array.isArray(normalized.tool_calls) && normalized.tool_calls.length > 0) {
    if (normalized.reasoning_content) stored.reasoning_content = normalized.reasoning_content;
    stored.tool_calls = normalized.tool_calls;
  }
  return stored;
}

function mergeAssistant(primary, followup) {
  if (!followup) return primary;
  const merged = {
    role: "assistant",
    content: [primary.content, followup.content].filter(Boolean).join("\n\n"),
    reasoning_content: [primary.reasoning_content, followup.reasoning_content].filter(Boolean).join("\n\n"),
  };
  const calls = [];
  if (Array.isArray(primary.tool_calls)) calls.push(...primary.tool_calls);
  if (Array.isArray(followup.tool_calls)) calls.push(...followup.tool_calls);
  if (calls.length > 0) merged.tool_calls = calls;
  return merged;
}

function budgetHistoryMessages(messages, options = {}) {
  const maxMessages = Number(options.maxMessages) || MAX_HISTORY_MESSAGES;
  const maxBytes = Number(options.maxBytes) || MAX_HISTORY_BYTES;
  const sanitized = sanitizeHistoryMessages(messages);
  const output = [];
  let totalBytes = 0;
  for (let index = sanitized.length - 1; index >= 0; index -= 1) {
    const message = sanitized[index];
    const bytes = byteLength(JSON.stringify(message));
    if (output.length >= maxMessages || (output.length > 0 && totalBytes + bytes > maxBytes)) break;
    output.unshift(message);
    totalBytes += bytes;
  }
  return options.enforceProtocol === false ? output : enforceChatToolProtocol(output, options);
}

function sanitizeHistoryMessages(messages) {
  const output = [];
  for (const message of Array.isArray(messages) ? messages : []) {
    const cleaned = sanitizeMessageForHistory(message);
    if (cleaned) output.push(cleaned);
  }
  return output;
}

function sanitizeMessageForHistory(message) {
  if (!message || typeof message !== "object") return null;
  const role = normalizeRole(message.role);
  const cleaned = { role };
  if (typeof message.content === "string") cleaned.content = truncateText(message.content, role === "tool" ? MAX_TOOL_OUTPUT_BYTES : MAX_MESSAGE_CONTENT_BYTES);
  else cleaned.content = truncateText(extractText(message.content), MAX_MESSAGE_CONTENT_BYTES);
  if (role === "assistant" && Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
    if (typeof message.reasoning_content === "string" && message.reasoning_content) cleaned.reasoning_content = truncateText(message.reasoning_content, MAX_MESSAGE_CONTENT_BYTES);
    cleaned.tool_calls = message.tool_calls.map(cleanToolCallForChat);
  }
  if (role === "tool" && message.tool_call_id) cleaned.tool_call_id = message.tool_call_id;
  if (!cleaned.content && !cleaned.tool_calls) return null;
  return cleaned;
}

function sanitizeToolContent(value) {
  return truncateText(repairMojibakeText(value), MAX_TOOL_OUTPUT_BYTES);
}

function sanitizeResponseForStorage(response) {
  if (!response || typeof response !== "object") return response;
  const stored = Object.assign({}, response);
  if (Array.isArray(stored.output)) stored.output = stored.output.map(sanitizeResponseItemForStorage).filter(Boolean);
  return stored;
}

function sanitizeResponseItemForStorage(item) {
  if (!item || typeof item !== "object") return null;
  if (item.type === "reasoning") return null;
  if (isDisplayOnlyItem(item) || isDisplayOnlyText(extractText(item.content))) return null;
  const copy = cloneJson(item);
  if (copy.type === "message" && Array.isArray(copy.content)) {
    copy.content = copy.content.map((part) => {
      if (!part || typeof part !== "object") return part;
      const next = Object.assign({}, part);
      if (typeof next.text === "string") next.text = truncateText(next.text, MAX_MESSAGE_CONTENT_BYTES);
      return next;
    });
  }
  return copy;
}

function sanitizeInputItems(items) {
  return (Array.isArray(items) ? items : []).slice(-MAX_STORED_INPUT_ITEMS).map((item) => {
    if (!item || typeof item !== "object") return item;
    if (item.type === "reasoning") return null;
    if (isDisplayOnlyItem(item) || isDisplayOnlyText(extractText(item.content))) return null;
    const copy = cloneJson(item);
    if (copy.content) copy.content = normalizeContentParts(copy.content).map((part) => {
      const next = Object.assign({}, part);
      if (typeof next.text === "string") next.text = truncateText(next.text, MAX_MESSAGE_CONTENT_BYTES);
      return next;
    });
    if (typeof copy.output === "string") copy.output = sanitizeToolContent(copy.output);
    return copy;
  }).filter(Boolean);
}

function truncateText(value, maxBytes) {
  const text = String(value || "");
  const limit = Math.max(1, Number(maxBytes) || 1);
  if (byteLength(text) <= limit) return text;
  let end = Math.min(text.length, limit);
  while (end > 0 && byteLength(text.slice(0, end)) > limit) end -= 1;
  return text.slice(0, end) + "\n[truncated: content exceeded " + limit + " bytes]";
}

function byteLength(value) {
  return Buffer.byteLength(String(value || ""), "utf8");
}

function estimateTokensForInput(input) {
  return Math.max(1, Math.ceil(extractTextFromInput(input).length / 4));
}

function extractTextFromInput(input) {
  if (typeof input === "string") return input;
  if (!Array.isArray(input)) return extractText(input && input.content);
  return input.map((item) => {
    if (typeof item === "string") return item;
    return extractText(item && item.content);
  }).join("\n");
}

function normalizeMessage(item) {
  const content = item.content !== undefined ? item.content : "";
  return {
    id: item.id || makeId("msg"),
    type: "message",
    status: item.status || "completed",
    role: normalizeRole(item.role || "user"),
    content: normalizeContentParts(content),
  };
}

function messageItem(role, text) {
  return { id: makeId("msg"), type: "message", status: "completed", role, content: [{ type: "input_text", text }] };
}

function normalizeContentParts(content) {
  if (typeof content === "string") return [{ type: "input_text", text: content }];
  if (!Array.isArray(content)) return [{ type: "input_text", text: String(content || "") }];
  return content.map((part) => {
    if (typeof part === "string") return { type: "input_text", text: part };
    if (!part || typeof part !== "object") return null;
    if (part.type === "input_text" || part.type === "output_text") return { type: part.type, text: part.text || "", annotations: part.annotations || undefined };
    if (part.type === "refusal") return { type: "output_text", text: part.refusal || "", annotations: [] };
    return null;
  }).filter(Boolean);
}

function normalizeRole(role) {
  if (role === "developer") return "system";
  if (["system", "user", "assistant", "tool"].includes(role)) return role;
  return "user";
}

function normalizeToolOutputText(item, options = {}) {
  const output = item.output !== undefined ? item.output : item.results;
  const text = typeof output === "string" ? repairMojibakeText(output) : repairMojibakeText(JSON.stringify(output || ""));
  return annotateApplyPatchFailureForModel(text, item, options);
}

function annotateApplyPatchFailureForModel(text, item, options = {}) {
  if (!isApplyPatchOutput(item, options)) return text;
  if (!looksLikeApplyPatchFailure(text)) return text;
  if (/CodeSeeX note: apply_patch failed/i.test(text)) return text;
  return String(text || "").trimEnd() + "\n\n" + [
    "CodeSeeX note: apply_patch failed because the patch did not match the current file contents or patch format.",
    "Re-read the target file before retrying, then submit a smaller patch with exact current context lines.",
    "Do not reuse remembered context.",
  ].join("\n");
}

function isApplyPatchOutput(item, options = {}) {
  const name = String(options.toolName || item && item.name || "").trim();
  return name === "apply_patch";
}

function looksLikeApplyPatchFailure(text) {
  const value = String(text || "");
  if (/Success\. Updated|Done!/i.test(value)) return false;
  const failureSignal = /(Exit code:\s*[1-9]\d*|Failed|Error|Invalid|The patch format is wrong|Unknown Line|expected lines|No such file)/i;
  const patchSignal = /(patch|expected lines|Invalid Context|Add File|Update File|Delete File|Move to|\*\*\*)/i;
  return failureSignal.test(value) && patchSignal.test(value);
}

function resolveToolCallId(item) {
  if (!item || typeof item !== "object") return "";
  return item.call_id || item.tool_call_id || item.id || "";
}

function parseAssistantDisplay(text) {
  const cleaned = stripDsmlToolBlocks(String(text || "")).trim();
  if (!cleaned) return { content: "", reasoning_content: "" };
  if (cleaned === "---") return { content: "", reasoning_content: "" };
  if (isDisplayOnlyText(cleaned)) return { content: "", reasoning_content: "" };

  const withoutToolDisplays = stripProxyToolDisplayBlocks(cleaned);
  if (!withoutToolDisplays) return { content: "", reasoning_content: "" };

  const modern = parseModernAssistantDisplay(withoutToolDisplays);
  if (modern) return modern;

  const legacyThinkingMatch = withoutToolDisplays.match(/^\*\*[^\n]+\*\*\n\n((?:>.*(?:\n|$))+)(?:\n+([\s\S]*))?$/);
  if (!legacyThinkingMatch) return { content: stripDisplayOnlySeparators(withoutToolDisplays), reasoning_content: "" };

  const reasoning = legacyThinkingMatch[1]
    .split("\n")
    .map((line) => line.replace(/^>\s?/, ""))
    .map(stripLegacyThinkingLineStyle)
    .join("\n")
    .trim();

  return {
    content: stripDisplayOnlySeparators(String(legacyThinkingMatch[2] || "").trim()),
    reasoning_content: reasoning,
  };
}

function parseModernAssistantDisplay(cleaned) {
  const blocks = splitDisplayBlocks(cleaned);
  if (blocks.length === 0) return null;
  const first = blocks[0].trim();
  const lines = first.split("\n");
  if (lines.length < 2) return null;
  if (!/^(DeepSeek Thinking|Thinking)$/i.test(lines[0].trim())) return null;
  const reasoning = lines.slice(1).join("\n").trim();
  const content = blocks.slice(1)
    .filter((block) => !isProxyToolDisplayBlock(block))
    .join("\n\n")
    .trim();
  return {
    content: stripDisplayOnlySeparators(content),
    reasoning_content: reasoning,
  };
}

function splitDisplayBlocks(text) {
  return String(text || "")
    .split(/(?:^|\n)---(?:\n|$)/)
    .map((block) => block.trim())
    .filter(Boolean);
}

function isProxyToolDisplayBlock(block) {
  return isProxyToolDisplayText(block);
}

function isProxyToolDisplayText(block) {
  const firstLine = String(block || "").trim().split("\n")[0] || "";
  return /^(?:>\s*)?(?:\u5df2\u4f7f\u7528\u5de5\u5177|\u4f7f\u7528\u5de5\u5177|\u6d63\u8de8\u6564\u5bb8\u30e5\u53ff)\s*`[^`]+`/.test(firstLine);
}

function stripProxyToolDisplayBlocks(text) {
  return splitDisplayBlocks(text)
    .filter((block) => !isProxyToolDisplayText(block))
    .join("\n\n---\n\n")
    .trim();
}

function stripDisplayOnlySeparators(text) {
  return String(text || "")
    .replace(/(?:^|\n)---\s*$/g, "")
    .trim();
}

function stripLegacyThinkingLineStyle(line) {
  return String(line || "")
    .replace(/^<span\s+style=["']font-style:\s*normal;?["']>/i, "")
    .replace(/<\/span>$/i, "")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");
}

function stripAssistantDecorations(text) {
  return parseAssistantDisplay(text).content;
}

function isDisplayOnlyItem(item) {
  if (!item || typeof item !== "object") return false;
  if (item.codeseex_display_only) return true;
  const metadata = item.metadata && typeof item.metadata === "object" ? item.metadata : null;
  return Boolean(metadata && metadata.codeseex_display_only);
}

function isDisplayOnlyText(text) {
  const value = String(text || "").trim();
  if (!value) return false;
  if (THINKING_DISPLAY_ONLY_PATTERN.test(value)) return true;
  return /^---\s*\n(?:<!--\s*)?codeseex_display_only:\s*thinking_markdown[\s\S]*?\n---$/i.test(value)
    || /^---\s*\n\*\*[^*]+Thinking\*\*\s*\n(?:>\s*.*\n?)+---$/i.test(value);
}

function extractReasoningItem(item) {
  if (!item || typeof item !== "object") return "";
  const decoded = decodeHiddenReasoning(item.encrypted_content);
  if (decoded) return decoded;

  const summary = Array.isArray(item.summary) ? item.summary : [];
  const summaryText = summary.map((part) => {
    if (!part || typeof part !== "object") return "";
    if (typeof part.text === "string") return part.text;
    if (typeof part.summary_text === "string") return part.summary_text;
    return "";
  }).filter(Boolean).join("\n\n");
  if (summaryText) return summaryText.trim();

  if (Array.isArray(item.content)) {
    const contentText = item.content.map((part) => {
      if (!part || typeof part !== "object") return "";
      if (part.type === "summary_text" && typeof part.text === "string") return part.text;
      return "";
    }).filter(Boolean).join("\n\n");
    if (contentText) return contentText.trim();
  }

  if (typeof item.content === "string") return item.content.trim();
  if (Array.isArray(item.content)) return extractText(item.content).trim();
  return "";
}

function encodeHiddenReasoning(reasoning) {
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv("aes-256-gcm", hiddenReasoningKey(), iv);
  const encrypted = Buffer.concat([cipher.update(String(reasoning || ""), "utf8"), cipher.final()]);
  const tag = cipher.getAuthTag();
  return HIDDEN_REASONING_PREFIX + [iv, tag, encrypted].map(base64UrlEncode).join(".");
}

function decodeHiddenReasoning(value) {
  const raw = String(value || "");
  if (!raw.startsWith(HIDDEN_REASONING_PREFIX)) return "";
  const parts = raw.slice(HIDDEN_REASONING_PREFIX.length).split(".");
  if (parts.length === 1) {
    try {
      return base64UrlDecode(parts[0]).toString("utf8");
    } catch {
      return "";
    }
  }
  if (parts.length !== 3) return "";
  try {
    const iv = base64UrlDecode(parts[0]);
    const tag = base64UrlDecode(parts[1]);
    const encrypted = base64UrlDecode(parts[2]);
    const decipher = crypto.createDecipheriv("aes-256-gcm", hiddenReasoningKey(), iv);
    decipher.setAuthTag(tag);
    return Buffer.concat([decipher.update(encrypted), decipher.final()]).toString("utf8");
  } catch {
    return "";
  }
}

function hiddenReasoningKey() {
  return crypto.createHash("sha256").update("codeseex-hidden-reasoning-v1").digest();
}

function base64UrlEncode(buffer) {
  return Buffer.from(buffer).toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function base64UrlDecode(value) {
  const base64 = String(value || "").replace(/-/g, "+").replace(/_/g, "/");
  return Buffer.from(base64 + "=".repeat((4 - base64.length % 4) % 4), "base64");
}

module.exports = {
  assistantForStorage,
  buildConversation,
  buildResponseRecord,
  buildStoredRecord,
  estimateTokensForInput,
  inputToMessages,
  mergeAssistant,
  normalizeAssistant,
  normalizeChatToolCall,
  normalizeInput,
  responseOutputFromAssistant,
  sanitizeToolContent,
  budgetHistoryMessages,
};
