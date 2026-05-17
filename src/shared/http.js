const crypto = require("node:crypto");

async function readJsonBody(req, limitBytes) {
  const chunks = [];
  let size = 0;
  for await (const chunk of req) {
    size += chunk.length;
    if (size > limitBytes) throw httpError(413, "Request body is too large.", "invalid_request_error", "request_too_large");
    chunks.push(chunk);
  }
  const text = Buffer.concat(chunks).toString("utf8").trim();
  if (!text) return {};
  try {
    return JSON.parse(text);
  } catch {
    throw httpError(400, "Request body is not valid JSON.", "invalid_request_error", "invalid_json");
  }
}

async function parseJsonResponse(response) {
  const text = await response.text();
  if (!text) return {};
  try {
    return JSON.parse(text);
  } catch {
    return { raw: text };
  }
}

function sendJson(res, status, body) {
  const json = JSON.stringify(body, null, 2);
  res.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Content-Length": Buffer.byteLength(json),
  });
  res.end(json);
}

function addCorsHeaders(req, res, options = {}) {
  const origin = req && req.headers ? req.headers.origin : "";
  if (origin && isAllowedOrigin(req, origin, options)) {
    res.setHeader("Access-Control-Allow-Origin", origin);
    res.setHeader("Vary", "Origin");
    res.setHeader("Access-Control-Allow-Private-Network", "true");
  }
  res.setHeader("Access-Control-Allow-Headers", "Authorization, Content-Type, X-Requested-With");
  res.setHeader("Access-Control-Allow-Methods", "GET,POST,DELETE,OPTIONS");
}

function enforceLocalAccess(req, res, options = {}) {
  if (!isLoopbackAddress(req && req.socket ? req.socket.remoteAddress : "")) {
    sendJson(res, 403, errorBody("Only local loopback clients may access this service.", "invalid_request_error", "forbidden_remote_client"));
    return false;
  }

  const origin = req && req.headers ? req.headers.origin : "";
  if (origin && !isAllowedOrigin(req, origin, options)) {
    sendJson(res, 403, errorBody("Cross-origin access is not allowed.", "invalid_request_error", "forbidden_origin"));
    return false;
  }

  addCorsHeaders(req, res, options);
  return true;
}

function isAllowedOrigin(req, origin, options = {}) {
  if (!origin) return true;
  if (options.allowDesktopAppOrigins && isDesktopAppOrigin(origin)) return true;
  try {
    const url = new URL(origin);
    const host = String(req && req.headers ? req.headers.host || "" : "").toLowerCase();
    return (url.protocol === "http:" || url.protocol === "https:")
      && url.host.toLowerCase() === host
      && isLoopbackHostname(url.hostname);
  } catch {
    return false;
  }
}

function isDesktopAppOrigin(origin) {
  const value = String(origin || "").toLowerCase();
  return value === "null"
    || value === "file://"
    || value.startsWith("app://")
    || value.startsWith("vscode-file://");
}

function isLoopbackAddress(address) {
  const value = String(address || "").toLowerCase();
  return value === "127.0.0.1"
    || value === "::1"
    || value === "::ffff:127.0.0.1"
    || value === "localhost"
    || value.startsWith("::ffff:127.");
}

function isLoopbackHostname(hostname) {
  const value = String(hostname || "").toLowerCase();
  return value === "localhost"
    || value === "[::1]"
    || value === "::1"
    || /^127(?:\.\d{1,3}){3}$/.test(value);
}

function emitSse(res, eventName, payload) {
  res.write("event: " + eventName + "\n");
  res.write("data: " + JSON.stringify(payload) + "\n\n");
}

function httpError(status, message, type = "server_error", code = "error") {
  const error = new Error(message);
  error.status = status;
  error.type = type;
  error.code = code;
  return error;
}

function errorBody(message, type, code) {
  return { error: { message, type, code } };
}

function handleHttpError(res, error) {
  const status = Number(error.status) || 500;
  if (!res.headersSent) {
    sendJson(res, status, errorBody(error.message || "Internal server error.", error.type || "server_error", error.code || "internal_error"));
    return;
  }
  try {
    res.end();
  } catch {}
}

function makeId(prefix) {
  return prefix + "_" + crypto.randomBytes(12).toString("hex");
}

function nowSeconds() {
  return Math.floor(Date.now() / 1000);
}

function createSequence() {
  let value = 0;
  return {
    next() {
      value += 1;
      return value;
    },
  };
}

module.exports = {
  addCorsHeaders,
  createSequence,
  enforceLocalAccess,
  emitSse,
  errorBody,
  handleHttpError,
  httpError,
  makeId,
  nowSeconds,
  parseJsonResponse,
  readJsonBody,
  sendJson,
};
