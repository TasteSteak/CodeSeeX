const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { buildResponseRecord, buildStoredRecord, normalizeInput } = require("../src/proxy/conversation");
const { compileContext } = require("../src/proxy/context-compiler");
const { resolvePreviousContext } = require("../src/proxy/conversation-state");
const { createProxyContext } = require("../src/proxy/server");
const { normalizeMaxResponseChainDepth } = require("../src/shared/config");
const { readJsonStrict } = require("../src/shared/json-store");

function run() {
  testMaxResponseChainDepthDefaultsToSafetyCap();
  testStrictJsonReadRejectsInvalidState();
  testCreateProxyContextDoesNotOverwriteInvalidState();
  testCreateProxyContextRecoversInterruptedRequests();
  testStoredRecordLifecycleShape();
  testIncompleteRecordsOnlyContributeSafeFacts();
  testLegacyUpstreamMessagesStillReconstruct();
  testLegacyCumulativeRecordsDoNotDuplicateHistory();
  testLoopedResponseChainStopsSafely();
  testSelfLoopedResponseChainStopsSafely();
  testChainDepthIsIndependentFromStorageSoftLimit();
  testDefaultChainDepthHasSafetyCap();
  testOptionalChainDepthCapStillWorks();
  console.log("agent state tests passed");
}

function testMaxResponseChainDepthDefaultsToSafetyCap() {
  assert.equal(normalizeMaxResponseChainDepth(undefined), 10000);
  assert.equal(normalizeMaxResponseChainDepth(""), 10000);
  assert.equal(normalizeMaxResponseChainDepth("0"), 0);
  assert.equal(normalizeMaxResponseChainDepth("unlimited"), 0);
  assert.equal(normalizeMaxResponseChainDepth("25"), 100);
  assert.equal(normalizeMaxResponseChainDepth("500001"), 500000);
}

function testStrictJsonReadRejectsInvalidState() {
  const dir = makeTempDir();
  const file = path.join(dir, "proxy-state.json");
  fs.writeFileSync(file, "{", "utf8");
  assert.throws(() => readJsonStrict(file, { responses: {} }), /JSON file is invalid/);
  assert.equal(fs.readFileSync(file, "utf8"), "{");
}

function testCreateProxyContextDoesNotOverwriteInvalidState() {
  const dir = makeTempDir();
  const stateFile = path.join(dir, "proxy-state.json");
  const runtimeFile = path.join(dir, "runtime.json");
  fs.writeFileSync(stateFile, "{", "utf8");

  assert.throws(() => createProxyContext(minimalConfig(dir, stateFile, runtimeFile)), /JSON file is invalid/);
  assert.equal(fs.readFileSync(stateFile, "utf8"), "{");

  const runtime = JSON.parse(fs.readFileSync(runtimeFile, "utf8"));
  assert.equal(runtime.status, "error");
  assert.equal(runtime.error.code, "STATE_FILE_INVALID");
}

function testCreateProxyContextRecoversInterruptedRequests() {
  const dir = makeTempDir();
  const stateFile = path.join(dir, "proxy-state.json");
  const runtimeFile = path.join(dir, "runtime.json");
  fs.writeFileSync(stateFile, JSON.stringify({
    responses: {
      resp_in_progress: {
        id: "resp_in_progress",
        status: "in_progress",
        created_at: 1,
        response: { id: "resp_in_progress", status: "in_progress", output: [] },
        turn_messages: [
          { role: "user", content: "survives as safe input" },
          { role: "assistant", content: "partial text must stay unsafe" },
        ],
        turn_tool_facts: [
          { type: "tool_pair_completed", tool: "web_search", call_id: "call_recovered", status: "completed", result: "RECOVERED_FACT" },
        ],
      },
      resp_completed: {
        id: "resp_completed",
        status: "completed",
        created_at: 2,
        response: { id: "resp_completed", status: "completed", output: [] },
        turn_messages: [{ role: "user", content: "done" }],
        turn_tool_facts: [],
      },
    },
  }, null, 2), "utf8");

  const context = createProxyContext(minimalConfig(dir, stateFile, runtimeFile));
  assert.equal(context.state.responses.resp_in_progress.status, "interrupted");
  assert.equal(context.state.responses.resp_in_progress.response.status, "interrupted");
  assert.equal(context.state.responses.resp_completed.status, "completed");

  const saved = JSON.parse(fs.readFileSync(stateFile, "utf8"));
  assert.equal(saved.responses.resp_in_progress.status, "interrupted");
  assert.equal(saved.responses.resp_in_progress.response.error.code, "request_interrupted");
}

function testStoredRecordLifecycleShape() {
  const input = normalizeInput("remember lifecycle checkpoint");
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });
  const response = buildResponseRecord({
    id: "resp_lifecycle",
    createdAt: 1,
    model: "deepseek-v4-pro",
    output: [],
    usage: null,
    status: "in_progress",
  });
  const record = buildStoredRecord({
    id: "resp_lifecycle",
    createdAt: 1,
    startedAt: "2026-05-25T00:00:00.000Z",
    status: "in_progress",
    response,
    requestBody: { model: "deepseek-v4-pro" },
    normalizedInput: input,
    currentMessages: compiled.currentMessages,
    storedMessages: [],
    rawAssistant: { role: "assistant", content: "" },
    turnMessages: compiled.currentMessages,
    toolFacts: [],
    contextDiagnostic: compiled.diagnostic,
  });

  assert.equal(record.status, "in_progress");
  assert.equal(record.started_at, "2026-05-25T00:00:00.000Z");
  assert.ok(record.updated_at);
  assert.ok(Array.isArray(record.turn_messages));
  assert.equal(record.upstream_messages, undefined);
}

function testIncompleteRecordsOnlyContributeSafeFacts() {
  const context = { config: { maxStoredResponses: 100 }, state: { responses: {} } };
  context.state.responses.resp_completed = {
    id: "resp_completed",
    status: "completed",
    created_at: 1,
    previous_response_id: null,
    turn_messages: [
      { role: "user", content: "completed user" },
      { role: "assistant", content: "completed assistant marker" },
    ],
    turn_tool_facts: [],
  };
  context.state.responses.resp_failed = {
    id: "resp_failed",
    status: "failed",
    created_at: 2,
    previous_response_id: "resp_completed",
    turn_messages: [
      { role: "user", content: "failed user survives" },
      { role: "assistant", content: "PARTIAL_ASSISTANT_MUST_NOT_SURVIVE" },
      { role: "tool", tool_call_id: "call_state", content: "raw tool protocol should not be replayed without assistant call" },
    ],
    turn_tool_facts: [
      {
        type: "tool_pair_completed",
        call_id: "call_state",
        tool: "web_search",
        status: "completed",
        result: "VERIFIED_TOOL_FACT_SURVIVES",
      },
    ],
  };
  context.state.responses.resp_leaf = {
    id: "resp_leaf",
    status: "completed",
    created_at: 3,
    previous_response_id: "resp_failed",
    turn_messages: [
      { role: "user", content: "leaf user" },
      { role: "assistant", content: "leaf assistant" },
    ],
    turn_tool_facts: [],
  };

  const previous = resolvePreviousContext("resp_leaf", context);
  const text = JSON.stringify(previous.upstream_messages);
  assert.ok(text.includes("completed assistant marker"));
  assert.ok(text.includes("failed user survives"));
  assert.ok(text.includes("leaf assistant"));
  assert.ok(!text.includes("PARTIAL_ASSISTANT_MUST_NOT_SURVIVE"));
  assert.ok(JSON.stringify(previous.tool_facts).includes("VERIFIED_TOOL_FACT_SURVIVES"));
  assert.equal(previous.chain_diagnostic.status_counts.failed, 1);
  assert.equal(previous.chain_diagnostic.unsafe_message_count, 2);
}

function testLegacyUpstreamMessagesStillReconstruct() {
  const context = { config: { maxStoredResponses: 100 }, state: { responses: {} } };
  context.state.responses.resp_legacy = {
    id: "resp_legacy",
    created_at: 1,
    previous_response_id: null,
    upstream_messages: [
      { role: "user", content: "LEGACY_USER_MARKER" },
      { role: "assistant", content: "LEGACY_ASSISTANT_MARKER" },
    ],
    tool_facts: [
      { type: "tool_pair_completed", tool: "shell_command", call_id: "call_legacy", status: "completed", result: "LEGACY_TOOL_FACT" },
    ],
  };
  context.state.responses.resp_after_legacy = {
    id: "resp_after_legacy",
    status: "completed",
    created_at: 2,
    previous_response_id: "resp_legacy",
    turn_messages: [
      { role: "user", content: "DELTA_AFTER_LEGACY" },
    ],
    turn_tool_facts: [],
  };

  const previous = resolvePreviousContext("resp_after_legacy", context);
  const text = JSON.stringify(previous.upstream_messages);
  assert.ok(text.includes("LEGACY_USER_MARKER"));
  assert.ok(text.includes("LEGACY_ASSISTANT_MARKER"));
  assert.ok(text.includes("DELTA_AFTER_LEGACY"));
  assert.ok(JSON.stringify(previous.tool_facts).includes("LEGACY_TOOL_FACT"));
  assert.equal(previous.chain_diagnostic.legacy_record_count, 1);
}

function testLegacyCumulativeRecordsDoNotDuplicateHistory() {
  const context = { config: { maxStoredResponses: 100 }, state: { responses: {} } };
  context.state.responses.resp_legacy_1 = {
    id: "resp_legacy_1",
    created_at: 1,
    previous_response_id: null,
    upstream_messages: [
      { role: "user", content: "LEGACY_CUMULATIVE_ROOT" },
      { role: "assistant", content: "legacy root answer" },
    ],
  };
  context.state.responses.resp_legacy_2 = {
    id: "resp_legacy_2",
    created_at: 2,
    previous_response_id: "resp_legacy_1",
    upstream_messages: [
      { role: "user", content: "LEGACY_CUMULATIVE_ROOT" },
      { role: "assistant", content: "legacy root answer" },
      { role: "user", content: "LEGACY_CUMULATIVE_LEAF" },
    ],
  };

  const previous = resolvePreviousContext("resp_legacy_2", context);
  const text = JSON.stringify(previous.upstream_messages);
  assert.equal((text.match(/LEGACY_CUMULATIVE_ROOT/g) || []).length, 1);
  assert.ok(text.includes("LEGACY_CUMULATIVE_LEAF"));
  assert.equal(previous.chain_diagnostic.legacy_record_count, 2);
}

function testLoopedResponseChainStopsSafely() {
  const context = { config: { maxStoredResponses: 100 }, state: { responses: {} } };
  context.state.responses.resp_loop_a = {
    id: "resp_loop_a",
    status: "completed",
    created_at: 1,
    previous_response_id: "resp_loop_b",
    turn_messages: [{ role: "user", content: "LOOP_A_SURVIVES_ONCE" }],
    turn_tool_facts: [],
  };
  context.state.responses.resp_loop_b = {
    id: "resp_loop_b",
    status: "completed",
    created_at: 2,
    previous_response_id: "resp_loop_a",
    turn_messages: [{ role: "user", content: "LOOP_B_SURVIVES_ONCE" }],
    turn_tool_facts: [],
  };

  const previous = resolvePreviousContext("resp_loop_a", context);
  const text = JSON.stringify(previous.upstream_messages);
  assert.equal(previous.chain_diagnostic.loop_detected, true);
  assert.equal(previous.chain_diagnostic.record_count, 2);
  assert.equal((text.match(/LOOP_A_SURVIVES_ONCE/g) || []).length, 1);
  assert.equal((text.match(/LOOP_B_SURVIVES_ONCE/g) || []).length, 1);
}

function testSelfLoopedResponseChainStopsSafely() {
  const context = { config: { maxStoredResponses: 100 }, state: { responses: {} } };
  context.state.responses.resp_self_loop = {
    id: "resp_self_loop",
    status: "completed",
    created_at: 1,
    previous_response_id: "resp_self_loop",
    turn_messages: [{ role: "user", content: "SELF_LOOP_SURVIVES_ONCE" }],
    turn_tool_facts: [],
  };

  const previous = resolvePreviousContext("resp_self_loop", context);
  const text = JSON.stringify(previous.upstream_messages);
  assert.equal(previous.chain_diagnostic.loop_detected, true);
  assert.equal(previous.chain_diagnostic.record_count, 1);
  assert.equal((text.match(/SELF_LOOP_SURVIVES_ONCE/g) || []).length, 1);
}

function testChainDepthIsIndependentFromStorageSoftLimit() {
  const context = { config: { maxStoredResponses: 10 }, state: { responses: {} } };
  let previousId = null;
  for (let index = 0; index < 140; index += 1) {
    const id = "resp_long_chain_" + index;
    context.state.responses[id] = {
      id,
      status: "completed",
      created_at: index,
      previous_response_id: previousId,
      turn_messages: [
        { role: "user", content: "chain user " + index },
        { role: "assistant", content: "chain assistant " + index },
      ],
      turn_tool_facts: [],
    };
    previousId = id;
  }

  const previous = resolvePreviousContext(previousId, context);
  assert.equal(previous.chain_diagnostic.record_count, 140);
  assert.equal(previous.chain_diagnostic.truncated, false);
  assert.ok(JSON.stringify(previous.upstream_messages).includes("chain user 0"));
}

function testDefaultChainDepthHasSafetyCap() {
  const context = { config: { maxStoredResponses: 20000, maxResponseChainDepth: 10000 }, state: { responses: {} } };
  let previousId = null;
  for (let index = 0; index < 10025; index += 1) {
    const id = "resp_default_cap_" + index;
    context.state.responses[id] = {
      id,
      status: "completed",
      created_at: index,
      previous_response_id: previousId,
      turn_messages: [{ role: "user", content: "default capped chain user " + index }],
      turn_tool_facts: [],
    };
    previousId = id;
  }

  const previous = resolvePreviousContext(previousId, context);
  assert.equal(previous.chain_diagnostic.record_count, 10000);
  assert.equal(previous.chain_diagnostic.truncated, true);
  assert.ok(JSON.stringify(previous.upstream_messages).includes("default capped chain user 10024"));
  assert.ok(!JSON.stringify(previous.upstream_messages).includes("default capped chain user 0"));
}

function testOptionalChainDepthCapStillWorks() {
  const context = { config: { maxStoredResponses: 500, maxResponseChainDepth: 25 }, state: { responses: {} } };
  let previousId = null;
  for (let index = 0; index < 40; index += 1) {
    const id = "resp_capped_chain_" + index;
    context.state.responses[id] = {
      id,
      status: "completed",
      created_at: index,
      previous_response_id: previousId,
      turn_messages: [{ role: "user", content: "capped chain user " + index }],
      turn_tool_facts: [],
    };
    previousId = id;
  }

  const previous = resolvePreviousContext(previousId, context);
  assert.equal(previous.chain_diagnostic.record_count, 25);
  assert.equal(previous.chain_diagnostic.truncated, true);
  assert.ok(!JSON.stringify(previous.upstream_messages).includes("capped chain user 0"));
}

function minimalConfig(dir, stateFile, runtimeFile) {
  return {
    rootDir: dir,
    dataDir: dir,
    host: "127.0.0.1",
    port: 8787,
    deepseekBaseUrl: "https://api.deepseek.com",
    stateFile,
    runtimeFile,
    eventLogFile: path.join(dir, "events.jsonl"),
    logRetentionDays: 7,
    maxStoredResponses: 100,
  };
}

function makeTempDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "codeseex-agent-state-"));
}

run();
