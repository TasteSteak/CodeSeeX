const crypto = require("node:crypto");

const { httpError } = require("../shared/http");

function resolvePreviousContext(id, context) {
  if (!id) return null;
  const leaf = resolveStoredResponse(id, context);
  const chain = collectResponseChain(id, context);
  const state = reconstructDeltaState(chain.records);
  return Object.assign({}, leaf, {
    upstream_messages: state.messages,
    tool_facts: state.toolFacts,
    compactions: state.compactions,
    chain_diagnostic: {
      requested_previous_response_id: id,
      leaf_response_id: leaf.id || id,
      record_count: chain.records.length,
      reconstructed_message_count: state.messages.length,
      reconstructed_json_bytes: safeJsonByteLength(state.messages),
      reconstructed_tool_fact_count: state.toolFacts.length,
      reconstructed_compaction_count: state.compactions.length,
      delta_record_count: state.deltaRecordCount,
      legacy_record_count: state.legacyRecordCount,
      status_counts: state.statusCounts,
      unsafe_message_count: state.unsafeMessageCount,
      missing_parent_id: chain.missingParentId,
      loop_detected: chain.loopDetected,
      truncated: chain.truncated,
    },
  });
}

function resolveStoredResponse(id, context) {
  const record = context && context.state && context.state.responses
    ? context.state.responses[id]
    : null;
  if (!record) throw httpError(404, "Response " + id + " was not found.", "invalid_request_error", "response_not_found");
  return record;
}

function collectResponseChain(id, context) {
  const records = [];
  const seen = new Set();
  const maxRecords = resolveMaxChainDepth(context);
  let currentId = id;
  let missingParentId = "";
  let loopDetected = false;
  let truncated = false;

  while (currentId) {
    if (seen.has(currentId)) {
      loopDetected = true;
      break;
    }
    seen.add(currentId);

    const record = context.state.responses[currentId];
    if (!record) {
      missingParentId = currentId;
      break;
    }
    records.push(record);

    if (Number.isFinite(maxRecords) && records.length >= maxRecords) {
      truncated = Boolean(record.previous_response_id);
      break;
    }
    currentId = record.previous_response_id || "";
  }

  records.reverse();
  return { records, missingParentId, loopDetected, truncated };
}

function resolveMaxChainDepth(context) {
  const configured = Number(context && context.config && context.config.maxResponseChainDepth);
  if (Number.isFinite(configured) && configured > 0) return Math.min(Math.floor(configured), 500000);
  return Infinity;
}

function reconstructDeltaState(records) {
  const messages = [];
  const toolFacts = [];
  const factSeen = new Set();
  const compactions = [];
  const compactionSeen = new Set();
  let deltaRecordCount = 0;
  let legacyRecordCount = 0;
  let unsafeMessageCount = 0;
  const statusCounts = {};

  for (const record of Array.isArray(records) ? records : []) {
    const status = normalizeRecordStatus(record && record.status);
    statusCounts[status] = Number(statusCounts[status] || 0) + 1;
    const rawTurnMessages = Array.isArray(record && record.turn_messages)
      ? record.turn_messages.filter(isChatMessageLike)
      : [];
    const turnMessages = status === "completed"
      ? rawTurnMessages
      : rawTurnMessages.filter(isSafeIncompleteMessage);
    unsafeMessageCount += Math.max(0, rawTurnMessages.length - turnMessages.length);
    if (turnMessages.length > 0) {
      messages.push(...turnMessages);
      deltaRecordCount += 1;
    } else if (Array.isArray(record && record.upstream_messages)) {
      const rawLegacyMessages = record.upstream_messages.filter(isChatMessageLike);
      const legacyMessages = status === "completed"
        ? rawLegacyMessages
        : rawLegacyMessages.filter(isSafeIncompleteMessage);
      unsafeMessageCount += Math.max(0, rawLegacyMessages.length - legacyMessages.length);
      messages.splice(0, messages.length, ...legacyMessages);
      legacyRecordCount += 1;
    }

    addUniqueFacts(toolFacts, factSeen, record && record.turn_tool_facts);
    addUniqueFacts(toolFacts, factSeen, record && record.tool_facts);
    addUniqueCompactions(compactions, compactionSeen, record && record.compactions);
  }

  return { messages, toolFacts, compactions, deltaRecordCount, legacyRecordCount, statusCounts, unsafeMessageCount };
}

function isChatMessageLike(message) {
  return Boolean(message && typeof message === "object" && typeof message.role === "string");
}

function isSafeIncompleteMessage(message) {
  if (!isChatMessageLike(message)) return false;
  return message.role === "user" || message.role === "system";
}

function normalizeRecordStatus(status) {
  const value = String(status || "completed").trim().toLowerCase();
  if (value === "in_progress" || value === "failed" || value === "interrupted" || value === "completed") return value;
  return "completed";
}

function addUniqueFacts(output, seen, facts) {
  for (const fact of Array.isArray(facts) ? facts : []) {
    if (!fact || typeof fact !== "object") continue;
    const key = [
      fact.type || "",
      fact.call_id || "",
      fact.tool || "",
      fact.status || "",
      fact.argument_hash || hashValue(fact.arguments || ""),
      fact.result_hash || hashValue(fact.result || ""),
    ].join("|");
    if (seen.has(key)) continue;
    seen.add(key);
    output.push(fact);
  }
}

function addUniqueCompactions(output, seen, compactions) {
  for (const item of Array.isArray(compactions) ? compactions : []) {
    if (!item || typeof item !== "object") continue;
    const text = String(item.text || "");
    if (!text) continue;
    const key = item.hash || hashValue(text);
    if (seen.has(key)) continue;
    seen.add(key);
    output.push(item);
  }
}

function hashValue(value) {
  return crypto.createHash("sha256").update(String(value || "")).digest("hex").slice(0, 16);
}

function safeJsonByteLength(value) {
  try {
    return Buffer.byteLength(JSON.stringify(value || null), "utf8");
  } catch {
    return 0;
  }
}

module.exports = {
  collectResponseChain,
  reconstructDeltaState,
  resolvePreviousContext,
};
