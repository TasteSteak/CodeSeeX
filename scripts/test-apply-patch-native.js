"use strict";

const { buildDeepSeekPayload } = require("../src/proxy/deepseek-client");
const { buildConversation, inputToMessages, normalizeInput } = require("../src/proxy/conversation");
const { chatToolCallFromResponseItem, createToolContext, responseToolItemFromChat, splitToolCalls } = require("../src/proxy/tools");

main();

function main() {
  const patch = [
    "*** Begin Patch",
    "*** Add File: native-apply-patch-smoke.txt",
    "+hello",
    "*** End Patch",
  ].join("\n");

  const context = createToolContext([]);
  const toolNames = context.upstreamTools.map((tool) => tool.function && tool.function.name).filter(Boolean);
  assert(toolNames.includes("apply_patch"), "apply_patch must be exposed to the upstream model");
  assert(context.upstreamTools.filter((tool) => tool.function && tool.function.name === "apply_patch").length === 1, "apply_patch must be exposed exactly once");

  const nativeDescription = "Use the apply_patch tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.";
  const nativeContext = createToolContext([{
    type: "function",
    function: {
      name: "apply_patch",
      description: nativeDescription,
      parameters: {
        type: "object",
        properties: {
          patch: { type: "string" },
        },
        required: ["patch"],
      },
    },
  }]);
  const nativeTool = nativeContext.upstreamTools.find((tool) => tool.function && tool.function.name === "apply_patch");
  assert(nativeTool.function.description.includes("Use the apply_patch tool to edit files."), "Codex-provided apply_patch declaration should be preserved");
  assert(!nativeTool.function.description.includes("do not wrap the patch in JSON"), "Chat Completions schema must not keep conflicting freeform JSON guidance");
  assert(nativeTool.function.description.includes("CodeSeeX adapts this Chat Completions function"), "apply_patch schema should explain the native bridge");
  assert(nativeTool.function.description.includes("*** Update File: <path>"), "apply_patch schema should document update headers");
  assert(nativeTool.function.description.includes("Use a bare `@@` line only"), "apply_patch schema should document safe hunk separators");
  assert(nativeTool.function.description.includes("Do not use unified diff headers"), "apply_patch schema should reject unified diff headers");
  assert(nativeTool.function.description.includes("re-read the target file"), "apply_patch schema should guide context-mismatch recovery");
  assert(nativeTool.function.description.includes("Do not retry from remembered context"), "apply_patch schema should discourage stale-context retries");
  assert(nativeTool.function.parameters.properties.patch.description.includes("*** Begin Patch"), "apply_patch parameter should document patch grammar");
  assert(nativeContext.upstreamTools.filter((tool) => tool.function && tool.function.name === "apply_patch").length === 1, "native apply_patch declaration must not duplicate fallback schema");

  const payload = buildDeepSeekPayload(
    { model: "deepseek-v4-pro", stream: false },
    [{ role: "user", content: "edit a file" }],
    context,
    {},
    { stream: false }
  );
  assert(Array.isArray(payload.tools), "DeepSeek payload did not include tools");
  assert(payload.tools.some((tool) => tool.function && tool.function.name === "apply_patch"), "DeepSeek payload did not include apply_patch");

  const toolCall = {
    id: "call_native_patch",
    type: "function",
    function: {
      name: "apply_patch",
      arguments: JSON.stringify({ patch }),
    },
  };

  const split = splitToolCalls([toolCall], context);
  assert(split.internal.length === 1, "apply_patch must be treated as an internal bridge call");
  assert(split.external.length === 0, "apply_patch must not be sent as a normal external function_call");
  assert(split.hosted.length === 0, "apply_patch must not be executed as a proxy-hosted tool");

  const item = responseToolItemFromChat(toolCall, context);
  assert(item.type === "custom_tool_call", "apply_patch must be returned as native custom_tool_call");
  assert(item.name === "apply_patch", "apply_patch custom tool name was lost");
  assert(item.call_id === "call_native_patch", "apply_patch call_id was not preserved");
  assert(item.input === patch, "apply_patch patch text was not preserved as input");
  assert(item.status === "completed", "apply_patch item status must be completed");
  assert(!Object.prototype.hasOwnProperty.call(item, "arguments"), "custom_tool_call must not expose function arguments");
  assert(item.type !== "function_call", "apply_patch must not be returned as function_call");

  const replayed = chatToolCallFromResponseItem({
    type: "custom_tool_call",
    call_id: "call_native_patch",
    name: "apply_patch",
    input: patch,
  });
  assert(replayed.type === "function", "history replay must use Chat Completions function tool_calls");
  assert(replayed.id === "call_native_patch", "history replay lost call_id");
  assert(replayed.function.name === "apply_patch", "history replay lost apply_patch name");
  assert(JSON.parse(replayed.function.arguments).patch === patch, "history replay lost patch text");

  const rawPatchCall = {
    id: "call_raw_patch",
    type: "function",
    function: {
      name: "apply_patch",
      arguments: patch,
    },
  };
  const rawPatchItem = responseToolItemFromChat(rawPatchCall, context);
  assert(rawPatchItem.type === "custom_tool_call", "raw patch text must still map to custom_tool_call");
  assert(rawPatchItem.input === patch, "raw patch text was not preserved");

  const complexPatch = [
    "*** Begin Patch",
    "*** Add File: src/example.js",
    "+const newlineLiteral = \"\\\\n\";",
    "+const windowsPath = \"C:\\\\Temp\\\\CodeSeeX\";",
    "+const json = \"{\\\"ok\\\":true}\";",
    "*** Update File: README.md",
    "@@",
    "-old text",
    "+new text with *** literal marker",
    "*** Move to: docs/README.md",
    "*** Delete File: obsolete.txt",
    "*** End Patch",
  ].join("\n");
  const complexItem = responseToolItemFromChat({
    id: "call_complex_patch",
    type: "function",
    function: {
      name: "apply_patch",
      arguments: JSON.stringify({ patch: complexPatch }),
    },
  }, context);
  assert(complexItem.type === "custom_tool_call", "complex patch must map to custom_tool_call");
  assert(complexItem.input === complexPatch, "complex patch content must be preserved byte-for-byte");
  assert(complexItem.input.includes('"+const newlineLiteral = "\\\\n";'.slice(1)), "literal \\\\n must not be converted to a real newline");
  assert(complexItem.input.includes("C:\\\\Temp\\\\CodeSeeX"), "Windows path backslashes must be preserved");
  assert(complexItem.input.includes('"{\\"ok\\":true}"'), "escaped JSON string content must be preserved");

  const stringCommandItem = responseToolItemFromChat({
    id: "call_string_patch",
    type: "function",
    function: {
      name: "apply_patch",
      arguments: JSON.stringify({ input: complexPatch }),
    },
  }, context);
  assert(stringCommandItem.input === complexPatch, "alternate input field should preserve patch text");

  const responseInputItems = normalizeInput([
    { type: "message", role: "user", content: "Please edit files." },
    {
      type: "custom_tool_call",
      call_id: "call_replay_patch",
      name: "apply_patch",
      input: complexPatch,
      status: "completed",
    },
    {
      type: "custom_tool_call_output",
      call_id: "call_replay_patch",
      output: "Exit code: 0\nWall time: 0 seconds\nOutput:\nSuccess. Updated the following files:\nM README.md\n",
      status: "completed",
    },
    { type: "message", role: "user", content: "Continue after the patch." },
  ]);
  const replayMessages = inputToMessages(responseInputItems);
  const replayAssistant = replayMessages.find((message) => message.role === "assistant" && Array.isArray(message.tool_calls));
  const replayTool = replayMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_replay_patch");
  assert(replayAssistant, "Responses input replay must include assistant apply_patch call");
  assert(replayAssistant.tool_calls[0].function.name === "apply_patch", "Responses input replay lost apply_patch tool name");
  assert(JSON.parse(replayAssistant.tool_calls[0].function.arguments).patch === complexPatch, "Responses input replay lost complex patch text");
  assert(replayTool && replayTool.content.includes("Success. Updated the following files"), "Responses input replay lost apply_patch result");
  assert(!replayTool.content.includes("Do not reuse remembered context"), "successful apply_patch output must not include retry guidance");

  const failedReplayMessages = inputToMessages(normalizeInput([
    {
      type: "custom_tool_call",
      call_id: "call_failed_patch",
      name: "apply_patch",
      input: complexPatch,
      status: "completed",
    },
    {
      type: "custom_tool_call_output",
      call_id: "call_failed_patch",
      output: [
        "Exit code: 1",
        "Wall time: 0 seconds",
        "Output:",
        "Failed to find expected lines in src/example.js:",
        "const stale = true;",
      ].join("\n"),
      status: "completed",
    },
  ]));
  const failedReplayTool = failedReplayMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_failed_patch");
  assert(failedReplayTool && failedReplayTool.content.includes("CodeSeeX note: apply_patch failed"), "failed apply_patch output should include recovery guidance");
  assert(failedReplayTool.content.includes("Re-read the target file before retrying"), "failed apply_patch output should ask the model to re-read the target file");
  assert(failedReplayTool.content.includes("Do not reuse remembered context"), "failed apply_patch output should discourage stale-context retries");

  const conversation = buildConversation({ instructions: "system", previous_response_id: null }, null, replayMessages);
  const conversationAssistant = conversation.find((message) => message.role === "assistant" && Array.isArray(message.tool_calls));
  const conversationTool = conversation.find((message) => message.role === "tool" && message.tool_call_id === "call_replay_patch");
  assert(conversationAssistant && conversationTool, "conversation protocol must preserve completed apply_patch call and result");

  console.log("Native apply_patch bridge test passed.");
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}
