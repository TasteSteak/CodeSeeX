"use strict";

const { buildDeepSeekPayload } = require("../src/proxy/deepseek-client");
const { chatToolCallFromResponseItem, createToolContext, responseToolItemFromChat, splitToolCalls } = require("../src/proxy/tools");

main();

function main() {
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

  console.log("Native MCP passthrough test passed.");
  console.log("Model-facing tools:", names.filter((name) => name.includes("codeseex_smoke")).join(", "));
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}
