const assert = require("node:assert/strict");

const { buildDeepSeekPayload, resolveChatCompletionsUrl } = require("../src/proxy/deepseek-client");
const { buildCodeseexCompaction, buildStorageMessages, buildTurnStorageMessages, compileContext, decodeCodeseexCompaction } = require("../src/proxy/context-compiler");
const { buildStoredRecord, normalizeInput, responseOutputFromAssistant } = require("../src/proxy/conversation");
const { resolvePreviousContext } = require("../src/proxy/conversation-state");
const { maybeBuildAutomaticCompaction, resolveCompactThreshold, sanitizeDebugValue } = require("../src/proxy/server");

function run() {
  testUnresolvedWebSearchFactSurvives();
  testLongHistoryNoFixedSixtyMessageCutoff();
  testToolProtocolValidityForIncompleteCalls();
  testStorageCompactsLargeToolOutput();
  testStorageDoesNotPersistContextPrelude();
  testStoredRecordUsesDeltaSchemaOnly();
  testTurnStorageDoesNotDuplicatePreviousHistory();
  testResponseChainReconstructsDeltaHistory();
  testCompactionIsModelVisible();
  testCodeseexCompactionPayloadIsEffective();
  testCodeseexCompactionDoesNotRecursivelySummarizeItself();
  testAutomaticCompactionThreshold();
  testAutomaticCompactionKeepsHighInformationAnchors();
  testAutomaticCompactionOmitsEphemeralReplyDirectives();
  testTinyBudgetKeepsFactsFirst();
  testToolFactBeatsAssistantSelfDescription();
  testToolHistoryKeepsDeepSeekThinkingField();
  testCompletedToolPairDoesNotInjectDynamicPrelude();
  testTypedImageToolOutputIsStructurallyOmitted();
  testPlainJsonArrayWithImageUrlKeepsNonTypedFields();
  testLargeBase64ToolOutputIsOmitted();
  testLargeBase64MessageContentIsOmitted();
  testContextDiagnosticRedactsSecrets();
  testThinkingModeAndVisibilityAreSeparate();
  testTemperaturePresetMapping();
  testDeepSeekChatCompletionsUrlCompatibility();
  console.log("context compiler tests passed");
}

function testUnresolvedWebSearchFactSurvives() {
  const input = normalizeInput([
    { type: "message", role: "user", content: "查询今天价格，必要时联网搜索。" },
    { type: "web_search_call", id: "ws_1", call_id: "call_ws_1", action: { query: "DeepSeek pricing today" }, status: "completed" },
    { type: "message", role: "assistant", content: "我没有搜索，只是根据记忆回答。" },
  ]);

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });

  assert.equal(compiled.toolFacts.length, 1);
  assert.equal(compiled.toolFacts[0].tool, "web_search");
  assert.equal(compiled.toolFacts[0].status, "result_not_present_in_client_input");
  assert.ok(compiled.conflicts.length >= 1);
  assert.ok(JSON.stringify(compiled.messages).includes("CodeSeeX verified conversation facts"));
  assert.ok(JSON.stringify(compiled.messages).includes("web_search"));
}

function testLongHistoryNoFixedSixtyMessageCutoff() {
  const input = [];
  for (let index = 0; index < 140; index += 1) {
    input.push({ type: "message", role: "user", content: "user message " + index });
    input.push({ type: "message", role: "assistant", content: "assistant message " + index });
  }

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: normalizeInput(input),
    config: {},
  });

  assert.ok(compiled.messages.length > 60, "should not use the old fixed 60-message cutoff");
  assert.ok(JSON.stringify(compiled.messages).includes("user message 0"), "large 1M budget should keep early messages when affordable");
}

function testToolProtocolValidityForIncompleteCalls() {
  const input = normalizeInput([
    { type: "message", role: "user", content: "search something" },
    { type: "function_call", id: "fc_1", call_id: "call_1", name: "some_tool", arguments: "{\"q\":\"x\"}" },
  ]);

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });

  const assistantToolCalls = compiled.messages
    .filter((message) => message.role === "assistant" && Array.isArray(message.tool_calls))
    .flatMap((message) => message.tool_calls);
  assert.equal(assistantToolCalls.length, 0, "incomplete tool calls must not be emitted as Chat tool_calls");
  assert.ok(JSON.stringify(compiled.messages).includes("some_tool"), "incomplete call should survive as a verified fact");
}

function testStorageCompactsLargeToolOutput() {
  const bigOutput = "token:TEST_SECRET_TOKEN_SHOULD_BE_REDACTED\n" + "A".repeat(200000);
  const input = normalizeInput([
    { type: "function_call", id: "fc_1", call_id: "call_1", name: "read_file_range", arguments: "{\"path\":\"a.txt\"}" },
    { type: "function_call_output", id: "out_1", call_id: "call_1", output: bigOutput },
  ]);
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });
  const stored = buildStorageMessages(compiled, [], {});
  const storedText = JSON.stringify(stored);
  assert.ok(storedText.length < 120000, "storage should be compacted");
  assert.ok(!storedText.includes("TEST_SECRET_TOKEN_SHOULD_BE_REDACTED"), "storage should redact API-key shaped values");
  assert.ok(storedText.includes("sha256="), "storage should include deterministic truncation hash");
}

function testStorageDoesNotPersistContextPrelude() {
  const input = normalizeInput([
    { type: "web_search_call", id: "ws_1", call_id: "call_ws_1", action: { query: "x" } },
    { type: "message", role: "user", content: "hello" },
  ]);
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", instructions: "system instructions", input },
    normalizedInput: input,
    config: {},
  });
  assert.ok(JSON.stringify(compiled.messages).includes("CodeSeeX verified conversation facts"));
  const stored = buildStorageMessages(compiled, [], {}, { requestInstructions: "system instructions" });
  const storedText = JSON.stringify(stored);
  assert.ok(!storedText.includes("CodeSeeX verified conversation facts"));
  assert.ok(!storedText.includes("system instructions"));
}

function testStoredRecordUsesDeltaSchemaOnly() {
  const record = buildStoredRecord({
    id: "resp_delta_schema",
    createdAt: 1,
    response: { id: "resp_delta_schema", output: [], usage: null },
    requestBody: { model: "deepseek-v4-pro" },
    normalizedInput: [],
    currentMessages: [{ role: "user", content: "delta user" }],
    storedMessages: [{ role: "assistant", content: "delta answer" }],
    rawAssistant: { role: "assistant", content: "delta answer" },
    turnMessages: [{ role: "user", content: "delta user" }, { role: "assistant", content: "delta answer" }],
    toolFacts: [{ tool: "web_search", call_id: "call_delta", status: "completed" }],
  });

  assert.equal(record.state_schema, "codeseex-response-delta-v1");
  assert.ok(Array.isArray(record.turn_messages));
  assert.ok(Array.isArray(record.turn_tool_facts));
  assert.equal(record.upstream_messages, undefined);
  assert.equal(record.tool_facts, undefined);
}

function testTurnStorageDoesNotDuplicatePreviousHistory() {
  const previousRecord = {
    upstream_messages: [
      { role: "user", content: "OLD_CHAIN_MARKER_SHOULD_NOT_BE_DUPLICATED_IN_TURN_STORAGE" },
      { role: "assistant", content: "old answer" },
    ],
  };
  const input = normalizeInput([
    { type: "message", role: "user", content: "new turn marker CODESEEX_TURN_ONLY_MARKER" },
  ]);
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    previousRecord,
    normalizedInput: input,
    config: {},
  });
  const turn = buildTurnStorageMessages(compiled, [{ role: "assistant", content: "new answer" }], {});
  const text = JSON.stringify(turn);
  assert.ok(text.includes("CODESEEX_TURN_ONLY_MARKER"));
  assert.ok(text.includes("new answer"));
  assert.ok(!text.includes("OLD_CHAIN_MARKER_SHOULD_NOT_BE_DUPLICATED_IN_TURN_STORAGE"), "turn storage must not copy previous cumulative history");
}

function testResponseChainReconstructsDeltaHistory() {
  const context = { config: { maxStoredResponses: 500 }, state: { responses: {} } };
  let previousId = null;
  for (let index = 0; index < 140; index += 1) {
    const id = "resp_chain_" + index;
    const marker = index === 0 ? " CODESEEX_EARLY_CHAIN_MARKER" : "";
    const turnMessages = [
      { role: "user", content: "chain user " + index + marker },
      { role: "assistant", content: "chain assistant " + index },
    ];
    context.state.responses[id] = {
      id,
      created_at: index,
      previous_response_id: previousId,
      turn_messages: turnMessages,
      turn_tool_facts: [],
    };
    previousId = id;
  }

  const previousContext = resolvePreviousContext(previousId, context);
  assert.equal(previousContext.chain_diagnostic.record_count, 140);
  assert.equal(previousContext.chain_diagnostic.delta_record_count, 140);
  assert.ok(JSON.stringify(previousContext.upstream_messages).includes("CODESEEX_EARLY_CHAIN_MARKER"));

  const input = normalizeInput([
    { type: "message", role: "user", content: "What early marker did I give you?" },
  ]);
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    previousRecord: previousContext,
    normalizedInput: input,
    config: {},
  });
  assert.ok(JSON.stringify(compiled.messages).includes("CODESEEX_EARLY_CHAIN_MARKER"), "compiled context should include early delta-chain messages when within budget");
}

function testCompactionIsModelVisible() {
  const input = normalizeInput([
    { type: "compaction", id: "cmp_1", summary: [{ type: "summary_text", text: "The user wants a reliable context compiler." }] },
    { type: "message", role: "user", content: "继续" },
  ]);
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });
  assert.ok(JSON.stringify(compiled.messages).includes("client compaction summaries"));
  assert.ok(JSON.stringify(compiled.messages).includes("reliable context compiler"));
}

function testCodeseexCompactionPayloadIsEffective() {
  const input = normalizeInput([
    { type: "message", role: "user", content: "Use a tool and remember the result." },
    { type: "function_call", id: "fc_1", call_id: "call_1", name: "shell_command", arguments: "{\"command\":\"echo COMPACT_FACT\"}" },
    { type: "function_call_output", id: "out_1", call_id: "call_1", output: "COMPACT_FACT\n" },
  ]);
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: { compactionSecret: "unit-test-compaction-secret-value" },
  });
  const compact = buildCodeseexCompaction({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    compiledContext: compiled,
    config: { compactionSecret: "unit-test-compaction-secret-value" },
  });

  assert.ok(compact.encrypted_content.startsWith("codeseex-compaction-v1:"));
  assert.ok(!compact.encrypted_content.includes("COMPACT_FACT"));
  const decoded = decodeCodeseexCompaction(compact.encrypted_content, { compactionSecret: "unit-test-compaction-secret-value" });
  assert.equal(decoded.purpose, "codeseex_deepseek_context_compaction");
  assert.ok(JSON.stringify(decoded).includes("COMPACT_FACT"));

  const nextInput = normalizeInput([
    { type: "compaction", id: "cmp_1", encrypted_content: compact.encrypted_content },
    { type: "message", role: "user", content: "What happened earlier?" },
  ]);
  const nextCompiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input: nextInput },
    normalizedInput: nextInput,
    config: { compactionSecret: "unit-test-compaction-secret-value" },
  });
  const text = JSON.stringify(nextCompiled.messages);
  assert.ok(text.includes("CodeSeeX compacted conversation state"));
  assert.ok(text.includes("shell_command"));
  assert.ok(text.includes("COMPACT_FACT"));
}

function testCodeseexCompactionDoesNotRecursivelySummarizeItself() {
  const config = { compactionSecret: "unit-test-compaction-secret-value" };
  const input = normalizeInput([
    { type: "message", role: "user", content: "Remember recursive compact marker CODESEEX_NO_RECURSIVE_COMPACT." },
  ]);
  const firstCompiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config,
  });
  const firstCompact = buildCodeseexCompaction({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    compiledContext: firstCompiled,
    config,
  });
  const secondInput = normalizeInput([
    { type: "compaction", id: firstCompact.payload.id, encrypted_content: firstCompact.encrypted_content },
    { type: "message", role: "user", content: "Continue after compact." },
  ]);
  const secondCompiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input: secondInput },
    normalizedInput: secondInput,
    config,
  });
  const secondCompact = buildCodeseexCompaction({
    requestBody: { model: "deepseek-v4-pro", input: secondInput },
    normalizedInput: secondInput,
    compiledContext: secondCompiled,
    config,
  });

  assert.ok(secondCompact.payload.messages.some((message) => JSON.stringify(message).includes("CODESEEX_NO_RECURSIVE_COMPACT")));
  assert.deepEqual(secondCompact.payload.compaction_summaries, [], "CodeSeeX encrypted compactions must not feed rendered summaries back into later compactions");
  assert.ok(secondCompact.text.length < firstCompact.text.length + 2500, "re-compacting should stay bounded rather than nesting prior rendered compaction text");
}

function testAutomaticCompactionThreshold() {
  assert.equal(resolveCompactThreshold({ compact_threshold: 12 }), 12);
  assert.equal(resolveCompactThreshold([{ compact_threshold: 0 }, { compaction: { threshold: 34 } }]), 34);

  const input = normalizeInput([
    { type: "message", role: "user", content: "auto compact marker AUTO_COMPACT_FACT" },
  ]);
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input, context_management: { compact_threshold: 1 } },
    normalizedInput: input,
    config: { compactionSecret: "unit-test-compaction-secret-value" },
  });
  const compact = maybeBuildAutomaticCompaction({
    requestBody: { model: "deepseek-v4-pro", input, context_management: { compact_threshold: 1 } },
    previousRecord: null,
    normalizedInput: input,
    compiledContext: compiled,
    conversationMessages: compiled.messages,
    config: { compactionSecret: "unit-test-compaction-secret-value" },
  });
  assert.ok(compact && compact.item, "context_management threshold should generate an automatic compaction item");
  assert.ok(compact.item.encrypted_content.startsWith("codeseex-compaction-v1:"));
  assert.ok(!JSON.stringify(compact.item).includes("AUTO_COMPACT_FACT"));
  const decoded = decodeCodeseexCompaction(compact.item.encrypted_content, { compactionSecret: "unit-test-compaction-secret-value" });
  assert.ok(JSON.stringify(decoded).includes("AUTO_COMPACT_FACT"));
}

function testAutomaticCompactionKeepsHighInformationAnchors() {
  const marker = "CODESEEX_LONG_MIDDLE_MARKER_AUTO";
  const longStable = "stable-cache-prefix ".repeat(800);
  const config = { compactionSecret: "unit-test-compaction-secret-value" };
  const input = normalizeInput([
    { type: "message", role: "user", content: longStable + "\nRemember auto marker: " + marker + ". Reply OK only." },
  ]);
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-flash", input, context_management: { compact_threshold: 1 } },
    normalizedInput: input,
    config,
  });
  const compact = maybeBuildAutomaticCompaction({
    requestBody: { model: "deepseek-v4-flash", input, context_management: { compact_threshold: 1 } },
    previousRecord: null,
    normalizedInput: input,
    compiledContext: compiled,
    conversationMessages: compiled.messages,
    config,
  });
  const nextInput = normalizeInput([
    compact.item,
    { type: "message", role: "user", content: "What exact automatic compact marker was stored?" },
  ]);
  const nextCompiled = compileContext({
    requestBody: { model: "deepseek-v4-flash", input: nextInput },
    normalizedInput: nextInput,
    config,
  });
  assert.ok(JSON.stringify(nextCompiled.messages).includes(marker), "automatic compaction must preserve high-information anchors from long messages");
}

function testAutomaticCompactionOmitsEphemeralReplyDirectives() {
  const marker = "CODESEEX_REPLY_DIRECTIVE_MARKER_AUTO";
  const config = { compactionSecret: "unit-test-compaction-secret-value" };
  const input = normalizeInput([
    { type: "message", role: "user", content: "Remember auto marker: " + marker + ". Reply OK only." },
  ]);
  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-flash", input, context_management: { compact_threshold: 1 } },
    normalizedInput: input,
    config,
  });
  const compact = maybeBuildAutomaticCompaction({
    requestBody: { model: "deepseek-v4-flash", input, context_management: { compact_threshold: 1 } },
    previousRecord: null,
    normalizedInput: input,
    compiledContext: compiled,
    conversationMessages: compiled.messages,
    config,
  });
  const nextInput = normalizeInput([
    compact.item,
    { type: "message", role: "user", content: "What exact automatic compact marker was stored?" },
  ]);
  const nextCompiled = compileContext({
    requestBody: { model: "deepseek-v4-flash", input: nextInput },
    normalizedInput: nextInput,
    config,
  });
  const text = JSON.stringify(nextCompiled.messages);
  assert.ok(text.includes(marker));
  assert.ok(!/Reply OK only/i.test(text), "ephemeral reply directives must not remain executable in compacted history");
}

function testTinyBudgetKeepsFactsFirst() {
  const input = normalizeInput([
    { type: "web_search_call", id: "ws_1", call_id: "call_ws_1", action: { query: "context compiler regression" } },
  ].concat(Array.from({ length: 80 }, (_, index) => ({
    type: "message",
    role: index % 2 === 0 ? "user" : "assistant",
    content: "large filler " + index + " " + "x".repeat(1000),
  }))));

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {
      contextWindow: 8192,
      effectiveContextWindowPercent: 10,
      contextReservedOutputTokens: 1024,
      contextMaxToolOutputBytes: 4096,
    },
  });

  assert.ok(compiled.messages.some((message) => message.content && message.content.includes("CodeSeeX verified conversation facts")));
  assert.ok(compiled.diagnostic.compressed);
  const assistantToolCalls = compiled.messages
    .filter((message) => message.role === "assistant" && Array.isArray(message.tool_calls))
    .flatMap((message) => message.tool_calls);
  assert.equal(assistantToolCalls.length, 0);
}

function testToolFactBeatsAssistantSelfDescription() {
  const input = normalizeInput([
    { type: "message", role: "user", content: "Please use a tool." },
    { type: "function_call", id: "fc_1", call_id: "call_1", name: "shell_command", arguments: "{\"command\":\"echo CODESEEX_FACT\"}" },
    { type: "function_call_output", id: "out_1", call_id: "call_1", output: "CODESEEX_FACT\n" },
    { type: "message", role: "assistant", content: "I did not use any tool." },
    { type: "message", role: "user", content: "What happened?" },
  ]);

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });

  const text = JSON.stringify(compiled.messages);
  assert.equal(compiled.toolFacts.length, 1);
  assert.ok(text.includes("CodeSeeX verified conversation facts"));
  assert.ok(text.includes("shell_command"));
  assert.ok(text.includes("CODESEEX_FACT"));
}

function testToolHistoryKeepsDeepSeekThinkingField() {
  const input = normalizeInput([
    { type: "message", role: "user", content: "Please use a tool." },
    { type: "function_call", id: "fc_1", call_id: "call_1", name: "shell_command", arguments: "{\"command\":\"echo ok\"}" },
    { type: "function_call_output", id: "out_1", call_id: "call_1", output: "ok\n" },
  ]);

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });

  const assistantToolMessage = compiled.messages.find((message) => message.role === "assistant" && Array.isArray(message.tool_calls));
  assert.ok(assistantToolMessage, "compiled history should contain the completed tool call");
  assert.equal(typeof assistantToolMessage.reasoning_content, "string");
}

function testCompletedToolPairDoesNotInjectDynamicPrelude() {
  const input = normalizeInput([
    { type: "message", role: "user", content: "Use a tool." },
    { type: "function_call", id: "fc_1", call_id: "call_1", name: "shell_command", arguments: "{\"command\":\"echo CACHE_STABLE\"}" },
    { type: "function_call_output", id: "out_1", call_id: "call_1", output: "CACHE_STABLE\n" },
    { type: "message", role: "assistant", content: "The tool returned CACHE_STABLE." },
    { type: "message", role: "user", content: "Continue normally." },
  ]);

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });

  const text = JSON.stringify(compiled.messages);
  assert.ok(text.includes("CACHE_STABLE"));
  assert.ok(!text.includes("CodeSeeX verified conversation facts"), "completed tool pairs already exist in native history and should not add dynamic cache-breaking facts");
}

function testLargeBase64ToolOutputIsOmitted() {
  const data = Buffer.from("fake image bytes ".repeat(1000)).toString("base64").repeat(4);
  const input = normalizeInput([
    { type: "message", role: "user", content: "Inspect screenshot." },
    { type: "function_call", id: "fc_1", call_id: "call_1", name: "browser_screenshot", arguments: "{\"fullPage\":false}" },
    { type: "function_call_output", id: "out_1", call_id: "call_1", output: "{\"image_url\":\"data:image/jpeg;base64," + data + "\",\"note\":\"screenshot captured\"}" },
  ]);

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });

  const text = JSON.stringify(compiled.messages);
  assert.ok(text.includes("binary payload omitted"));
  assert.ok(text.includes("mime=image/jpeg"));
  assert.ok(text.includes("sha256="));
  assert.ok(!text.includes(data.slice(0, 1200)), "raw base64 screenshot bytes must not enter model-visible context");
}

function testTypedImageToolOutputIsStructurallyOmitted() {
  const data = Buffer.from("codex typed screenshot bytes ".repeat(1000)).toString("base64").repeat(4);
  const input = normalizeInput([
    { type: "message", role: "user", content: "Capture a browser screenshot." },
    { type: "function_call", id: "fc_1", call_id: "call_1", name: "browser_screenshot", arguments: "{\"fullPage\":false}" },
    {
      type: "function_call_output",
      id: "out_1",
      call_id: "call_1",
      output: [
        { type: "input_text", text: "Wall time: 3.6632 seconds\nOutput:" },
        { type: "input_image", image_url: "data:image/jpeg;base64," + data, detail: "original" },
      ],
    },
  ]);

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });

  const toolMessage = compiled.messages.find((message) => message.role === "tool" && message.tool_call_id === "call_1");
  assert.ok(toolMessage, "typed image output should still produce a valid Chat tool message");
  assert.ok(toolMessage.content.includes("Wall time: 3.6632 seconds"));
  assert.ok(toolMessage.content.includes("image omitted"));
  assert.ok(toolMessage.content.includes("mime=image/jpeg"));
  assert.ok(toolMessage.content.includes("detail=original"));

  const text = JSON.stringify(compiled.messages);
  assert.ok(!text.includes("data:image/jpeg;base64"), "typed data URL must be removed before Chat conversion");
  assert.ok(!text.includes(data.slice(0, 1200)), "typed image raw base64 must not enter model-visible context");

  assert.equal(compiled.toolFacts.length, 1);
  assert.ok(compiled.toolFacts[0].result.includes("image omitted"));
  assert.ok(!JSON.stringify(compiled.toolFacts).includes(data.slice(0, 1200)), "tool fact ledger must not retain typed image base64");
}

function testPlainJsonArrayWithImageUrlKeepsNonTypedFields() {
  const data = Buffer.from("plain json screenshot bytes ".repeat(800)).toString("base64").repeat(3);
  const input = normalizeInput([
    { type: "message", role: "user", content: "Inspect structured tool output." },
    { type: "function_call", id: "fc_1", call_id: "call_1", name: "custom_report", arguments: "{\"format\":\"json\"}" },
    {
      type: "function_call_output",
      id: "out_1",
      call_id: "call_1",
      output: [
        { label: "before", image_url: "data:image/png;base64," + data, value: 42 },
        { label: "after", notes: ["kept", "as json"] },
      ],
    },
  ]);

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });
  const toolMessage = compiled.messages.find((message) => message.role === "tool" && message.tool_call_id === "call_1");
  assert.ok(toolMessage, "plain JSON array output should still produce a valid tool message");
  assert.ok(toolMessage.content.includes("\"label\":\"before\""));
  assert.ok(toolMessage.content.includes("\"value\":42"));
  assert.ok(toolMessage.content.includes("\"notes\":[\"kept\",\"as json\"]"));
  assert.ok(toolMessage.content.includes("mime=image/png"));
  assert.ok(!toolMessage.content.includes(data.slice(0, 1200)), "plain JSON image field should omit raw base64 without dropping sibling fields");
}

function testLargeBase64MessageContentIsOmitted() {
  const data = Buffer.from("message image bytes ".repeat(1000)).toString("base64").repeat(3);
  const input = normalizeInput([
    { type: "message", role: "user", content: "Here is a text-wrapped image data:image/png;base64," + data },
  ]);

  const compiled = compileContext({
    requestBody: { model: "deepseek-v4-pro", input },
    normalizedInput: input,
    config: {},
  });

  const text = JSON.stringify(compiled.messages);
  assert.ok(text.includes("binary payload omitted"));
  assert.ok(text.includes("mime=image/png"));
  assert.ok(!text.includes(data.slice(0, 1200)), "raw base64 in message text must not enter model-visible context");
}

function testContextDiagnosticRedactsSecrets() {
  const sanitized = sanitizeDebugValue({
    prompt: "token:TEST_SECRET_TOKEN_SHOULD_BE_REDACTED and Bearer TEST_BEARER_TOKEN_SHOULD_BE_REDACTED",
    nested: { api_key: "TEST_API_KEY_SHOULD_BE_REDACTED" },
  });
  const text = JSON.stringify(sanitized);
  assert.ok(!text.includes("TEST_SECRET_TOKEN_SHOULD_BE_REDACTED"));
  assert.ok(!text.includes("TEST_BEARER_TOKEN_SHOULD_BE_REDACTED"));
  assert.ok(!text.includes("TEST_API_KEY_SHOULD_BE_REDACTED"));
}

function testThinkingModeAndVisibilityAreSeparate() {
  const toolContext = {
    upstreamTools: [],
    normalizeToolChoice: () => undefined,
    responseToolItemFromChat: () => null,
  };
  const autoNoEffort = buildDeepSeekPayload({ model: "deepseek-v4-pro" }, [], toolContext, { thinkingMode: "auto" });
  const autoHigh = buildDeepSeekPayload({ model: "deepseek-v4-pro", reasoning: { effort: "xhigh" } }, [], toolContext, { thinkingMode: "auto" });
  const autoNone = buildDeepSeekPayload({ model: "deepseek-v4-pro", reasoning: { effort: "none" } }, [], toolContext, { thinkingMode: "auto" });
  const forcedOn = buildDeepSeekPayload({ model: "deepseek-v4-pro" }, [], toolContext, { thinkingMode: "enabled" });
  const forcedOff = buildDeepSeekPayload({ model: "deepseek-v4-pro", reasoning: { effort: "xhigh" } }, [], toolContext, { thinkingMode: "disabled" });

  assert.equal(autoNoEffort.thinking, undefined);
  assert.deepEqual(autoHigh.thinking, { type: "enabled" });
  assert.deepEqual(autoNone.thinking, { type: "disabled" });
  assert.deepEqual(forcedOn.thinking, { type: "enabled" });
  assert.deepEqual(forcedOff.thinking, { type: "disabled" });

  const assistant = { role: "assistant", reasoning_content: "private reasoning", content: "final" };
  const shown = responseOutputFromAssistant(assistant, null, toolContext, { visibleThinkingEnabled: true }, { phase: "final_answer" });
  const hidden = responseOutputFromAssistant(assistant, null, toolContext, { visibleThinkingEnabled: false }, { phase: "final_answer" });
  assert.equal(shown[0].type, "reasoning");
  assert.equal(shown[0].summary.length, 1);
  assert.equal(hidden[0].type, "reasoning");
  assert.equal(hidden[0].summary.length, 0);
  assert.ok(hidden[0].encrypted_content);
}

function testTemperaturePresetMapping() {
  const toolContext = {
    upstreamTools: [],
    normalizeToolChoice: () => undefined,
    responseToolItemFromChat: () => null,
  };
  const base = { model: "deepseek-v4-pro", temperature: 0.8 };
  assert.equal(buildDeepSeekPayload(base, [], toolContext, { temperaturePreset: "default" }).temperature, 0.8);
  assert.equal(buildDeepSeekPayload(base, [], toolContext, { temperaturePreset: "strict" }).temperature, 0);
  assert.equal(buildDeepSeekPayload(base, [], toolContext, { temperaturePreset: "balanced" }).temperature, 1);
  assert.equal(buildDeepSeekPayload(base, [], toolContext, { temperaturePreset: "general" }).temperature, 1.3);
  assert.equal(buildDeepSeekPayload(base, [], toolContext, { temperaturePreset: "creative" }).temperature, 1.5);
  assert.equal(buildDeepSeekPayload({ model: "deepseek-v4-pro" }, [], toolContext, { temperaturePreset: "default" }).temperature, undefined);
}

function testDeepSeekChatCompletionsUrlCompatibility() {
  assert.equal(resolveChatCompletionsUrl("https://api.deepseek.com"), "https://api.deepseek.com/v1/chat/completions");
  assert.equal(resolveChatCompletionsUrl("https://api.deepseek.com/"), "https://api.deepseek.com/v1/chat/completions");
  assert.equal(resolveChatCompletionsUrl("https://api.deepseek.com", { officialV1Compat: true }), "https://api.deepseek.com/v1/chat/completions");
  assert.equal(resolveChatCompletionsUrl("https://api.deepseek.com", { officialV1Compat: false }), "https://api.deepseek.com/chat/completions");
  assert.equal(resolveChatCompletionsUrl("https://api.deepseek.com/v1"), "https://api.deepseek.com/v1/chat/completions");
  assert.equal(resolveChatCompletionsUrl("https://api.deepseek.com/chat/completions"), "https://api.deepseek.com/chat/completions");
  assert.equal(resolveChatCompletionsUrl("http://127.0.0.1:8000/v1"), "http://127.0.0.1:8000/v1/chat/completions");
  assert.equal(resolveChatCompletionsUrl("http://127.0.0.1:8000/v1/chat/completions"), "http://127.0.0.1:8000/v1/chat/completions");
}

run();
