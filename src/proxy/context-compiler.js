const crypto = require("node:crypto");

const {
  enforceChatToolProtocol,
  inputToMessages,
} = require("./conversation");
const { extractText, sanitizeLargeBinaryText, toolOutputValueToText } = require("./text-utils");

const DEFAULT_CONTEXT_WINDOW = 1000000;
const DEFAULT_EFFECTIVE_CONTEXT_PERCENT = 90;
const DEFAULT_RESERVED_OUTPUT_TOKENS = 64000;
const DEFAULT_MAX_TOOL_OUTPUT_BYTES = 512 * 1024;
const STORAGE_MAX_TOOL_OUTPUT_BYTES = 24 * 1024;
const STORAGE_MAX_MESSAGE_BYTES = 96 * 1024;
const STORAGE_MAX_TOTAL_BYTES = 1536 * 1024;
const FACT_PRELUDE_PREFIX = "CodeSeeX verified conversation facts";
const COMPACTION_PREFIX = "CodeSeeX client compaction summaries";
const CODESEEX_COMPACTION_PREFIX = "codeseex-compaction-v1:";
const MAX_FACTS_IN_PRELUDE = 80;
const MAX_COMPACTIONS_IN_PRELUDE = 6;
const MAX_COMPACT_MESSAGES = 48;
const MAX_COMPACT_FACTS = 80;
const MAX_RETAINED_COMPACT_ITEMS = 80;
const MAX_RETAINED_COMPACT_BYTES = 512 * 1024;

function compileContext({ requestBody = {}, previousRecord = null, previousContext = null, normalizedInput = [], config = {} } = {}) {
  const started = Date.now();
  const budget = resolveContextBudget(requestBody, config);
  const previousState = normalizePreviousState(previousRecord, previousContext);
  const previousMessages = sanitizePreviousMessages(previousState.upstream_messages);
  const currentMessages = inputToMessages(normalizedInput, { maxToolOutputBytes: budget.maxToolOutputBytes });
  const baseMessages = [];
  if (requestBody.instructions) baseMessages.push({ role: "system", content: String(requestBody.instructions) });

  const compactPayloads = collectCodeseexCompactionPayloads(normalizedInput, config);
  const ledger = collectToolFacts({
    currentInput: normalizedInput,
    previousRecord: previousState,
  });
  const facts = mergeFactsForCompaction(
    compactPayloads.flatMap((payload) => payload.tool_facts || []),
    ledger.facts
  );
  const compactions = mergeCompactionSummaries(previousState.compactions, collectCompactionSummaries(normalizedInput, config));
  const conflicts = detectToolSelfDescriptionConflicts(previousMessages.concat(currentMessages), facts);
  const preludeMessages = buildPreludeMessages({
    facts: factsForImmediatePrelude(facts, {
      hasCompactionPayloads: compactPayloads.length > 0,
      conflicts,
    }),
    compactions,
    conflicts,
  });
  const historyMessages = dedupeCurrentMessagesAlreadyInHistory(previousMessages, currentMessages);
  const combinedMessages = baseMessages
    .concat(appendPreludeMessagesAfterHistory(historyMessages, preludeMessages));
  const protocolMessages = enforceDeepSeekThinkingToolProtocol(enforceChatToolProtocol(combinedMessages));
  const budgeted = budgetMessages(protocolMessages, budget, {
    facts,
    compactions,
    conflicts,
  });

  const diagnostic = buildCompilerDiagnostic({
    started,
    requestBody,
    previousMessages,
    currentMessages,
    protocolMessages,
    compiledMessages: budgeted.messages,
    facts,
    compactions,
    conflicts,
    budget,
    budgeted,
  });

  return {
    messages: budgeted.messages,
    currentMessages,
    toolFacts: facts,
    compactions,
    conflicts,
    budget,
    diagnostic,
  };
}

function buildStorageMessages(compiled, storedMessages = [], config = {}, options = {}) {
  const source = (compiled && Array.isArray(compiled.messages) ? compiled.messages : [])
    .concat(Array.isArray(storedMessages) ? storedMessages : []);
  const compacted = source
    .filter((message) => shouldStoreConversationMessage(message, options))
    .map((message) => compactMessageForStorage(message))
    .filter(Boolean);
  const budget = {
    maxBytes: Math.min(resolveContextBudget({}, config).maxBytes, STORAGE_MAX_TOTAL_BYTES),
    maxToolOutputBytes: STORAGE_MAX_TOOL_OUTPUT_BYTES,
  };
  return budgetMessages(compacted, budget, {
    facts: compiled && compiled.toolFacts,
    storage: true,
  }).messages;
}

function buildTurnStorageMessages(compiled, storedMessages = [], config = {}, options = {}) {
  const source = (compiled && Array.isArray(compiled.currentMessages) ? compiled.currentMessages : [])
    .concat(Array.isArray(storedMessages) ? storedMessages : []);
  const compacted = source
    .filter((message) => shouldStoreConversationMessage(message, options))
    .map((message) => compactMessageForStorage(message))
    .filter(Boolean);
  const budget = {
    maxBytes: Math.min(resolveContextBudget({}, config).maxBytes, STORAGE_MAX_TOTAL_BYTES),
    maxToolOutputBytes: STORAGE_MAX_TOOL_OUTPUT_BYTES,
  };
  return budgetMessages(compacted, budget, {
    facts: compiled && compiled.toolFacts,
    storage: true,
    turnStorage: true,
  }).messages;
}

function shouldStoreConversationMessage(message, options = {}) {
  if (!message || typeof message !== "object") return false;
  if (isContextPreludeMessage(message)) return false;
  if (message.role === "system" && options.requestInstructions && message.content === String(options.requestInstructions)) return false;
  return true;
}

function normalizePreviousState(previousRecord, previousContext) {
  const source = previousContext && typeof previousContext === "object" ? previousContext : previousRecord;
  if (!source || typeof source !== "object") {
    return { upstream_messages: [], tool_facts: [], compactions: [] };
  }
  return {
    upstream_messages: Array.isArray(source.upstream_messages) ? source.upstream_messages : [],
    tool_facts: Array.isArray(source.tool_facts) ? source.tool_facts : [],
    compactions: Array.isArray(source.compactions) ? source.compactions : [],
  };
}

function mergeToolFactsForStorage(compiled, response, storedMessages) {
  const facts = [];
  const seen = new Set();
  addFacts(compiled && compiled.toolFacts, "compiled");
  addFacts(collectToolFacts({ currentInput: response && response.output }).facts, "response");
  addFacts(collectToolFactsFromChatMessages(storedMessages), "stored_messages");
  return facts;

  function addFacts(list, fallbackSource) {
    for (const fact of Array.isArray(list) ? list : []) {
      if (!fact || typeof fact !== "object") continue;
      const next = Object.assign({}, fact);
      if (!next.source) next.source = fallbackSource;
      const key = factSignature(next);
      if (seen.has(key)) continue;
      seen.add(key);
      facts.push(next);
    }
  }
}

function mergeTurnToolFactsForStorage(normalizedInput, response, storedMessages) {
  const facts = [];
  const seen = new Set();
  addFacts(collectToolFacts({ currentInput: normalizedInput }).facts, "client_input");
  addFacts(collectToolFacts({ currentInput: response && response.output }).facts, "response");
  addFacts(collectToolFactsFromChatMessages(storedMessages), "stored_messages");
  return facts;

  function addFacts(list, fallbackSource) {
    for (const fact of Array.isArray(list) ? list : []) {
      if (!fact || typeof fact !== "object") continue;
      const next = Object.assign({}, fact);
      if (!next.source) next.source = fallbackSource;
      const key = factSignature(next);
      if (seen.has(key)) continue;
      seen.add(key);
      facts.push(next);
    }
  }
}

function collectToolFacts({ currentInput = [], previousRecord = null } = {}) {
  const facts = [];
  const seen = new Set();
  addStoredFacts(previousRecord && previousRecord.tool_facts);
  addResponseItems(currentInput, "client_input");
  return { facts, stats: { total: facts.length } };

  function addStoredFacts(list) {
    for (const fact of Array.isArray(list) ? list : []) {
      addFact(Object.assign({ source: "stored_response" }, fact));
    }
  }

  function addResponseItems(items, source) {
    const calls = new Map();
    const outputs = new Map();
    const order = [];
    for (const item of Array.isArray(items) ? items : []) {
      if (!item || typeof item !== "object") continue;
      if (isResponseToolCall(item)) {
        const callId = resolveToolCallId(item);
        if (!callId) continue;
        calls.set(callId, item);
        order.push(callId);
        continue;
      }
      if (isResponseToolOutput(item)) {
        const callId = resolveToolCallId(item);
        if (!callId) continue;
        outputs.set(callId, item);
        if (!order.includes(callId)) order.push(callId);
      }
    }

    for (const callId of order) {
      const call = calls.get(callId);
      const output = outputs.get(callId);
      addFact(toolFactFromResponsePair(callId, call, output, source));
    }
  }

  function addFact(fact) {
    if (!fact) return;
    const key = factSignature(fact);
    if (seen.has(key)) return;
    seen.add(key);
    facts.push(fact);
  }
}

function collectToolFactsFromChatMessages(messages) {
  const facts = [];
  const assistantCalls = new Map();
  for (const message of Array.isArray(messages) ? messages : []) {
    if (!message || typeof message !== "object") continue;
    if (message.role === "assistant" && Array.isArray(message.tool_calls)) {
      for (const call of message.tool_calls) {
        if (!call || !call.id) continue;
        assistantCalls.set(call.id, call);
      }
      continue;
    }
    if (message.role === "tool" && message.tool_call_id) {
      const call = assistantCalls.get(message.tool_call_id);
      facts.push(toolFactFromChatPair(message.tool_call_id, call, message, "stored_chat"));
    }
  }
  return facts;
}

function toolFactFromResponsePair(callId, call, output, source) {
  const name = toolNameFromResponseItem(call || output);
  const args = call ? responseToolArguments(call) : "";
  const outputText = output ? responseToolOutputText(output) : "";
  return compactFact({
    type: output ? "tool_pair_completed" : "tool_call_unresolved",
    source,
    call_id: callId,
    tool: name || "tool",
    status: output ? "completed" : "result_not_present_in_client_input",
    arguments: args,
    result: outputText,
    argument_hash: args ? hashText(args) : "",
    result_hash: outputText ? hashText(outputText) : "",
    result_bytes: outputText ? byteLength(outputText) : 0,
  });
}

function toolFactFromChatPair(callId, call, output, source) {
  const name = call && call.function ? call.function.name : "tool";
  const args = call && call.function ? call.function.arguments || "" : "";
  const outputText = output && output.content ? String(output.content) : "";
  return compactFact({
    type: "tool_pair_completed",
    source,
    call_id: callId,
    tool: name || "tool",
    status: "completed",
    arguments: args,
    result: outputText,
    argument_hash: args ? hashText(args) : "",
    result_hash: outputText ? hashText(outputText) : "",
    result_bytes: outputText ? byteLength(outputText) : 0,
  });
}

function compactFact(fact) {
  const output = {
    type: fact.type || "tool_event",
    source: fact.source || "",
    call_id: String(fact.call_id || "").slice(0, 120),
    tool: sanitizeFactText(fact.tool || "tool", 80),
    status: sanitizeFactText(fact.status || "", 120),
  };
  const argumentSummary = summarizeLargeText(fact.arguments || "", 900);
  if (argumentSummary.text) output.arguments = argumentSummary.text;
  if (fact.argument_hash) output.argument_hash = fact.argument_hash;
  const resultSummary = summarizeLargeText(fact.result || "", 1200);
  if (resultSummary.text) output.result = resultSummary.text;
  if (fact.result_hash) output.result_hash = fact.result_hash;
  if (fact.result_bytes) output.result_bytes = fact.result_bytes;
  return output;
}

function collectCompactionSummaries(items, config = {}) {
  const output = [];
  for (const item of Array.isArray(items) ? items : []) {
    if (!item || item.type !== "compaction") continue;
    const decoded = decodeCodeseexCompaction(item.encrypted_content, config);
    if (decoded) {
      const text = renderCodeseexCompactionText(decoded, { includeFacts: false });
      if (!text) continue;
      output.push({
        id: item.id || decoded.id || "",
        status: item.status || decoded.status || "",
        text: summarizeConversationMessageContent(text, 3200),
        hash: hashText(text),
        bytes: byteLength(text),
        source: "codeseex_compaction_payload",
      });
      continue;
    }
    const text = compactionText(item);
    if (!text) continue;
    output.push({
      id: item.id || "",
      status: item.status || "",
      text: summarizeLargeText(text, 1800).text,
      hash: hashText(text),
      bytes: byteLength(text),
      source: "plain_compaction_summary",
    });
  }
  return output;
}

function mergeCompactionSummaries(...lists) {
  const output = [];
  const seen = new Set();
  for (const list of lists) {
    for (const item of Array.isArray(list) ? list : []) {
      if (!item || typeof item !== "object") continue;
      const text = String(item.text || "");
      if (!text) continue;
      const key = item.hash || hashText(text);
      if (seen.has(key)) continue;
      seen.add(key);
      output.push(Object.assign({}, item, { hash: key }));
    }
  }
  return output;
}

function buildCodeseexCompaction({ requestBody = {}, previousRecord = null, previousContext = null, normalizedInput = [], compiledContext = null, config = {} } = {}) {
  const budget = resolveContextBudget(requestBody, config);
  const previousState = normalizePreviousState(previousRecord, previousContext);
  const previousMessages = sanitizePreviousMessages(previousState.upstream_messages);
  const currentMessages = compiledContext && Array.isArray(compiledContext.currentMessages)
    ? compiledContext.currentMessages
    : inputToMessages(normalizedInput, { maxToolOutputBytes: budget.maxToolOutputBytes });
  const carriedPayloads = collectCodeseexCompactionPayloads(normalizedInput, config);
  const sourceMessages = previousMessages.concat(currentMessages);
  const facts = mergeFactsForCompaction(
    carriedPayloads.flatMap((payload) => payload.tool_facts || []),
    previousState.tool_facts,
    compiledContext && compiledContext.toolFacts,
    collectToolFacts({ currentInput: normalizedInput, previousRecord: previousState }).facts,
    collectToolFactsFromChatMessages(sourceMessages)
  );
  const inheritedSummaries = mergeCompactionSummaries(previousState.compactions, collectCompactionSummaries(normalizedInput, config))
    .filter((item) => item.source === "plain_compaction_summary")
    .map((item) => item.text)
    .slice(-MAX_COMPACTIONS_IN_PRELUDE);
  const messages = mergeCompactedMessages(
    carriedPayloads.flatMap((payload) => payload.messages || []),
    compactMessagesForCompaction(sourceMessages)
  );
  const payload = {
    version: 1,
    id: "cmp_" + hashText(String(Date.now()) + ":" + safeJsonStringify([messages.length, facts.length])),
    status: "completed",
    created_at: new Date().toISOString(),
    model: requestBody && requestBody.model || "",
    purpose: "codeseex_deepseek_context_compaction",
    message_count: sourceMessages.length,
    retained_message_count: messages.length,
    tool_fact_count: facts.length,
    compaction_summaries: inheritedSummaries,
    messages,
    tool_facts: facts,
    notes: [
      "This is a CodeSeeX-readable compaction payload, not an OpenAI opaque server state.",
      "When tool facts conflict with assistant self-description, tool facts are authoritative.",
    ],
  };
  const text = renderCodeseexCompactionText(payload);
  return {
    payload,
    encrypted_content: encodeCodeseexCompaction(payload, config),
    text,
    summary: summarizeLargeText(text, 2200).text,
  };
}

function buildCodeseexCompactionWindow({ requestBody = {}, previousRecord = null, previousContext = null, normalizedInput = [], config = {} } = {}) {
  const partition = partitionCompactionInput(normalizedInput);
  const compact = buildCodeseexCompaction({
    requestBody,
    previousRecord,
    previousContext,
    normalizedInput: partition.compacted,
    config,
  });
  const compactionItem = {
    id: compact.payload.id,
    type: "compaction",
    status: "completed",
    encrypted_content: compact.encrypted_content,
  };
  return Object.assign({}, compact, {
    output: [compactionItem].concat(partition.retained),
    retainedItems: partition.retained,
    compactedItems: partition.compacted,
  });
}

function buildCodeseexCompactionItem({ requestBody = {}, previousRecord = null, previousContext = null, normalizedInput = [], compiledContext = null, config = {} } = {}) {
  const compact = buildCodeseexCompaction({ requestBody, previousRecord, previousContext, normalizedInput, compiledContext, config });
  return {
    compact,
    item: {
      id: compact.payload.id,
      type: "compaction",
      status: "completed",
      encrypted_content: compact.encrypted_content,
    },
  };
}

function encodeCodeseexCompaction(payload, config = {}) {
  const json = safeJsonStringify(payload || {});
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv("aes-256-gcm", resolveCompactionKey(config), iv);
  cipher.setAAD(Buffer.from(CODESEEX_COMPACTION_PREFIX, "utf8"));
  const encrypted = Buffer.concat([cipher.update(json, "utf8"), cipher.final()]);
  const tag = cipher.getAuthTag();
  return CODESEEX_COMPACTION_PREFIX + [iv, tag, encrypted].map(base64UrlEncode).join(".");
}

function decodeCodeseexCompaction(value, config = {}) {
  const raw = String(value || "");
  if (!raw.startsWith(CODESEEX_COMPACTION_PREFIX)) return null;
  const parts = raw.slice(CODESEEX_COMPACTION_PREFIX.length).split(".");
  if (parts.length !== 3) return null;
  try {
    const iv = base64UrlDecode(parts[0]);
    const tag = base64UrlDecode(parts[1]);
    const encrypted = base64UrlDecode(parts[2]);
    const decipher = crypto.createDecipheriv("aes-256-gcm", resolveCompactionKey(config), iv);
    decipher.setAAD(Buffer.from(CODESEEX_COMPACTION_PREFIX, "utf8"));
    decipher.setAuthTag(tag);
    const json = Buffer.concat([decipher.update(encrypted), decipher.final()]).toString("utf8");
    const parsed = JSON.parse(json);
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch {
    return null;
  }
}

function collectCodeseexCompactionPayloads(items, config = {}) {
  const payloads = [];
  for (const item of Array.isArray(items) ? items : []) {
    if (!item || item.type !== "compaction") continue;
    const payload = decodeCodeseexCompaction(item.encrypted_content, config);
    if (payload) payloads.push(payload);
  }
  return payloads;
}

function renderCodeseexCompactionText(payload, options = {}) {
  if (!payload || typeof payload !== "object") return "";
  const lines = [
    "CodeSeeX compacted conversation state.",
    "Purpose: preserve high-evidence context for DeepSeek in ordinary text messages.",
  ];
  if (payload.message_count !== undefined) lines.push("Original message count: " + Number(payload.message_count || 0));
  if (payload.retained_message_count !== undefined) lines.push("Retained compact message count: " + Number(payload.retained_message_count || 0));
  const facts = Array.isArray(payload.tool_facts) ? payload.tool_facts : [];
  if (options.includeFacts !== false && facts.length > 0) {
    lines.push("Verified tool facts:");
    for (const fact of facts.slice(-MAX_FACTS_IN_PRELUDE)) lines.push("- " + renderFactLine(fact));
  }
  const summaries = Array.isArray(payload.compaction_summaries) ? payload.compaction_summaries : [];
  if (summaries.length > 0) {
    lines.push("Earlier client compaction summaries:");
    for (const summary of summaries.slice(-MAX_COMPACTIONS_IN_PRELUDE)) lines.push("- " + oneLine(summary, 420));
  }
  const messages = Array.isArray(payload.messages) ? payload.messages : [];
  if (messages.length > 0) {
    lines.push("Recent compacted conversation:");
    for (const message of messages.slice(-MAX_COMPACT_MESSAGES)) {
      lines.push("- " + renderCompactedMessageLine(message));
    }
    lines.push("The compacted conversation above is historical context only; follow the latest user message for current output format and task instructions.");
  }
  return lines.join("\n");
}

function partitionCompactionInput(items) {
  const source = Array.isArray(items) ? items : [];
  const retained = [];
  const retainedIndexes = new Set();
  let retainedBytes = 0;
  for (let index = source.length - 1; index >= 0; index -= 1) {
    const item = source[index];
    if (!item || typeof item !== "object") continue;
    if (item.type === "compaction" || item.type === "reasoning") continue;
    const bytes = safeJsonByteLength(item);
    if (retained.length >= MAX_RETAINED_COMPACT_ITEMS || (retained.length > 0 && retainedBytes + bytes > MAX_RETAINED_COMPACT_BYTES)) break;
    retained.unshift(item);
    retainedIndexes.add(index);
    retainedBytes += bytes;
  }
  const compacted = source.filter((_item, index) => !retainedIndexes.has(index));
  return {
    compacted,
    retained,
    retainedBytes,
  };
}

function compactMessagesForCompaction(messages) {
  const source = Array.isArray(messages) ? messages : [];
  const output = [];
  for (const message of source.slice(-MAX_COMPACT_MESSAGES)) {
    if (!message || typeof message !== "object") continue;
    if (isContextPreludeMessage(message)) continue;
    const role = message.role || "message";
    const entry = { role };
    if (message.tool_call_id) entry.tool_call_id = String(message.tool_call_id).slice(0, 120);
    if (Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
      entry.tool_calls = message.tool_calls.map((call) => ({
        id: String(call && call.id || "").slice(0, 120),
        name: sanitizeFactText(call && call.function && call.function.name || "tool", 80),
        arguments: summarizeLargeText(call && call.function && call.function.arguments || "", 600).text,
      }));
    }
    if (typeof message.content === "string") entry.content = summarizeConversationMessageContent(message.content, role === "tool" ? 900 : 2400);
    else if (message.content !== undefined) entry.content = summarizeConversationMessageContent(extractText(message.content), 2400);
    if (entry.content || entry.tool_calls || entry.tool_call_id) output.push(entry);
  }
  return output;
}

function mergeCompactedMessages(...lists) {
  const output = [];
  const seen = new Set();
  for (const list of lists) {
    for (const message of Array.isArray(list) ? list : []) {
      if (!message || typeof message !== "object") continue;
      const key = hashText(safeJsonStringify(message));
      if (seen.has(key)) continue;
      seen.add(key);
      output.push(message);
    }
  }
  return output.slice(-MAX_COMPACT_MESSAGES);
}

function mergeFactsForCompaction(...lists) {
  const output = [];
  const seen = new Set();
  for (const list of lists) {
    for (const fact of Array.isArray(list) ? list : []) {
      if (!fact || typeof fact !== "object") continue;
      const compacted = compactFact(fact);
      const key = factSignature(compacted);
      if (seen.has(key)) continue;
      seen.add(key);
      output.push(compacted);
    }
  }
  return output.slice(-MAX_COMPACT_FACTS);
}

function renderCompactedMessageLine(message) {
  const parts = ["role=" + sanitizeFactText(message.role || "message", 40)];
  if (message.tool_call_id) parts.push("tool_call_id=" + sanitizeFactText(message.tool_call_id, 80));
  if (Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
    parts.push("tool_calls=" + message.tool_calls.map((call) => sanitizeFactText(call.name || "tool", 60)).join(","));
  }
  if (message.content) parts.push("content=" + renderCompactedMessageContent(message.content));
  return parts.join("; ");
}

function renderCompactedMessageContent(value) {
  const text = stripEphemeralReplyDirectives(String(value || ""));
  if (text.length <= 420) return oneLine(text, 420);
  const anchors = extractHighInformationLines(text)
    .map((line) => oneLine(line, 180))
    .filter(Boolean)
    .slice(-4);
  const parts = [
    oneLine(text, 220),
  ];
  if (anchors.length > 0) parts.push("anchors=" + anchors.join(" | "));
  parts.push("tail=" + oneLine(text.slice(-260), 180));
  return parts.join(" ... ");
}

function stripEphemeralReplyDirectives(value) {
  return String(value || "")
    .replace(/\b(?:reply|respond|answer)\s+(?:with\s+)?(?:only\s+)?OK\s+only\.?/gi, "[historical reply-format instruction omitted]")
    .replace(/\b(?:reply|respond|answer)\s+(?:with\s+)?(?:only\s+)?OK\.?/gi, "[historical reply-format instruction omitted]")
    .replace(/\b(?:reply|respond|answer)\s+(?:only\s+)?(?:the\s+)?(?:marker|id|token)\b\.?/gi, "[historical reply-format instruction omitted]")
    .replace(/(?:只|僅|仅)(?:回复|回答|返回)\s*(?:OK|marker|标记|標記|ID|token)[。.]?/gi, "[historical reply-format instruction omitted]");
}

function buildPreludeMessages({ facts, compactions, conflicts }) {
  const messages = [];
  const factList = Array.isArray(facts) ? facts : [];
  const compactList = Array.isArray(compactions) ? compactions : [];
  const conflictList = Array.isArray(conflicts) ? conflicts : [];

  if (factList.length > 0 || conflictList.length > 0) {
    const lines = [FACT_PRELUDE_PREFIX + " (proxy-verified event records)."];
    if (conflictList.length > 0) {
      lines.push("Some assistant self-descriptions conflict with these event records; the event records are the authoritative source for tool usage.");
    }
    for (const fact of factList.slice(-MAX_FACTS_IN_PRELUDE)) {
      lines.push("- " + renderFactLine(fact));
    }
    const hidden = Math.max(0, factList.length - MAX_FACTS_IN_PRELUDE);
    if (hidden > 0) lines.push("- " + hidden + " older tool fact(s) omitted from this compact prelude.");
    messages.push({ role: "user", content: lines.join("\n") });
  }

  if (compactList.length > 0) {
    const lines = [COMPACTION_PREFIX + " (lower evidence than verified tool facts)."];
    for (const item of compactList.slice(-MAX_COMPACTIONS_IN_PRELUDE)) {
      lines.push("- " + item.text);
    }
    const hidden = Math.max(0, compactList.length - MAX_COMPACTIONS_IN_PRELUDE);
    if (hidden > 0) lines.push("- " + hidden + " older compaction summary item(s) omitted.");
    messages.push({ role: "user", content: lines.join("\n") });
  }

  return messages;
}

function factsForImmediatePrelude(facts, options = {}) {
  const factList = Array.isArray(facts) ? facts : [];
  if (factList.length === 0) return [];
  if (options.hasCompactionPayloads) return factList;
  if (Array.isArray(options.conflicts) && options.conflicts.length > 0) return factList;
  return factList.filter((fact) => String(fact && fact.status || "") !== "completed");
}

function appendPreludeMessagesAfterHistory(messages, preludeMessages) {
  const base = (Array.isArray(messages) ? messages : []).filter(Boolean);
  const prelude = (Array.isArray(preludeMessages) ? preludeMessages : []).filter(Boolean);
  if (prelude.length === 0) return base;

  let insertAt = base.length;
  while (insertAt > 0 && isTrailingCurrentUserMessage(base[insertAt - 1])) insertAt -= 1;
  return base.slice(0, insertAt).concat(prelude, base.slice(insertAt));
}

function isTrailingCurrentUserMessage(message) {
  return Boolean(message && message.role === "user");
}

function renderFactLine(fact) {
  const entry = {
    tool_name: sanitizeFactText(fact.tool || "tool", 80),
    call_id: sanitizeFactText(fact.call_id || "", 80),
    status: sanitizeFactText(fact.status || "", 120),
    source: sanitizeFactText(fact.source || "", 80),
  };
  if (fact.arguments) entry.tool_arguments = oneLine(fact.arguments, 240);
  if (fact.result) entry.tool_result_text = oneLine(fact.result, 300);
  if (fact.result_hash) entry.tool_result_hash = fact.result_hash;
  if (fact.result_bytes) entry.tool_result_bytes = fact.result_bytes;
  return safeJsonStringify(entry);
}

function budgetMessages(messages, budget, metadata = {}) {
  const protocolMessages = enforceDeepSeekThinkingToolProtocol(enforceChatToolProtocol(messages));
  const maxBytes = Math.max(4096, Number(budget && budget.maxBytes) || 4096);
  const initialBytes = safeJsonByteLength(protocolMessages);
  if (initialBytes <= maxBytes) {
    return {
      messages: protocolMessages,
      initialBytes,
      finalBytes: initialBytes,
      estimatedTokens: estimateTokensFromBytes(initialBytes),
      compressed: false,
      droppedBlocks: 0,
      compactedMessages: 0,
    };
  }

  const compacted = protocolMessages.map((message) => compactMessageForBudget(message, budget));
  let compactedBytes = safeJsonByteLength(compacted);
  if (compactedBytes <= maxBytes) {
    return {
      messages: enforceDeepSeekThinkingToolProtocol(enforceChatToolProtocol(compacted)),
      initialBytes,
      finalBytes: compactedBytes,
      estimatedTokens: estimateTokensFromBytes(compactedBytes),
      compressed: true,
      droppedBlocks: 0,
      compactedMessages: countChangedMessages(protocolMessages, compacted),
    };
  }

  const selected = selectBudgetedBlocks(compacted, maxBytes, metadata);
  compactedBytes = safeJsonByteLength(selected.messages);
  return {
    messages: enforceDeepSeekThinkingToolProtocol(enforceChatToolProtocol(selected.messages)),
    initialBytes,
    finalBytes: compactedBytes,
    estimatedTokens: estimateTokensFromBytes(compactedBytes),
    compressed: true,
    droppedBlocks: selected.droppedBlocks,
    compactedMessages: countChangedMessages(protocolMessages, compacted),
  };
}

function selectBudgetedBlocks(messages, maxBytes, metadata = {}) {
  const protectedMessages = [];
  const blocks = [];
  for (const block of messageBlocks(messages)) {
    if (block.messages.some(isProtectedContextMessage)) protectedMessages.push(...block.messages);
    else blocks.push(block);
  }

  const selected = [];
  let totalBytes = safeJsonByteLength(protectedMessages);
  let droppedBlocks = 0;
  for (let index = blocks.length - 1; index >= 0; index -= 1) {
    const block = blocks[index];
    const bytes = safeJsonByteLength(block.messages);
    if (totalBytes + bytes <= maxBytes || selected.length === 0) {
      selected.unshift(...block.messages);
      totalBytes += bytes;
      continue;
    }
    droppedBlocks += 1;
  }

  const output = protectedMessages.concat(selected);
  if (metadata && droppedBlocks > 0 && Array.isArray(metadata.facts) && metadata.facts.length > 0 && !output.some(isContextPreludeMessage)) {
    output.splice(protectedMessages.length, 0, ...buildPreludeMessages({ facts: metadata.facts, compactions: [], conflicts: [] }));
  }
  if (output.length === 0 && metadata && Array.isArray(metadata.facts) && metadata.facts.length > 0) {
    output.push(...buildPreludeMessages({ facts: metadata.facts, compactions: [], conflicts: [] }));
  }

  return { messages: output, droppedBlocks };
}

function enforceDeepSeekThinkingToolProtocol(messages) {
  return (Array.isArray(messages) ? messages : []).map((message) => {
    if (!message || message.role !== "assistant") return message;
    if (!Array.isArray(message.tool_calls) || message.tool_calls.length === 0) return message;
    if (message.reasoning_content !== undefined && message.reasoning_content !== null) return message;
    return Object.assign({}, message, { reasoning_content: "" });
  });
}

function messageBlocks(messages) {
  const blocks = [];
  const source = Array.isArray(messages) ? messages : [];
  for (let index = 0; index < source.length; index += 1) {
    const message = source[index];
    if (!message || typeof message !== "object") continue;
    if (message.role === "assistant" && Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
      const expected = new Set(message.tool_calls.map((call) => call && call.id).filter(Boolean));
      const group = [message];
      while (index + 1 < source.length && source[index + 1] && source[index + 1].role === "tool" && expected.has(source[index + 1].tool_call_id)) {
        index += 1;
        group.push(source[index]);
      }
      blocks.push({ kind: "tool_group", messages: group });
      continue;
    }
    blocks.push({ kind: message.role || "message", messages: [message] });
  }
  return blocks;
}

function compactMessageForBudget(message, budget = {}) {
  if (!message || typeof message !== "object") return null;
  const copy = Object.assign({}, message);
  if (typeof copy.content === "string") {
    const limit = copy.role === "tool"
      ? Math.max(4096, Number(budget.maxToolOutputBytes) || DEFAULT_MAX_TOOL_OUTPUT_BYTES)
      : STORAGE_MAX_MESSAGE_BYTES * 2;
    copy.content = summarizeLargeText(copy.content, limit).text;
  }
  if (copy.role === "assistant" && typeof copy.reasoning_content === "string" && !Array.isArray(copy.tool_calls)) {
    delete copy.reasoning_content;
  }
  return copy;
}

function compactMessageForStorage(message) {
  if (!message || typeof message !== "object") return null;
  if (isContextPreludeMessage(message)) return message;
  const copy = Object.assign({}, message);
  if (typeof copy.content === "string") {
    copy.content = summarizeLargeText(copy.content, copy.role === "tool" ? STORAGE_MAX_TOOL_OUTPUT_BYTES : STORAGE_MAX_MESSAGE_BYTES).text;
  }
  if (copy.role === "assistant" && typeof copy.reasoning_content === "string" && !Array.isArray(copy.tool_calls)) {
    delete copy.reasoning_content;
  }
  return copy;
}

function sanitizePreviousMessages(messages) {
  return (Array.isArray(messages) ? messages : [])
    .filter((message) => message && typeof message === "object")
    .filter((message) => !isContextPreludeMessage(message));
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
  if (message.role === "assistant" && Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
    return "assistant|calls|" + message.tool_calls.map((call) => [
      call && call.id,
      call && call.function ? call.function.name : "",
      call && call.function ? call.function.arguments : "",
    ].join(":")).join("|");
  }
  if (message.role === "tool") return "tool|" + (message.tool_call_id || "") + "|" + (message.content || "");
  return String(message.role || "") + "|" + String(message.content || "");
}

function resolveContextBudget(requestBody = {}, config = {}) {
  const contextWindow = positiveInt(config.contextWindow, DEFAULT_CONTEXT_WINDOW);
  const effectivePercent = clamp(positiveInt(config.effectiveContextWindowPercent, DEFAULT_EFFECTIVE_CONTEXT_PERCENT), 10, 100);
  const effectiveWindow = Math.floor(contextWindow * effectivePercent / 100);
  const toolBytes = safeJsonByteLength(requestBody.tools || []);
  const toolTokens = estimateTokensFromBytes(toolBytes);
  const reservedOutputTokens = positiveInt(config.contextReservedOutputTokens, DEFAULT_RESERVED_OUTPUT_TOKENS);
  const targetTokens = Math.max(4096, effectiveWindow - reservedOutputTokens - toolTokens);
  return {
    mode: "high_fidelity",
    contextWindow,
    effectivePercent,
    effectiveWindow,
    reservedOutputTokens,
    toolTokens,
    maxTokens: targetTokens,
    maxBytes: targetTokens * 4,
    maxToolOutputBytes: positiveInt(config.contextMaxToolOutputBytes, DEFAULT_MAX_TOOL_OUTPUT_BYTES),
  };
}

function buildCompilerDiagnostic(details) {
  const {
    started,
    requestBody,
    previousMessages,
    currentMessages,
    protocolMessages,
    compiledMessages,
    facts,
    compactions,
    conflicts,
    budget,
    budgeted,
  } = details;
  const factList = Array.isArray(facts) ? facts : [];
  return {
    id: "ctxcompile_" + hashText(String(Date.now()) + ":" + Math.random()).slice(0, 12),
    at: new Date().toISOString(),
    model: requestBody && requestBody.model || "",
    mode: "high_fidelity",
    compile_ms: Math.max(0, Date.now() - started),
    previous_message_count: Array.isArray(previousMessages) ? previousMessages.length : 0,
    current_message_count: Array.isArray(currentMessages) ? currentMessages.length : 0,
    protocol_message_count: Array.isArray(protocolMessages) ? protocolMessages.length : 0,
    compiled_message_count: Array.isArray(compiledMessages) ? compiledMessages.length : 0,
    tool_fact_count: Array.isArray(factList) ? factList.length : 0,
    compaction_summary_count: Array.isArray(compactions) ? compactions.length : 0,
    conflict_count: Array.isArray(conflicts) ? conflicts.length : 0,
    compressed: Boolean(budgeted && budgeted.compressed),
    dropped_blocks: Number(budgeted && budgeted.droppedBlocks || 0),
    compacted_messages: Number(budgeted && budgeted.compactedMessages || 0),
    initial_json_bytes: Number(budgeted && budgeted.initialBytes || 0),
    final_json_bytes: Number(budgeted && budgeted.finalBytes || 0),
    estimated_tokens: Number(budgeted && budgeted.estimatedTokens || 0),
    budget: {
      mode: budget.mode,
      context_window: budget.contextWindow,
      effective_context_window_percent: budget.effectivePercent,
      effective_window: budget.effectiveWindow,
      reserved_output_tokens: budget.reservedOutputTokens,
      tool_definition_tokens: budget.toolTokens,
      max_tokens: budget.maxTokens,
      max_bytes: budget.maxBytes,
      max_tool_output_bytes: budget.maxToolOutputBytes,
    },
  };
}

function detectToolSelfDescriptionConflicts(messages, facts) {
  const factList = Array.isArray(facts) ? facts : [];
  if (factList.length === 0) return [];
  const hasSearch = factList.some((fact) => /^web_search/i.test(String(fact.tool || "")));
  const hasAnyTool = factList.some((fact) => String(fact.tool || ""));
  const conflicts = [];
  for (const message of Array.isArray(messages) ? messages : []) {
    if (!message || message.role !== "assistant") continue;
    const content = String(message.content || "");
    if ((hasSearch && /(?:did\s+not|didn't|haven't|have\s+not)\s+(?:use\s+)?(?:web\s*)?search/i.test(content))
      || (hasAnyTool && /(?:did\s+not|didn't|haven't|have\s+not)\s+(?:use|call|invoke|run)\s+(?:any\s+)?tools?/i.test(content))
      || /(?:没有|未|没)(?:进行|使用|调用)?(?:联网|网页|网络)?搜索/.test(content)) {
      conflicts.push({
        kind: "assistant_self_description_conflicts_with_tool_fact",
        tool: hasSearch ? "web_search" : "tool",
        message_hash: hashText(content),
      });
    }
  }
  return conflicts;
}

function isContextPreludeMessage(message) {
  const content = String(message && message.content || "");
  return message && (message.role === "system" || message.role === "user") && (
    content.startsWith(FACT_PRELUDE_PREFIX)
    || content.startsWith(COMPACTION_PREFIX)
  );
}

function isProtectedContextMessage(message) {
  return Boolean(message && (message.role === "system" || isContextPreludeMessage(message)));
}

function isResponseToolCall(item) {
  return item.type === "function_call"
    || item.type === "custom_tool_call"
    || item.type === "web_search_call"
    || item.type === "proxy_tool_call";
}

function isResponseToolOutput(item) {
  return item.type === "function_call_output"
    || item.type === "custom_tool_call_output"
    || item.type === "web_search_call_output";
}

function resolveToolCallId(item) {
  if (!item || typeof item !== "object") return "";
  return item.call_id || item.tool_call_id || item.id || "";
}

function toolNameFromResponseItem(item) {
  if (!item || typeof item !== "object") return "";
  if (item.name) return String(item.name);
  if (item.type === "web_search_call" || item.type === "web_search_call_output") return "web_search";
  if (item.type === "custom_tool_call" || item.type === "custom_tool_call_output") return "custom_tool";
  if (item.function && item.function.name) return String(item.function.name);
  if (item.namespace) return String(item.namespace);
  return "tool";
}

function responseToolArguments(item) {
  if (!item || typeof item !== "object") return "";
  if (typeof item.arguments === "string") return item.arguments;
  if (item.arguments !== undefined) return safeJsonStringify(item.arguments);
  if (item.action !== undefined) return safeJsonStringify(item.action);
  if (item.input !== undefined) return typeof item.input === "string" ? item.input : safeJsonStringify(item.input);
  return "";
}

function responseToolOutputText(item) {
  if (!item || typeof item !== "object") return "";
  if (item.output !== undefined) return toolOutputValueToText(item.output);
  if (item.results !== undefined) return toolOutputValueToText(item.results);
  if (item.content !== undefined) return extractText(item.content);
  return toolOutputValueToText(item);
}

function compactionText(item) {
  if (!item || typeof item !== "object") return "";
  if (typeof item.text === "string") return item.text;
  if (typeof item.summary === "string") return item.summary;
  if (Array.isArray(item.summary)) {
    const text = item.summary.map((part) => part && (part.text || part.summary_text || "")).filter(Boolean).join("\n\n");
    if (text) return text;
  }
  if (item.content !== undefined) return extractText(item.content);
  return "";
}

function summarizeLargeText(value, maxBytes) {
  const text = sanitizeSensitiveText(sanitizeLargeBinaryText(String(value || "")));
  const limit = Math.max(64, Number(maxBytes) || 64);
  const bytes = byteLength(text);
  if (bytes <= limit) return { text, truncated: false, bytes, hash: hashText(text) };
  const marker = "\n[truncated: original_bytes=" + bytes + " sha256=" + hashText(text) + "]";
  const headLimit = Math.max(32, Math.floor((limit - byteLength(marker)) * 0.65));
  const tailLimit = Math.max(16, limit - byteLength(marker) - headLimit);
  return {
    text: trimToBytes(text, headLimit) + marker + "\n" + trimToLastBytes(text, tailLimit),
    truncated: true,
    bytes,
    hash: hashText(text),
  };
}

function summarizeConversationMessageContent(value, maxBytes) {
  const text = sanitizeSensitiveText(sanitizeLargeBinaryText(String(value || "")));
  const limit = Math.max(128, Number(maxBytes) || 128);
  if (byteLength(text) <= limit) return text;

  const anchors = extractHighInformationLines(text)
    .filter((line) => line && line.length <= 500)
    .slice(-8);
  const anchorText = anchors.length > 0
    ? "\n[retained high-information lines]\n" + anchors.join("\n")
    : "";
  if (!anchorText) return summarizeLargeText(text, limit).text;

  const marker = "\n[truncated: original_bytes=" + byteLength(text) + " sha256=" + hashText(text) + "]";
  const remaining = limit - byteLength(marker) - byteLength(anchorText) - 2;
  if (remaining <= 128) return summarizeLargeText(text, limit).text;
  const headLimit = Math.max(64, Math.floor(remaining * 0.55));
  const tailLimit = Math.max(64, remaining - headLimit);
  return trimToBytes(text, headLimit) + marker + anchorText + "\n" + trimToLastBytes(text, tailLimit);
}

function extractHighInformationLines(value) {
  const lines = String(value || "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .split("\n")
    .flatMap((line) => splitLongLineForAnchors(line));
  return lines
    .map((line) => line.trim())
    .filter((line) => line.length >= 12)
    .filter((line) => hasHighInformationSignal(line));
}

function splitLongLineForAnchors(line) {
  const text = String(line || "");
  if (text.length <= 500) return [text];
  const parts = [];
  const pattern = /(?:marker|remember|exact|token|id|path|file|error|failed|result|output|call_id|sha256|CODESEEX|AUTO|[A-Z0-9_]{8,})[^.!?\n]{0,220}/gi;
  let match;
  while ((match = pattern.exec(text))) parts.push(match[0]);
  return parts.length > 0 ? parts : [text.slice(0, 240), text.slice(-240)];
}

function hasHighInformationSignal(line) {
  return /(?:marker|remember|exact|token|id|path|file|error|failed|result|output|call_id|sha256|CODESEEX|AUTO|[A-Z0-9_]{8,})/i.test(line);
}

function sanitizeSensitiveText(value) {
  return String(value || "")
    .replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer ********")
    .replace(/sk-[A-Za-z0-9]{12,}/g, "sk-********")
    .replace(/(["']?(?:api[_-]?key|authorization|token|secret|password)["']?\s*[:=]\s*["']?)[^"',\s}]+/gi, "$1********");
}

function sanitizeFactText(value, maxChars) {
  return oneLine(sanitizeSensitiveText(value), maxChars);
}

function oneLine(value, maxChars) {
  const text = String(value || "").replace(/\s+/g, " ").trim();
  const limit = Math.max(1, Number(maxChars) || 1);
  return text.length <= limit ? text : text.slice(0, limit - 3) + "...";
}

function trimToBytes(value, maxBytes) {
  const text = String(value || "");
  let end = Math.min(text.length, Math.max(0, Number(maxBytes) || 0));
  while (end > 0 && byteLength(text.slice(0, end)) > maxBytes) end -= 1;
  return text.slice(0, end);
}

function trimToLastBytes(value, maxBytes) {
  const text = String(value || "");
  let start = Math.max(0, text.length - Math.max(0, Number(maxBytes) || 0));
  while (start < text.length && byteLength(text.slice(start)) > maxBytes) start += 1;
  return text.slice(start);
}

function countChangedMessages(left, right) {
  const count = Math.min(left.length, right.length);
  let changed = Math.abs(left.length - right.length);
  for (let index = 0; index < count; index += 1) {
    if (safeJsonStringify(left[index]) !== safeJsonStringify(right[index])) changed += 1;
  }
  return changed;
}

function factSignature(fact) {
  return [
    fact.type || "",
    fact.source || "",
    fact.call_id || "",
    fact.tool || "",
    fact.status || "",
    fact.argument_hash || hashText(fact.arguments || ""),
    fact.result_hash || hashText(fact.result || ""),
  ].join("|");
}

function hashText(value) {
  return crypto.createHash("sha256").update(String(value || "")).digest("hex").slice(0, 16);
}

function byteLength(value) {
  return Buffer.byteLength(String(value || ""), "utf8");
}

function safeJsonByteLength(value) {
  return byteLength(safeJsonStringify(value));
}

function safeJsonStringify(value) {
  try {
    return JSON.stringify(value === undefined ? null : value);
  } catch {
    return String(value || "");
  }
}

function base64UrlEncode(buffer) {
  return Buffer.from(buffer).toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function base64UrlDecode(value) {
  let text = String(value || "").replace(/-/g, "+").replace(/_/g, "/");
  while (text.length % 4) text += "=";
  return Buffer.from(text, "base64");
}

function resolveCompactionKey(config = {}) {
  const secret = String(config.compactionSecret || "");
  if (!secret) throw new Error("CodeSeeX compact encryption key is not configured.");
  return crypto.createHash("sha256").update(secret, "utf8").digest();
}

function estimateTokensFromBytes(bytes) {
  return Math.max(0, Math.ceil((Number(bytes) || 0) / 4));
}

function positiveInt(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : fallback;
}

function clamp(value, min, max) {
  return Math.min(max, Math.max(min, Number(value) || min));
}

module.exports = {
  buildCodeseexCompaction,
  buildCodeseexCompactionItem,
  buildCodeseexCompactionWindow,
  buildStorageMessages,
  buildTurnStorageMessages,
  collectCompactionSummaries,
  collectToolFacts,
  compileContext,
  decodeCodeseexCompaction,
  encodeCodeseexCompaction,
  mergeToolFactsForStorage,
  mergeTurnToolFactsForStorage,
  resolveContextBudget,
};
