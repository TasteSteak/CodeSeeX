const fs = require("node:fs");
const path = require("node:path");

const DEFAULT_JSONL_MAX_LINES = 5000;
const DEFAULT_JSONL_MAX_BYTES = 5 * 1024 * 1024;
const JSONL_TAIL_CHUNK_BYTES = 64 * 1024;
const JSONL_PRUNE_INTERVAL_MS = 30000;
const lastPruneByFile = new Map();

function readJson(filePath, fallback) {
  try {
    if (!fs.existsSync(filePath)) return fallback;
    const parsed = JSON.parse(fs.readFileSync(filePath, "utf8"));
    return parsed && typeof parsed === "object" ? parsed : fallback;
  } catch {
    return fallback;
  }
}

function readJsonStrict(filePath, fallback) {
  if (!fs.existsSync(filePath)) return fallback;
  let parsed;
  try {
    parsed = JSON.parse(fs.readFileSync(filePath, "utf8"));
  } catch (error) {
    const wrapped = new Error("JSON file is invalid: " + filePath + "; " + (error && error.message ? error.message : String(error)));
    wrapped.code = "JSON_STORE_INVALID";
    wrapped.path = filePath;
    wrapped.cause = error;
    throw wrapped;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    const error = new Error("JSON file must contain an object: " + filePath);
    error.code = "JSON_STORE_INVALID";
    error.path = filePath;
    throw error;
  }
  return parsed;
}

function writeJson(filePath, value) {
  return writeJsonText(filePath, JSON.stringify(value, null, 2));
}

function writeJsonCompact(filePath, value) {
  return writeJsonText(filePath, JSON.stringify(value));
}

function writeJsonText(filePath, text) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  const tempPath = filePath + ".tmp-" + process.pid + "-" + Date.now();
  fs.writeFileSync(tempPath, text, "utf8");
  fs.renameSync(tempPath, filePath);
  return Buffer.byteLength(text, "utf8");
}

function appendJsonl(filePath, value, options = {}) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.appendFileSync(filePath, JSON.stringify(value) + "\n", "utf8");
  maybePruneJsonl(filePath, options);
}

function readJsonlTail(filePath, limit, beforeTs = null) {
  let fd = null;
  try {
    if (!fs.existsSync(filePath)) return [];
    const max = Math.max(1, Number(limit) || 80);
    const before = beforeTs ? String(beforeTs) : null;
    const stat = fs.statSync(filePath);
    if (!stat.size) return [];
    const output = [];
    fd = fs.openSync(filePath, "r");
    let position = stat.size;
    let pending = Buffer.alloc(0);
    while (position > 0 && output.length < max) {
      const readSize = Math.min(JSONL_TAIL_CHUNK_BYTES, position);
      position -= readSize;
      const buffer = Buffer.allocUnsafe(readSize);
      fs.readSync(fd, buffer, 0, readSize, position);
      const combined = pending.length > 0 ? Buffer.concat([buffer, pending]) : buffer;
      const parts = splitBufferLines(combined);
      pending = position > 0 ? parts.shift() || Buffer.alloc(0) : Buffer.alloc(0);
      collectTailLines(parts, output, max, before);
    }
    return output.reverse();
  } catch {
    return [];
  } finally {
    if (fd !== null) {
      try {
        fs.closeSync(fd);
      } catch {}
    }
  }
}

function splitBufferLines(buffer) {
  const lines = [];
  let end = buffer.length;
  for (let index = buffer.length - 1; index >= 0; index -= 1) {
    if (buffer[index] !== 0x0a) continue;
    const start = index + 1;
    lines.unshift(buffer.subarray(start, end));
    end = index > 0 && buffer[index - 1] === 0x0d ? index - 1 : index;
  }
  lines.unshift(buffer.subarray(0, end));
  return lines;
}

function collectTailLines(lineBuffers, output, max, before) {
  for (let index = lineBuffers.length - 1; index >= 0 && output.length < max; index -= 1) {
    const line = trimLineBuffer(lineBuffers[index]);
    if (!line.length) continue;
    const parsed = parseJsonLine(line.toString("utf8"));
    if (!parsed) continue;
    if (before && String(parsed.ts || "") >= before) continue;
    output.push(parsed);
  }
}

function trimLineBuffer(buffer) {
  let start = 0;
  let end = buffer.length;
  while (start < end && (buffer[start] === 0x0a || buffer[start] === 0x0d)) start += 1;
  while (end > start && (buffer[end - 1] === 0x0a || buffer[end - 1] === 0x0d)) end -= 1;
  return buffer.subarray(start, end);
}

function parseJsonLine(line) {
  try {
    return JSON.parse(line);
  } catch {
    return null;
  }
}

function maybePruneJsonl(filePath, options = {}) {
  const retentionDays = options.retentionDays;
  const days = Number(retentionDays);
  const maxLines = Math.max(1, Number(options.maxLines) || DEFAULT_JSONL_MAX_LINES);
  const maxBytes = Math.max(1024, Number(options.maxBytes) || DEFAULT_JSONL_MAX_BYTES);
  if (!Number.isFinite(days) || days <= 0) return;
  if (!shouldPruneJsonl(filePath, maxBytes, options)) return;
  pruneJsonl(filePath, { retentionDays: days, maxLines, maxBytes });
}

function shouldPruneJsonl(filePath, maxBytes, options = {}) {
  if (options.forcePrune) return true;
  try {
    if (!fs.existsSync(filePath)) return false;
    const now = Date.now();
    const last = lastPruneByFile.get(filePath) || 0;
    const stat = fs.statSync(filePath);
    if (stat.size > maxBytes) return true;
    if (now - last < JSONL_PRUNE_INTERVAL_MS) return false;
    lastPruneByFile.set(filePath, now);
    return true;
  } catch {
    return false;
  }
}

function pruneJsonl(filePath, options = {}) {
  const days = Number(options.retentionDays);
  const maxLines = Math.max(1, Number(options.maxLines) || DEFAULT_JSONL_MAX_LINES);
  const maxBytes = Math.max(1024, Number(options.maxBytes) || DEFAULT_JSONL_MAX_BYTES);
  if (!Number.isFinite(days) || days <= 0) return;
  try {
    if (!fs.existsSync(filePath)) return;
    const cutoff = Date.now() - days * 24 * 60 * 60 * 1000;
    const lines = fs.readFileSync(filePath, "utf8").split(/\r?\n/).filter(Boolean);
    let kept = lines.filter((line) => {
      const parsed = parseJsonLine(line);
      if (!parsed || !parsed.ts) return true;
      const time = new Date(parsed.ts).getTime();
      return !Number.isFinite(time) || time >= cutoff;
    });
    if (kept.length > maxLines) kept = kept.slice(-maxLines);
    while (kept.length > 1 && Buffer.byteLength(kept.join("\n") + "\n") > maxBytes) kept.shift();
    if (kept.length !== lines.length) fs.writeFileSync(filePath, kept.join("\n") + (kept.length ? "\n" : ""), "utf8");
    lastPruneByFile.set(filePath, Date.now());
  } catch {}
}

function cloneJson(value) {
  return JSON.parse(JSON.stringify(value));
}

module.exports = {
  appendJsonl,
  cloneJson,
  pruneJsonl,
  readJson,
  readJsonStrict,
  readJsonlTail,
  writeJson,
  writeJsonCompact,
};
