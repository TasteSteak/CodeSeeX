"use strict";

const fs = require("node:fs");
const path = require("node:path");

const { appendJsonl, readJsonlTail } = require("./json-store");

const EVENT_LOG_PREFIX = "logs-";
const EVENT_LOG_EXTENSION = ".jsonl";
const LEGACY_EVENT_LOG_FILE = "events.jsonl";

function eventLogDir(rootDir, dataDir = rootDir) {
  const baseDir = path.resolve(dataDir || rootDir) === path.resolve(rootDir) ? rootDir : dataDir;
  return path.join(baseDir, "logs");
}

function eventLogPath(rootDir, dataDir = rootDir, date = new Date()) {
  return path.join(eventLogDir(rootDir, dataDir), eventLogFileName(date));
}

function eventLogFileName(date = new Date()) {
  return EVENT_LOG_PREFIX + isoDate(date) + EVENT_LOG_EXTENSION;
}

function appendEventLog(rootDir, dataDir, event, options = {}) {
  const dir = eventLogDir(rootDir, dataDir);
  const filePath = path.join(dir, eventLogFileName(event && event.ts ? new Date(event.ts) : new Date()));
  appendJsonl(filePath, event, {
    retentionDays: Number(options.retentionDays) || 7,
    maxLines: Number(options.maxLines) || 5000,
    maxBytes: Number(options.maxBytes) || 5 * 1024 * 1024,
    forcePrune: Boolean(options.forcePrune),
  });
  pruneEventLogs(dir, options);
  return filePath;
}

function readEventLogTail(rootDir, dataDir, limit, before = null) {
  const output = [];
  for (const filePath of eventLogFiles(rootDir, dataDir).reverse()) {
    if (output.length >= limit) break;
    const batch = readJsonlTail(filePath, Math.max(1, limit - output.length), before);
    output.unshift(...batch);
    if (batch.length > 0) before = batch[0].ts || before;
  }
  return output.slice(-limit);
}

function pruneEventLogs(dir, options = {}) {
  const retentionDays = Number(options.retentionDays);
  if (!Number.isFinite(retentionDays) || retentionDays <= 0) return;
  const cutoff = startOfUtcDay(Date.now() - retentionDays * 24 * 60 * 60 * 1000);
  for (const filePath of eventLogFilesInDir(dir)) {
    const date = eventLogDateFromPath(filePath);
    if (!date || date.getTime() >= cutoff) continue;
    try {
      fs.rmSync(filePath, { force: true });
    } catch {}
  }
}

function eventLogFiles(rootDir, dataDir = rootDir) {
  return eventLogFilesInDir(eventLogDir(rootDir, dataDir));
}

function eventLogFilesInDir(dir) {
  try {
    if (!fs.existsSync(dir)) return [];
    return fs.readdirSync(dir)
      .filter((name) => isEventLogFileName(name) || name === LEGACY_EVENT_LOG_FILE)
      .map((name) => path.join(dir, name))
      .sort(compareEventLogPaths);
  } catch {
    return [];
  }
}

function isEventLogFileName(name) {
  return new RegExp("^" + EVENT_LOG_PREFIX + "\\d{8}\\" + EVENT_LOG_EXTENSION + "$").test(String(name || ""));
}

function compareEventLogPaths(left, right) {
  const leftDate = eventLogDateFromPath(left);
  const rightDate = eventLogDateFromPath(right);
  const leftTime = leftDate ? leftDate.getTime() : 0;
  const rightTime = rightDate ? rightDate.getTime() : 0;
  if (leftTime !== rightTime) return leftTime - rightTime;
  return String(left).localeCompare(String(right));
}

function eventLogDateFromPath(filePath) {
  const name = path.basename(filePath);
  if (name === LEGACY_EVENT_LOG_FILE) return new Date(0);
  const match = /^logs-(\d{4})(\d{2})(\d{2})\.jsonl$/.exec(name);
  if (!match) return null;
  const date = new Date(match[1] + "-" + match[2] + "-" + match[3] + "T00:00:00.000Z");
  return Number.isFinite(date.getTime()) ? date : null;
}

function isoDate(date) {
  const value = date instanceof Date ? date : new Date(date);
  if (!Number.isFinite(value.getTime())) return compactDate(new Date());
  return compactDate(value);
}

function compactDate(value) {
  return value.toISOString().slice(0, 10).replace(/-/g, "");
}

function startOfUtcDay(timeMs) {
  const date = new Date(timeMs);
  return Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate());
}

module.exports = {
  appendEventLog,
  eventLogDir,
  eventLogPath,
  eventLogFiles,
  eventLogFileName,
  pruneEventLogs,
  readEventLogTail,
};
