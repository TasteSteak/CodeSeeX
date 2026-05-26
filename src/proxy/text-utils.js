const crypto = require("node:crypto");

function extractText(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return String(content || "");
  return content.map((part) => {
    if (typeof part === "string") return part;
    if (!part || typeof part !== "object") return "";
    if (part.type === "input_text" || part.type === "output_text") return part.text || "";
    if (part.type === "refusal") return part.refusal || "";
    return "";
  }).join("");
}

function sanitizeLargeBinaryText(value) {
  let output = String(value || "");
  output = output.replace(/data:([a-z0-9.+-]+\/[a-z0-9.+-]+);base64,([A-Za-z0-9+/=\r\n]{256,})/gi, (_match, mime, data) => {
    return binaryPlaceholder({ mime, encoding: "base64-data-url", data });
  });
  output = output.replace(/(["'](?:base64|image_base64|screenshot_base64|data_base64|blob_base64)["']\s*:\s*["'])([A-Za-z0-9+/=\r\n]{4096,})(["'])/gi, (match, prefix, data, suffix) => {
    if (!looksLikeBase64(data)) return match;
    return prefix + binaryPlaceholder({ mime: "application/octet-stream", encoding: "base64-json-field", data }) + suffix;
  });
  output = output.replace(/\b([A-Za-z0-9+/]{8192,}={0,2})\b/g, (match, data) => {
    if (!looksLikeBase64(data)) return match;
    return binaryPlaceholder({ mime: "application/octet-stream", encoding: "base64-inline", data });
  });
  return output;
}

function toolOutputValueToText(value) {
  if (value === undefined || value === null) return "";
  if (typeof value === "string") return sanitizeLargeBinaryText(value);

  const typedText = typedContentToText(value);
  if (typedText !== null) return typedText;

  return safeStringifySanitizedToolOutput(value);
}

function typedContentToText(value) {
  const parts = typedContentParts(value);
  if (!parts) return null;

  const lines = [];
  for (const part of parts) {
    const text = typedContentPartToText(part);
    if (text) lines.push(text);
  }
  return sanitizeLargeBinaryText(lines.join("\n"));
}

function typedContentParts(value) {
  const parts = Array.isArray(value)
    ? value
    : value && typeof value === "object" && Array.isArray(value.content) ? value.content : null;
  if (!parts) return null;
  return isTypedContentArray(parts) ? parts : null;
}

function isTypedContentArray(parts) {
  if (!Array.isArray(parts) || parts.length === 0) return false;
  let typedCount = 0;
  for (const part of parts) {
    if (typeof part === "string") continue;
    if (!isTypedContentPart(part)) return false;
    typedCount += 1;
  }
  return typedCount > 0;
}

function isTypedContentPart(part) {
  if (!part || typeof part !== "object") return false;
  const type = String(part.type || "").toLowerCase();
  return type === "input_text"
    || type === "output_text"
    || type === "text"
    || type === "summary_text"
    || type === "refusal"
    || type === "input_image"
    || type === "output_image"
    || type === "image"
    || type === "input_file"
    || type === "output_file"
    || type === "file"
    || type === "attachment";
}

function typedContentPartToText(part) {
  if (typeof part === "string") return part;
  if (!part || typeof part !== "object") return "";
  const type = String(part.type || "").toLowerCase();

  if ((type === "input_text" || type === "output_text" || type === "text" || type === "summary_text") && typeof part.text === "string") {
    return part.text;
  }
  if (type === "refusal" && typeof part.refusal === "string") return part.refusal;
  if (type === "input_image" || type === "output_image" || type === "image") {
    return describeImagePart(part);
  }
  if (type === "input_file" || type === "output_file" || type === "file" || type === "attachment") {
    return describeAttachmentPart(part);
  }
  return "";
}

function safeStringifySanitizedToolOutput(value) {
  try {
    return JSON.stringify(sanitizeToolOutputJson(value));
  } catch {
    return sanitizeLargeBinaryText(String(value || ""));
  }
}

function sanitizeToolOutputJson(value, seen = new WeakSet()) {
  if (value === undefined || value === null) return value;
  if (typeof value === "string") return sanitizeLargeBinaryText(value);
  if (typeof value === "bigint") return value.toString();
  if (typeof value !== "object") return value;

  const typedText = typedContentToText(value);
  if (typedText !== null) return typedText;

  if (typeof Buffer !== "undefined" && Buffer.isBuffer(value)) {
    return "[binary payload omitted: " + binaryDescriptor({
      mime: "application/octet-stream",
      encoding: "buffer",
      bytes: value.length,
      hash: hashText(value),
    }) + "]";
  }
  if (seen.has(value)) return "[circular reference omitted]";
  seen.add(value);

  if (Array.isArray(value)) {
    const mapped = value.map((child) => sanitizeToolOutputJson(child, seen));
    seen.delete(value);
    return mapped;
  }
  if (isBufferJson(value)) {
    seen.delete(value);
    return "[binary payload omitted: " + binaryDescriptor({
      mime: "application/octet-stream",
      encoding: "buffer-json",
      bytes: value.data.length,
      hash: hashText(value.data.join(",")),
    }) + "]";
  }

  const copy = {};
  for (const [key, child] of Object.entries(value)) {
    if (typeof child === "string" && isImageLikeKey(key)) {
      copy[key] = describeImageReference(child);
      continue;
    }
    if (typeof child === "string" && isBase64LikeKey(key) && (isDataUrl(child) || looksLikeBase64(child))) {
      copy[key] = "[binary payload omitted: " + binaryDescriptor({
        mime: mimeFromDataUrl(child) || "application/octet-stream",
        encoding: isDataUrl(child) ? "base64-data-url" : key,
        data: payloadFromDataUrl(child) || child,
      }) + "]";
      continue;
    }
    copy[key] = sanitizeToolOutputJson(child, seen);
  }
  seen.delete(value);
  return copy;
}

function describeImagePart(part) {
  const reference = part.image_url || part.imageUrl || part.url || part.source || part.data || "";
  const detail = part.detail ? " detail=" + String(part.detail).slice(0, 40) : "";
  return "[image omitted: " + describeImageReference(reference) + detail + "]";
}

function describeAttachmentPart(part) {
  const contentType = part.content_type || part.contentType || part.mimeType || "application/octet-stream";
  const length = part.length || part.bytes || "";
  const name = part.filename || part.name || "";
  return "[attachment omitted: mime=" + String(contentType).toLowerCase()
    + (length ? " bytes=" + Number(length) : "")
    + (name ? " name=" + String(name).slice(0, 120) : "")
    + "]";
}

function describeImageReference(value) {
  if (value && typeof value === "object") {
    if (value.url || value.image_url || value.imageUrl || value.data) {
      return describeImageReference(value.url || value.image_url || value.imageUrl || value.data);
    }
    if (value.file_id || value.fileId) return "file_id=" + String(value.file_id || value.fileId).slice(0, 120);
    return "source=object";
  }

  const text = String(value || "");
  if (!text) return "source=unknown";
  if (isDataUrl(text)) {
    return binaryDescriptor({
      mime: mimeFromDataUrl(text) || "application/octet-stream",
      encoding: "base64-data-url",
      data: payloadFromDataUrl(text),
    });
  }
  if (looksLikeBase64(text)) {
    return binaryDescriptor({ mime: "image/*", encoding: "base64", data: text });
  }
  return "url=" + text.slice(0, 240);
}

function binaryPlaceholder({ mime, encoding, data }) {
  return "[binary payload omitted: " + binaryDescriptor({ mime, encoding, data }) + "]";
}

function binaryDescriptor({ mime, encoding, data, bytes, hash }) {
  const normalized = data === undefined ? "" : String(data || "").replace(/\s+/g, "");
  const size = bytes !== undefined ? Number(bytes) : estimateBase64DecodedBytes(normalized);
  const digest = hash || hashText(normalized);
  return "mime=" + String(mime || "application/octet-stream").toLowerCase()
    + " encoding=" + String(encoding || "base64")
    + " base64_chars=" + (data === undefined ? 0 : normalized.length)
    + " decoded_bytes~=" + size
    + " sha256=" + digest;
}

function looksLikeBase64(value) {
  const text = String(value || "").replace(/\s+/g, "");
  if (text.length < 256 || text.length % 4 === 1) return false;
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(text)) return false;
  const sample = text.slice(0, 256);
  const unique = new Set(sample).size;
  return unique >= 12;
}

function isDataUrl(value) {
  return /^data:([^;,]+)(?:;[^,]*)?;base64,/i.test(String(value || ""));
}

function mimeFromDataUrl(value) {
  const match = String(value || "").match(/^data:([^;,]+)(?:;[^,]*)?;base64,/i);
  return match ? match[1] : "";
}

function payloadFromDataUrl(value) {
  const match = String(value || "").match(/^data:[^,]*;base64,([\s\S]*)$/i);
  return match ? match[1] : "";
}

function isImageLikeKey(key) {
  return /(?:image_url|imageUrl|image|screenshot|thumbnail|preview)$/i.test(String(key || ""));
}

function isBase64LikeKey(key) {
  return /(?:base64|blob|bytes|data)$/i.test(String(key || ""));
}

function isBufferJson(value) {
  return value
    && value.type === "Buffer"
    && Array.isArray(value.data)
    && value.data.length > 256
    && value.data.every((item) => Number.isInteger(item) && item >= 0 && item <= 255);
}

function estimateBase64DecodedBytes(value) {
  const text = String(value || "").replace(/\s+/g, "");
  if (!text) return 0;
  const padding = text.endsWith("==") ? 2 : text.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor(text.length * 3 / 4) - padding);
}

function hashText(value) {
  return crypto.createHash("sha256").update(String(value || "")).digest("hex").slice(0, 16);
}

function stripDsmlToolBlocks(text, options = {}) {
  let output = String(text || "");
  output = output.replace(/<[^>]*DSML[^>]*tool_calls[^>]*>[\s\S]*?<\/[^>]*DSML[^>]*tool_calls[^>]*>/gi, "");
  output = output.replace(/<[^>]*DSML[^>]*tool_results[^>]*>[\s\S]*?<\/[^>]*DSML[^>]*tool_results[^>]*>/gi, "");
  output = output.replace(/<｜｜DSML｜｜tool_calls>[\s\S]*?<｜｜DSML｜｜\/tool_calls>/gi, "");
  output = output.replace(/<｜｜DSML｜｜tool_results>[\s\S]*?<｜｜DSML｜｜\/tool_results>/gi, "");
  output = stripUnclosedDsmlToolBlock(output, "tool_calls");
  output = stripUnclosedDsmlToolBlock(output, "tool_results");
  return options.preserveWhitespace ? output : output.trim();
}

const DSML_STREAM_TAIL_CHARS = 512;

function createDsmlToolBlockStripper() {
  let buffer = "";
  let blockType = "";

  function push(text) {
    buffer += String(text || "");
    return drain(false);
  }

  function flush() {
    const output = drain(true);
    buffer = "";
    blockType = "";
    return output;
  }

  function drain(flushAll) {
    let output = "";

    while (buffer) {
      if (blockType) {
        const close = findDsmlCloseTag(buffer, blockType);
        if (!close) {
          if (flushAll) buffer = "";
          else if (buffer.length > DSML_STREAM_TAIL_CHARS) buffer = buffer.slice(-DSML_STREAM_TAIL_CHARS);
          break;
        }
        buffer = buffer.slice(close.end);
        blockType = "";
        continue;
      }

      const open = findDsmlOpenTag(buffer);
      if (!open) {
        const safeLength = flushAll ? buffer.length : safeDsmlPrefixLength(buffer);
        if (safeLength > 0) {
          output += buffer.slice(0, safeLength);
          buffer = buffer.slice(safeLength);
        }
        break;
      }

      if (open.index > 0) output += buffer.slice(0, open.index);
      buffer = buffer.slice(open.end);
      blockType = open.type;
    }

    return output;
  }

  return { push, flush };
}

function stripUnclosedDsmlToolBlock(text, type) {
  const source = String(text || "");
  const open = findDsmlOpenTag(source, type);
  if (!open) return source;
  return source.slice(0, open.index);
}

function findDsmlOpenTag(text, expectedType = "") {
  return findDsmlTag(text, (tag) => {
    const parsed = parseDsmlTag(tag);
    return parsed && !parsed.close && (!expectedType || parsed.type === expectedType);
  });
}

function findDsmlCloseTag(text, expectedType) {
  return findDsmlTag(text, (tag) => {
    const parsed = parseDsmlTag(tag);
    return parsed && parsed.close && parsed.type === expectedType;
  });
}

function findDsmlTag(text, predicate) {
  const pattern = /<[^>]*DSML[^>]*(?:tool_calls|tool_results)[^>]*>/gi;
  let match;
  while ((match = pattern.exec(String(text || ""))) !== null) {
    if (!predicate(match[0])) continue;
    const parsed = parseDsmlTag(match[0]);
    return {
      index: match.index,
      end: match.index + match[0].length,
      type: parsed.type,
    };
  }
  return null;
}

function parseDsmlTag(tag) {
  const value = String(tag || "").toLowerCase();
  const type = value.includes("tool_calls") ? "tool_calls" : (value.includes("tool_results") ? "tool_results" : "");
  if (!type) return null;
  return {
    type,
    close: /^<\s*\//.test(value) || new RegExp("dsml[^>]*/\\s*" + type, "i").test(value),
  };
}

function safeDsmlPrefixLength(text) {
  const source = String(text || "");
  const lastTagStart = source.lastIndexOf("<");
  if (lastTagStart === -1) return source.length;
  if (source.length - lastTagStart > DSML_STREAM_TAIL_CHARS) return source.length;
  return lastTagStart;
}

function extractTaggedThinking(text) {
  const reasoning = [];
  const content = stripDsmlToolBlocks(text).replace(/<\s*(think|thinking|thought|reasoning)\s*>[\s\S]*?<\s*\/\s*\1\s*>/gi, (match, tag) => {
    const pattern = new RegExp("^<\\s*" + tag + "\\s*>|<\\s*\\/\\s*" + tag + "\\s*>$", "gi");
    const inner = match.replace(pattern, "").trim();
    if (inner) reasoning.push(inner);
    return "";
  });
  return { content: content.trim(), reasoning: reasoning.join("\n\n") };
}

function normalizeReasoningText(text) {
  return String(text || "").replace(/\r\n/g, "\n").replace(/\r/g, "\n").trim();
}

function renderVisibleContent(text) {
  const body = String(text || "").replace(/\r\n/g, "\n").replace(/\r/g, "\n").trim();
  return body;
}

function renderProxyToolCallDisplay(item) {
  const name = item && item.name ? item.name : "tool";
  const lines = ["> \u4f7f\u7528\u5de5\u5177 `" + escapeInlineCode(name) + "`"];
  const args = parseJsonObject(item && item.arguments);
  for (const key of Object.keys(args).slice(0, 8)) {
    const value = compactToolValue(args[key]);
    if (value) lines.push("> " + key + " `" + escapeInlineCode(value) + "`");
  }
  return lines.join("\n");
}

function compactToolValue(value) {
  if (value === undefined || value === null) return "";
  const raw = typeof value === "string" ? value : JSON.stringify(value);
  return String(raw || "").replace(/\s+/g, " ").trim().slice(0, 300);
}

function escapeInlineCode(value) {
  return String(value || "").replace(/`/g, "\\`");
}

function parseJsonObject(value) {
  if (value && typeof value === "object" && !Array.isArray(value)) return value;
  try {
    const parsed = JSON.parse(String(value || "{}"));
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

function normalizeFileLinks(text) {
  let output = String(text || "");

  for (let i = 0; i < 4; i += 1) {
    const next = output.replace(/\[\[([^\]]+)\]\(([^)]+)\)\]\(([^)]+)\)/g, "[$1]($2)");
    if (next === output) break;
    output = next;
  }

  output = output.replace(/`([A-Za-z]:[\\/][^`\r\n]+?)`/g, (match, rawPath) => {
    if (/^\[[^\]]+\]\([^)]+\)$/.test(rawPath)) return match;
    return markdownLinkForPath(rawPath);
  });

  output = output.replace(/\[([^\]]+)\]\(([A-Za-z]:\\[^)]+)\)/g, (match, label, target) => {
    return "[" + label + "](" + encodeMarkdownTarget(target.replace(/\\/g, "/")) + ")";
  });

  return output;
}

function markdownLinkForPath(rawPath) {
  const normalized = String(rawPath || "").replace(/\\/g, "/");
  const label = normalized.split("/").filter(Boolean).pop() || normalized;
  return "[" + escapeMarkdownLabel(label) + "](" + encodeMarkdownTarget(normalized) + ")";
}

function escapeMarkdownLabel(label) {
  return String(label || "").replace(/([\\\]\[])/g, "\\$1");
}

function encodeMarkdownTarget(target) {
  return String(target || "").replace(/\)/g, "%29").replace(/\s/g, "%20");
}

module.exports = {
  createDsmlToolBlockStripper,
  extractTaggedThinking,
  extractText,
  normalizeFileLinks,
  normalizeReasoningText,
  renderProxyToolCallDisplay,
  renderVisibleContent,
  sanitizeLargeBinaryText,
  sanitizeToolXmlInText,
  stripDsmlToolBlocks,
  toolOutputValueToText,
};
function sanitizeToolXmlInText(text, hasRealToolCalls) {
  if (hasRealToolCalls) return String(text || "");
  const raw = String(text || "");
  if (!/<\s*\/?\s*(tool_calls|invoke|parameter|\/tool_calls|\/invoke|\/parameter)\b/i.test(raw)) return raw;

  let fixed = raw
    .replace(/<(\s*\/?\s*(?:tool_calls|invoke|parameter|\/tool_calls|\/invoke|\/parameter)\b[^>]*)>/gi, "&lt;$1&gt;")
    .replace(/<\/(\s*(?:tool_calls|invoke|parameter)\s*)>/gi, "&lt;/$1&gt;");

  return fixed;
}
