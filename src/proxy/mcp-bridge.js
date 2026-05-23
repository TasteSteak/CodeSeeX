"use strict";

const { execFile } = require("node:child_process");
const { spawn } = require("node:child_process");

const packageJson = require("../../package.json");
const { codexCliInvocation } = require("../codex/model-catalog");

const DEFAULT_DISCOVERY_CACHE_MS = 120000;
const DEFAULT_CODEX_MCP_TIMEOUT_MS = 5000;
const DEFAULT_MCP_STARTUP_TIMEOUT_MS = 2500;
const DEFAULT_MCP_TOOL_TIMEOUT_MS = 60000;
const STREAMABLE_HTTP_PROTOCOL_VERSION = "2025-06-18";
const CLIENT_INFO = {
  name: "codeseex",
  version: String(packageJson.version || "0.0.0"),
};

let discoveryCache = {
  at: 0,
  key: "",
  value: null,
};

async function discoverCodexMcpTools(config = {}, options = {}) {
  if (config.mcpBridgeEnabled === false || options.enabled === false) {
    return emptyBridge();
  }

  const cacheKey = [
    process.env.CODEX_HOME || "",
    process.env.HOME || "",
    process.env.USERPROFILE || "",
    process.env.APPDATA || "",
  ].join("|");
  const cacheMs = Number(options.cacheMs || config.mcpDiscoveryCacheMs || DEFAULT_DISCOVERY_CACHE_MS);
  if (discoveryCache.value && discoveryCache.key === cacheKey && Date.now() - discoveryCache.at < cacheMs) {
    return discoveryCache.value;
  }

  try {
    const servers = await readCodexMcpServers(config);
    const bridge = await discoverMcpToolsFromServers(servers, config);
    discoveryCache = { at: Date.now(), key: cacheKey, value: bridge };
    return bridge;
  } catch (error) {
    const bridge = emptyBridge(error);
    discoveryCache = { at: Date.now(), key: cacheKey, value: bridge };
    return bridge;
  }
}

async function discoverMcpToolsFromServers(servers, config = {}) {
  const normalizedServers = (Array.isArray(servers) ? servers : [])
    .map(normalizeServerConfig)
    .filter((server) => server && server.enabled && isSupportedTransport(server.transport));
  const settled = await Promise.all(normalizedServers.map(async (server) => {
    try {
      return { ok: true, server: await discoverServerCapabilities(server, config) };
    } catch (error) {
      return { ok: false, error: { server: server.name, message: safeErrorMessage(error) } };
    }
  }));
  const output = settled.filter((item) => item.ok).map((item) => item.server);
  const errors = settled.filter((item) => !item.ok).map((item) => item.error);

  return {
    ok: output.length > 0 || errors.length === 0,
    servers: output,
    errors,
  };
}

function buildMcpInputTools(bridge = {}) {
  const tools = [];
  if (hasMcpResourceOrPromptCapabilities(bridge)) {
    tools.push({
      type: "mcp",
      name: "codeseex_mcp",
      server_label: "codeseex_mcp",
      description: "CodeSeeX MCP resource and prompt helpers",
      tools: mcpHelperToolDeclarations(),
    });
  }
  for (const server of Array.isArray(bridge.servers) ? bridge.servers : []) {
    const nested = (Array.isArray(server.tools) ? server.tools : [])
      .filter((tool) => tool && tool.name)
      .map((tool) => ({
        type: "function",
        name: String(tool.name),
        description: String(tool.description || tool.name),
        parameters: tool.inputSchema || { type: "object", properties: {} },
        mcp_server: String(server.name || ""),
      }));
    if (nested.length === 0) continue;
    tools.push({
      type: "mcp",
      name: String(server.name || ""),
      server_label: String(server.name || ""),
      description: "MCP server " + String(server.name || ""),
      tools: nested,
    });
  }
  return tools;
}

function mcpHelperToolDeclarations() {
  return [
    {
      type: "function",
      name: "list_mcp_resources",
      description: "List resources exposed by configured MCP servers.",
      parameters: {
        type: "object",
        properties: {
          server: { type: "string", description: "Optional MCP server name." },
        },
        additionalProperties: false,
      },
      mcp_helper: true,
    },
    {
      type: "function",
      name: "list_mcp_resource_templates",
      description: "List resource templates exposed by configured MCP servers.",
      parameters: {
        type: "object",
        properties: {
          server: { type: "string", description: "Optional MCP server name." },
        },
        additionalProperties: false,
      },
      mcp_helper: true,
    },
    {
      type: "function",
      name: "read_mcp_resource",
      description: "Read a resource exposed by a configured MCP server.",
      parameters: {
        type: "object",
        properties: {
          server: { type: "string" },
          uri: { type: "string" },
        },
        required: ["server", "uri"],
        additionalProperties: false,
      },
      mcp_helper: true,
    },
    {
      type: "function",
      name: "list_mcp_prompts",
      description: "List prompts exposed by configured MCP servers.",
      parameters: {
        type: "object",
        properties: {
          server: { type: "string", description: "Optional MCP server name." },
        },
        additionalProperties: false,
      },
      mcp_helper: true,
    },
    {
      type: "function",
      name: "get_mcp_prompt",
      description: "Read a prompt exposed by a configured MCP server.",
      parameters: {
        type: "object",
        properties: {
          server: { type: "string" },
          name: { type: "string" },
          arguments: { type: "object", additionalProperties: true },
        },
        required: ["server", "name"],
        additionalProperties: false,
      },
      mcp_helper: true,
    },
  ];
}

function hasMcpResourceOrPromptCapabilities(bridge = {}) {
  return (Array.isArray(bridge.servers) ? bridge.servers : []).some((server) => (
    (Array.isArray(server.resources) && server.resources.length > 0)
    || (Array.isArray(server.resourceTemplates) && server.resourceTemplates.length > 0)
    || (Array.isArray(server.prompts) && server.prompts.length > 0)
  ));
}

async function executeMcpToolCall(item, config = {}) {
  const bridge = config.mcpBridge && Array.isArray(config.mcpBridge.servers)
    ? config.mcpBridge
    : await discoverCodexMcpTools(config);
  const serverName = String(item && (item.mcp_server || item.server || "") || "");
  const toolName = String(item && item.name || "");
  const server = (bridge.servers || []).find((entry) => entry.name === serverName);
  if (!server) {
    return {
      ok: false,
      error: "mcp_server_not_found",
      server: serverName,
      tool: toolName,
    };
  }

  const client = createMcpClient(server, config);
  try {
    await client.start();
    const result = await client.request("tools/call", {
      name: toolName,
      arguments: parseJsonObject(item && item.arguments),
    }, timeoutMs(server.tool_timeout_sec, config.mcpToolTimeoutMs, DEFAULT_MCP_TOOL_TIMEOUT_MS));
    return normalizeToolResult(result, serverName, toolName);
  } catch (error) {
    return {
      ok: false,
      error: "mcp_tool_call_failed",
      server: serverName,
      tool: toolName,
      message: safeErrorMessage(error),
    };
  } finally {
    client.close();
  }
}

async function executeMcpHelperCall(item, config = {}) {
  const name = String(item && item.name || "");
  const args = parseJsonObject(item && item.arguments);
  if (name === "list_mcp_resources") return listMcpResources(args, config);
  if (name === "list_mcp_resource_templates") return listMcpResourceTemplates(args, config);
  if (name === "read_mcp_resource") return readMcpResource(args, config);
  if (name === "list_mcp_prompts") return listMcpPrompts(args, config);
  if (name === "get_mcp_prompt") return getMcpPrompt(args, config);
  return { ok: false, error: "unsupported_mcp_helper", helper: name };
}

async function listMcpResources(args = {}, config = {}) {
  const bridge = await bridgeFromConfig(config);
  const resources = [];
  for (const server of filterBridgeServers(bridge, args.server)) {
    const listed = Array.isArray(server.resources) ? server.resources : [];
    resources.push(...listed.map((resource) => Object.assign({ server: server.name }, resource)));
  }
  return { resources };
}

async function listMcpResourceTemplates(args = {}, config = {}) {
  const bridge = await bridgeFromConfig(config);
  const resourceTemplates = [];
  for (const server of filterBridgeServers(bridge, args.server)) {
    const listed = Array.isArray(server.resourceTemplates) ? server.resourceTemplates : [];
    resourceTemplates.push(...listed.map((template) => Object.assign({ server: server.name }, template)));
  }
  return { resourceTemplates };
}

async function readMcpResource(args = {}, config = {}) {
  const serverName = String(args.server || "").trim();
  const uri = String(args.uri || "").trim();
  if (!serverName) return { ok: false, error: "missing_mcp_server" };
  if (!uri) return { ok: false, error: "missing_mcp_resource_uri" };
  const server = await resolveBridgeServer(serverName, config);
  if (!server) return { ok: false, error: "mcp_server_not_found", server: serverName };
  return requestMcpServer(server, "resources/read", { uri }, config);
}

async function listMcpPrompts(args = {}, config = {}) {
  const bridge = await bridgeFromConfig(config);
  const prompts = [];
  for (const server of filterBridgeServers(bridge, args.server)) {
    const listed = Array.isArray(server.prompts) ? server.prompts : [];
    prompts.push(...listed.map((prompt) => Object.assign({ server: server.name }, prompt)));
  }
  return { prompts };
}

async function getMcpPrompt(args = {}, config = {}) {
  const serverName = String(args.server || "").trim();
  const name = String(args.name || args.prompt || "").trim();
  if (!serverName) return { ok: false, error: "missing_mcp_server" };
  if (!name) return { ok: false, error: "missing_mcp_prompt_name" };
  const server = await resolveBridgeServer(serverName, config);
  if (!server) return { ok: false, error: "mcp_server_not_found", server: serverName };
  return requestMcpServer(server, "prompts/get", { name, arguments: parseJsonObject(args.arguments) }, config);
}

async function readCodexMcpServers(config = {}) {
  const timeout = Number(config.codexMcpTimeoutMs || DEFAULT_CODEX_MCP_TIMEOUT_MS);
  const listed = await execCodexJson(["mcp", "list", "--json"], timeout);
  if (!Array.isArray(listed)) return [];

  const servers = [];
  for (const server of listed) {
    if (!server || server.enabled === false) continue;
    try {
      const detailed = await execCodexJson(["mcp", "get", String(server.name || ""), "--json"], timeout);
      servers.push(Object.assign({}, server, detailed || {}));
    } catch {
      servers.push(server);
    }
  }
  return servers;
}

function execCodexJson(args, timeoutMs) {
  const invocation = codexCliInvocation();
  return new Promise((resolve, reject) => {
    const child = execFile(invocation.command, invocation.args.concat(args), {
      encoding: "utf8",
      env: process.env,
      windowsHide: true,
      timeout: timeoutMs,
      maxBuffer: 16 * 1024 * 1024,
    }, (error, stdout, stderr) => {
      if (error) {
        const message = stderr && String(stderr).trim() ? String(stderr).trim() : error.message;
        reject(new Error(message));
        return;
      }
      try {
        resolve(JSON.parse(String(stdout || "").replace(/^\uFEFF/, "")));
      } catch (parseError) {
        reject(parseError);
      }
    });
    child.on("error", reject);
  });
}

async function discoverServerCapabilities(server, config = {}) {
  const client = createMcpClient(server, config);
  try {
    const initialized = await client.start();
    const tools = await listMcpCollection(client, "tools/list", "tools", server, config);
    const resources = await listMcpCollection(client, "resources/list", "resources", server, config);
    const resourceTemplates = await listMcpCollection(client, "resources/templates/list", "resourceTemplates", server, config);
    const prompts = await listMcpCollection(client, "prompts/list", "prompts", server, config);
    return Object.assign({}, server, {
      capabilities: initialized && initialized.capabilities ? initialized.capabilities : {},
      tools: filterServerTools(tools, server),
      resources: normalizeResources(resources),
      resourceTemplates: normalizeResourceTemplates(resourceTemplates),
      prompts: normalizePrompts(prompts),
    });
  } finally {
    client.close();
  }
}

async function listMcpCollection(client, method, field, server, config = {}) {
  const output = [];
  let cursor = null;
  for (let page = 0; page < 20; page += 1) {
    let result = null;
    try {
      const params = cursor ? { cursor } : {};
      result = await client.request(method, params, timeoutMs(server.tool_timeout_sec, config.mcpToolTimeoutMs, DEFAULT_MCP_TOOL_TIMEOUT_MS));
    } catch (error) {
      if (isUnsupportedMcpMethodError(error)) return output;
      throw error;
    }
    const items = Array.isArray(result && result[field]) ? result[field] : [];
    output.push(...items);
    cursor = result && (result.nextCursor || result.next_cursor);
    if (!cursor) break;
  }
  return output;
}

async function requestMcpServer(server, method, params, config = {}) {
  const client = createMcpClient(server, config);
  try {
    await client.start();
    return await client.request(method, params || {}, timeoutMs(server.tool_timeout_sec, config.mcpToolTimeoutMs, DEFAULT_MCP_TOOL_TIMEOUT_MS));
  } catch (error) {
    return {
      ok: false,
      error: "mcp_request_failed",
      server: server.name,
      method,
      message: safeErrorMessage(error),
    };
  } finally {
    client.close();
  }
}

function createMcpClient(server, config = {}) {
  const options = {
    startupTimeoutMs: timeoutMs(server.startup_timeout_sec, config.mcpStartupTimeoutMs, DEFAULT_MCP_STARTUP_TIMEOUT_MS),
    toolTimeoutMs: timeoutMs(server.tool_timeout_sec, config.mcpToolTimeoutMs, DEFAULT_MCP_TOOL_TIMEOUT_MS),
  };
  if (server && server.transport && server.transport.type === "sse") return new LegacySseMcpClient(server, options);
  if (server && server.transport && isHttpTransport(server.transport)) return new HttpMcpClient(server, options);
  return new StdioMcpClient(server, options);
}

class StdioMcpClient {
  constructor(server, options = {}) {
    this.server = server;
    this.options = options;
    this.child = null;
    this.buffer = Buffer.alloc(0);
    this.pending = new Map();
    this.nextId = 1;
    this.stderr = "";
  }

  async start() {
    const transport = this.server.transport || {};
    const command = transport.command;
    if (!command) throw new Error("MCP stdio command is missing.");

    this.child = spawn(command, Array.isArray(transport.args) ? transport.args : [], {
      cwd: transport.cwd || undefined,
      env: Object.assign({}, process.env, transport.env || {}),
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    this.child.stdout.on("data", (chunk) => this.onData(chunk));
    this.child.stderr.on("data", (chunk) => {
      this.stderr = (this.stderr + chunk.toString("utf8")).slice(-4000);
    });
    this.child.on("error", (error) => this.rejectAll(error));
    this.child.on("exit", (code, signal) => {
      if (this.pending.size > 0) {
        this.rejectAll(new Error("MCP server exited before replying: code=" + code + " signal=" + signal));
      }
    });

    await this.request("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: CLIENT_INFO,
    }, this.options.startupTimeoutMs || DEFAULT_MCP_STARTUP_TIMEOUT_MS);
    this.notify("notifications/initialized", {});
  }

  request(method, params, timeoutMs) {
    const id = this.nextId++;
    this.write({ jsonrpc: "2.0", id, method, params: params || {} });
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        if (!this.pending.has(id)) return;
        this.pending.delete(id);
        reject(new Error("Timed out waiting for MCP method: " + method));
      }, timeoutMs || DEFAULT_MCP_TOOL_TIMEOUT_MS);
      if (timer.unref) timer.unref();
      this.pending.set(id, { resolve, reject, timer });
    });
  }

  notify(method, params) {
    this.write({ jsonrpc: "2.0", method, params: params || {} });
  }

  write(message) {
    if (!this.child || !this.child.stdin || this.child.stdin.destroyed) {
      throw new Error("MCP server stdin is not available.");
    }
    const raw = JSON.stringify(message);
    this.child.stdin.write("Content-Length: " + Buffer.byteLength(raw, "utf8") + "\r\n\r\n" + raw);
  }

  onData(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (this.buffer.length > 0) {
      const framed = this.tryReadFramedMessage();
      if (framed === null) {
        const line = this.tryReadLineMessage();
        if (line === null) return;
        this.handleMessage(line);
        continue;
      }
      this.handleMessage(framed);
    }
  }

  tryReadFramedMessage() {
    const marker = Buffer.from("\r\n\r\n");
    const headerEnd = this.buffer.indexOf(marker);
    if (headerEnd === -1) {
      if (this.buffer[0] === 123) return null;
      return null;
    }
    const header = this.buffer.slice(0, headerEnd).toString("utf8");
    const match = header.match(/(?:^|\r\n)Content-Length:\s*(\d+)/i);
    if (!match) return null;
    const length = Number(match[1]);
    const bodyStart = headerEnd + marker.length;
    const bodyEnd = bodyStart + length;
    if (!Number.isFinite(length) || length < 0 || this.buffer.length < bodyEnd) return null;
    const raw = this.buffer.slice(bodyStart, bodyEnd).toString("utf8");
    this.buffer = this.buffer.slice(bodyEnd);
    return parseJson(raw);
  }

  tryReadLineMessage() {
    const lineEnd = this.buffer.indexOf(10);
    if (lineEnd === -1) return null;
    const raw = this.buffer.slice(0, lineEnd).toString("utf8").trim();
    this.buffer = this.buffer.slice(lineEnd + 1);
    return raw ? parseJson(raw) : {};
  }

  handleMessage(message) {
    if (!message || typeof message !== "object" || !Object.prototype.hasOwnProperty.call(message, "id")) return;
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    clearTimeout(pending.timer);
    if (message.error) {
      pending.reject(new Error(message.error.message || JSON.stringify(message.error)));
    } else {
      pending.resolve(message.result);
    }
  }

  rejectAll(error) {
    for (const [id, pending] of this.pending.entries()) {
      this.pending.delete(id);
      clearTimeout(pending.timer);
      pending.reject(error);
    }
  }

  close() {
    this.rejectAll(new Error("MCP client closed."));
    if (!this.child) return;
    try {
      this.child.kill();
    } catch {}
    this.child = null;
  }
}

class HttpMcpClient {
  constructor(server, options = {}) {
    this.server = server;
    this.options = options;
    this.nextId = 1;
    this.sessionId = "";
    this.protocolVersion = STREAMABLE_HTTP_PROTOCOL_VERSION;
  }

  async start() {
    const result = await this.request("initialize", {
      protocolVersion: this.protocolVersion,
      capabilities: {},
      clientInfo: CLIENT_INFO,
    }, this.options.startupTimeoutMs || DEFAULT_MCP_STARTUP_TIMEOUT_MS);
    if (result && result.protocolVersion) this.protocolVersion = String(result.protocolVersion);
    await this.notify("notifications/initialized", {});
  }

  async request(method, params, timeoutMs) {
    const id = this.nextId++;
    const message = { jsonrpc: "2.0", id, method, params: params || {} };
    const response = await this.post(message, timeoutMs || this.options.toolTimeoutMs || DEFAULT_MCP_TOOL_TIMEOUT_MS);
    return resultFromJsonRpcResponse(response, id);
  }

  async notify(method, params) {
    await this.post({ jsonrpc: "2.0", method, params: params || {} }, this.options.toolTimeoutMs || DEFAULT_MCP_TOOL_TIMEOUT_MS, {
      notification: true,
    });
  }

  async post(message, timeoutMs, options = {}) {
    const transport = this.server.transport || {};
    const url = transport.url;
    if (!url) throw new Error("MCP HTTP URL is missing.");

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs || DEFAULT_MCP_TOOL_TIMEOUT_MS);
    if (timer.unref) timer.unref();
    try {
      const response = await fetch(url, {
        method: "POST",
        headers: this.headers(),
        body: JSON.stringify(message),
        signal: controller.signal,
      });
      const sessionId = response.headers.get("mcp-session-id");
      if (sessionId) this.sessionId = sessionId;

      if (options.notification && response.status === 202) return null;
      if (!response.ok) {
        const text = await safeResponseText(response);
        throw new Error("MCP HTTP request failed: status=" + response.status + " body=" + safeErrorMessage(text));
      }

      const contentType = String(response.headers.get("content-type") || "").toLowerCase();
      if (contentType.includes("text/event-stream")) {
        return readSseJsonRpcResponse(response, message.id);
      }
      if (response.status === 202) return null;
      return readJsonRpcResponse(response, message.id);
    } catch (error) {
      if (error && error.name === "AbortError") {
        throw new Error("Timed out waiting for MCP HTTP method: " + (message.method || "unknown"));
      }
      throw error;
    } finally {
      clearTimeout(timer);
    }
  }

  headers() {
    const transport = this.server.transport || {};
    const headers = Object.assign({}, transport.http_headers || {});
    const envHeaders = transport.env_http_headers || {};
    for (const [name, envName] of Object.entries(envHeaders)) {
      const value = process.env[String(envName || "")];
      if (value) headers[name] = value;
    }
    const bearerEnv = String(transport.bearer_token_env_var || "").trim();
    if (bearerEnv && process.env[bearerEnv] && !hasHeader(headers, "authorization")) {
      headers.Authorization = "Bearer " + process.env[bearerEnv];
    }
    headers["Content-Type"] = "application/json";
    headers.Accept = "application/json, text/event-stream";
    headers["MCP-Protocol-Version"] = this.protocolVersion || STREAMABLE_HTTP_PROTOCOL_VERSION;
    if (this.sessionId) headers["Mcp-Session-Id"] = this.sessionId;
    return headers;
  }

  close() {}
}

class LegacySseMcpClient extends HttpMcpClient {
  constructor(server, options = {}) {
    super(server, options);
    this.messageEndpoint = "";
    this.eventReader = null;
    this.eventResponse = null;
    this.eventBuffer = "";
    this.pendingEvents = new Map();
    this.bufferedResponses = new Map();
    this.reading = false;
    this.eventDecoder = new TextDecoder();
  }

  async start() {
    const transport = this.server.transport || {};
    if (!transport.url) throw new Error("MCP SSE URL is missing.");
    await this.openEventStream(transport.url, this.options.startupTimeoutMs || DEFAULT_MCP_STARTUP_TIMEOUT_MS);
    return super.start();
  }

  async openEventStream(url, timeoutMs) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs || DEFAULT_MCP_STARTUP_TIMEOUT_MS);
    if (timer.unref) timer.unref();
    try {
      const response = await fetch(url, {
        method: "GET",
        headers: Object.assign({}, this.headers(), { Accept: "text/event-stream" }),
        signal: controller.signal,
      });
      if (!response.ok) {
        const text = await safeResponseText(response);
        throw new Error("MCP SSE endpoint failed: status=" + response.status + " body=" + safeErrorMessage(text));
      }
      if (!response.body || typeof response.body.getReader !== "function") {
        const text = await response.text();
        const endpoint = endpointFromSseText(text);
        if (!endpoint) throw new Error("MCP SSE endpoint event was not received.");
        this.messageEndpoint = new URL(endpoint, url).toString();
        return;
      }
      this.eventResponse = response;
      this.eventReader = response.body.getReader();
      const endpoint = await this.readUntilEndpoint(url, timeoutMs);
      if (!endpoint) throw new Error("MCP SSE endpoint event was not received.");
      this.messageEndpoint = endpoint;
    } catch (error) {
      if (error && error.name === "AbortError") throw new Error("Timed out waiting for MCP SSE endpoint.");
      throw error;
    } finally {
      clearTimeout(timer);
    }
  }

  async readUntilEndpoint(url, timeoutMs) {
    const deadline = Date.now() + (timeoutMs || DEFAULT_MCP_STARTUP_TIMEOUT_MS);
    while (Date.now() < deadline) {
      const events = await this.readNextSseEvents();
      for (const event of events) {
        if (event.event === "endpoint" && event.data) return new URL(event.data, url).toString();
        this.dispatchSseEvent(event);
      }
    }
    return "";
  }

  async post(message, timeoutMs, options = {}) {
    if (!this.messageEndpoint) throw new Error("MCP SSE message endpoint is missing.");
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs || DEFAULT_MCP_TOOL_TIMEOUT_MS);
    if (timer.unref) timer.unref();
    try {
      const response = await fetch(this.messageEndpoint, {
        method: "POST",
        headers: this.headers(),
        body: JSON.stringify(message),
        signal: controller.signal,
      });
      if (options.notification && (response.status === 202 || response.status === 204)) return null;
      if (!response.ok) {
        const text = await safeResponseText(response);
        throw new Error("MCP SSE request failed: status=" + response.status + " body=" + safeErrorMessage(text));
      }
      const contentType = String(response.headers.get("content-type") || "").toLowerCase();
      if (contentType.includes("text/event-stream")) return readSseJsonRpcResponse(response, message.id);
      if (response.status === 202 || response.status === 204) {
        if (options.notification) return null;
        return await this.waitForEventResponse(message.id, timeoutMs);
      }
      return readJsonRpcResponse(response, message.id);
    } catch (error) {
      if (error && error.name === "AbortError") {
        throw new Error("Timed out waiting for MCP SSE method: " + (message.method || "unknown"));
      }
      throw error;
    } finally {
      clearTimeout(timer);
    }
  }

  async waitForEventResponse(id, timeoutMs) {
    const existing = this.takeBufferedEventResponse(id);
    if (existing) return existing;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pendingEvents.delete(id);
        reject(new Error("Timed out waiting for MCP SSE event response id: " + id));
      }, timeoutMs || DEFAULT_MCP_TOOL_TIMEOUT_MS);
      if (timer.unref) timer.unref();
      this.pendingEvents.set(id, { resolve, reject, timer });
      this.ensureEventPump();
    });
  }

  takeBufferedEventResponse(id) {
    if (this.bufferedResponses.has(id)) {
      const message = this.bufferedResponses.get(id);
      this.bufferedResponses.delete(id);
      return message;
    }
    const result = consumeSseMessages(this.eventBuffer, id);
    this.eventBuffer = result.remainder;
    return result.message || null;
  }

  ensureEventPump() {
    if (this.reading || !this.eventReader) return;
    this.reading = true;
    this.pumpEvents().finally(() => {
      this.reading = false;
    });
  }

  async pumpEvents() {
    try {
      while (this.pendingEvents.size > 0 && this.eventReader) {
        const events = await this.readNextSseEvents();
        if (events.length === 0) break;
        for (const event of events) this.dispatchSseEvent(event);
      }
    } catch (error) {
      for (const [id, pending] of this.pendingEvents.entries()) {
        this.pendingEvents.delete(id);
        clearTimeout(pending.timer);
        pending.reject(error);
      }
    }
  }

  async readNextSseEvents() {
    if (!this.eventReader) return [];
    while (true) {
      const separator = this.eventBuffer.replace(/\r\n/g, "\n").indexOf("\n\n");
      if (separator >= 0) {
        const normalized = this.eventBuffer.replace(/\r\n/g, "\n");
        const rawEvent = normalized.slice(0, separator + 2);
        this.eventBuffer = normalized.slice(separator + 2);
        return parseSseEvents(rawEvent);
      }
      const chunk = await this.eventReader.read();
      if (chunk.done) return [];
      this.eventBuffer += this.eventDecoder.decode(chunk.value, { stream: true });
    }
  }

  dispatchSseEvent(event) {
    if (!event || !event.data) return;
    const message = parseJson(event.data);
    if (!message || typeof message !== "object" || !Object.prototype.hasOwnProperty.call(message, "id")) return;
    const pending = this.pendingEvents.get(message.id);
    if (!pending) {
      this.bufferedResponses.set(message.id, message);
      return;
    }
    this.pendingEvents.delete(message.id);
    clearTimeout(pending.timer);
    pending.resolve(message);
  }

  close() {
    for (const [id, pending] of this.pendingEvents.entries()) {
      this.pendingEvents.delete(id);
      clearTimeout(pending.timer);
      pending.reject(new Error("MCP SSE client closed."));
    }
    if (this.eventReader) {
      try {
        this.eventReader.cancel();
      } catch {}
    }
    this.eventReader = null;
    this.eventResponse = null;
    this.bufferedResponses.clear();
  }
}

function normalizeServerConfig(server) {
  if (!server || typeof server !== "object") return null;
  const transport = normalizeTransport(server.transport || server);
  if (!transport) return null;
  return {
    name: String(server.name || "").trim(),
    enabled: server.enabled !== false && !server.disabled_reason,
    disabled_reason: server.disabled_reason || null,
    transport,
    enabled_tools: Array.isArray(server.enabled_tools) ? server.enabled_tools.map(String) : null,
    disabled_tools: Array.isArray(server.disabled_tools) ? server.disabled_tools.map(String) : null,
    startup_timeout_sec: server.startup_timeout_sec,
    tool_timeout_sec: server.tool_timeout_sec,
  };
}

function normalizeTransport(transport) {
  if (!transport || typeof transport !== "object") return null;
  const type = String(transport.type || "stdio").toLowerCase();
  if (type !== "stdio") {
    if (type === "streamable_http" || type === "http") {
      return {
        type: "streamable_http",
        url: String(transport.url || "").trim(),
        bearer_token_env_var: transport.bearer_token_env_var ? String(transport.bearer_token_env_var) : "",
        http_headers: normalizeHeaderMap(transport.http_headers),
        env_http_headers: normalizeHeaderMap(transport.env_http_headers),
      };
    }
    if (type === "sse") {
      return {
        type: "sse",
        url: String(transport.url || "").trim(),
        bearer_token_env_var: transport.bearer_token_env_var ? String(transport.bearer_token_env_var) : "",
        http_headers: normalizeHeaderMap(transport.http_headers),
        env_http_headers: normalizeHeaderMap(transport.env_http_headers),
      };
    }
    return { type };
  }
  return {
    type,
    command: String(transport.command || "").trim(),
    args: Array.isArray(transport.args) ? transport.args.map(String) : [],
    env: transport.env && typeof transport.env === "object" ? Object.assign({}, transport.env) : {},
    cwd: transport.cwd ? String(transport.cwd) : null,
  };
}

function isSupportedTransport(transport) {
  return Boolean(transport && (transport.type === "stdio" || isHttpTransport(transport) || transport.type === "sse"));
}

function isHttpTransport(transport) {
  return Boolean(transport && (transport.type === "streamable_http" || transport.type === "http"));
}

function normalizeHeaderMap(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const output = {};
  for (const [key, entry] of Object.entries(value)) {
    const name = String(key || "").trim();
    if (!name) continue;
    output[name] = String(entry || "");
  }
  return output;
}

function filterServerTools(tools, server) {
  const enabled = server.enabled_tools ? new Set(server.enabled_tools) : null;
  const disabled = server.disabled_tools ? new Set(server.disabled_tools) : null;
  return (Array.isArray(tools) ? tools : [])
    .filter((tool) => tool && typeof tool === "object" && tool.name)
    .filter((tool) => !enabled || enabled.has(String(tool.name)))
    .filter((tool) => !disabled || !disabled.has(String(tool.name)))
    .map((tool) => ({
      name: String(tool.name),
      description: String(tool.description || tool.name),
      inputSchema: tool.inputSchema || tool.input_schema || tool.parameters || { type: "object", properties: {} },
    }));
}

function normalizeResources(resources) {
  return (Array.isArray(resources) ? resources : [])
    .filter((resource) => resource && typeof resource === "object" && resource.uri)
    .map((resource) => ({
      uri: String(resource.uri),
      name: resource.name ? String(resource.name) : String(resource.uri),
      description: resource.description ? String(resource.description) : "",
      mimeType: resource.mimeType || resource.mime_type || "",
    }));
}

function normalizeResourceTemplates(templates) {
  return (Array.isArray(templates) ? templates : [])
    .filter((template) => template && typeof template === "object" && (template.uriTemplate || template.uri_template))
    .map((template) => ({
      uriTemplate: String(template.uriTemplate || template.uri_template),
      name: template.name ? String(template.name) : String(template.uriTemplate || template.uri_template),
      description: template.description ? String(template.description) : "",
      mimeType: template.mimeType || template.mime_type || "",
    }));
}

function normalizePrompts(prompts) {
  return (Array.isArray(prompts) ? prompts : [])
    .filter((prompt) => prompt && typeof prompt === "object" && prompt.name)
    .map((prompt) => ({
      name: String(prompt.name),
      title: prompt.title ? String(prompt.title) : "",
      description: prompt.description ? String(prompt.description) : "",
      arguments: Array.isArray(prompt.arguments) ? prompt.arguments : [],
    }));
}

function normalizeToolResult(result, server, tool) {
  const content = normalizeMcpContent(result && result.content);
  return {
    ok: !(result && result.isError),
    server,
    tool,
    is_error: Boolean(result && result.isError),
    content,
  };
}

function bridgeFromConfig(config = {}) {
  return config.mcpBridge && Array.isArray(config.mcpBridge.servers)
    ? Promise.resolve(config.mcpBridge)
    : discoverCodexMcpTools(config);
}

function filterBridgeServers(bridge = {}, serverName = "") {
  const servers = Array.isArray(bridge.servers) ? bridge.servers : [];
  const wanted = String(serverName || "").trim();
  if (!wanted) return servers;
  return servers.filter((server) => server && server.name === wanted);
}

async function resolveBridgeServer(serverName, config = {}) {
  const bridge = await bridgeFromConfig(config);
  return filterBridgeServers(bridge, serverName)[0] || null;
}

function normalizeMcpContent(content) {
  const items = Array.isArray(content) ? content : [];
  if (items.length === 1 && items[0] && items[0].type === "text") return String(items[0].text || "");
  return items.map((item) => {
    if (!item || typeof item !== "object") return item;
    if (item.type === "text") return { type: "text", text: String(item.text || "") };
    if (item.type === "image") return { type: "image", mimeType: item.mimeType || item.mime_type || "", data: item.data || "" };
    if (item.type === "resource") return { type: "resource", resource: item.resource || null };
    return item;
  });
}

function timeoutMs(seconds, configuredMs, fallbackMs) {
  const fromSeconds = Number(seconds);
  if (Number.isFinite(fromSeconds) && fromSeconds > 0) return Math.max(1, Math.floor(fromSeconds * 1000));
  const fromConfig = Number(configuredMs);
  if (Number.isFinite(fromConfig) && fromConfig > 0) return Math.floor(fromConfig);
  return fallbackMs;
}

function parseJsonObject(value) {
  if (!value) return {};
  if (typeof value === "object") return value;
  try {
    const parsed = JSON.parse(String(value));
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

function parseJson(raw) {
  try {
    return JSON.parse(String(raw || ""));
  } catch {
    return null;
  }
}

async function readJsonRpcResponse(response, id) {
  const text = await response.text();
  if (!text.trim()) return null;
  const parsed = parseJson(text);
  const message = findJsonRpcResponse(parsed, id);
  if (!message) throw new Error("MCP HTTP response did not include a matching JSON-RPC response.");
  return message;
}

async function readSseJsonRpcResponse(response, id) {
  if (!response.body || typeof response.body.getReader !== "function") {
    const text = await response.text();
    const message = findJsonRpcResponse(parseSseMessages(text), id);
    if (!message) throw new Error("MCP SSE response did not include a matching JSON-RPC response.");
    return message;
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  try {
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      buffer += decoder.decode(chunk.value, { stream: true });
      const result = consumeSseMessages(buffer, id);
      buffer = result.remainder;
      if (result.message) return result.message;
    }
    buffer += decoder.decode();
    const result = consumeSseMessages(buffer + "\n\n", id);
    if (result.message) return result.message;
    throw new Error("MCP SSE response did not include a matching JSON-RPC response.");
  } finally {
    try {
      await reader.cancel();
    } catch {}
  }
}

function consumeSseMessages(input, id) {
  let buffer = String(input || "").replace(/\r\n/g, "\n");
  let message = null;
  while (true) {
    const separator = buffer.indexOf("\n\n");
    if (separator === -1) break;
    const rawEvent = buffer.slice(0, separator);
    buffer = buffer.slice(separator + 2);
    for (const parsed of parseSseMessages(rawEvent + "\n\n")) {
      const matched = findJsonRpcResponse(parsed, id);
      if (matched) message = matched;
    }
    if (message) break;
  }
  return { message, remainder: buffer };
}

function parseSseMessages(text) {
  const events = [];
  for (const event of parseSseEvents(text)) {
    if (!event.data) continue;
    const parsed = parseJson(event.data);
    if (parsed) events.push(parsed);
  }
  return events;
}

function parseSseEvents(text) {
  const events = [];
  const normalized = String(text || "").replace(/\r\n/g, "\n");
  for (const rawEvent of normalized.split("\n\n")) {
    const lines = rawEvent.split("\n");
    let event = "message";
    const data = [];
    for (const line of lines) {
      if (line.startsWith("event:")) event = line.slice(6).trim();
      else if (line.startsWith("data:")) data.push(line.slice(5).trimStart());
    }
    if (data.length > 0) events.push({ event, data: data.join("\n").trim() });
  }
  return events;
}

function endpointFromSseText(text) {
  const event = parseSseEvents(text).find((item) => item.event === "endpoint");
  return event ? event.data : "";
}

function findJsonRpcResponse(value, id) {
  const items = Array.isArray(value) ? value : [value];
  return items.find((item) => item && typeof item === "object" && item.id === id) || null;
}

function resultFromJsonRpcResponse(message, id) {
  if (!message || typeof message !== "object") {
    throw new Error("MCP response missing for id: " + id);
  }
  if (message.error) throw new Error(message.error.message || JSON.stringify(message.error));
  return message.result;
}

async function safeResponseText(response) {
  try {
    return await response.text();
  } catch {
    return "";
  }
}

function hasHeader(headers, name) {
  const normalized = String(name || "").toLowerCase();
  return Object.keys(headers || {}).some((key) => String(key).toLowerCase() === normalized);
}

function safeErrorMessage(error) {
  return String(error && error.message ? error.message : error || "unknown_error")
    .replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer [redacted]")
    .slice(0, 500);
}

function isUnsupportedMcpMethodError(error) {
  const message = String(error && error.message ? error.message : error || "");
  return /\bmethod_not_found\b|Method not found|not found|unsupported/i.test(message);
}

function emptyBridge(error = null) {
  return {
    ok: !error,
    servers: [],
    errors: error ? [{ server: "", message: safeErrorMessage(error) }] : [],
  };
}

module.exports = {
  buildMcpInputTools,
  executeMcpHelperCall,
  HttpMcpClient,
  LegacySseMcpClient,
  StdioMcpClient,
  discoverCodexMcpTools,
  discoverMcpToolsFromServers,
  executeMcpToolCall,
  getMcpPrompt,
  listMcpPrompts,
  listMcpResourceTemplates,
  listMcpResources,
  normalizeServerConfig,
  readMcpResource,
};
