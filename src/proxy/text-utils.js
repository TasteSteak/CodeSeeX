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
  sanitizeToolXmlInText,
  stripDsmlToolBlocks,
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
