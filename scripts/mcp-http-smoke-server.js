"use strict";

const http = require("node:http");

const TOOLS = [
  {
    name: "codeseex_http_smoke_echo",
    description: "Return the provided text with a CodeSeeX HTTP MCP smoke-test marker.",
    inputSchema: {
      type: "object",
      properties: {
        text: { type: "string" },
      },
      required: ["text"],
      additionalProperties: false,
    },
  },
  {
    name: "codeseex_http_smoke_add",
    description: "Add two numbers over streamable HTTP and return the numeric result as text.",
    inputSchema: {
      type: "object",
      properties: {
        a: { type: "number" },
        b: { type: "number" },
      },
      required: ["a", "b"],
      additionalProperties: false,
    },
  },
];
const RESOURCES = [
  {
    uri: "codeseex-http-smoke://status",
    name: "CodeSeeX HTTP MCP smoke status",
    description: "Static status resource for HTTP MCP resource-read compatibility tests.",
    mimeType: "text/plain",
  },
];
const RESOURCE_TEMPLATES = [
  {
    uriTemplate: "codeseex-http-smoke://echo/{text}",
    name: "CodeSeeX HTTP MCP smoke echo template",
    description: "Echo a path variable from a templated HTTP MCP resource URI.",
    mimeType: "text/plain",
  },
];
const PROMPTS = [
  {
    name: "codeseex_http_smoke_prompt",
    description: "Return a smoke-test prompt message over HTTP MCP.",
    arguments: [
      {
        name: "topic",
        description: "Prompt topic.",
        required: false,
      },
    ],
  },
];

function createHttpSmokeServer(options = {}) {
  const sse = Boolean(options.sse);
  const expectedAuth = options.expectedAuth || "";
  const server = http.createServer(async (req, res) => {
    if (req.method !== "POST" || req.url !== "/mcp") {
      sendJson(res, 404, { error: "not_found" });
      return;
    }
    if (expectedAuth && req.headers.authorization !== expectedAuth) {
      sendJson(res, 401, { error: "missing_authorization" });
      return;
    }

    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("end", () => {
      const message = parseJson(body);
      if (!message || typeof message !== "object") {
        sendJson(res, 400, { error: "invalid_json" });
        return;
      }
      if (!Object.prototype.hasOwnProperty.call(message, "id")) {
        res.writeHead(202, sessionHeaders({ "Content-Type": "text/plain" }));
        res.end("");
        return;
      }

      const response = handleMessage(message);
      if (sse) {
        sendSse(res, response);
      } else {
        sendJson(res, 200, response, sessionHeaders());
      }
    });
  });
  return server;
}

function handleMessage(message) {
  try {
    if (message.method === "initialize") {
      return reply(message.id, {
        protocolVersion: "2025-06-18",
        capabilities: { tools: {}, resources: {}, prompts: {} },
        serverInfo: { name: "codeseex-http-smoke", version: "0.0.0" },
      });
    }
    if (message.method === "tools/list") {
      return reply(message.id, { tools: TOOLS });
    }
    if (message.method === "tools/call") {
      return reply(message.id, callTool(message.params || {}));
    }
    if (message.method === "resources/list") {
      return reply(message.id, { resources: RESOURCES });
    }
    if (message.method === "resources/templates/list") {
      return reply(message.id, { resourceTemplates: RESOURCE_TEMPLATES });
    }
    if (message.method === "resources/read") {
      return reply(message.id, readResource(message.params || {}));
    }
    if (message.method === "prompts/list") {
      return reply(message.id, { prompts: PROMPTS });
    }
    if (message.method === "prompts/get") {
      return reply(message.id, getPrompt(message.params || {}));
    }
    return replyError(message.id, -32601, "method_not_found");
  } catch (error) {
    return replyError(message.id, -32000, error && error.message ? error.message : String(error));
  }
}

function readResource(params) {
  const uri = String(params.uri || "");
  if (uri === "codeseex-http-smoke://status") {
    return {
      contents: [{
        uri,
        mimeType: "text/plain",
        text: "codeseex-http-mcp-resource-ok",
      }],
    };
  }
  const echoMatch = uri.match(/^codeseex-http-smoke:\/\/echo\/(.+)$/);
  if (echoMatch) {
    return {
      contents: [{
        uri,
        mimeType: "text/plain",
        text: "codeseex-http-mcp-template-ok:" + decodeURIComponent(echoMatch[1]),
      }],
    };
  }
  throw new Error("unknown_resource:" + uri);
}

function getPrompt(params) {
  const name = String(params.name || "");
  if (name !== "codeseex_http_smoke_prompt") throw new Error("unknown_prompt:" + name);
  const args = params.arguments && typeof params.arguments === "object" ? params.arguments : {};
  return {
    description: "CodeSeeX HTTP smoke prompt",
    messages: [{
      role: "user",
      content: {
        type: "text",
        text: "codeseex-http-mcp-prompt-ok:" + String(args.topic || "default"),
      },
    }],
  };
}

function callTool(params) {
  const name = String(params.name || "");
  const args = params.arguments && typeof params.arguments === "object" ? params.arguments : {};
  if (name === "codeseex_http_smoke_echo") {
    return textResult("codeseex-http-mcp-ok:" + String(args.text || ""));
  }
  if (name === "codeseex_http_smoke_add") {
    const a = Number(args.a);
    const b = Number(args.b);
    if (!Number.isFinite(a) || !Number.isFinite(b)) throw new Error("a and b must be numbers");
    return textResult(String(a + b));
  }
  throw new Error("unknown_tool:" + name);
}

function textResult(text) {
  return {
    content: [{ type: "text", text }],
    isError: false,
  };
}

function reply(id, result) {
  return { jsonrpc: "2.0", id, result };
}

function replyError(id, code, message) {
  return { jsonrpc: "2.0", id, error: { code, message } };
}

function sendJson(res, status, body, headers = {}) {
  res.writeHead(status, Object.assign({ "Content-Type": "application/json" }, headers));
  res.end(JSON.stringify(body));
}

function sendSse(res, message) {
  res.writeHead(200, sessionHeaders({
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
  }));
  res.end("event: message\ndata: " + JSON.stringify(message) + "\n\n");
}

function sessionHeaders(headers = {}) {
  return Object.assign({ "Mcp-Session-Id": "codeseex-http-smoke-session" }, headers);
}

function parseJson(raw) {
  try {
    return JSON.parse(String(raw || ""));
  } catch {
    return null;
  }
}

if (require.main === module) {
  const port = Number(process.env.PORT || "0") || 0;
  const server = createHttpSmokeServer({ sse: process.env.MCP_SMOKE_SSE === "1" });
  server.listen(port, "127.0.0.1", () => {
    const address = server.address();
    console.log(JSON.stringify({ port: address && address.port }));
  });
}

module.exports = {
  createHttpSmokeServer,
};
