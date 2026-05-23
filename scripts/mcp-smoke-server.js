"use strict";

// Minimal stdio MCP server for CodeSeeX compatibility testing.
// It intentionally has no project-specific behavior: Codex discovers tools,
// invokes them, and CodeSeeX must preserve the native name + namespace routing.

const TOOLS = [
  {
    name: "codeseex_smoke_echo",
    description: "Return the provided text with a CodeSeeX MCP smoke-test marker.",
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
    name: "codeseex_smoke_add",
    description: "Add two numbers and return the numeric result as text.",
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
    uri: "codeseex-smoke://status",
    name: "CodeSeeX MCP smoke status",
    description: "Static status resource for MCP resource-read compatibility tests.",
    mimeType: "text/plain",
  },
];
const RESOURCE_TEMPLATES = [
  {
    uriTemplate: "codeseex-smoke://echo/{text}",
    name: "CodeSeeX MCP smoke echo template",
    description: "Echo a path variable from a templated MCP resource URI.",
    mimeType: "text/plain",
  },
];
const PROMPTS = [
  {
    name: "codeseex_smoke_prompt",
    description: "Return a smoke-test prompt message.",
    arguments: [
      {
        name: "topic",
        description: "Prompt topic.",
        required: false,
      },
    ],
  },
];

let buffer = Buffer.alloc(0);

process.stdin.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  drainMessages();
});

function drainMessages() {
  while (buffer.length > 0) {
    const framed = tryReadContentLengthMessage();
    if (framed) {
      handleMessage(framed.message, "framed");
      continue;
    }

    const lineEnd = buffer.indexOf(10);
    if (lineEnd === -1) return;
    const raw = buffer.slice(0, lineEnd).toString("utf8").trim();
    buffer = buffer.slice(lineEnd + 1);
    if (!raw) continue;
    handleRawMessage(raw, "line");
  }
}

function tryReadContentLengthMessage() {
  const headerEnd = buffer.indexOf(Buffer.from("\r\n\r\n"));
  if (headerEnd === -1) return null;
  const header = buffer.slice(0, headerEnd).toString("utf8");
  const match = header.match(/(?:^|\r\n)Content-Length:\s*(\d+)/i);
  if (!match) return null;
  const length = Number(match[1]);
  if (!Number.isFinite(length) || length < 0) return null;
  const bodyStart = headerEnd + 4;
  const bodyEnd = bodyStart + length;
  if (buffer.length < bodyEnd) return null;
  const raw = buffer.slice(bodyStart, bodyEnd).toString("utf8");
  buffer = buffer.slice(bodyEnd);
  return { message: parseJson(raw), style: "framed" };
}

function handleRawMessage(raw, style) {
  handleMessage(parseJson(raw), style);
}

function handleMessage(message, style) {
  if (!message || typeof message !== "object") return;
  if (!message.id) return;

  try {
    if (message.method === "initialize") {
      reply(style, message.id, {
        protocolVersion: "2024-11-05",
        capabilities: {
          tools: {},
          resources: {},
        },
        serverInfo: { name: "codeseex-smoke", version: "0.0.0" },
      });
      return;
    }

    if (message.method === "tools/list") {
      reply(style, message.id, { tools: TOOLS });
      return;
    }

    if (message.method === "tools/call") {
      reply(style, message.id, callTool(message.params || {}));
      return;
    }

    if (message.method === "resources/list") {
      reply(style, message.id, { resources: RESOURCES });
      return;
    }

    if (message.method === "resources/templates/list") {
      reply(style, message.id, { resourceTemplates: RESOURCE_TEMPLATES });
      return;
    }

    if (message.method === "resources/read") {
      reply(style, message.id, readResource(message.params || {}));
      return;
    }

    if (message.method === "prompts/list") {
      reply(style, message.id, { prompts: PROMPTS });
      return;
    }

    if (message.method === "prompts/get") {
      reply(style, message.id, getPrompt(message.params || {}));
      return;
    }

    replyError(style, message.id, -32601, "method_not_found");
  } catch (error) {
    replyError(style, message.id, -32000, error && error.message ? error.message : String(error));
  }
}

function getPrompt(params) {
  const name = String(params.name || "");
  if (name !== "codeseex_smoke_prompt") throw new Error("unknown_prompt:" + name);
  const args = params.arguments && typeof params.arguments === "object" ? params.arguments : {};
  return {
    description: "CodeSeeX smoke prompt",
    messages: [{
      role: "user",
      content: {
        type: "text",
        text: "codeseex-mcp-prompt-ok:" + String(args.topic || "default"),
      },
    }],
  };
}

function readResource(params) {
  const uri = String(params.uri || "");
  if (uri === "codeseex-smoke://status") {
    return {
      contents: [{
        uri,
        mimeType: "text/plain",
        text: "codeseex-mcp-resource-ok",
      }],
    };
  }
  const echoMatch = uri.match(/^codeseex-smoke:\/\/echo\/(.+)$/);
  if (echoMatch) {
    return {
      contents: [{
        uri,
        mimeType: "text/plain",
        text: "codeseex-mcp-template-ok:" + decodeURIComponent(echoMatch[1]),
      }],
    };
  }
  throw new Error("unknown_resource:" + uri);
}

function callTool(params) {
  const name = String(params.name || "");
  const args = params.arguments && typeof params.arguments === "object" ? params.arguments : {};
  if (name === "codeseex_smoke_echo") {
    return textResult("codeseex-mcp-ok:" + String(args.text || ""));
  }
  if (name === "codeseex_smoke_add") {
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

function reply(style, id, result) {
  writeMessage(style, { jsonrpc: "2.0", id, result });
}

function replyError(style, id, code, message) {
  writeMessage(style, { jsonrpc: "2.0", id, error: { code, message } });
}

function writeMessage(style, message) {
  const raw = JSON.stringify(message);
  if (style === "framed") {
    process.stdout.write("Content-Length: " + Buffer.byteLength(raw, "utf8") + "\r\n\r\n" + raw);
    return;
  }
  process.stdout.write(raw + "\n");
}

function parseJson(raw) {
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}
