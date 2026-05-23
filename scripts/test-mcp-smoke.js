"use strict";

const { spawn } = require("node:child_process");
const path = require("node:path");
const { buildDeepSeekPayload } = require("../src/proxy/deepseek-client");
const { buildMcpInputTools, discoverMcpToolsFromServers, executeMcpToolCall, getMcpPrompt, readMcpResource } = require("../src/proxy/mcp-bridge");
const { chatToolCallFromResponseItem, createToolContext, normalizeTools, responseToolItemFromChat } = require("../src/proxy/tools");
const { createHttpSmokeServer } = require("./mcp-http-smoke-server");
const { createLegacySseSmokeServer } = require("./mcp-legacy-sse-smoke-server");

const serverPath = path.join(__dirname, "mcp-smoke-server.js");
const HTTP_SMOKE_ADD = "codeseex_http_smoke_add";

main().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});

async function main() {
  const client = startMcpServer();
  try {
    const init = await client.request("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "codeseex-mcp-smoke-test", version: "0.0.0" },
    });
    client.notify("notifications/initialized", {});

    const listed = await client.request("tools/list", {});
    const tools = Array.isArray(listed.tools) ? listed.tools : [];
    const listedResources = await client.request("resources/list", {});
    const resources = Array.isArray(listedResources.resources) ? listedResources.resources : [];
    const listedTemplates = await client.request("resources/templates/list", {});
    const resourceTemplates = Array.isArray(listedTemplates.resourceTemplates) ? listedTemplates.resourceTemplates : [];
    const listedPrompts = await client.request("prompts/list", {});
    const prompts = Array.isArray(listedPrompts.prompts) ? listedPrompts.prompts : [];
    assert(init.serverInfo && init.serverInfo.name === "codeseex-smoke", "unexpected MCP server name");
    assert(tools.some((tool) => tool.name === "codeseex_smoke_echo"), "echo tool was not discovered");
    assert(tools.some((tool) => tool.name === "codeseex_smoke_add"), "add tool was not discovered");
    assert(resources.some((resource) => resource.uri === "codeseex-smoke://status"), "status resource was not discovered");
    assert(resourceTemplates.some((template) => template.uriTemplate === "codeseex-smoke://echo/{text}"), "echo resource template was not discovered");
    assert(prompts.some((prompt) => prompt.name === "codeseex_smoke_prompt"), "prompt was not discovered");

    const echo = await client.request("tools/call", {
      name: "codeseex_smoke_echo",
      arguments: { text: "hello" },
    });
    assert(textFromMcpResult(echo) === "codeseex-mcp-ok:hello", "echo tool returned unexpected result");

    const add = await client.request("tools/call", {
      name: "codeseex_smoke_add",
      arguments: { a: 21, b: 21 },
    });
    assert(textFromMcpResult(add) === "42", "add tool returned unexpected result");

    const statusResource = await client.request("resources/read", {
      uri: "codeseex-smoke://status",
    });
    assert(textFromResourceResult(statusResource) === "codeseex-mcp-resource-ok", "status resource returned unexpected result");

    const templateResource = await client.request("resources/read", {
      uri: "codeseex-smoke://echo/hello",
    });
    assert(textFromResourceResult(templateResource) === "codeseex-mcp-template-ok:hello", "template resource returned unexpected result");

    const prompt = await client.request("prompts/get", {
      name: "codeseex_smoke_prompt",
      arguments: { topic: "hello" },
    });
    assert(textFromPromptResult(prompt) === "codeseex-mcp-prompt-ok:hello", "prompt returned unexpected result");

    const bridge = await discoverMcpToolsFromServers([{
      name: "codeseex_smoke",
      enabled: true,
      transport: {
        type: "stdio",
        command: process.execPath,
        args: [serverPath],
      },
    }]);
    assert(bridge.servers.length === 1, "bridge did not discover smoke MCP server");
    assert(bridge.servers[0].tools.some((tool) => tool.name === "codeseex_smoke_add"), "bridge did not discover add tool");
    assert(bridge.servers[0].resources.some((resource) => resource.uri === "codeseex-smoke://status"), "bridge did not discover resource");
    assert(bridge.servers[0].resourceTemplates.some((template) => template.uriTemplate === "codeseex-smoke://echo/{text}"), "bridge did not discover resource template");
    assert(bridge.servers[0].prompts.some((item) => item.name === "codeseex_smoke_prompt"), "bridge did not discover prompt");
    const helperContext = createToolContext([], {
      extraTools: buildMcpInputTools(bridge),
    });
    assert(helperContext.byName.has("read_mcp_resource"), "MCP resource helper was not exposed upstream");
    assert(helperContext.byName.has("get_mcp_prompt"), "MCP prompt helper was not exposed upstream");
    const resourceHelperItem = responseToolItemFromChat({
      id: "call_resource_helper",
      type: "function",
      function: {
        name: "read_mcp_resource",
        arguments: "{\"server\":\"codeseex_smoke\",\"uri\":\"codeseex-smoke://status\"}",
      },
    }, helperContext);
    assert(resourceHelperItem.type === "proxy_tool_call", "MCP resource helper was not marked as hosted");
    assert(resourceHelperItem.namespace === "codeseex_mcp", "MCP resource helper lost helper namespace");
    const overriddenHelperContext = createToolContext([{
      type: "function",
      name: "read_mcp_resource",
      description: "Client-provided MCP helper that should be handled by CodeSeeX bridge.",
      parameters: { type: "object", properties: {} },
    }], {
      extraTools: buildMcpInputTools(bridge),
    });
    const helperToolCount = overriddenHelperContext.upstreamTools.filter((tool) => (
      tool && tool.function && tool.function.name === "read_mcp_resource"
    )).length;
    assert(helperToolCount === 1, "MCP helper override should keep a single upstream helper name");
    const overriddenResourceItem = responseToolItemFromChat({
      id: "call_resource_helper_override",
      type: "function",
      function: {
        name: "read_mcp_resource",
        arguments: "{\"server\":\"codeseex_smoke\",\"uri\":\"codeseex-smoke://status\"}",
      },
    }, overriddenHelperContext);
    assert(overriddenResourceItem.type === "proxy_tool_call", "MCP helper override was not hosted");
    assert(overriddenResourceItem.namespace === "codeseex_mcp", "MCP helper override lost helper namespace");
    const bridgeCall = await executeMcpToolCall({
      type: "proxy_tool_call",
      mcp_server: "codeseex_smoke",
      name: "codeseex_smoke_add",
      arguments: "{\"a\":21,\"b\":21}",
    }, { mcpBridge: bridge });
    assert(bridgeCall.ok === true, "bridge tool call failed");
    assert(bridgeCall.content === "42", "bridge tool call returned unexpected result");
    const partialFailureBridge = await discoverMcpToolsFromServers([
      {
        name: "codeseex_smoke",
        enabled: true,
        transport: {
          type: "stdio",
          command: process.execPath,
          args: [serverPath],
        },
      },
      {
        name: "codeseex_missing",
        enabled: true,
        transport: {
          type: "stdio",
          command: process.execPath,
          args: [path.join(__dirname, "missing-mcp-server.js")],
        },
      },
    ]);
    assert(partialFailureBridge.ok === true, "partial MCP discovery failure should keep working servers available");
    assert(partialFailureBridge.servers.length === 1, "partial MCP discovery should keep the successful server");
    assert(partialFailureBridge.errors.length === 1, "partial MCP discovery should report one failed server");

    const httpJson = await testHttpBridge({ sse: false, serverName: "codeseex_http_json" });
    const httpSse = await testHttpBridge({ sse: true, serverName: "codeseex_http_sse" });
    const httpAuth = await testHttpBridge({
      sse: false,
      serverName: "codeseex_http_auth",
      expectedAuth: "Bearer codeseex-smoke-token",
      bearerTokenEnvVar: "CODESEEX_SMOKE_MCP_TOKEN",
      bearerTokenValue: "codeseex-smoke-token",
    });
    const legacySse = await testLegacySseBridge({ serverName: "codeseex_legacy_sse" });
    assert(httpJson.result.content === "42", "HTTP JSON MCP bridge returned unexpected result");
    assert(httpSse.result.content === "42", "HTTP SSE MCP bridge returned unexpected result");
    assert(httpAuth.result.content === "42", "HTTP MCP bearer env auth failed");
    assert(legacySse.result.content === "42", "legacy SSE MCP bridge returned unexpected result");
    assert(httpJson.resourceText === "codeseex-http-mcp-resource-ok", "HTTP JSON resource read failed");
    assert(httpSse.resourceText === "codeseex-http-mcp-resource-ok", "HTTP SSE resource read failed");
    assert(legacySse.resourceText === "codeseex-http-mcp-resource-ok", "legacy SSE resource read failed");
    assert(httpJson.promptText === "codeseex-http-mcp-prompt-ok:hello", "HTTP JSON prompt get failed");
    assert(httpSse.promptText === "codeseex-http-mcp-prompt-ok:hello", "HTTP SSE prompt get failed");
    assert(legacySse.promptText === "codeseex-http-mcp-prompt-ok:hello", "legacy SSE prompt get failed");

    const toolContext = normalizeTools([
      {
        type: "namespace",
        name: "mcp__codeseex_smoke__",
        description: "CodeSeeX generic MCP smoke namespace",
        tools: tools.map((tool) => ({
          type: "function",
          name: tool.name,
          description: tool.description,
          parameters: tool.inputSchema,
        })),
      },
      {
        type: "namespace",
        name: "mcp__codeseex_conflict__",
        description: "Namespace with a colliding child tool name",
        tools: [{
          type: "function",
          name: "codeseex_smoke_echo",
          description: "Second echo tool for collision testing",
          parameters: {
            type: "object",
            properties: { text: { type: "string" } },
            required: ["text"],
          },
        }],
      },
    ]);

    const names = toolContext.upstreamTools.map((tool) => tool.function && tool.function.name).filter(Boolean);
    assert(names.includes("codeseex_smoke_echo"), "primary namespace child tool was not exposed upstream");
    assert(names.includes("codeseex_smoke_add"), "add namespace child tool was not exposed upstream");
    const conflictName = names.find((name) => name !== "codeseex_smoke_echo" && name.endsWith("codeseex_smoke_echo"));
    assert(conflictName, "conflicting namespace child tool did not receive a stable unique model name");

    const primaryItem = responseToolItemFromChat({
      id: "call_primary",
      type: "function",
      function: { name: "codeseex_smoke_add", arguments: "{\"a\":21,\"b\":21}" },
    }, toolContext);
    assert(primaryItem.name === "codeseex_smoke_add", "primary response item lost original child tool name");
    assert(primaryItem.namespace === "mcp__codeseex_smoke__", "primary response item lost namespace");

    const conflictItem = responseToolItemFromChat({
      id: "call_conflict",
      type: "function",
      function: { name: conflictName, arguments: "{\"text\":\"hello\"}" },
    }, toolContext);
    assert(conflictItem.name === "codeseex_smoke_echo", "conflict response item did not restore child tool name");
    assert(conflictItem.namespace === "mcp__codeseex_conflict__", "conflict response item lost namespace");

    const replayedCall = chatToolCallFromResponseItem({
      type: "function_call",
      call_id: "call_replay",
      name: "codeseex_smoke_add",
      namespace: "mcp__codeseex_smoke__",
      arguments: "{\"a\":21,\"b\":21}",
    });
    assert(replayedCall.function.name === "codeseex_smoke_add", "history replay lost child tool name");
    assert(replayedCall.namespace === "mcp__codeseex_smoke__", "history replay lost namespace");

    const bridgedContext = normalizeTools([], {
      extraTools: [{
        type: "mcp",
        name: "codeseex_smoke",
        server_label: "codeseex_smoke",
        tools: [{
          type: "function",
          name: "codeseex_smoke_add",
          description: "Add two numbers",
          parameters: {
            type: "object",
            properties: { a: { type: "number" }, b: { type: "number" } },
            required: ["a", "b"],
          },
          mcp_server: "codeseex_smoke",
        }],
      }],
    });
    const bridgedNames = bridgedContext.upstreamTools.map((tool) => tool.function && tool.function.name).filter(Boolean);
    assert(bridgedNames.includes("codeseex_smoke_add"), "bridged MCP tool was not exposed upstream");
    const bridgedItem = responseToolItemFromChat({
      id: "call_bridge",
      type: "function",
      function: { name: "codeseex_smoke_add", arguments: "{\"a\":21,\"b\":21}" },
    }, bridgedContext);
    assert(bridgedItem.type === "proxy_tool_call", "bridged MCP call was not marked as proxy-hosted");
    assert(bridgedItem.mcp_server === "codeseex_smoke", "bridged MCP call lost server name");

    const bridgedPayloadContext = createToolContext([], {
      extraTools: [{
        type: "mcp",
        name: "codeseex_smoke",
        server_label: "codeseex_smoke",
        tools: [{
          type: "function",
          name: "codeseex_smoke_add",
          description: "Add two numbers",
          parameters: {
            type: "object",
            properties: { a: { type: "number" }, b: { type: "number" } },
            required: ["a", "b"],
          },
          mcp_server: "codeseex_smoke",
        }],
      }],
    });
    const payload = buildDeepSeekPayload(
      { model: "deepseek-v4-pro", stream: false },
      [{ role: "user", content: "call MCP" }],
      bridgedPayloadContext,
      {},
      { stream: false }
    );
    assert(Array.isArray(payload.tools), "DeepSeek payload did not include tools");
    assert(payload.tools.some((tool) => tool.function && tool.function.name === "codeseex_smoke_add"), "DeepSeek payload did not expose bridged MCP tool");

    console.log("MCP smoke test passed.");
    console.log("Discovered tools:", tools.map((tool) => tool.name).join(", "));
    console.log("Discovered resources:", resources.map((resource) => resource.uri).join(", "));
    console.log("Discovered resource templates:", resourceTemplates.map((template) => template.uriTemplate).join(", "));
    console.log("Discovered prompts:", prompts.map((item) => item.name).join(", "));
    console.log("Mapped upstream tools:", names.filter((name) => name.includes("codeseex_smoke")).join(", "));
    console.log("HTTP MCP smoke tests: JSON, SSE, resources, and prompts passed.");
    console.log("Legacy SSE MCP smoke test passed.");
  } finally {
    client.close();
  }
}

async function testHttpBridge(options) {
  if (options.bearerTokenEnvVar) process.env[options.bearerTokenEnvVar] = options.bearerTokenValue || "";
  const server = createHttpSmokeServer({ sse: Boolean(options.sse), expectedAuth: options.expectedAuth });
  await listen(server);
  const address = server.address();
  try {
    const bridge = await discoverMcpToolsFromServers([{
      name: options.serverName,
      enabled: true,
      transport: {
        type: "streamable_http",
        url: "http://127.0.0.1:" + address.port + "/mcp",
        bearer_token_env_var: options.bearerTokenEnvVar || "",
      },
    }]);
    assert(bridge.servers.length === 1, "HTTP bridge did not discover smoke MCP server");
    assert(
      bridge.servers[0].tools.some((tool) => tool.name === "codeseex_http_smoke_add"),
      "HTTP bridge did not discover add tool"
    );
    const result = await executeMcpToolCall({
      type: "proxy_tool_call",
      mcp_server: options.serverName,
      name: HTTP_SMOKE_ADD,
      arguments: "{\"a\":21,\"b\":21}",
    }, { mcpBridge: bridge });
    return {
      result,
      resourceText: await readHttpResourceText(bridge, options.serverName),
      promptText: await readHttpPromptText(bridge, options.serverName),
    };
  } finally {
    if (options.bearerTokenEnvVar) delete process.env[options.bearerTokenEnvVar];
    await closeServer(server);
  }
}

async function testLegacySseBridge(options) {
  const server = createLegacySseSmokeServer();
  await listen(server);
  const address = server.address();
  try {
    const bridge = await discoverMcpToolsFromServers([{
      name: options.serverName,
      enabled: true,
      transport: {
        type: "sse",
        url: "http://127.0.0.1:" + address.port + "/sse",
      },
    }]);
    assert(bridge.servers.length === 1, "legacy SSE bridge did not discover smoke MCP server");
    assert(
      bridge.servers[0].tools.some((tool) => tool.name === HTTP_SMOKE_ADD),
      "legacy SSE bridge did not discover add tool"
    );
    const result = await executeMcpToolCall({
      type: "proxy_tool_call",
      mcp_server: options.serverName,
      name: HTTP_SMOKE_ADD,
      arguments: "{\"a\":21,\"b\":21}",
    }, { mcpBridge: bridge });
    return {
      result,
      resourceText: await readHttpResourceText(bridge, options.serverName),
      promptText: await readHttpPromptText(bridge, options.serverName),
    };
  } finally {
    await closeServer(server);
  }
}

async function readHttpResourceText(bridge, serverName) {
  const result = await readMcpResource({
    server: serverName,
    uri: "codeseex-http-smoke://status",
  }, { mcpBridge: bridge });
  return textFromResourceResult(result);
}

async function readHttpPromptText(bridge, serverName) {
  const result = await getMcpPrompt({
    server: serverName,
    name: "codeseex_http_smoke_prompt",
    arguments: { topic: "hello" },
  }, { mcpBridge: bridge });
  return textFromPromptResult(result);
}

function startMcpServer() {
  const child = spawn(process.execPath, [serverPath], {
    stdio: ["pipe", "pipe", "pipe"],
    windowsHide: true,
  });
  let id = 1;
  let buffer = "";
  const pending = new Map();

  child.stderr.on("data", (chunk) => process.stderr.write(chunk));
  child.stdout.on("data", (chunk) => {
    buffer += chunk.toString("utf8");
    drain();
  });

  function drain() {
    while (true) {
      const newline = buffer.indexOf("\n");
      if (newline === -1) return;
      const raw = buffer.slice(0, newline).trim();
      buffer = buffer.slice(newline + 1);
      if (!raw) continue;
      const message = JSON.parse(raw);
      if (!message.id || !pending.has(message.id)) continue;
      const callbacks = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) callbacks.reject(new Error(message.error.message || JSON.stringify(message.error)));
      else callbacks.resolve(message.result);
    }
  }

  return {
    request(method, params) {
      const requestId = id++;
      child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: requestId, method, params }) + "\n");
      return new Promise((resolve, reject) => {
        pending.set(requestId, { resolve, reject });
        setTimeout(() => {
          if (!pending.has(requestId)) return;
          pending.delete(requestId);
          reject(new Error("Timed out waiting for MCP method: " + method));
        }, 5000).unref();
      });
    },
    notify(method, params) {
      child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n");
    },
    close() {
      child.kill();
    },
  };
}

function textFromMcpResult(result) {
  return (result.content || [])
    .map((item) => item && item.type === "text" ? item.text || "" : "")
    .join("");
}

function textFromResourceResult(result) {
  return (result.contents || [])
    .map((item) => item && typeof item.text === "string" ? item.text : "")
    .join("");
}

function textFromPromptResult(result) {
  return (result.messages || [])
    .map((message) => {
      const content = message && message.content;
      if (typeof content === "string") return content;
      if (content && typeof content.text === "string") return content.text;
      return "";
    })
    .join("");
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function listen(server) {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve();
    });
  });
}

function closeServer(server) {
  return new Promise((resolve) => server.close(resolve));
}
