"use strict";

const { buildDeepSeekPayload } = require("../src/proxy/deepseek-client");
const { runDeepSeekTurn } = require("../src/proxy/server");
const { chatToolCallFromResponseItem, createToolContext, responseToolItemFromChat, splitToolCalls } = require("../src/proxy/tools");

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exitCode = 1;
});

async function main() {
  const context = createToolContext([{
    type: "namespace",
    name: "mcp__codeseex_smoke__",
    description: "Codex native MCP smoke namespace",
    tools: [
      {
        type: "function",
        name: "codeseex_smoke_add",
        description: "Add two numbers through a Codex native MCP server.",
        parameters: {
          type: "object",
          properties: {
            a: { type: "number" },
            b: { type: "number" },
          },
          required: ["a", "b"],
        },
      },
      {
        type: "function",
        name: "codeseex_smoke_echo",
        description: "Echo text through a Codex native MCP server.",
        parameters: {
          type: "object",
          properties: { text: { type: "string" } },
          required: ["text"],
        },
      },
    ],
  }, {
    type: "namespace",
    name: "mcp__codeseex_conflict__",
    description: "Second native MCP namespace with a colliding child tool name.",
    tools: [{
      type: "function",
      name: "codeseex_smoke_echo",
      description: "Echo text through another Codex native MCP server.",
      parameters: {
        type: "object",
        properties: { text: { type: "string" } },
        required: ["text"],
      },
    }],
  }]);

  const names = context.upstreamTools.map((tool) => tool.function && tool.function.name).filter(Boolean);
  assert(names.includes("codeseex_smoke_add"), "primary MCP child tool was not exposed to DeepSeek");
  assert(names.includes("codeseex_smoke_echo"), "primary MCP echo tool was not exposed to DeepSeek");
  const conflictName = names.find((name) => name !== "codeseex_smoke_echo" && name.endsWith("codeseex_smoke_echo"));
  assert(conflictName, "conflicting MCP child tool did not receive a stable unique model-facing name");

  const payload = buildDeepSeekPayload(
    { model: "deepseek-v4-pro", stream: false },
    [{ role: "user", content: "call native MCP" }],
    context,
    {},
    { stream: false }
  );
  assert(Array.isArray(payload.tools), "DeepSeek payload did not include tools");
  assert(payload.tools.some((tool) => tool.function && tool.function.name === "codeseex_smoke_add"), "DeepSeek payload did not expose native MCP tool");

  const primaryItem = responseToolItemFromChat({
    id: "call_primary",
    type: "function",
    function: { name: "codeseex_smoke_add", arguments: "{\"a\":21,\"b\":21}" },
  }, context);
  assert(primaryItem.type === "function_call", "native MCP call must be returned as function_call");
  assert(primaryItem.name === "codeseex_smoke_add", "native MCP call lost child tool name");
  assert(primaryItem.namespace === "mcp__codeseex_smoke__", "native MCP call lost namespace");
  assert(!primaryItem.mcp_server, "native MCP call must not be marked as proxy-hosted");

  const conflictItem = responseToolItemFromChat({
    id: "call_conflict",
    type: "function",
    function: { name: conflictName, arguments: "{\"text\":\"hello\"}" },
  }, context);
  assert(conflictItem.type === "function_call", "conflicting native MCP call must be returned as function_call");
  assert(conflictItem.name === "codeseex_smoke_echo", "conflicting native MCP call did not restore child tool name");
  assert(conflictItem.namespace === "mcp__codeseex_conflict__", "conflicting native MCP call lost namespace");
  assert(!conflictItem.mcp_server, "conflicting native MCP call must not be marked as proxy-hosted");

  const replayedCall = chatToolCallFromResponseItem({
    type: "function_call",
    call_id: "call_replay",
    name: "codeseex_smoke_add",
    namespace: "mcp__codeseex_smoke__",
    arguments: "{\"a\":21,\"b\":21}",
  });
  assert(replayedCall.function.name === "codeseex_smoke_add", "history replay lost native MCP child tool name");
  assert(replayedCall.namespace === "mcp__codeseex_smoke__", "history replay lost native MCP namespace");

  const split = splitToolCalls([{
    id: "call_primary",
    type: "function",
    function: { name: "codeseex_smoke_add", arguments: "{\"a\":21,\"b\":21}" },
  }], context);
  assert(split.external.length === 1, "native MCP calls must stay external for Codex App execution");
  assert(split.hosted.length === 0, "native MCP calls must not be executed by CodeSeeX proxy");
  assert(split.internal.length === 0, "native MCP calls must not be treated as internal tools");

  const mcpToolContext = createToolContext([{
    type: "mcp",
    name: "codeseex_smoke",
    server_label: "codeseex_smoke",
    tools: [{
      type: "function",
      name: "codeseex_mcp_shape_add",
      description: "Add two numbers through a Codex native MCP tool declaration.",
      parameters: {
        type: "object",
        properties: {
          a: { type: "number" },
          b: { type: "number" },
        },
        required: ["a", "b"],
      },
    }],
  }]);
  const mcpShapeItem = responseToolItemFromChat({
    id: "call_mcp_shape",
    type: "function",
    function: { name: "codeseex_mcp_shape_add", arguments: "{\"a\":20,\"b\":22}" },
  }, mcpToolContext);
  assert(mcpShapeItem.type === "function_call", "native type=mcp declarations must be returned as function_call");
  assert(mcpShapeItem.name === "codeseex_mcp_shape_add", "native type=mcp declaration lost child tool name");
  assert(mcpShapeItem.namespace === "codeseex_smoke", "native type=mcp declaration lost namespace");
  assert(!mcpShapeItem.mcp_server, "native type=mcp declarations must not be marked as proxy-hosted");

  const nestedMcpToolContext = createToolContext([{
    type: "mcp",
    name: "codeseex_nested",
    tools: [{
      tool: {
        name: "codeseex_nested_add",
        description: "Add two numbers through a nested MCP tool declaration.",
        inputSchema: {
          type: "object",
          properties: {
            a: { type: "number" },
            b: { type: "number" },
          },
          required: ["a", "b"],
        },
      },
    }],
  }]);
  assert(nestedMcpToolContext.upstreamTools.some((tool) => tool.function && tool.function.name === "codeseex_nested_add"), "nested MCP tool declaration was not exposed to DeepSeek");
  const nestedMcpItem = responseToolItemFromChat({
    id: "call_nested_mcp",
    type: "function",
    function: { name: "codeseex_nested_add", arguments: "{\"a\":19,\"b\":23}" },
  }, nestedMcpToolContext);
  assert(nestedMcpItem.type === "function_call", "nested MCP declarations must be returned as function_call");
  assert(nestedMcpItem.name === "codeseex_nested_add", "nested MCP declaration lost child tool name");
  assert(nestedMcpItem.namespace === "codeseex_nested", "nested MCP declaration lost namespace");
  assert(!nestedMcpItem.mcp_server, "nested MCP declarations must not be marked as proxy-hosted");

  const hostedContext = createToolContext([]);
  const hostedSplit = splitToolCalls([{
    id: "call_list",
    type: "function",
    function: { name: "list_directory", arguments: "{\"path\":\".\",\"depth\":1}" },
  }], hostedContext);
  assert(hostedSplit.hosted.length === 1, "CodeSeeX hosted tools must still be executed by the proxy");
  const hostedItem = responseToolItemFromChat(hostedSplit.hosted[0], hostedContext);
  assert(hostedItem.type === "proxy_tool_call", "CodeSeeX hosted tools must keep proxy display shape");
  assert(hostedItem.name === "list_directory", "hosted proxy display item lost tool name");

  const webContext = createToolContext([{ type: "web_search" }]);
  const webItem = responseToolItemFromChat({
    id: "call_web",
    type: "function",
    function: { name: "web_search", arguments: "{\"query\":\"CodeSeeX\"}" },
  }, webContext);
  assert(webItem.type === "web_search_call", "system web_search must keep native web_search_call shape");

  const communityContext = createToolContext([{
    type: "function",
    name: "community_echo",
    description: "Community hosted echo tool.",
    parameters: {
      type: "object",
      properties: { text: { type: "string" } },
      required: ["text"],
    },
  }]);
  communityContext.byName.set("community_echo", {
    kind: "hosted_community_echo",
    nativeTool: {
      async executeProxyTool({ arguments: argsText }) {
        const parsed = JSON.parse(argsText || "{}");
        return { ok: true, echoed: parsed.text || "" };
      },
    },
    responseName: "community_echo",
  });
  const communityResult = await runDeepSeekTurn({
    requestBody: { model: "deepseek-v4-pro", stream: false },
    messages: [{ role: "user", content: "call community echo" }],
    toolContext: communityContext,
    config: {},
    callJson: fakeToolCallJson("community_echo", "{\"text\":\"hello community\"}", "done"),
  });
  const communityText = JSON.stringify(communityResult.storedMessages);
  assert(communityText.includes("hello community"), "community hosted executeProxyTool result was not injected");
  assert(!communityText.includes("proxy_web_search_failed"), "community hosted tools must not fall through to web_search execution");

  const missingHostedContext = createToolContext([{
    type: "function",
    name: "missing_hosted",
    description: "Missing hosted implementation.",
    parameters: { type: "object", properties: {} },
  }]);
  missingHostedContext.byName.set("missing_hosted", {
    kind: "hosted_missing",
    nativeTool: {},
    responseName: "missing_hosted",
  });
  const missingResult = await runDeepSeekTurn({
    requestBody: { model: "deepseek-v4-pro", stream: false },
    messages: [{ role: "user", content: "call missing hosted" }],
    toolContext: missingHostedContext,
    config: {},
    callJson: fakeToolCallJson("missing_hosted", "{}", "done"),
  });
  const missingText = JSON.stringify(missingResult.storedMessages);
  assert(missingText.includes("proxy_hosted_tool_not_implemented"), "missing hosted implementation should return an explicit protocol error");
  assert(!missingText.includes("proxy_web_search_failed"), "missing hosted implementation must not be misrouted to web_search");

  console.log("Native MCP passthrough test passed.");
  console.log("Model-facing tools:", names.filter((name) => name.includes("codeseex_smoke")).join(", "));
}

function fakeToolCallJson(toolName, argsText, finalText) {
  let count = 0;
  return async () => {
    count += 1;
    if (count === 1) {
      return {
        choices: [{
          message: {
            role: "assistant",
            content: "",
            tool_calls: [{
              id: "call_" + toolName,
              type: "function",
              function: { name: toolName, arguments: argsText },
            }],
          },
        }],
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
      };
    }
    return {
      choices: [{ message: { role: "assistant", content: finalText || "done" } }],
      usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
    };
  };
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}
